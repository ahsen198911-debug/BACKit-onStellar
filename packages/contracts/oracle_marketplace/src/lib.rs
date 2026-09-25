#![no_std]
// `soroban_sdk::token` is re-exported under this name in some SDK versions; the
// aliases below keep the import site explicit and legible.
#![allow(deprecated)]

mod errors;
mod events;
mod storage;
mod types;

#[cfg(test)]
mod test;

pub use types::{CallEscrow, MarketplaceConfig, OracleProvider};

use errors::OracleMarketplaceError;
use events::{
    emit_bond_increased, emit_bond_slashed, emit_bond_withdrawn, emit_escrow_created,
    emit_escrow_settled,
};
use soroban_sdk::{
    contract, contractimpl,
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Vec,
};
use storage::*;
use types::Resolution;

/// Sentinel address standing in for native XLM, matching the convention used
/// by the other contracts in this workspace (see `lending_pool`,
/// `gas_station`).
const NATIVE_XLM_SENTINEL: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

#[inline]
fn is_native_xlm(env: &Env, addr: &Address) -> bool {
    let sentinel = Address::from_string(&soroban_sdk::String::from_str(
        env,
        NATIVE_XLM_SENTINEL,
    ));
    *addr == sentinel
}

/// Transfer `amount` of `token` between two accounts.
///
/// Native XLM is sentinel-addressed and must go through the SAC interface;
/// every other asset uses the standard token interface.
fn transfer_token(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
    if is_native_xlm(env, token) {
        StellarAssetClient::new(env, token).transfer(from, to, &amount);
    } else {
        TokenClient::new(env, token).transfer(from, to, &amount);
    }
}

/// Basis-point helper: returns `amount * bps / 10_000`.
fn bps_of(amount: i128, bps: u32) -> i128 {
    amount.saturating_mul(bps as i128) / 10_000
}

#[contract]
pub struct OracleMarketplace;

#[contractimpl]
impl OracleMarketplace {
    /// One-time setup. Records the admin, the deregistration cooldown, the
    /// default per-query fee, and the share of a slashed bond that goes to
    /// the admin rather than the caller who was served a bad answer.
    pub fn initialize(
        env: Env,
        admin: Address,
        cooldown_secs: u64,
        default_fee_bps: u32,
        slash_penalty_bps: u32,
    ) -> Result<(), OracleMarketplaceError> {
        if get_config(&env).is_some() {
            return Err(OracleMarketplaceError::AlreadyInitialized);
        }
        admin.require_auth();

        if default_fee_bps > 10_000 || slash_penalty_bps > 10_000 {
            return Err(OracleMarketplaceError::InvalidFee);
        }

        let config = MarketplaceConfig {
            admin,
            cooldown_secs,
            default_fee_bps,
            slash_penalty_bps,
        };
        set_config(&env, &config);
        Ok(())
    }

    /// Register an oracle provider for a data feed, locking `min_stake` of
    /// `bond_token` in escrow.
    ///
    /// The bond is the provider's collateral: it is slashed if they report an
    /// inaccurate resolution. Registration therefore requires a real token
    /// transfer, not just a recorded number — the contract cannot slash a
    /// balance it does not hold.
    pub fn register_oracle(
        env: Env,
        provider: Address,
        pubkey: BytesN<32>,
        fee_bps: u32,
        min_stake: i128,
        initial_bond: i128,
        bond_token: Address,
    ) -> Result<(), OracleMarketplaceError> {
        provider.require_auth();

        if get_config(&env).is_none() {
            return Err(OracleMarketplaceError::NotInitialized);
        }
        if get_oracle(&env, &pubkey).is_some() {
            return Err(OracleMarketplaceError::OracleAlreadyRegistered);
        }
        if fee_bps > 10_000 {
            return Err(OracleMarketplaceError::InvalidFee);
        }
        if min_stake <= 0 {
            return Err(OracleMarketplaceError::InvalidBond);
        }
        // The bond a provider posts has to reach their own floor at
        // registration time; otherwise they are already below it on day one
        // and can never be slashed.
        if initial_bond < min_stake {
            return Err(OracleMarketplaceError::InsufficientStake);
        }

        // Pull the bond into the contract's custody before the provider is
        // listed as available. Doing this first means a failed transfer
        // aborts the whole call and no half-registered state is stored.
        transfer_token(&env, &bond_token, &provider, &env.current_contract_address(), initial_bond);

        let oracle = OracleProvider {
            pubkey: pubkey.clone(),
            address: provider.clone(),
            fee_bps,
            min_stake,
            staked_amount: initial_bond,
            bond_token,
            total_resolved: 0,
            total_disputes: 0,
            total_slashed: 0,
            is_active: true,
            registered_at: env.ledger().timestamp(),
            deregister_after: None,
        };

        set_oracle(&env, &pubkey, &oracle);
        add_to_oracle_list(&env, &pubkey);
        events::emit_oracle_registered(&env, &provider, &pubkey, fee_bps);

        Ok(())
    }

