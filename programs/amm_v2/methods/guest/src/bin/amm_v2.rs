// amm_v2 — combined AMM + private-swap program. Full drop-in
// replacement of `amm` PLUS combined disposable swap variants
// (token↔token, LEZ→token, token→LEZ). Receipts verify under upstream
// PRIVACY_PRESERVING_CIRCUIT_ID — testnet-compatible.

#![no_main]

use std::num::NonZeroU128;

use nssa_core::account::{AccountId, AccountWithMetadata};
use spel_framework::context::ProgramContext;
use spel_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

#[lez_program(instruction = "amm_v2_core::Instruction")]
mod amm_v2 {
    #[allow(unused_imports)]
    use super::*;

    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn new_definition(
        ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        lp_lock_holding: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::new_definition::new_definition(
            pool,
            vault_a,
            vault_b,
            pool_definition_lp,
            lp_lock_holding,
            user_holding_a,
            user_holding_b,
            user_holding_lp,
            NonZeroU128::new(token_a_amount).expect("token_a_amount must be nonzero"),
            NonZeroU128::new(token_b_amount).expect("token_b_amount must be nonzero"),
            fees,
            ctx.self_program_id,
            /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn add_liquidity(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::add::add_liquidity(
            pool,
            vault_a,
            vault_b,
            pool_definition_lp,
            user_holding_a,
            user_holding_b,
            user_holding_lp,
            NonZeroU128::new(min_amount_liquidity).expect("min_amount_liquidity must be nonzero"),
            max_amount_to_add_token_a,
            max_amount_to_add_token_b,
            /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn remove_liquidity(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::remove::remove_liquidity(
            pool,
            vault_a,
            vault_b,
            pool_definition_lp,
            user_holding_a,
            user_holding_b,
            user_holding_lp,
            NonZeroU128::new(remove_liquidity_amount)
                .expect("remove_liquidity_amount must be nonzero"),
            min_amount_to_remove_token_a,
            min_amount_to_remove_token_b,
            /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Mode-0 PUBLIC swap. amm_v2's public swap DOES skip the clock /
    /// oracle update (no Clock in the account list) to keep the
    /// account schema identical between the public and private circuit
    /// variants — simplifies the FFI / cpp_plugin / pool indexer. The
    /// pool's oracle stays at block_ts_last=0 (no on-chain price
    /// history on amm_v2 pools — analytics callers should consume the
    /// on-chain cum_volume / cum_fees counters which amm_v2 DOES
    /// update on every swap).
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn swap_exact_input(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::swap::swap_exact_input(
            pool,
            vault_a,
            vault_b,
            user_holding_a,
            user_holding_b,
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Mode-1 PRIVATE (PrivateOwned) swap. Drift-free pre-state set
    /// for slow CPU privacy proofs.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn swap_exact_input_circuit(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::swap::swap_exact_input_circuit(
            pool,
            vault_a,
            vault_b,
            user_holding_a,
            user_holding_b,
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Combined disposable swap (mode-2 RFP-literal account-A,
    /// token↔token). Inlines AMM math; chains 4× token::Transfer.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn disposable_swap(
        ctx: ProgramContext,
        user_holding_in: AccountWithMetadata,
        a_holding_a: AccountWithMetadata,
        a_holding_b: AccountWithMetadata,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_out: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_v2_program::disposable_swap(
            ctx.self_program_id,
            user_holding_in,
            a_holding_a,
            a_holding_b,
            pool,
            vault_a,
            vault_b,
            user_holding_out,
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            fees,
            deadline,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Mode-2 disposable with native-LEZ input (LEZ → token).
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn disposable_swap_native_in(
        ctx: ProgramContext,
        user_native: AccountWithMetadata,
        wlez_vault: AccountWithMetadata,
        wlez_definition: AccountWithMetadata,
        a_wlez_holding: AccountWithMetadata,
        a_holding_out: AccountWithMetadata,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_out: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_v2_program::disposable_swap_native_in(
            ctx.self_program_id,
            user_native,
            wlez_vault,
            wlez_definition,
            a_wlez_holding,
            a_holding_out,
            pool,
            vault_a,
            vault_b,
            user_holding_out,
            swap_amount_in,
            min_amount_out,
            fees,
            deadline,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — mode-0 public swap with the user side using ATAs.
    /// Account order: `[pool, vault_a, vault_b, owner, ata_a, ata_b]`
    /// (no Clock — amm_v2 pools skip the on-chain oracle).
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn swap_exact_input_ata(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::swap_ata::swap_exact_input_ata(
            pool, vault_a, vault_b, owner, ata_a, ata_b,
            swap_amount_in, min_amount_out, token_definition_id_in,
            ata_program_id, /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — mode-0 public exact-output swap via ATAs.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn swap_exact_output_ata(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::swap_ata::swap_exact_output_ata(
            pool, vault_a, vault_b, owner, ata_a, ata_b,
            exact_amount_out, max_amount_in, token_definition_id_in,
            ata_program_id, /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — create a new amm_v2 pool with user-side ATAs.
    /// Mint LP into `ata(owner, lp_def)`.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn new_definition_ata(
        ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        lp_lock_holding: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        ata_lp: AccountWithMetadata,
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_v2_program::new_definition_ata(
            pool, vault_a, vault_b, pool_definition_lp, lp_lock_holding,
            owner, ata_a, ata_b, ata_lp,
            NonZeroU128::new(token_a_amount).expect("token_a_amount must be nonzero"),
            NonZeroU128::new(token_b_amount).expect("token_b_amount must be nonzero"),
            fees, ctx.self_program_id, ata_program_id,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — remove liquidity with the user side using ATAs.
    /// Drains `ata_lp` via `ata::Burn`; returns underlying via vault
    /// PDA-authed `token::Transfer` into `ata_a` / `ata_b`.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn remove_liquidity_ata(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        ata_lp: AccountWithMetadata,
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_v2_program::remove_liquidity_ata(
            pool, vault_a, vault_b, pool_definition_lp,
            owner, ata_a, ata_b, ata_lp,
            NonZeroU128::new(remove_liquidity_amount)
                .expect("remove_liquidity_amount must be nonzero"),
            min_amount_to_remove_token_a,
            min_amount_to_remove_token_b,
            ata_program_id,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — add liquidity with the user side using ATAs.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn add_liquidity_ata(
        _ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        ata_lp: AccountWithMetadata,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_program::add_ata::add_liquidity_ata(
            pool, vault_a, vault_b, pool_definition_lp,
            owner, ata_a, ata_b, ata_lp,
            NonZeroU128::new(min_amount_liquidity).expect("min_amount_liquidity must be nonzero"),
            max_amount_to_add_token_a, max_amount_to_add_token_b,
            ata_program_id, /*clock_ts=*/ 0,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Mode-2 disposable with native-LEZ output (token → LEZ).
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn disposable_swap_native_out(
        ctx: ProgramContext,
        user_holding_in: AccountWithMetadata,
        a_holding_in: AccountWithMetadata,
        a_wlez_holding: AccountWithMetadata,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        wlez_definition: AccountWithMetadata,
        wlez_vault: AccountWithMetadata,
        user_native: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let (post_states, chained_calls) = amm_v2_program::disposable_swap_native_out(
            ctx.self_program_id,
            user_holding_in,
            a_holding_in,
            a_wlez_holding,
            pool,
            vault_a,
            vault_b,
            wlez_definition,
            wlez_vault,
            user_native,
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            fees,
            deadline,
        );
        Ok(spel_framework::SpelOutput::execute(post_states, chained_calls)
            .with_timestamp_validity_window(..deadline))
    }
}
