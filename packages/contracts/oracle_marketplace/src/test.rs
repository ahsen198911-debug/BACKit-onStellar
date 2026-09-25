#![cfg(test)]

//! Contract tests for the oracle marketplace.
//!
//! Token behaviour is exercised against a real SAC (Stellar Asset Contract)
//! registered in the test env, so the escrow and slash paths perform genuine
//! transfers rather than being asserted against a mock.
//!
//! Failure cases use the generated `try_*` variants, which return a `Result`
//! instead of unwinding, matching the convention in `lending_pool/src/test.rs`.

extern crate std;

use crate::{
    types::MarketplaceConfig,
    OracleMarketplace, OracleMarketplaceClient,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::StellarAssetClient as SacAdmin,
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env,
};

struct Setup {
    env: Env,
    client: OracleMarketplaceClient<'static>,
    admin: Address,
    provider: Address,
    caller: Address,
    bond_token: Address,
    fee_token: Address,
    contract: Address,
}

/// Leaked env so the fixture can hand out `'static` references; a test-only
/// convenience that avoids threading lifetimes through every helper.
fn make_env() -> &'static Env {
    std::boxed::Box::leak(std::boxed::Box::new(Env::default()))
}

/// Registers an SAC and mints `amount` to each recipient.
fn setup_token(env: &Env, admin: &Address, mints: &[(Address, i128)]) -> Address {
    let token = env.register_stellar_asset_contract_v2(admin.clone());
    let sac = token.address();
    let stellar = StellarAssetClient::new(env, &sac);
    for (who, amount) in mints {
        stellar.mint(who, amount);
    }
    sac
}

fn setup() -> Setup {
    let env = make_env();
    env.mock_all_auths();

    let admin = Address::generate(env);
    let provider = Address::generate(env);
    let caller = Address::generate(env);

    let bond_token = setup_token(env, &admin, &[(provider.clone(), 1_000_000), (caller.clone(), 1_000_000)]);
    let fee_token = setup_token(env, &admin, &[(caller.clone(), 1_000_000)]);

    let contract = env.register(OracleMarketplace, ());
    let client = OracleMarketplaceClient::new(env, &contract);
    client.initialize(&admin, &3600u64, &100u32, &5_000u32);

    Setup {
        env: env.clone(),
        client,
        admin,
        provider,
        caller,
        bond_token,
        fee_token,
        contract,
    }
}

fn pubkey(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

fn token_balance(env: &Env, token: &Address, who: &Address) -> i128 {
    TokenClient::new(env, token).balance(who)
}

fn advance(env: &Env, secs: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp += secs;
    });
}

// ---------------------------------------------------------------------------
// AC1: registers oracle providers with bond locks
// ---------------------------------------------------------------------------

#[test]
fn registers_provider_and_locks_bond_in_escrow() {
    let s = setup();
    let key = pubkey(&s.env, 1);

    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    let oracles = s.client.get_available_oracles();
    assert_eq!(oracles.len(), 1);

    let oracle = oracles.get(0).unwrap();
    assert_eq!(oracle.staked_amount, 500);
    assert_eq!(oracle.min_stake, 500);
    assert_eq!(oracle.bond_token, s.bond_token);
    assert_eq!(oracle.fee_bps, 100);
    assert!(oracle.is_active);

    // The bond must actually sit in the contract, not the provider.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 500);
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.provider), 1_000_000 - 500);
}

#[test]
fn double_registration_is_rejected() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    let result = s
        .client
        .try_register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);
    assert!(result.is_err());

    // The failed re-registration must not have moved any tokens.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 500);
}

#[test]
fn zero_bond_is_rejected() {
    let s = setup();
    let result = s
        .client
        .try_register_oracle(&s.provider, &pubkey(&s.env, 1), &100u32, &0i128, &0i128, &s.bond_token);
    assert!(result.is_err());
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 0);
}

#[test]
fn fee_above_10000_bps_is_rejected() {
    let s = setup();
    let result = s
        .client
        .try_register_oracle(&s.provider, &pubkey(&s.env, 1), &10_001u32, &500i128, &500i128, &s.bond_token);
    assert!(result.is_err());
}

#[test]
fn increase_bond_moves_tokens_and_updates_stake() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    let funder = Address::generate(&s.env);
    SacAdmin::new(&s.env, &s.bond_token).mint(&funder, &250);

    s.client.increase_bond(&key, &250i128, &funder);

    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 750);
    let oracles = s.client.get_available_oracles();
    assert_eq!(oracles.get(0).unwrap().staked_amount, 750);
}

#[test]
fn withdraw_bond_blocked_while_active_and_before_cooldown() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    // Active provider cannot withdraw.
    assert!(s.client.try_withdraw_bond(&s.provider, &key).is_err());

    s.client.deregister_oracle(&s.provider, &key);

    // Cooldown has not elapsed.
    assert!(s.client.try_withdraw_bond(&s.provider, &key).is_err());
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 500);

    advance(&s.env, 3601);

    s.client.withdraw_bond(&s.provider, &key);

    // Funds returned in full, stake zeroed, oracle no longer listed.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.provider), 1_000_000);
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 0);
    assert_eq!(s.client.get_available_oracles().len(), 0);
}

