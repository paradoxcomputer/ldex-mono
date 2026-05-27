//! Core types for the **amm_v2 combined private-swap program**.
//!
//! amm_v2 is a full drop-in replacement of the existing `amm` program
//! PLUS combined private-swap instructions (`DisposableSwap`,
//! `DisposableSwapNativeIn/Out`) that inline the orchestration the
//! `private_swap_router` does today. Pools created under amm_v2 are
//! amm_v2-owned (separate PDA space from the canonical AMM). Receipts
//! verify under upstream `PRIVACY_PRESERVING_CIRCUIT_ID` — amm_v2 is a
//! regular LEZ program called as a chained-call dependency by the
//! upstream privacy circuit, so no sequencer/nssa changes are needed.
//! Testnet-compatible.
//!
//! Pool layout reuses `amm_core::PoolDefinition` verbatim; the only
//! semantic difference vs canonical amm is that amm_v2 pools skip the
//! on-chain TWAP oracle on the *circuit* swap variant
//! (`SwapExactInputCircuit`) — that keeps the privacy proof's pre-state
//! set drift-free on slow CPU provers (no Clock account in the account
//! list). The public `SwapExactInput` variant still updates the oracle
//! (mode-0 public swaps benefit from price history).

use nssa_core::{account::AccountId, program::ProgramId};
use serde::{Deserialize, Serialize};

/// amm_v2 program instruction enum.
#[derive(Serialize, Deserialize)]
pub enum Instruction {
    /// Create a new amm_v2 pool. Delegates to `amm_program::new_definition`
    /// with amm_v2's `self_program_id`, so PDAs derive under amm_v2 →
    /// the pool + LP token are amm_v2-owned. No clock account in the
    /// account list (amm_v2 pools start with block_ts_last=0).
    ///
    /// Required accounts: pool, vault_a, vault_b, pool_definition_lp,
    /// lp_lock_holding, user_holding_a, user_holding_b, user_holding_lp.
    NewDefinition {
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        deadline: u64,
    },

    /// Add liquidity to an amm_v2 pool. Same shape as
    /// `amm_core::Instruction::AddLiquidity`. No clock account.
    AddLiquidity {
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        deadline: u64,
    },

    /// Remove liquidity from an amm_v2 pool. Same shape as
    /// `amm_core::Instruction::RemoveLiquidity`. No clock account.
    RemoveLiquidity {
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        deadline: u64,
    },

    /// **Mode-0 PUBLIC swap.** Updates the on-chain oracle (writes the
    /// pool's block_ts_last). Same shape as `amm_core::Instruction::
    /// SwapExactInput`. Account list includes Clock at the tail.
    SwapExactInput {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    },

    /// **Mode-1 PRIVATE (PrivateOwned) swap.** No oracle update (no
    /// Clock account in the account list) — drift-free pre-state set
    /// for slow CPU privacy proofs. Same shape as
    /// `amm_core::Instruction::SwapExactInputCircuit`.
    SwapExactInputCircuit {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    },

    /// **Mode-2 disposable swap** (RFP-literal account-A, combined).
    /// Inlines the router's deshield→swap→reshield orchestration AND
    /// the AMM's pool/reserve math; chains only 4 `token::Transfer`
    /// calls. Saves 1 `env::verify` vs the recursive router+amm path.
    ///
    /// Required accounts: user_holding_in (PrivateOwned),
    /// a_holding_a, a_holding_b, pool, vault_a, vault_b,
    /// user_holding_out (PrivateOwned).
    DisposableSwap {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        fees: u128,
        deadline: u64,
    },

    /// **Mode-2 disposable swap with native-LEZ INPUT** (combined).
    /// Inlines the router's WLEZ::Wrap → swap → reshield
    /// orchestration. Chains: 1× WLEZ::Wrap (which itself chains
    /// auth_transfer + token::Mint internally), 2× token::Transfer
    /// (vault movements), 1× token::Transfer (reshield).
    ///
    /// Required accounts: user_native (pub), wlez_vault (pub),
    /// wlez_definition (pub), a_wlez_holding (pub), a_holding_out
    /// (pub), pool (pub), vault_a (pub), vault_b (pub),
    /// user_holding_out (PrivateOwned).
    DisposableSwapNativeIn {
        swap_amount_in: u128,
        min_amount_out: u128,
        fees: u128,
        deadline: u64,
    },

    /// **RFP Func #8** — mode-0 public swap with the user side using
    /// Associated Token Accounts. Owner authorises the spend (signer);
    /// the chained `ata::Transfer` PDA-authorises the sender ATA.
    /// Same shape as `amm_core::Instruction::SwapExactInputAta`.
    SwapExactInputAta {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        ata_program_id: ProgramId,
        deadline: u64,
    },

    /// **RFP Func #8** — mode-0 public exact-output swap via ATAs.
    SwapExactOutputAta {
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        ata_program_id: ProgramId,
        deadline: u64,
    },

    /// **RFP Func #8** — add liquidity with the user side using ATAs.
    AddLiquidityAta {
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        ata_program_id: ProgramId,
        deadline: u64,
    },

    /// **RFP Func #8** — create a new amm_v2 pool with user-side ATAs.
    /// Chains 2× `ata::Transfer` (owner → vault) for deposits,
    /// `token::NewFungibleDefinition` for the LP, and `token::Mint`
    /// to seed `ata_lp = ata(owner, lp_def)` with the initial LP
    /// minus `MINIMUM_LIQUIDITY` (which goes into the lp_lock PDA).
    ///
    /// Required accounts: pool, vault_a, vault_b, pool_definition_lp,
    /// lp_lock_holding, owner (signer), ata_a, ata_b, ata_lp.
    NewDefinitionAta {
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        ata_program_id: ProgramId,
        deadline: u64,
    },

    /// **RFP Func #8** — remove liquidity with the user side using ATAs.
    /// Chains: 1× `ata::Burn` to drain `ata_lp`, 2× `token::Transfer`
    /// (vault → ATA with vault PDA-auth) to return the underlying tokens.
    ///
    /// Required accounts: pool, vault_a, vault_b, pool_definition_lp,
    /// owner (signer), ata_a, ata_b, ata_lp.
    RemoveLiquidityAta {
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        ata_program_id: ProgramId,
        deadline: u64,
    },

    /// **Mode-2 disposable swap with native-LEZ OUTPUT** (combined).
    /// Inlines deshield → swap → WLEZ::Unwrap. Chains: 1×
    /// token::Transfer (deshield), 2× token::Transfer (vault
    /// movements), 1× WLEZ::Unwrap (chains token::Burn internally).
    ///
    /// Required accounts: user_holding_in (PrivateOwned),
    /// a_holding_in (pub), a_wlez_holding (pub), pool (pub),
    /// vault_a (pub), vault_b (pub), wlez_definition (pub),
    /// wlez_vault (pub), user_native (pub).
    DisposableSwapNativeOut {
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        fees: u128,
        deadline: u64,
    },
}
