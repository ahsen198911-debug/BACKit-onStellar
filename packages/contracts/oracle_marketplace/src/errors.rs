use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u32)]
pub enum OracleMarketplaceError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    OracleAlreadyRegistered = 4,
    OracleNotFound = 5,
    OracleNotActive = 6,
    InsufficientStake = 7,
    CooldownActive = 8,
    InvalidFee = 9,
    CallNotFound = 10,
    OracleNotSelectedForCall = 11,
    AlreadyRated = 12,
    InvalidRating = 13,
    /// Escrow for this call id already exists — call ids must be unique.
    EscrowAlreadyExists = 14,
    /// The call has already been resolved; it cannot be resolved twice.
    EscrowAlreadyResolved = 15,
    /// The call has been resolved but the bounty has not been settled yet.
    EscrowNotSettled = 16,
    /// Escrow amount is zero or negative — nothing to lock.
    InvalidEscrowAmount = 17,
    /// The address that called `resolve_call` is not the provider selected
    /// for this call.
    NotSelectedOracle = 18,
    /// Slashing would push the provider below their own `min_stake`, which
    /// would let a provider avoid being slashed by posting a tiny bond.
    SlashBelowMinStake = 19,
    /// Bond amount is not positive.
    InvalidBond = 20,
    /// Settlement amount exceeds what was escrowed.
    SettlementOverflow = 21,
}
