//! This crate contains core data structures and utilities for the AMM Program.

use borsh::{BorshDeserialize, BorshSerialize};
use nssa_core::{
    account::{AccountId, AccountWithMetadata, Data},
    program::{PdaSeed, ProgramId},
};
use serde::{Deserialize, Serialize};
use spel_framework_macros::account_type;

// These stable seed bytes are part of the PDA derivation scheme and must stay unchanged for
// compatibility.
const LIQUIDITY_TOKEN_PDA_SEED: [u8; 32] = [0; 32];
const LP_LOCK_HOLDING_PDA_SEED: [u8; 32] = [1; 32];

/// AMM Program Instruction.
#[derive(Serialize, Deserialize)]
pub enum Instruction {
    /// Initializes a new Pool (or re-initializes an existing zero-supply Pool).
    ///
    /// On initialization, `MINIMUM_LIQUIDITY` LP tokens are permanently locked
    /// in the LP-lock holding PDA; the caller receives `initial_lp - MINIMUM_LIQUIDITY`.
    ///
    /// Required accounts:
    /// - AMM Pool
    /// - Vault Holding Account for Token A
    /// - Vault Holding Account for Token B
    /// - Pool Liquidity Token Definition
    /// - LP Lock Holding Account, derived as `compute_lp_lock_holding_pda(self_program_id,
    ///   pool.account_id)`
    /// - User Holding Account for Token A (authorized)
    /// - User Holding Account for Token B (authorized)
    /// - User Holding Account for Pool Liquidity (authorized when uninitialized)
    NewDefinition {
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// Adds liquidity to the Pool
    ///
    /// Required accounts:
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    /// - Pool Liquidity Token Definition (initialized)
    /// - User Holding Account for Token A (authorized)
    /// - User Holding Account for Token B (authorized)
    /// - User Holding Account for Pool Liquidity
    AddLiquidity {
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// Removes liquidity from the Pool
    ///
    /// Required accounts:
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    /// - Pool Liquidity Token Definition (initialized)
    /// - User Holding Account for Token A (initialized)
    /// - User Holding Account for Token B (initialized)
    /// - User Holding Account for Pool Liquidity (authorized)
    RemoveLiquidity {
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// Swap some quantity of Tokens (either Token A or Token B)
    /// while maintaining the Pool constant product.
    ///
    /// Required accounts:
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    /// - User Holding Account for Token A
    /// - User Holding Account for Token B; either is authorized.
    SwapExactInput {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// Swap tokens specifying the exact desired output amount,
    /// while maintaining the Pool constant product.
    ///
    /// Required accounts:
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    /// - User Holding Account for Token A
    /// - User Holding Account for Token B; either is authorized.
    SwapExactOutput {
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// Sync pool reserves with current vault balances.
    ///
    /// Required accounts:
    /// - AMM Pool (initialized, with LP supply at or above minimum liquidity)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    SyncReserves,

    /// RFP Func #8 — swap_exact_in but with the user side using
    /// **Associated Token Accounts (ATAs)** instead of keypair token
    /// holdings. Chains `ata::Transfer` (owner-authorized, ATA program
    /// internally PDA-authorizes the sender ATA) for the input leg, and a
    /// vault-PDA-authorized `token::Transfer` for the output leg into the
    /// recipient ATA. ATAs are deterministic per `(owner, definition)`.
    ///
    /// Required accounts:
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A (initialized)
    /// - Vault Holding Account for Token B (initialized)
    /// - Owner Account (signer — authorizes the ATA spend)
    /// - User ATA for Token A (must equal `for_public_pda(ata_pid,
    ///   sha256(owner ‖ def_a))`)
    /// - User ATA for Token B (same derivation rule with def_b)
    /// - Clock account (read-only, oracle update)
    SwapExactInputAta {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        /// ATA program id — needed to dispatch the chained `ata::Transfer`
        /// for the input leg. (We can't read it from `ata_a.program_owner`
        /// because ATAs are token holdings owned by the *token* program;
        /// the ATA program holds PDA authority, not storage ownership.)
        ata_program_id: nssa_core::program::ProgramId,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },

    /// RFP Func #8 — `SwapExactOutput` with the user side using ATAs.
    /// Same account layout & chaining strategy as `SwapExactInputAta`.
    SwapExactOutputAta {
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    },

    /// RFP Func #8 — `AddLiquidity` with the user side using ATAs.
    /// The deposit legs go through `ata::Transfer` (owner-authorised); the
    /// LP mint into the user's ATA-LP uses the existing PDA-authorised
    /// `token::Mint` (recipient is just a Fungible token holding).
    ///
    /// Required accounts (in this order):
    /// - AMM Pool (initialised)
    /// - Vault A (initialised), Vault B (initialised)
    /// - Pool LP token-definition (PDA)
    /// - Owner account (signer — authorises the ATA spends)
    /// - User ATA for Token A, B, and LP (all deterministic from
    ///   `(owner, definition)` via the ATA program)
    /// - Clock account (read-only, oracle update)
    AddLiquidityAta {
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    },

    /// Like `SwapExactInput` but **without** the clock account in
    /// pre-state. The on-chain TWAP price oracle (§5.11③) is **not**
    /// updated for swaps that take this path — the swap math, the
    /// reserve update, the volume / fee accumulators, and the chained
    /// token transfers all run identically to `SwapExactInput`; only
    /// the oracle ring + `block_ts_last` carry over unchanged from
    /// pre to post. Public swaps (mode 0) keep using `SwapExactInput`
    /// so the TWAP stays fed.
    ///
    /// Required accounts (one fewer than `SwapExactInput` — no clock):
    /// - AMM Pool (initialized)
    /// - Vault Holding Account for Token A
    /// - Vault Holding Account for Token B
    /// - User Holding Account for Token A
    /// - User Holding Account for Token B
    ///
    /// Designed specifically for use as a chained call inside a
    /// privacy-preserving transaction: CLOCK_01 advances every block,
    /// so any privacy proof that captures it in `public_pre_states`
    /// becomes stale the moment the next block fires. A CPU-bound real
    /// STARK takes minutes — many blocks — so the proof fails to
    /// verify with `InvalidPrivacyPreservingProof` (sequencer's
    /// reconstruction uses *current* CLOCK_01 ≠ what the proof
    /// committed). Dropping CLOCK_01 from the proof side removes that
    /// drift surface entirely. Trade-off: this swap doesn't tick the
    /// oracle. Acceptable because (a) private swaps shouldn't reveal
    /// their precise time in the oracle anyway and (b) the oracle
    /// keeps being fed by all public swaps that use `SwapExactInput`.
    SwapExactInputCircuit {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        /// Unix timestamp (milliseconds) after which this transaction is invalid.
        deadline: u64,
    },
}

pub const MINIMUM_LIQUIDITY: u128 = 1_000;

/// Canonical on-chain Clock account (sequencer-updated every block with
/// `{block_id, timestamp_ms}`). Fixed well-known id — exactly 32 bytes.
/// Threaded read-only into the mutating AMM instructions so the on-chain
/// price oracle uses on-chain time (design.md §5.11③).
pub const CLOCK_01: AccountId = AccountId::new(*b"/LEZ/ClockProgramAccount/0000001");

/// Bounded on-chain observation ring (Uniswap-V3-style). Each mutating tx
/// pushes one; the chart reads these directly from the pool account →
/// exact, gapless recent price history with **no off-chain indexer**.
pub const ORACLE_RING_CAP: usize = 64;

/// Local mirror of `clock_core::ClockAccountData` (avoids an extra git
/// dep; Borsh layout is identical: `u64` then the `Timestamp(i64)` ms).
#[derive(Clone, Copy, BorshDeserialize)]
pub struct ClockData {
    pub block_id: u64,
    pub timestamp: i64,
}

/// One on-chain price observation: cumulative price accumulators
/// snapshotted at `ts` (ms). TWAP over [t1,t2] = (cum2-cum1)/(t2-t1).
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Observation {
    pub ts: u64,
    pub cum_a: u128,
    pub cum_b: u128,
}

#[account_type]
#[derive(Clone, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct PoolDefinition {
    pub definition_token_a_id: AccountId,
    pub definition_token_b_id: AccountId,
    pub vault_a_id: AccountId,
    pub vault_b_id: AccountId,
    pub liquidity_pool_id: AccountId,
    /// Total LP supply tracked by the pool. After initialization it includes the permanently
    /// locked `MINIMUM_LIQUIDITY`; a zero supply means the pool is uninitialized
    pub liquidity_pool_supply: u128,
    pub reserve_a: u128,
    pub reserve_b: u128,
    /// Fee tier in basis points.
    pub fees: u128,
    // --- on-chain price oracle (design.md §5.11③) ---
    /// Q64.64 time-cumulative of price(A in B), wrapping (Uniswap-V2 style).
    pub price_a_cum_last: u128,
    /// Q64.64 time-cumulative of price(B in A), wrapping.
    pub price_b_cum_last: u128,
    /// On-chain ms timestamp of the last oracle update (Clock).
    pub block_ts_last: i64,
    /// Bounded ring of recent observations (≤ `ORACLE_RING_CAP`).
    pub obs: Vec<Observation>,
    // --- on-chain aggregate volume + LP fee accumulators (RFP Usability #3,
    //     exact: incremented per swap by swap.rs::swap_logic). LPs receive
    //     the fee implicitly via invariant growth; cum_fees_{a,b} records
    //     the explicit fee taken off the input. ---
    /// Lifetime cumulative throughput of token A across all swaps (input
    /// side or output side — the swap input is in one token, the output
    /// in the other; this tracks `swap_amount_in` when token A is input,
    /// and `swap_amount_out` when token A is output).
    pub cum_volume_a: u128,
    /// Lifetime cumulative throughput of token B across all swaps.
    pub cum_volume_b: u128,
    /// Lifetime cumulative LP fees taken (in input-token units). When
    /// token A was input, fee accrues to `cum_fees_a`; symmetric for B.
    pub cum_fees_a: u128,
    pub cum_fees_b: u128,
}

impl PoolDefinition {
    /// Accumulate the time-weighted price and push a ring observation,
    /// using the on-chain Clock timestamp. MUST be called **before**
    /// reserves are mutated by a swap/liquidity op (Uniswap invariant:
    /// the cumulative integrates the price that held over the elapsed
    /// interval). Idempotent within a block (dt==0 ⇒ no-op). Wrapping
    /// arithmetic — readers recover deltas via modular subtraction.
    ///
    /// Precision/scale requirement: reserves < 2^64 (Q64.64 headroom in u128).
    /// Enforced at runtime below — `reserve << 64` would otherwise silently wrap
    /// in the release guest (overflow-checks off) and corrupt the cumulative
    /// price. Realistic reserves are far below this bound, so the guard never
    /// fires in practice; it fails closed if the assumption is ever violated.
    pub fn oracle_update(&mut self, now_ms: i64) {
        if self.reserve_a == 0 || self.reserve_b == 0 {
            self.block_ts_last = now_ms;
            return;
        }
        let dt = now_ms.saturating_sub(self.block_ts_last);
        if dt <= 0 {
            return;
        }
        let dt = dt as u128;
        // Hard domain guard (NOT debug_assert — that is compiled out of the
        // release guest): a reserve >= 2^64 would overflow the `<< 64` below.
        assert!(
            self.reserve_a < (1u128 << 64) && self.reserve_b < (1u128 << 64),
            "oracle reserve exceeds the 2^64 Q64.64 domain"
        );
        let price_a = (self.reserve_b << 64) / self.reserve_a; // price A in B, Q64.64
        let price_b = (self.reserve_a << 64) / self.reserve_b; // price B in A, Q64.64
        self.price_a_cum_last = self.price_a_cum_last.wrapping_add(price_a.wrapping_mul(dt));
        self.price_b_cum_last = self.price_b_cum_last.wrapping_add(price_b.wrapping_mul(dt));
        self.block_ts_last = now_ms;
        self.obs.push(Observation {
            ts: now_ms as u64,
            cum_a: self.price_a_cum_last,
            cum_b: self.price_b_cum_last,
        });
        let n = self.obs.len();
        if n > ORACLE_RING_CAP {
            self.obs.drain(0..n - ORACLE_RING_CAP);
        }
    }
}

pub const FEE_BPS_DENOMINATOR: u128 = 10_000;
pub const FEE_TIER_BPS_1: u128 = 1;
pub const FEE_TIER_BPS_5: u128 = 5;
pub const FEE_TIER_BPS_30: u128 = 30;
pub const FEE_TIER_BPS_100: u128 = 100;

pub fn is_supported_fee_tier(fees: u128) -> bool {
    matches!(
        fees,
        FEE_TIER_BPS_1 | FEE_TIER_BPS_5 | FEE_TIER_BPS_30 | FEE_TIER_BPS_100
    )
}

pub fn assert_supported_fee_tier(fees: u128) {
    assert!(
        is_supported_fee_tier(fees),
        "Fee tier must be one of 1, 5, 30, or 100 basis points"
    );
}

impl TryFrom<&Data> for PoolDefinition {
    type Error = std::io::Error;

    fn try_from(data: &Data) -> Result<Self, Self::Error> {
        PoolDefinition::try_from_slice(data.as_ref())
    }
}

impl From<&PoolDefinition> for Data {
    fn from(definition: &PoolDefinition) -> Self {
        // Using size_of_val as size hint for Vec allocation
        let mut data = Vec::with_capacity(std::mem::size_of_val(definition));

        BorshSerialize::serialize(definition, &mut data)
            .expect("Serialization to Vec should not fail");

        Data::try_from(data).expect("Token definition encoded data should fit into Data")
    }
}

pub fn compute_pool_pda(
    amm_program_id: ProgramId,
    definition_token_a_id: AccountId,
    definition_token_b_id: AccountId,
    fees: u128,
) -> AccountId {
    AccountId::for_public_pda(
        &amm_program_id,
        &compute_pool_pda_seed(definition_token_a_id, definition_token_b_id, fees),
    )
}

/// Pool PDA seed binds the (token pair, fee tier) triple so that pools for
/// the same pair with different fee tiers have distinct addresses and can
/// coexist (RFP-004 Func #6). The fee tier is part of the stable derivation
/// scheme and must stay unchanged for compatibility.
pub fn compute_pool_pda_seed(
    definition_token_a_id: AccountId,
    definition_token_b_id: AccountId,
    fees: u128,
) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256};

    let (token_1, token_2) = match definition_token_a_id
        .value()
        .cmp(definition_token_b_id.value())
    {
        std::cmp::Ordering::Less => (definition_token_b_id, definition_token_a_id),
        std::cmp::Ordering::Greater => (definition_token_a_id, definition_token_b_id),
        std::cmp::Ordering::Equal => panic!("Definitions match"),
    };

    let mut bytes = [0; 80];
    bytes[0..32].copy_from_slice(&token_1.to_bytes());
    bytes[32..64].copy_from_slice(&token_2.to_bytes());
    bytes[64..80].copy_from_slice(&fees.to_le_bytes());

    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("Hash output must be exactly 32 bytes long"),
    )
}

pub fn compute_vault_pda(
    amm_program_id: ProgramId,
    pool_id: AccountId,
    definition_token_id: AccountId,
) -> AccountId {
    AccountId::for_public_pda(
        &amm_program_id,
        &compute_vault_pda_seed(pool_id, definition_token_id),
    )
}

pub fn compute_vault_pda_seed(pool_id: AccountId, definition_token_id: AccountId) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256};

    let mut bytes = [0; 64];
    bytes[0..32].copy_from_slice(&pool_id.to_bytes());
    bytes[32..].copy_from_slice(&definition_token_id.to_bytes());

    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("Hash output must be exactly 32 bytes long"),
    )
}