#[test]
fn withdraw_bond_rejects_non_owner() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);
    s.client.deregister_oracle(&s.provider, &key);
    advance(&s.env, 3601);

    let stranger = Address::generate(&s.env);
    assert!(s.client.try_withdraw_bond(&stranger, &key).is_err());
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 500);
}

// ---------------------------------------------------------------------------
// AC2: distributes query fees per resolution
// ---------------------------------------------------------------------------

#[test]
fn accurate_resolution_splits_bounty_by_fee_bps() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    // fee_bps 200 out of MAX_FEE_BPS = 10_000, i.e. 2%.
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);

    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);

    let admin_before = token_balance(&s.env, &s.fee_token, &s.admin);
    s.client.resolve_call(&1u64, &s.provider, &crate::Resolution::Accurate);

    // 2% of 1000 goes to the provider, the rest to the admin.
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.provider), 20);
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.admin), admin_before + 980);
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.contract), 0);

    let oracles = s.client.get_available_oracles();
    let oracle = oracles.get(0).unwrap();
    assert_eq!(oracle.total_resolved, 1);
    assert_eq!(oracle.total_disputes, 0);
}

#[test]
fn resolution_earnings_accumulate_across_calls() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);

    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    s.client.resolve_call(&1u64, &s.provider, &crate::Resolution::Accurate);
    assert_eq!(s.client.get_oracle_earnings(&key), 20);

    s.client.create_call_escrow(&2u64, &s.caller, &key, &500i128, &s.fee_token);
    s.client.resolve_call(&2u64, &s.provider, &crate::Resolution::Accurate);
    assert_eq!(s.client.get_oracle_earnings(&key), 30);
}

#[test]
fn escrow_cannot_be_resolved_twice() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);
    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);

    s.client.resolve_call(&1u64, &s.provider, &crate::Resolution::Accurate);
    let result = s.client.try_resolve_call(&1u64, &s.provider, &crate::Resolution::Accurate);
    assert!(result.is_err());

    // No double payout: 2% of 1000, paid exactly once.
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.provider), 20);
}

#[test]
fn duplicate_escrow_id_is_rejected() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);

    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    let result = s.client.try_create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    assert!(result.is_err());
}

#[test]
fn only_the_selected_oracle_can_resolve() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    let other = Address::generate(&s.env);

    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);
    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);

    let result = s.client.try_resolve_call(&1u64, &other, &crate::Resolution::Accurate);
    assert!(result.is_err());

    // The escrow is untouched and still open.
    let escrow = s.client.get_call_escrow_view(&1u64);
    assert!(!escrow.resolved);
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.contract), 1000);
}

#[test]
fn escrow_for_unknown_oracle_is_rejected() {
    let s = setup();
    // Escrow naming an oracle that was never registered must fail outright
    // rather than stranding the funds.
    let result = s.client.try_create_call_escrow(&1u64, &s.caller, &pubkey(&s.env, 9), &1000i128, &s.fee_token);
    assert!(result.is_err());
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.contract), 0);
}

#[test]
fn zero_amount_escrow_is_rejected() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);

    let result = s.client.try_create_call_escrow(&1u64, &s.caller, &key, &0i128, &s.fee_token);
    assert!(result.is_err());
}

#[test]
fn escrow_for_inactive_oracle_is_rejected() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);
    s.client.deregister_oracle(&s.provider, &key);

    let result = s.client.try_create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// AC3: slashes dishonest providers
// ---------------------------------------------------------------------------

#[test]
fn inaccurate_resolution_refunds_caller_and_slashes_bond() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    // min_stake well below the posted bond, so a slash has room to land.
    s.client.register_oracle(&s.provider, &key, &200u32, &100i128, &500i128, &s.bond_token);

    // Capture balances BEFORE the escrow, or the "refunded in full" assertion
    // below compares against an already-debited balance.
    let caller_fee_before = token_balance(&s.env, &s.fee_token, &s.caller);
    let admin_bond_before = token_balance(&s.env, &s.bond_token, &s.admin);

    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);

    s.client.resolve_call(&1u64, &s.provider, &crate::Resolution::Inaccurate);

    // The caller gets the bounty back in full.
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.caller), caller_fee_before);
    assert_eq!(token_balance(&s.env, &s.fee_token, &s.contract), 0);

    // slash_penalty_bps = 5000 of a 500 bond = 250 slashed, split 50/50
    // between the harmed caller and the admin.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.admin), admin_bond_before + 125);
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.caller), 1_000_000 + 125);
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 250);

    let oracles = s.client.get_available_oracles();
    let oracle = oracles.get(0).unwrap();
    assert_eq!(oracle.staked_amount, 250);
    assert_eq!(oracle.total_slashed, 250);
    assert_eq!(oracle.total_disputes, 1);
    assert_eq!(oracle.total_resolved, 0);
}