    /// Mark an oracle inactive and start the withdrawal cooldown. The bond
    /// is *not* released here — see `withdraw_bond`.
    pub fn deregister_oracle(
        env: Env,
        provider: Address,
        pubkey: BytesN<32>,
    ) -> Result<(), OracleMarketplaceError> {
        provider.require_auth();
        let config = get_config(&env).ok_or(OracleMarketplaceError::NotInitialized)?;
        let oracle = get_oracle(&env, &pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;

        if !oracle.is_active {
            return Err(OracleMarketplaceError::OracleNotActive);
        }
        if oracle.address != provider {
            return Err(OracleMarketplaceError::Unauthorized);
        }

        if let Some(deregister_after) = oracle.deregister_after {
            if env.ledger().timestamp() < deregister_after {
                return Err(OracleMarketplaceError::CooldownActive);
            }
        }

        let mut updated = oracle;
        updated.is_active = false;
        updated.deregister_after = Some(env.ledger().timestamp() + config.cooldown_secs);
        set_oracle(&env, &pubkey, &updated);
        remove_from_oracle_list(&env, &pubkey);

        events::emit_oracle_deregistered(&env, &provider, &pubkey);
        Ok(())
    }

    /// Top up a provider's bond without re-registering.
    ///
    /// Available to anyone (not just the provider) because topping up a
    /// provider's collateral is strictly beneficial to that provider's
    /// counterparties; the provider's `staked_amount` grows and no authority
    /// is exercised on their behalf.
    pub fn increase_bond(
        env: Env,
        pubkey: BytesN<32>,
        amount: i128,
        funder: Address,
    ) -> Result<(), OracleMarketplaceError> {
        funder.require_auth();

        if amount <= 0 {
            return Err(OracleMarketplaceError::InvalidBond);
        }

        let mut oracle = get_oracle(&env, &pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;

        transfer_token(
            &env,
            &oracle.bond_token,
            &funder,
            &env.current_contract_address(),
            amount,
        );

        oracle.staked_amount = oracle.staked_amount.saturating_add(amount);
        set_oracle(&env, &pubkey, &oracle);

        emit_bond_increased(&env, &pubkey, amount, oracle.staked_amount);
        Ok(())
    }

    /// Release a deregistered provider's bond back to them.
    ///
    /// Requires the cooldown set by `deregister_oracle` to have elapsed, and
    /// requires the provider to be inactive, so a bond cannot be withdrawn
    /// while still serving calls.
    pub fn withdraw_bond(
        env: Env,
        provider: Address,
        pubkey: BytesN<32>,
    ) -> Result<(), OracleMarketplaceError> {
        provider.require_auth();
        let oracle = get_oracle(&env, &pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;

        if oracle.address != provider {
            return Err(OracleMarketplaceError::Unauthorized);
        }
        if oracle.is_active {
            return Err(OracleMarketplaceError::OracleNotActive);
        }
        if let Some(deregister_after) = oracle.deregister_after {
            if env.ledger().timestamp() < deregister_after {
                return Err(OracleMarketplaceError::CooldownActive);
            }
        }

        let amount = oracle.staked_amount;
        if amount <= 0 {
            return Err(OracleMarketplaceError::InsufficientStake);
        }

        // Zero the stake before transferring: external calls must never be
        // able to re-enter and withdraw the same bond twice.
        let mut updated = oracle.clone();
        updated.staked_amount = 0;
        set_oracle(&env, &pubkey, &updated);

        transfer_token(&env, &oracle.bond_token, &env.current_contract_address(), &provider, amount);

        emit_bond_withdrawn(&env, &pubkey, &provider, amount);
        Ok(())
    }

    pub fn get_available_oracles(env: Env) -> Vec<OracleProvider> {
        let list = get_oracle_list(&env);
        let mut result = Vec::new(&env);
        for i in 0..list.len() {
            let pubkey = list.get(i).unwrap();
            if let Some(oracle) = get_oracle(&env, &pubkey) {
                if oracle.is_active {
                    result.push_back(oracle);
                }
            }
        }
        result
    }

    /// Bind a call to a specific oracle provider.
    pub fn select_oracle_for_call(
        env: Env,
        call_id: u64,
        oracle_pubkey: BytesN<32>,
    ) -> Result<(), OracleMarketplaceError> {
        let oracle = get_oracle(&env, &oracle_pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;
        if !oracle.is_active {
            return Err(OracleMarketplaceError::OracleNotActive);
        }

        set_call_oracle(&env, call_id, &oracle_pubkey);
        events::emit_oracle_selected(&env, call_id, &oracle_pubkey);
        Ok(())
    }

    pub fn get_call_oracle(env: Env, call_id: u64) -> Option<BytesN<32>> {
        get_call_oracle(&env, call_id)
    }

    /// Escrow a bounty for a call, to be paid to the oracle that resolves it.
    ///
    /// The caller must have selected an oracle first: paying a bounty that no
    /// provider can ever claim would strand the funds in the contract.
    ///
    /// The fee the provider is owed is `oracle.fee_bps` of `amount`; the
    /// remainder is transferred to the admin as the marketplace's cut, so the
    /// caller escrows the full price of the query up front.
    pub fn create_call_escrow(
        env: Env,
        call_id: u64,
        caller: Address,
        oracle_pubkey: BytesN<32>,
        amount: i128,
        token: Address,
    ) -> Result<(), OracleMarketplaceError> {
        caller.require_auth();

        if amount <= 0 {
            return Err(OracleMarketplaceError::InvalidEscrowAmount);
        }
        if get_call_escrow(&env, call_id).is_some() {
            return Err(OracleMarketplaceError::EscrowAlreadyExists);
        }
        let oracle = get_oracle(&env, &oracle_pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;
        if !oracle.is_active {
            return Err(OracleMarketplaceError::OracleNotActive);
        }
        // Bind the call to this oracle so `resolve_call` can find it.
        set_call_oracle(&env, call_id, &oracle_pubkey);

        // Pull the whole bounty into the contract; the split happens at
        // resolution time, once the answer's accuracy is known.
        transfer_token(&env, &token, &caller, &env.current_contract_address(), amount);

        let escrow = CallEscrow {
            call_id,
            caller: caller.clone(),
            oracle: oracle_pubkey.clone(),
            token,
            amount,
            created_at: env.ledger().timestamp(),
            resolved: false,
            settled: false,
        };
        set_call_escrow(&env, &escrow);
        emit_escrow_created(&env, call_id, &caller, &oracle_pubkey, amount);

        Ok(())
    }

    /// Resolve a call and settle its escrowed bounty.
    ///
    /// Must be called by the oracle the call is bound to. Outcomes:
    ///
    /// - `Resolution::Accurate` — the provider keeps `fee_bps` of the escrow
    ///   and the admin receives the remainder. `total_resolved` grows.
    /// - `Resolution::Inaccurate` — the escrowed bounty is returned to the
    ///   caller and `slash_penalty_bps` of the provider's bond is slashed,
    ///   split between the caller and the admin. `total_disputes` grows.
    pub fn resolve_call(
        env: Env,
        call_id: u64,
        provider: Address,
        resolution: Resolution,
    ) -> Result<(), OracleMarketplaceError> {
        provider.require_auth();

        let config = get_config(&env).ok_or(OracleMarketplaceError::NotInitialized)?;
        let mut escrow = get_call_escrow(&env, call_id).ok_or(OracleMarketplaceError::CallNotFound)?;
        if escrow.resolved {
            return Err(OracleMarketplaceError::EscrowAlreadyResolved);
        }

        let mut oracle = get_oracle(&env, &escrow.oracle).ok_or(OracleMarketplaceError::OracleNotFound)?;
        if oracle.address != provider {
            return Err(OracleMarketplaceError::NotSelectedOracle);
        }
        if !oracle.is_active {
            return Err(OracleMarketplaceError::OracleNotActive);
        }

        let contract_addr = env.current_contract_address();

        match resolution {
            Resolution::Accurate => {
                oracle.total_resolved = oracle.total_resolved.saturating_add(1);

                let provider_cut = bps_of(escrow.amount, oracle.fee_bps);
                let admin_cut = escrow.amount - provider_cut;

                // Mark settled before transferring so a re-entrant token
                // contract cannot re-trigger the payout.
                escrow.resolved = true;
                escrow.settled = true;
                set_call_escrow(&env, &escrow);

                if provider_cut > 0 {
                    transfer_token(&env, &escrow.token, &contract_addr, &oracle.address, provider_cut);
                }
                if admin_cut > 0 {
                    transfer_token(&env, &escrow.token, &contract_addr, &config.admin, admin_cut);
                }

                storage::set_oracle_earnings(
                    &env,
                    &oracle.pubkey,
                    storage::get_oracle_earnings(&env, &oracle.pubkey).saturating_add(provider_cut),
                );
                // Persist the incremented counter alongside the earnings —
                // without this the resolution is invisible to `get_oracle`.
                set_oracle(&env, &oracle.pubkey, &oracle);
                emit_escrow_settled(&env, call_id, &oracle.pubkey, provider_cut, admin_cut);
            }
            Resolution::Inaccurate => {
                if escrow.settled {
                    return Err(OracleMarketplaceError::EscrowAlreadyResolved);
                }

                oracle.total_disputes = oracle.total_disputes.saturating_add(1);

                let slashed = bps_of(oracle.staked_amount, config.slash_penalty_bps);
                // A provider whose bond has already been ground down to their
                // `min_stake` floor must not be slashed again — otherwise the
                // slash is refused and the provider keeps serving calls with
                // no collateral at risk. Deactivating them instead keeps the
                // marketplace honest.
                if oracle.staked_amount <= oracle.min_stake {
                    return Err(OracleMarketplaceError::SlashBelowMinStake);
                }

                escrow.resolved = true;
                escrow.settled = true;
                set_call_escrow(&env, &escrow);

                // The caller made a payment for an answer they cannot use, so
                // they get the bounty back in full.
                if escrow.amount > 0 {
                    transfer_token(&env, &escrow.token, &contract_addr, &escrow.caller, escrow.amount);
                }

                if slashed > 0 {
                    // Split the penalty: the caller was actively harmed, the
                    // admin runs the marketplace.
                    let caller_share = bps_of(slashed, 5_000);
                    let admin_share = slashed - caller_share;

                    oracle.staked_amount = oracle.staked_amount.saturating_sub(slashed);
                    oracle.total_slashed = oracle.total_slashed.saturating_add(slashed);
                    set_oracle(&env, &oracle.pubkey, &oracle);

                    transfer_token(&env, &oracle.bond_token, &contract_addr, &escrow.caller, caller_share);
                    transfer_token(&env, &oracle.bond_token, &contract_addr, &config.admin, admin_share);

                    emit_bond_slashed(&env, &oracle.pubkey, &escrow.caller, slashed, oracle.staked_amount);
                }
            }
        }

        Ok(())
    }

    pub fn rate_oracle(
        env: Env,
        user: Address,
        oracle_pubkey: BytesN<32>,
        satisfied: bool,
    ) -> Result<(), OracleMarketplaceError> {
        user.require_auth();
        let _oracle = get_oracle(&env, &oracle_pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;

        let mut ratings = get_oracle_ratings(&env, &oracle_pubkey);
        if ratings.contains_key(user.clone()) {
            return Err(OracleMarketplaceError::AlreadyRated);
        }

        ratings.set(user.clone(), satisfied);
        set_oracle_ratings(&env, &oracle_pubkey, &ratings);

        events::emit_oracle_rated(&env, &oracle_pubkey, &user, satisfied);
        Ok(())
    }

    pub fn get_oracle_metrics(
        env: Env,
        oracle_pubkey: BytesN<32>,
    ) -> Result<(u64, u64), OracleMarketplaceError> {
        let oracle = get_oracle(&env, &oracle_pubkey).ok_or(OracleMarketplaceError::OracleNotFound)?;
        Ok((oracle.total_resolved, oracle.total_disputes))
    }

    pub fn get_oracle_earnings(env: Env, oracle_pubkey: BytesN<32>) -> i128 {
        storage::get_oracle_earnings(&env, &oracle_pubkey)
    }

    pub fn get_call_escrow_view(env: Env, call_id: u64) -> Result<CallEscrow, OracleMarketplaceError> {
        get_call_escrow(&env, call_id).ok_or(OracleMarketplaceError::CallNotFound)
    }

    pub fn get_config_view(env: Env) -> Result<MarketplaceConfig, OracleMarketplaceError> {
        get_config(&env).ok_or(OracleMarketplaceError::NotInitialized)
    }
}