pub fn compute_liquidity_token_pda(amm_program_id: ProgramId, pool_id: AccountId) -> AccountId {
    AccountId::for_public_pda(&amm_program_id, &compute_liquidity_token_pda_seed(pool_id))
}

pub fn compute_liquidity_token_pda_seed(pool_id: AccountId) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256};

    let mut bytes = [0; 64];
    bytes[0..32].copy_from_slice(&pool_id.to_bytes());
    bytes[32..].copy_from_slice(&LIQUIDITY_TOKEN_PDA_SEED);

    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("Hash output must be exactly 32 bytes long"),
    )
}

pub fn compute_lp_lock_holding_pda(amm_program_id: ProgramId, pool_id: AccountId) -> AccountId {
    AccountId::for_public_pda(&amm_program_id, &compute_lp_lock_holding_pda_seed(pool_id))
}

pub fn compute_lp_lock_holding_pda_seed(pool_id: AccountId) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256};

    let mut bytes = [0; 64];
    bytes[0..32].copy_from_slice(&pool_id.to_bytes());
    bytes[32..].copy_from_slice(&LP_LOCK_HOLDING_PDA_SEED);

    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("Hash output must be exactly 32 bytes long"),
    )
}

fn read_fungible_holding(account: &AccountWithMetadata, context: &str) -> (AccountId, u128) {
    let token_holding = token_core::TokenHolding::try_from(&account.account.data)
        .unwrap_or_else(|_| panic!("{context}: AMM Program expects a valid Token Holding Account"));

    let token_core::TokenHolding::Fungible {
        definition_id,
        balance,
    } = token_holding
    else {
        panic!("{context}: AMM Program expects a valid Fungible Token Holding Account");
    };

    (definition_id, balance)
}

