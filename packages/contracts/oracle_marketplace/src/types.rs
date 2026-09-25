use soroban_sdk::{contracttype, Address, BytesN};

/// A registered oracle provider.
///
/// `bond_token` is the asset the provider posts and can be slashed in;
/// `staked_amount` is the amount currently locked in escrow. `min_stake` is
/// the floor the provider must keep while active — a resolution that pushes
/// the balance below it is rejected rather than silently allowed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleProvider {
    pub pubkey: BytesN<32>,
    pub address: Address,
    pub fee_bps: u32,
    pub min_stake: i128,
    pub staked_amount: i128,
    pub bond_token: Address,
    pub total_resolved: u64,
    pub total_disputes: u64,
    pub total_slashed: i128,
    pub is_active: bool,
    pub registered_at: u64,
    pub deregister_after: Option<u64>,
}

/// A single rating left by a consumer of an oracle call.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleRating {
    pub oracle: BytesN<32>,
    pub user: Address,
    pub satisfied: bool,
    pub timestamp: u64,
}

/// Escrowed funds for one oracle call.
///
/// The bounty the caller pays for a query is held here until the provider
/// resolves; see `resolve_call`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CallEscrow {
    pub call_id: u64,
    pub caller: Address,
    pub oracle: BytesN<32>,
    pub token: Address,
    pub amount: i128,
    pub created_at: u64,
    pub resolved: bool,
    pub settled: bool,
}

/// Result of resolving an oracle call. `accurate` decides whether the
/// provider keeps the escrowed bounty or has their bond slashed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    Accurate,
    Inaccurate,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MarketplaceConfig {
    pub admin: Address,
    pub cooldown_secs: u64,
    pub default_fee_bps: u32,
    /// Basis points of a slashed bond that goes to the admin/treasury rather
    /// than the caller who was served a bad answer.
    pub slash_penalty_bps: u32,
}
