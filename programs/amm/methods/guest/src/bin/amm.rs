#![no_main]

use std::num::NonZeroU128;

use spel_framework::prelude::*;
use spel_framework::context::ProgramContext;
use nssa_core::{
    account::{AccountId, AccountWithMetadata},
    program::AccountPostState,
};

risc0_zkvm::guest::entry!(main);

#[lez_program(instruction = "amm_core::Instruction")]
mod amm {
    #[allow(unused_imports)]
    use super::*;

    /// On-chain ms timestamp from the threaded Clock account (§5.11③).
    /// Defaults to 0 if the account isn't a valid `ClockData` (the
    /// oracle then no-ops until a real Clock is present).
    fn clock_ms(clock: &AccountWithMetadata) -> i64 {
        // Only trust the canonical sequencer Clock. Without this an attacker can
        // pass any account they shaped into the clock slot and feed the TWAP
        // oracle an arbitrary timestamp (poison/freeze the cumulative price).
        assert!(clock.account_id == amm_core::CLOCK_01, "clock account must be CLOCK_01");
        <amm_core::ClockData as borsh::BorshDeserialize>::try_from_slice(
            clock.account.data.as_ref(),
        )
        .map(|c| c.timestamp)
        .unwrap_or(0)
    }

    /// The Clock is read-only (oracle input only) so the logic fns emit no
    /// post-state for it. The LEZ privacy circuit asserts a 1:1
    /// account↔state mapping (`visibility_mask.len() == states_iter.len()`,
    /// `privacy_preserving_circuit.rs`), so a Clock account passed in the
    /// privacy account vector with no post-state panics
    /// ("Invalid visibility mask length"). Echo the Clock back unchanged
    /// (no-op write) so states == accounts on BOTH the public and privacy
    /// paths. clock is the last account param → push to the tail.
    fn echo_clock(
        mut post_states: Vec<AccountPostState>,
        clock: AccountWithMetadata,
    ) -> Vec<AccountPostState> {
        post_states.push(AccountPostState::new(clock.account));
        post_states
    }

    /// Initializes a new Pool (or re-initializes an existing zero-supply Pool).
    /// A fresh user LP holding must be explicitly authorized by the caller.
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
        clock: AccountWithMetadata,
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
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
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Like `new_definition`, but pins the submitter-supplied `ata_program_id`
    /// into the pool so its ATA-routed ops become reachable (the pin asserts).
    #[instruction]
    pub fn new_definition_ata(
        ctx: ProgramContext,
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        lp_lock_holding: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        clock: AccountWithMetadata,
        token_a_amount: u128,
        token_b_amount: u128,
        fees: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::new_definition::new_definition_ata(
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
            ata_program_id,
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Adds liquidity to the Pool.
    #[instruction]
    pub fn add_liquidity(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        clock: AccountWithMetadata,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
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
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Removes liquidity from the Pool.
    #[instruction]
    pub fn remove_liquidity(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        user_holding_lp: AccountWithMetadata,
        clock: AccountWithMetadata,
        remove_liquidity_amount: u128,
        min_amount_to_remove_token_a: u128,
        min_amount_to_remove_token_b: u128,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
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
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Swap some quantity of tokens while maintaining the pool constant product.
    #[instruction]
    pub fn swap_exact_input(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        clock: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::swap::swap_exact_input(
            pool,
            vault_a,
            vault_b,
            user_holding_a,
            user_holding_b,
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// `SwapExactInput` without the clock account — for use inside a
    /// privacy-preserving transaction's chained-call tree. See
    /// `core::Instruction::SwapExactInputCircuit` for the full
    /// rationale (proof-time-vs-block-period mismatch on CPU). Public
    /// swaps continue to use `swap_exact_input` and feed the TWAP
    /// oracle; this variant skips the oracle update.
    #[instruction]
    pub fn swap_exact_input_circuit(
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

    /// Swap tokens specifying the exact desired output amount.
    #[instruction]
    pub fn swap_exact_output(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        user_holding_a: AccountWithMetadata,
        user_holding_b: AccountWithMetadata,
        clock: AccountWithMetadata,
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::swap::swap_exact_output(
            pool,
            vault_a,
            vault_b,
            user_holding_a,
            user_holding_b,
            exact_amount_out,
            max_amount_in,
            token_definition_id_in,
            clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — swap with the user side using ATAs (owner-authorised).
    /// Account order: `[pool, vault_a, vault_b, owner, ata_a, ata_b, clock]`.
    #[instruction]
    pub fn swap_exact_input_ata(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        clock: AccountWithMetadata,
        swap_amount_in: u128,
        min_amount_out: u128,
        token_definition_id_in: AccountId,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::swap_ata::swap_exact_input_ata(
            pool, vault_a, vault_b, owner, ata_a, ata_b,
            swap_amount_in, min_amount_out, token_definition_id_in,
            ata_program_id, clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — `SwapExactOutput` with the user side using ATAs.
    /// Account order: `[pool, vault_a, vault_b, owner, ata_a, ata_b, clock]`.
    #[instruction]
    pub fn swap_exact_output_ata(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        clock: AccountWithMetadata,
        exact_amount_out: u128,
        max_amount_in: u128,
        token_definition_id_in: AccountId,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::swap_ata::swap_exact_output_ata(
            pool, vault_a, vault_b, owner, ata_a, ata_b,
            exact_amount_out, max_amount_in, token_definition_id_in,
            ata_program_id, clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// RFP Func #8 — `AddLiquidity` with the user side using ATAs.
    /// Account order: `[pool, vault_a, vault_b, lp_def, owner, ata_a, ata_b, ata_lp, clock]`.
    #[instruction]
    pub fn add_liquidity_ata(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        pool_definition_lp: AccountWithMetadata,
        owner: AccountWithMetadata,
        ata_a: AccountWithMetadata,
        ata_b: AccountWithMetadata,
        ata_lp: AccountWithMetadata,
        clock: AccountWithMetadata,
        min_amount_liquidity: u128,
        max_amount_to_add_token_a: u128,
        max_amount_to_add_token_b: u128,
        ata_program_id: nssa_core::program::ProgramId,
        deadline: u64,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) = amm_program::add_ata::add_liquidity_ata(
            pool, vault_a, vault_b, pool_definition_lp,
            owner, ata_a, ata_b, ata_lp,
            NonZeroU128::new(min_amount_liquidity).expect("min_amount_liquidity must be nonzero"),
            max_amount_to_add_token_a, max_amount_to_add_token_b,
            ata_program_id, clock_ts,
        );
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls)
            .with_timestamp_validity_window(..deadline))
    }

    /// Sync pool reserves with current vault balances.
    #[instruction]
    pub fn sync_reserves(
        pool: AccountWithMetadata,
        vault_a: AccountWithMetadata,
        vault_b: AccountWithMetadata,
        clock: AccountWithMetadata,
    ) -> SpelResult {
        let clock_ts = clock_ms(&clock);
        let (post_states, chained_calls) =
            amm_program::sync::sync_reserves(pool, vault_a, vault_b, clock_ts);
        Ok(spel_framework::SpelOutput::execute(echo_clock(post_states, clock), chained_calls))
    }
}