pub fn read_vault_fungible_balances(
    context: &str,
    vault_a: &AccountWithMetadata,
    vault_b: &AccountWithMetadata,
) -> (u128, u128) {
    let vault_a_context = format!("{context}: Vault A");
    let vault_b_context = format!("{context}: Vault B");
    let (_, vault_a_balance) = read_fungible_holding(vault_a, &vault_a_context);
    let (_, vault_b_balance) = read_fungible_holding(vault_b, &vault_b_context);

    (vault_a_balance, vault_b_balance)
}

#[cfg(test)]
mod oracle_tests {
    use super::*;

    fn pool(ra: u128, rb: u128) -> PoolDefinition {
        PoolDefinition { reserve_a: ra, reserve_b: rb, ..Default::default() }
    }

    #[test]
    fn first_update_seeds_time_no_obs_when_uninit() {
        let mut p = pool(0, 0);
        p.oracle_update(1_000);
        assert_eq!(p.block_ts_last, 1_000);
        assert!(p.obs.is_empty(), "no observation while pool uninitialized");
    }

    #[test]
    fn accumulates_twap_and_pushes_observation() {
        let mut p = pool(1_000, 2_000); // price A in B = 2.0
        p.block_ts_last = 0;
        p.oracle_update(1_000); // dt = 1000 ms
        assert_eq!(p.obs.len(), 1);
        // price_a (Q64.64) = (2000<<64)/1000 = 2<<64 ; *1000 ms
        let expected_a = ((2_000u128 << 64) / 1_000).wrapping_mul(1_000);
        assert_eq!(p.price_a_cum_last, expected_a);
        assert_eq!(p.obs[0].cum_a, expected_a);
        assert_eq!(p.obs[0].ts, 1_000);
        // TWAP A over [0,1000] = (cum1-cum0)/dt = price_a (Q64.64) ≈ 2.0
        let twap_q64 = p.price_a_cum_last / 1_000;
        assert_eq!(twap_q64 >> 64, 2, "TWAP price A in B ≈ 2.0");
    }

