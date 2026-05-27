#![no_main]

use spel_framework::context::ProgramContext;
use spel_framework::prelude::*;
use nssa_core::account::{AccountId, AccountWithMetadata};

risc0_zkvm::guest::entry!(main);

#[lez_program(instruction = "private_swap_router_core::Instruction")]
mod private_swap_router {
    #[allow(unused_imports)]
    use super::*;

    /// Atomic deshield → public AMM swap → re-shield. No CLOCK_01 in
    /// the account list — the chained AMM call uses
    /// `SwapExactInputCircuit` which skips the TWAP oracle update, so
    /// the privacy proof's pre-state set is drift-free and a slow CPU
    /// proof verifies cleanly. See `amm_core::Instruction::
    /// SwapExactInputCircuit` for the full rationale.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn private_swap(
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
        let _ = ctx;
        let (post_states, chained_calls) =
            private_swap_router_program::private_swap(
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

    /// Atomic WLEZ::Wrap → public AMM swap → re-shield. Account order
    /// matches `core::Instruction::PrivateSwapNativeIn`. See the variant
    /// doc for the full account list.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn private_swap_native_in(
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
        // No clock — chained AMM uses SwapExactInputCircuit; the
        // proof's pre-state set stays drift-free.
        swap_amount_in: u128,
        min_amount_out: u128,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let _ = ctx;
        let (post_states, chained_calls) =
            private_swap_router_program::private_swap_native_in(
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

    /// Atomic deshield → public AMM swap → WLEZ::Unwrap. Account order
    /// matches `core::Instruction::PrivateSwapNativeOut`. See the variant
    /// doc for the full account list.
    #[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
    #[instruction]
    pub fn private_swap_native_out(
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
        // No clock — chained AMM uses SwapExactInputCircuit.
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let _ = ctx;
        let (post_states, chained_calls) =
            private_swap_router_program::private_swap_native_out(
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