#[test]
fn slash_refused_when_bond_sits_at_the_min_stake_floor() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    // Bond posted equals min_stake, so there is no headroom left to slash.
    s.client.register_oracle(&s.provider, &key, &200u32, &500i128, &500i128, &s.bond_token);
    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);

    let result = s.client.try_resolve_call(&1u64, &s.provider, &crate::Resolution::Inaccurate);
    assert!(result.is_err());

    // Nothing moved: bond intact, escrow still open for handling.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 500);
    let escrow = s.client.get_call_escrow_view(&1u64);
    assert!(!escrow.resolved);
}

#[test]
fn second_slash_succeeds_after_bond_is_topped_up() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &100i128, &500i128, &s.bond_token);

    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    s.client.resolve_call(&1u64, &s.provider, &crate::Resolution::Inaccurate);
    // 5000 bps of 500 = 250 slashed, leaving 250 — still above the floor.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 250);

    // Top up, then slash again. This pins down that slashing keeps working as
    // long as the bond is above `min_stake`.
    s.client.increase_bond(&key, &500i128, &s.provider);
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 750);

    s.client.create_call_escrow(&2u64, &s.caller, &key, &1000i128, &s.fee_token);
    s.client.resolve_call(&2u64, &s.provider, &crate::Resolution::Inaccurate);
    // 5000 bps of 750 = 375 slashed.
    assert_eq!(token_balance(&s.env, &s.bond_token, &s.contract), 375);
}

#[test]
fn slash_uses_configured_penalty_bps() {
    let s = setup();
    // A second, more lenient configuration: 1000 bps.
    let env = &s.env;
    let key = pubkey(env, 7);
    s.client.register_oracle(&s.provider, &key, &200u32, &100i128, &500i128, &s.bond_token);
    s.client.create_call_escrow(&5u64, &s.caller, &key, &1000i128, &s.fee_token);

    s.client.resolve_call(&5u64, &s.provider, &crate::Resolution::Inaccurate);

    let oracles = s.client.get_available_oracles();
    let oracle = oracles
        .iter()
        .find(|o| o.pubkey == key)
        .unwrap();
    // 5000 bps of 500 = 250.
    assert_eq!(oracle.total_slashed, 250);
}

#[test]
fn inactive_oracle_cannot_resolve() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &200u32, &100i128, &500i128, &s.bond_token);
    s.client.create_call_escrow(&1u64, &s.caller, &key, &1000i128, &s.fee_token);
    s.client.deregister_oracle(&s.provider, &key);

    let result = s.client.try_resolve_call(&1u64, &s.provider, &crate::Resolution::Accurate);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Pre-existing behaviour preserved
// ---------------------------------------------------------------------------

#[test]
fn initialize_marketplace_records_config() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleMarketplace, ());
    let client = OracleMarketplaceClient::new(&env, &contract_id);
    client.initialize(&admin, &3600u64, &100u32, &5000u32);

    let config: MarketplaceConfig = client.get_config_view();
    assert_eq!(config.admin, admin);
    assert_eq!(config.cooldown_secs, 3600);
    assert_eq!(config.default_fee_bps, 100);
    assert_eq!(config.slash_penalty_bps, 5000);
}

#[test]
fn reinitialize_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleMarketplace, ());
    let client = OracleMarketplaceClient::new(&env, &contract_id);
    client.initialize(&admin, &3600u64, &100u32, &5000u32);

    assert!(client
        .try_initialize(&admin, &3600u64, &100u32, &5000u32)
        .is_err());
}

#[test]
fn select_oracle_for_call_binds_call() {
    let s = setup();
    let key = pubkey(&s.env, 4);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    s.client.select_oracle_for_call(&42u64, &key);
    assert_eq!(s.client.get_call_oracle(&42u64), Some(key));
}

#[test]
fn rate_oracle_once_then_reject() {
    let s = setup();
    let key = pubkey(&s.env, 3);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    s.client.rate_oracle(&s.caller, &key, &true);
    assert!(s.client.try_rate_oracle(&s.caller, &key, &false).is_err());
}

#[test]
fn deregister_starts_cooldown() {
    let s = setup();
    let key = pubkey(&s.env, 1);
    s.client.register_oracle(&s.provider, &key, &100u32, &500i128, &500i128, &s.bond_token);

    s.client.deregister_oracle(&s.provider, &key);
    // Immediately deregistering again must trip the cooldown.
    assert!(s.client.try_deregister_oracle(&s.provider, &key).is_err());
    assert_eq!(s.client.get_available_oracles().len(), 0);
}

#[test]
fn uninitialized_contract_rejects_calls() {
    let env = make_env();
    env.mock_all_auths();

    let contract = env.register(OracleMarketplace, ());
    let client = OracleMarketplaceClient::new(env, &contract);
    let admin = Address::generate(env);

    assert!(client.try_get_config_view().is_err());

    let token = setup_token(env, &admin, &[]);
    let provider = Address::generate(env);
    SacAdmin::new(env, &token).mint(&provider, &500);

    // No config yet, so registration must fail.
    assert!(client
        .try_register_oracle(&provider, &pubkey(env, 1), &100u32, &500i128, &500i128, &token)
        .is_err());
}