    #[test]
    fn same_block_is_noop() {
        let mut p = pool(1_000, 2_000);
        p.block_ts_last = 500;
        p.oracle_update(500); // dt = 0
        assert!(p.obs.is_empty());
        assert_eq!(p.price_a_cum_last, 0);
    }

    #[test]
    fn ring_is_bounded() {
        let mut p = pool(1_000, 1_000);
        for i in 1..=(ORACLE_RING_CAP as i64 + 20) {
            p.oracle_update(i * 10);
        }
        assert_eq!(p.obs.len(), ORACLE_RING_CAP, "ring capped");
        // oldest retained advanced (front dropped)
        assert!(p.obs.first().unwrap().ts > 10);
    }

    #[test]
    fn gapless_twap_across_long_offline_gap() {
        // Two observations far apart still yield exact average price —
        // the integral lives on-chain, so an offline reader loses no info.
        let mut p = pool(1_000, 3_000); // price A in B = 3.0
        p.block_ts_last = 0;
        p.oracle_update(10);                 // t=10
        let c1 = p.price_a_cum_last;
        // ... long "gap" with no reads, one more update much later ...
        p.oracle_update(1_000_000);          // t=1e6
        let c2 = p.price_a_cum_last;
        let twap = (c2.wrapping_sub(c1)) / (1_000_000u128 - 10);
        assert_eq!(twap >> 64, 3, "exact TWAP over the gap, no data lost");
    }
}
