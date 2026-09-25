#![allow(deprecated)]

use soroban_sdk::{Address, BytesN, Env};

pub fn emit_oracle_registered(env: &Env, provider: &Address, pubkey: &BytesN<32>, fee_bps: u32) {
    env.events().publish(
        ("oracle_marketplace", "OracleRegistered"),
        (provider.clone(), pubkey.clone(), fee_bps),
    );
}

pub fn emit_oracle_deregistered(env: &Env, provider: &Address, pubkey: &BytesN<32>) {
    env.events().publish(
        ("oracle_marketplace", "OracleDeregistered"),
        (provider.clone(), pubkey.clone()),
    );
}

pub fn emit_oracle_selected(env: &Env, call_id: u64, provider: &BytesN<32>) {
    env.events().publish(
        ("oracle_marketplace", "OracleSelectedForCall"),
        (call_id, provider.clone()),
    );
}

pub fn emit_oracle_rated(env: &Env, provider: &BytesN<32>, user: &Address, satisfied: bool) {
    env.events().publish(
        ("oracle_marketplace", "OracleRated"),
        (provider.clone(), user.clone(), satisfied),
    );
}

/// A caller locked a bounty for a call.
pub fn emit_escrow_created(env: &Env, call_id: u64, caller: &Address, oracle: &BytesN<32>, amount: i128) {
    env.events().publish(
        ("oracle_marketplace", "EscrowCreated"),
        (call_id, caller.clone(), oracle.clone(), amount),
    );
}

/// An escrowed bounty was paid out to the provider that answered correctly.
pub fn emit_escrow_settled(
    env: &Env,
    call_id: u64,
    oracle: &BytesN<32>,
    provider_amount: i128,
    fee_amount: i128,
) {
    env.events().publish(
        ("oracle_marketplace", "EscrowSettled"),
        (call_id, oracle.clone(), provider_amount, fee_amount),
    );
}

/// A provider answered inaccurately; part of their bond was slashed.
pub fn emit_bond_slashed(
    env: &Env,
    oracle: &BytesN<32>,
    caller: &Address,
    slashed: i128,
    remaining_stake: i128,
) {
    env.events().publish(
        ("oracle_marketplace", "BondSlashed"),
        (oracle.clone(), caller.clone(), slashed, remaining_stake),
    );
}

/// A provider topped their bond back up.
pub fn emit_bond_increased(env: &Env, oracle: &BytesN<32>, amount: i128, total_stake: i128) {
    env.events().publish(
        ("oracle_marketplace", "BondIncreased"),
        (oracle.clone(), amount, total_stake),
    );
}

/// A provider withdrew bond after deregistering (subject to the cooldown).
pub fn emit_bond_withdrawn(env: &Env, oracle: &BytesN<32>, provider: &Address, amount: i128) {
    env.events().publish(
        ("oracle_marketplace", "BondWithdrawn"),
        (oracle.clone(), provider.clone(), amount),
    );
}
