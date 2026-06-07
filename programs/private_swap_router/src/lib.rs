//! Account-A private-swap router logic.
//!
//! Emits, as one chained-call tree under a single privacy-preserving
//! proof: (1) deshield - token `Transfer` from the user's
//! circuit-deshielded private input holding into fresh public account A;
//! (2) AMM `SwapExactInput` from A in the public pool; (3) re-shield -
//! token `Transfer` of A's output back to the user's private holding.
//!
//! The re-shield amount is the AMM constant-product output, which is not
//! observable before the swap runs (the whole tree is emitted up front).
//! We therefore **recompute it from the pool reserves the router sees in
//! pre-state, byte-for-byte mirroring `amm/src/swap.rs::swap_logic`**.
//! This duplicate-math coupling is intentional and is the core reason the
//! routerless `PrivateOwned` mode (design.md §5.10 "Private") is the
//! recommended path - this router exists only for verbatim RFP AC #4.

pub use private_swap_router_core as core;

use amm_core::{amm_exact_input_out, Instruction as AmmInstruction, PoolDefinition};
use nssa_core::{
    account::{AccountId, AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall},
};
use token_core::{Instruction as TokenInstruction, TokenHolding};
use wlez_core::Instruction as WlezInstruction;

/// Apply a signed balance delta to a Fungible token holding, returning the
/// updated `AccountWithMetadata`. Used by the router to build each
/// chained call's `pre_states` reflecting the running state produced by
/// prior chained calls in the same proof - the LEZ framework
/// (`nssa/src/validated_state_diff.rs`) checks each call's pre_states
/// against the running state_diff, so they must match.
fn shift_balance(awm: &AccountWithMetadata, delta_pos: u128, sign_pos: bool) -> AccountWithMetadata {
    let mut out = awm.clone();
    let mut h = TokenHolding::try_from(&out.account.data)
        .expect("Disposable router: account-A holding must be an initialized Fungible token holding");
    match &mut h {
        TokenHolding::Fungible { balance, .. } => {
            *balance = if sign_pos {
                balance
                    .checked_add(delta_pos)
                    .expect("Account-A balance overflow")
            } else {
                balance
                    .checked_sub(delta_pos)
                    .expect("Account-A insufficient balance")
            };
        }
        _ => panic!("Disposable router: account-A must be a Fungible token holding"),
    }
    out.account.data = Data::from(&h);
    out
}

/// Orchestrate one atomic deshield → public AMM swap → re-shield.
///
/// Account order: `[user_holding_in, a_holding_a, a_holding_b, pool,
/// vault_a, vault_b, user_holding_out]`. **No clock account** - the
/// chained AMM call uses `SwapExactInputCircuit` which deliberately
/// doesn't touch CLOCK_01 so the privacy proof's pre-state set is
/// drift-free (see `amm_core::Instruction::SwapExactInputCircuit` for
/// the full rationale on CPU-vs-block-period). Private swaps don't
/// tick the on-chain TWAP oracle; public swaps continue to.
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn private_swap(
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
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("Private-swap router expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");

    // Derive callee program ids from the accounts they own (mirrors how
    // `amm/src/swap.rs` derives the token program from a vault's owner) -
    // no hardcoded ids, stays correct across redeploys.
    let token_program_id = user_holding_in.account.program_owner;
    let amm_program_id = pool.account.program_owner;

    // Pick A's input/output holdings + reserve orientation by token.
    let (a_in, a_out, reserve_in, reserve_out) =
        if token_definition_id_in == pool_def.definition_token_a_id {
            (a_holding_a.clone(), a_holding_b.clone(), pool_def.reserve_a, pool_def.reserve_b)
        } else if token_definition_id_in == pool_def.definition_token_b_id {
            (a_holding_b.clone(), a_holding_a.clone(), pool_def.reserve_b, pool_def.reserve_a)
        } else {
            panic!("token_definition_id_in is not a token of this pool");
        };

    let out_amount = amm_exact_input_out(reserve_in, reserve_out, fees, swap_amount_in);
    assert!(
        out_amount >= min_amount_out,
        "Computed output below min_amount_out (slippage)"
    );

    let mut chained_calls = Vec::with_capacity(3);

    // (1) Deshield: user's private input holding → fresh public A.
    //     Effect: user_holding_in.balance -= swap_amount_in; a_in.balance += swap_amount_in.
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![user_holding_in.clone(), a_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // (2) Public AMM swap from A. The framework validates each chained
    //     call's pre_states against the **running state diff** from prior
    //     chained calls (nssa/src/validated_state_diff.rs ~L138-153), so
    //     a_holding_a/b here must reflect the post-deshield balances -
    //     not the proof-start clones. Whichever of a_holding_{a,b} matches
    //     the input side has +swap_amount_in; the other is unchanged.
    let (amm_a_a, amm_a_b) = if token_definition_id_in == pool_def.definition_token_a_id {
        (shift_balance(&a_holding_a, swap_amount_in, true), a_holding_b.clone())
    } else {
        (a_holding_a.clone(), shift_balance(&a_holding_b, swap_amount_in, true))
    };
    chained_calls.push(ChainedCall::new(
        amm_program_id,
        vec![
            pool.clone(),
            vault_a.clone(),
            vault_b.clone(),
            amm_a_a,
            amm_a_b,
            // No clock account - see `SwapExactInputCircuit` doc on
            // the AMM instruction enum for the rationale. Removing
            // CLOCK_01 from the proof's pre-state set lets a slow CPU
            // ZK proof complete and still verify cleanly: the AMM
            // skips its TWAP oracle update for this swap; public
            // swaps via the other variant continue to feed the TWAP.
        ],
        &AmmInstruction::SwapExactInputCircuit {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            deadline,
        },
    ));

    // (3) Re-shield: A's output holding → user's private output holding.
    //     After step (2) the AMM has drained a_in to 0 (swap-in) and
    //     credited a_out with `out_amount` (swap-out). The reshield's
    //     sender is a_out at its post-AMM balance = original + out_amount.
    let a_out_post_amm = shift_balance(&a_out, out_amount, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_out_post_amm, user_holding_out.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: out_amount,
        },
    ));

    // Router owns no persistent state; every balance mutation happens in
    // the chained sub-programs above. The LEZ privacy circuit's
    // `validate_execution` (rule #2 length + rule #3 unchanged-nonce)
    // demands one post-state per input account, matching pre.nonce
    // exactly - so explicitly echo all 8 inputs unchanged here. (Don't
    // rely on SPEL's auto-padding: it triggered ModifiedNonce on the
    // disposable A holdings.)
    let post_states = vec![
        AccountPostState::new(user_holding_in.account),
        AccountPostState::new(a_holding_a.account),
        AccountPostState::new(a_holding_b.account),
        AccountPostState::new(pool.account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(user_holding_out.account),
        // No clock - see chained-call comment above.
    ];

    (post_states, chained_calls)
}

/// Bump or burn `amount` from a native-LEZ public account's balance. Used
/// by the native-batched paths to reflect the running state for chained
/// calls (the framework's `validated_state_diff.rs` checks each call's
/// pre_states against the running diff, so they must match).
fn shift_native_balance(awm: &AccountWithMetadata, delta_pos: u128, sign_pos: bool) -> AccountWithMetadata {
    let mut out = awm.clone();
    out.account.balance = if sign_pos {
        out.account
            .balance
            .checked_add(delta_pos)
            .expect("native balance overflow")
    } else {
        out.account
            .balance
            .checked_sub(delta_pos)
            .expect("native balance underflow")
    };
    out
}

/// One-shot orchestrator for `PrivateSwapNativeIn`: WLEZ::Wrap → AMM::Swap
/// → re-shield, all in one privacy proof. See the variant doc on
/// `core::Instruction::PrivateSwapNativeIn` for the exact account list.
///
/// Replaces the two-tx `wrap → private_swap` flow for native LEZ input:
///   - saves one block wait (~10 s) and one tx-submit roundtrip;
///   - atomic - wrap+swap either both land or neither does (no
///     stuck-WLEZ failure mode).
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn private_swap_native_in(
    user_native: AccountWithMetadata,
    wlez_vault: AccountWithMetadata,
    wlez_definition: AccountWithMetadata,
    a_wlez_holding: AccountWithMetadata,
    a_holding_out: AccountWithMetadata,
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    user_holding_out: AccountWithMetadata,
    // No clock - the chained AMM call uses `SwapExactInputCircuit`
    // (no TWAP oracle update), so the proof's pre-state set is
    // CLOCK_01-free and won't drift during slow CPU proving.
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // Slippage / sanity guards - same shape as `private_swap`.
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("Native-in router expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");

    let wlez_def_id = wlez_definition.account_id;
    // The pool's input side must be WLEZ; output is the other side. Decide
    // orientation from the pool's definitions, exactly like `private_swap`.
    let (reserve_in, reserve_out, wlez_is_token_a) =
        if wlez_def_id == pool_def.definition_token_a_id {
            (pool_def.reserve_a, pool_def.reserve_b, true)
        } else if wlez_def_id == pool_def.definition_token_b_id {
            (pool_def.reserve_b, pool_def.reserve_a, false)
        } else {
            panic!("WLEZ definition is not a token in this pool");
        };

    let out_amount = amm_exact_input_out(reserve_in, reserve_out, fees, swap_amount_in);
    assert!(
        out_amount >= min_amount_out,
        "Computed output below min_amount_out (slippage)"
    );

    // Derive program ids from accounts (no hardcoded ids).
    let wlez_program_id = wlez_vault.account.program_owner;
    let amm_program_id = pool.account.program_owner;
    let token_program_id = wlez_definition.account.program_owner;

    let mut chained_calls = Vec::with_capacity(3);

    // (1) WLEZ::Wrap - user_native → vault drains `swap_amount_in`; mint
    //     adds `swap_amount_in` to `a_wlez_holding`. The wrap program
    //     itself emits two further chained calls (auth_transfer +
    //     token::Mint with PDA-auth on the definition). The chained call's
    //     `pre_states` here is the wrap's own input list; depth+1.
    chained_calls.push(ChainedCall::new(
        wlez_program_id,
        vec![
            user_native.clone(),
            wlez_vault.clone(),
            wlez_definition.clone(),
            a_wlez_holding.clone(),
        ],
        &WlezInstruction::Wrap { amount: swap_amount_in },
    ));

    // (2) AMM::SwapExactInput. The framework validates each chained call's
    //     pre_states against the running state_diff, so the holdings here
    //     must reflect post-wrap balances:
    //       - WLEZ-side A holding: +swap_amount_in (the wrap minted into it)
    //       - Output-side A holding: unchanged
    //     `shift_balance` (defined above) computes the bumped TokenHolding.
    let (amm_a_a, amm_a_b) = if wlez_is_token_a {
        (
            shift_balance(&a_wlez_holding, swap_amount_in, true),
            a_holding_out.clone(),
        )
    } else {
        (
            a_holding_out.clone(),
            shift_balance(&a_wlez_holding, swap_amount_in, true),
        )
    };
    chained_calls.push(ChainedCall::new(
        amm_program_id,
        vec![
            pool.clone(),
            vault_a.clone(),
            vault_b.clone(),
            amm_a_a,
            amm_a_b,
            // No clock - `SwapExactInputCircuit` doesn't tick the
            // TWAP oracle, so the proof's pre-state set has no
            // CLOCK_01 entry to drift. Same fix as `private_swap`.
        ],
        &AmmInstruction::SwapExactInputCircuit {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in: wlez_def_id,
            deadline,
        },
    ));

    // (3) Re-shield: A's output-token holding (post-AMM, balance bumped by
    //     `out_amount`) → user's private output holding.
    let a_out_post_amm = shift_balance(&a_holding_out, out_amount, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_out_post_amm, user_holding_out.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: out_amount,
        },
    ));

    // Echo all 10 inputs as pass-through post-states (the chained calls do
    // the actual balance work). Same shape rationale as `private_swap`.
    let post_states = vec![
        AccountPostState::new(user_native.account),
        AccountPostState::new(wlez_vault.account),
        AccountPostState::new(wlez_definition.account),
        AccountPostState::new(a_wlez_holding.account),
        AccountPostState::new(a_holding_out.account),
        AccountPostState::new(pool.account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(user_holding_out.account),
        // No clock - see chained-call comment above.
    ];

    (post_states, chained_calls)
}

/// One-shot orchestrator for `PrivateSwapNativeOut`: deshield → AMM::Swap
/// → WLEZ::Unwrap, all in one privacy proof. See the variant doc on
/// `core::Instruction::PrivateSwapNativeOut` for the exact account list.
///
/// Replaces the two-tx `private_swap → unwrap` flow for native LEZ output.
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn private_swap_native_out(
    user_holding_in: AccountWithMetadata,
    a_holding_in: AccountWithMetadata,
    a_wlez_holding: AccountWithMetadata,
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    wlez_definition: AccountWithMetadata,
    wlez_vault: AccountWithMetadata,
    user_native: AccountWithMetadata,
    // No clock - chained AMM uses SwapExactInputCircuit (no oracle).
    swap_amount_in: u128,
    min_amount_out: u128,
    token_definition_id_in: AccountId,
    fees: u128,
    deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // Slippage / sanity guards.
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("Native-out router expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");

    let wlez_def_id = wlez_definition.account_id;
    // The pool's OUTPUT side must be WLEZ (we unwrap it). Caller picks
    // input direction via `token_definition_id_in`; verify the other side
    // is WLEZ to prevent unwrap of an unrelated token.
    let (reserve_in, reserve_out, in_is_token_a) =
        if token_definition_id_in == pool_def.definition_token_a_id {
            assert_eq!(
                pool_def.definition_token_b_id, wlez_def_id,
                "Native-out: pool's non-input side must be WLEZ"
            );
            (pool_def.reserve_a, pool_def.reserve_b, true)
        } else if token_definition_id_in == pool_def.definition_token_b_id {
            assert_eq!(
                pool_def.definition_token_a_id, wlez_def_id,
                "Native-out: pool's non-input side must be WLEZ"
            );
            (pool_def.reserve_b, pool_def.reserve_a, false)
        } else {
            panic!("token_definition_id_in is not a token of this pool");
        };

    let out_amount = amm_exact_input_out(reserve_in, reserve_out, fees, swap_amount_in);
    assert!(
        out_amount >= min_amount_out,
        "Computed output below min_amount_out (slippage)"
    );

    let wlez_program_id = wlez_vault.account.program_owner;
    let amm_program_id = pool.account.program_owner;
    let token_program_id = wlez_definition.account.program_owner;

    let mut chained_calls = Vec::with_capacity(3);

    // (1) Deshield: user's private input → A's input-token holding.
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![user_holding_in.clone(), a_holding_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // (2) AMM::SwapExactInput. Running state diff: A's input-token holding
    //     has +swap_amount_in; A's WLEZ holding (the output side) is
    //     unchanged at swap-time. Choose order based on which side is
    //     token A in the pool.
    let (amm_a_a, amm_a_b) = if in_is_token_a {
        (
            shift_balance(&a_holding_in, swap_amount_in, true),
            a_wlez_holding.clone(),
        )
    } else {
        (
            a_wlez_holding.clone(),
            shift_balance(&a_holding_in, swap_amount_in, true),
        )
    };
    chained_calls.push(ChainedCall::new(
        amm_program_id,
        vec![
            pool.clone(),
            vault_a.clone(),
            vault_b.clone(),
            amm_a_a,
            amm_a_b,
            // No clock - see `private_swap` doc for the rationale.
        ],
        &AmmInstruction::SwapExactInputCircuit {
            swap_amount_in,
            min_amount_out,
            token_definition_id_in,
            deadline,
        },
    ));

    // (3) WLEZ::Unwrap. Running state diff: A's WLEZ holding has
    //     +out_amount (the AMM credited it). Wrap call sees post-AMM
    //     balance via its first pre_state.
    let a_wlez_post_amm = shift_balance(&a_wlez_holding, out_amount, true);
    chained_calls.push(ChainedCall::new(
        wlez_program_id,
        vec![
            a_wlez_post_amm,
            wlez_definition.clone(),
            wlez_vault.clone(),
            user_native.clone(),
        ],
        &WlezInstruction::Unwrap { amount: out_amount },
    ));

    // Echo all 10 inputs as pass-through post-states.
    let post_states = vec![
        AccountPostState::new(user_holding_in.account),
        AccountPostState::new(a_holding_in.account),
        AccountPostState::new(a_wlez_holding.account),
        AccountPostState::new(pool.account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(wlez_definition.account),
        AccountPostState::new(wlez_vault.account),
        AccountPostState::new(user_native.account),
        // No clock - see chained-call comment above.
    ];

    // `shift_native_balance` is used only by the host-side FFI plumbing
    // that mirrors the running native diff for `ldex_*` callers; the
    // chained Wrap/Unwrap inside the proof needs no such mirroring (it
    // already operates on the running pre_states). Silence unused warning
    // when the FFI path is not compiled in.
    let _ = shift_native_balance;

    (post_states, chained_calls)
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the wrap/unwrap-coupled native router paths
    //! (`private_swap_native_in` / `private_swap_native_out`), which were
    //! otherwise entirely unexercised - only the token↔token `private_swap`
    //! had been manually live-tested. These paths hand-build the most
    //! intricate chained-call trees in the program, and their correctness
    //! hinges on byte-for-byte pre-state reconciliation against the running
    //! state diff: the `shift_balance` direction/amount on the WLEZ-side A
    //! holding (the wrap mints into it for native-in; the AMM credits it for
    //! native-out), the WLEZ account vector order, and the
    //! `wlez_is_token_a` / `in_is_token_a` orientation pick. A wrong direction,
    //! a missing/extra balance shift, or an out-of-order WLEZ account vector
    //! would only surface at proof-reconstruction time as
    //! `InvalidPrivacyPreservingProof` / `UnauthorizedBalanceDecrease` - never
    //! at compile time and never in CI. The asserts below pin each of those so
    //! an off-by-one fails here instead of only at the sequencer. Both pool
    //! orientations (WLEZ as token A and as token B) are driven.
    //!
    //! Mirrors the fixture/decode style of `programs/amm_v2/src/lib.rs` tests.

    use super::*;
    use amm_core::{compute_pool_pda, compute_vault_pda, FEE_TIER_BPS_30};
    use nssa_core::{
        account::{Account, Nonce},
        program::ProgramId,
    };
    use token_core::TokenDefinition;

    const AMM_ID: ProgramId = [9; 8];
    const TOKEN_ID: ProgramId = [7; 8];
    const WLEZ_ID: ProgramId = [3; 8];
    const NATIVE_ID: ProgramId = [1; 8];

    const FEE: u128 = FEE_TIER_BPS_30;
    const RESERVE_A0: u128 = 5_000;
    const RESERVE_B0: u128 = 2_500;

    fn id(b: u8) -> AccountId {
        AccountId::new([b; 32])
    }

    fn fungible_account(definition_id: AccountId, balance: u128) -> Account {
        Account {
            program_owner: TOKEN_ID,
            balance: 0,
            data: Data::from(&TokenHolding::Fungible { definition_id, balance }),
            nonce: Nonce(0),
        }
    }

    /// `AccountWithMetadata` for a Fungible holding (token-program owned).
    fn holding(account_id: AccountId, definition_id: AccountId, balance: u128) -> AccountWithMetadata {
        AccountWithMetadata::new(fungible_account(definition_id, balance), false, account_id)
    }

    fn balance_of(awm: &AccountWithMetadata) -> u128 {
        match TokenHolding::try_from(&awm.account.data).expect("fungible") {
            TokenHolding::Fungible { balance, .. } => balance,
            _ => panic!("not fungible"),
        }
    }

    fn decode_token(call: &ChainedCall) -> TokenInstruction {
        risc0_zkvm::serde::from_slice(&call.instruction_data).expect("token instruction decode")
    }

    fn decode_wlez(call: &ChainedCall) -> WlezInstruction {
        risc0_zkvm::serde::from_slice(&call.instruction_data).expect("wlez instruction decode")
    }

    fn def_a() -> AccountId {
        id(3)
    }
    fn def_b() -> AccountId {
        id(4)
    }
    fn pool_id() -> AccountId {
        compute_pool_pda(AMM_ID, def_a(), def_b(), FEE)
    }
    fn vault_a_id() -> AccountId {
        compute_vault_pda(AMM_ID, pool_id(), def_a())
    }
    fn vault_b_id() -> AccountId {
        compute_vault_pda(AMM_ID, pool_id(), def_b())
    }

    /// A live, AMM-owned token↔token pool with the given reserves. The router
    /// reads `amm_program_id` from this account's `program_owner`.
    fn live_pool(reserve_a: u128, reserve_b: u128) -> AccountWithMetadata {
        let def = PoolDefinition {
            definition_token_a_id: def_a(),
            definition_token_b_id: def_b(),
            vault_a_id: vault_a_id(),
            vault_b_id: vault_b_id(),
            reserve_a,
            reserve_b,
            fees: FEE,
            ..Default::default()
        };
        let account = Account {
            program_owner: AMM_ID,
            balance: 0,
            data: Data::from(&def),
            nonce: Nonce(0),
        };
        AccountWithMetadata::new(account, false, pool_id())
    }

    // ---- WLEZ-side fixtures ----------------------------------------------
    //
    // The native handlers derive `wlez_program_id` from `wlez_vault`'s owner
    // and `token_program_id` from `wlez_definition`'s owner - they never call
    // the WLEZ PDA helpers, so synthetic ids suffice. The only binding the
    // handler enforces is `wlez_definition.account_id == pool's WLEZ side`.

    /// A plain native account (owned by the native-transfer program, not a
    /// token holding) - models `user_native`.
    fn native_account(account_id: AccountId, balance: u128) -> AccountWithMetadata {
        let account = Account {
            program_owner: NATIVE_ID,
            balance,
            data: Data::default(),
            nonce: Nonce(0),
        };
        AccountWithMetadata::new(account, false, account_id)
    }

    /// The WLEZ vault PDA account (WLEZ-program owned - its owner is read as
    /// `wlez_program_id`).
    fn wlez_vault(account_id: AccountId) -> AccountWithMetadata {
        let account = Account {
            program_owner: WLEZ_ID,
            balance: 0,
            data: Data::default(),
            nonce: Nonce(0),
        };
        AccountWithMetadata::new(account, false, account_id)
    }

    /// The WLEZ token-definition account (token-program owned - its owner is
    /// read as `token_program_id` and its id is matched against the pool's
    /// WLEZ side).
    fn wlez_definition(account_id: AccountId) -> AccountWithMetadata {
        let account = Account {
            program_owner: TOKEN_ID,
            balance: 0,
            data: Data::from(&TokenDefinition::Fungible {
                name: String::from("WLEZ"),
                total_supply: 0,
                metadata_id: None,
            }),
            nonce: Nonce(0),
        };
        AccountWithMetadata::new(account, false, account_id)
    }

    // ---- private_swap_native_in (LEZ -> token) ---------------------------

    /// WLEZ is token A of the pool; the output token is token B. Pins the
    /// full Wrap → AMM(SwapExactInputCircuit) → reshield tree, the
    /// post-wrap +swap_amount_in credit on the WLEZ-side A holding, and the
    /// post-AMM +out_amount credit on the output-side A holding.
    #[test]
    fn native_in_wlez_token_a_reconciles_chained_call_pre_states() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let user_native = native_account(id(50), swap_in);
        let wlez_vault = wlez_vault(id(51));
        let wlez_definition = wlez_definition(def_a()); // WLEZ == pool token A
        let a_wlez = holding(id(52), def_a(), 0);
        let a_out = holding(id(53), def_b(), 0);
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let user_out = holding(id(54), def_b(), 0);

        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        assert!(out_amount > 0, "fixture must produce nonzero output");

        let (post_states, calls) = private_swap_native_in(
            user_native.clone(),
            wlez_vault.clone(),
            wlez_definition.clone(),
            a_wlez.clone(),
            a_out.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            user_out.clone(),
            swap_in,
            out_amount, // min_amount_out == exact out: passes slippage check
            FEE,
            u64::MAX,
        );

        // Three calls: Wrap, AMM swap, reshield. No separate vault legs -
        // the AMM does its own swap internally via SwapExactInputCircuit.
        assert_eq!(calls.len(), 3, "Wrap/AMM-swap/reshield");

        // 1) WLEZ::Wrap - account order per wlez_core::Instruction::Wrap docs:
        //    [user_native, vault, definition, a_wlez_holding]. Amount = swap_in.
        assert_eq!(calls[0].program_id, WLEZ_ID, "Wrap runs on the WLEZ program");
        assert_eq!(calls[0].pre_states[0].account_id, user_native.account_id);
        assert_eq!(calls[0].pre_states[1].account_id, wlez_vault.account_id);
        assert_eq!(calls[0].pre_states[2].account_id, wlez_definition.account_id);
        assert_eq!(calls[0].pre_states[3].account_id, a_wlez.account_id);
        assert!(
            matches!(decode_wlez(&calls[0]), WlezInstruction::Wrap { amount } if amount == swap_in),
        );

        // 2) AMM::SwapExactInputCircuit - pre_states [pool, vault_a, vault_b,
        //    amm_a_a, amm_a_b]; exactly five (no clock). WLEZ-side A holding
        //    (token A here, so amm_a_a) MUST reflect the running diff: it was
        //    credited by `swap_in` by the Wrap mint. The output side (amm_a_b)
        //    is unchanged at swap time.
        assert_eq!(calls[1].program_id, AMM_ID, "swap runs on the AMM program");
        assert_eq!(calls[1].pre_states.len(), 5, "no clock in the AMM pre-state set");
        assert_eq!(calls[1].pre_states[0].account_id, pool_id());
        assert_eq!(calls[1].pre_states[1].account_id, vault_a.account_id);
        assert_eq!(calls[1].pre_states[2].account_id, vault_b.account_id);
        assert_eq!(calls[1].pre_states[3].account_id, a_wlez.account_id, "amm_a_a == WLEZ side");
        assert_eq!(
            balance_of(&calls[1].pre_states[3]),
            swap_in,
            "WLEZ-side A holding pre-state must include the Wrap-mint credit (shift_balance +)"
        );
        assert_eq!(calls[1].pre_states[4].account_id, a_out.account_id, "amm_a_b == output side");
        assert_eq!(
            balance_of(&calls[1].pre_states[4]),
            0,
            "output-side A holding is unchanged at swap time"
        );
        assert!(matches!(
            decode_amm(&calls[1]),
            AmmInstruction::SwapExactInputCircuit { swap_amount_in, token_definition_id_in, .. }
                if swap_amount_in == swap_in && token_definition_id_in == wlez_definition.account_id
        ));

        // 3) Reshield: a_out (post-AMM, credited by out_amount) -> user_out.
        assert_eq!(calls[2].program_id, TOKEN_ID);
        assert_eq!(calls[2].pre_states[0].account_id, a_out.account_id);
        assert_eq!(
            balance_of(&calls[2].pre_states[0]),
            out_amount,
            "a_out pre-state must include the AMM-out credit (shift_balance +)"
        );
        assert_eq!(calls[2].pre_states[1].account_id, user_out.account_id);
        assert!(
            matches!(decode_token(&calls[2]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == out_amount),
        );

        // All 9 inputs echoed as pass-through post-states, in declared order.
        assert_eq!(post_states.len(), 9, "one post-state per input, no clock");
        assert_eq!(post_states[0].account().program_owner, NATIVE_ID, "user_native first");
        assert_eq!(post_states[5].account().program_owner, AMM_ID, "pool at index 5");
    }

    /// Same path with WLEZ as the pool's token B - the orientation pick must
    /// place the credited WLEZ holding in the `amm_a_b` slot and the
    /// unchanged output holding in `amm_a_a`. This locks the
    /// `wlez_is_token_a` branch that the token-A test cannot reach.
    #[test]
    fn native_in_wlez_token_b_reconciles_chained_call_pre_states() {
        let swap_in = 1_000u128;
        // Pool is (token A = def_a non-WLEZ output, token B = def_b WLEZ).
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let user_native = native_account(id(50), swap_in);
        let wlez_vault = wlez_vault(id(51));
        let wlez_definition = wlez_definition(def_b()); // WLEZ == pool token B
        let a_wlez = holding(id(52), def_b(), 0);
        let a_out = holding(id(53), def_a(), 0);
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let user_out = holding(id(54), def_a(), 0);

        // WLEZ is the input side: reserve_in == reserve_b, reserve_out == reserve_a.
        let out_amount = amm_exact_input_out(RESERVE_B0, RESERVE_A0, FEE, swap_in);
        assert!(out_amount > 0, "fixture must produce nonzero output");

        let (_post_states, calls) = private_swap_native_in(
            user_native.clone(),
            wlez_vault.clone(),
            wlez_definition.clone(),
            a_wlez.clone(),
            a_out.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            user_out.clone(),
            swap_in,
            out_amount,
            FEE,
            u64::MAX,
        );

        assert_eq!(calls.len(), 3);
        // AMM call: amm_a_a is the (unchanged) output side, amm_a_b is the
        // WLEZ side credited by the Wrap mint.
        assert_eq!(calls[1].pre_states[3].account_id, a_out.account_id, "amm_a_a == output side");
        assert_eq!(balance_of(&calls[1].pre_states[3]), 0, "output side unchanged");
        assert_eq!(calls[1].pre_states[4].account_id, a_wlez.account_id, "amm_a_b == WLEZ side");
        assert_eq!(
            balance_of(&calls[1].pre_states[4]),
            swap_in,
            "WLEZ-side A holding (token B) must carry the Wrap-mint credit"
        );
        // Reshield still pays out_amount from the output-side holding.
        assert_eq!(calls[2].pre_states[0].account_id, a_out.account_id);
        assert_eq!(balance_of(&calls[2].pre_states[0]), out_amount);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn native_in_rejects_min_out_above_computed() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = private_swap_native_in(
            native_account(id(50), swap_in),
            wlez_vault(id(51)),
            wlez_definition(def_a()),
            holding(id(52), def_a(), 0),
            holding(id(53), def_b(), 0),
            pool,
            holding(vault_a_id(), def_a(), RESERVE_A0),
            holding(vault_b_id(), def_b(), RESERVE_B0),
            holding(id(54), def_b(), 0),
            swap_in,
            out_amount + 1, // demand more than achievable -> slippage panic
            FEE,
            u64::MAX,
        );
    }

    // ---- private_swap_native_out (token -> LEZ) --------------------------

    /// Input is token A (non-WLEZ); WLEZ is token B (the output side we
    /// unwrap). Pins the full deshield → AMM → Unwrap tree, the post-deshield
    /// +swap_amount_in credit on the input-side A holding, and the post-AMM
    /// +out_amount credit on the WLEZ-side A holding consumed by the Unwrap.
    #[test]
    fn native_out_wlez_token_b_reconciles_chained_call_pre_states() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let user_in = holding(id(60), def_a(), swap_in);
        let a_in = holding(id(61), def_a(), 0);
        let a_wlez = holding(id(62), def_b(), 0); // WLEZ side (token B)
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let wlez_definition = wlez_definition(def_b()); // WLEZ == pool token B
        let wlez_vault = wlez_vault(id(63));
        let user_native = native_account(id(64), 0);

        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        assert!(out_amount > 0, "fixture must produce nonzero output");

        let (post_states, calls) = private_swap_native_out(
            user_in.clone(),
            a_in.clone(),
            a_wlez.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            wlez_definition.clone(),
            wlez_vault.clone(),
            user_native.clone(),
            swap_in,
            out_amount, // min_amount_out == exact out: passes slippage check
            def_a(),    // token_in == token A (non-WLEZ side)
            FEE,
            u64::MAX,
        );

        // Three calls: deshield, AMM swap, Unwrap.
        assert_eq!(calls.len(), 3, "deshield/AMM-swap/Unwrap");

        // 1) Deshield: user_in -> a_in (token A side), full swap_in.
        assert_eq!(calls[0].program_id, TOKEN_ID);
        assert_eq!(calls[0].pre_states[0].account_id, user_in.account_id);
        assert_eq!(calls[0].pre_states[1].account_id, a_in.account_id);
        assert!(
            matches!(decode_token(&calls[0]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == swap_in),
        );

        // 2) AMM::SwapExactInputCircuit - five pre_states (no clock). The
        //    input-side A holding (token A == amm_a_a) MUST reflect the
        //    deshield credit; the WLEZ output side (amm_a_b) is unchanged.
        assert_eq!(calls[1].program_id, AMM_ID);
        assert_eq!(calls[1].pre_states.len(), 5, "no clock in the AMM pre-state set");
        assert_eq!(calls[1].pre_states[0].account_id, pool_id());
        assert_eq!(calls[1].pre_states[1].account_id, vault_a.account_id);
        assert_eq!(calls[1].pre_states[2].account_id, vault_b.account_id);
        assert_eq!(calls[1].pre_states[3].account_id, a_in.account_id, "amm_a_a == input side");
        assert_eq!(
            balance_of(&calls[1].pre_states[3]),
            swap_in,
            "input-side A holding pre-state must include the deshield credit (shift_balance +)"
        );
        assert_eq!(calls[1].pre_states[4].account_id, a_wlez.account_id, "amm_a_b == WLEZ side");
        assert_eq!(balance_of(&calls[1].pre_states[4]), 0, "WLEZ side unchanged at swap time");
        assert!(matches!(
            decode_amm(&calls[1]),
            AmmInstruction::SwapExactInputCircuit { swap_amount_in, token_definition_id_in, .. }
                if swap_amount_in == swap_in && token_definition_id_in == def_a()
        ));

        // 3) WLEZ::Unwrap - account order per wlez_core::Instruction::Unwrap
        //    docs: [a_wlez_holding, definition, vault, user_native]. The
        //    a_wlez pre-state reflects the running diff (credited by out_amount
        //    in call 2) before the burn. Amount = out_amount.
        assert_eq!(calls[2].program_id, WLEZ_ID, "Unwrap runs on the WLEZ program");
        assert_eq!(calls[2].pre_states[0].account_id, a_wlez.account_id);
        assert_eq!(
            balance_of(&calls[2].pre_states[0]),
            out_amount,
            "a_wlez pre-state must include the AMM-out credit (shift_balance +)"
        );
        assert_eq!(calls[2].pre_states[1].account_id, wlez_definition.account_id);
        assert_eq!(calls[2].pre_states[2].account_id, wlez_vault.account_id);
        assert_eq!(calls[2].pre_states[3].account_id, user_native.account_id);
        assert!(
            matches!(decode_wlez(&calls[2]), WlezInstruction::Unwrap { amount } if amount == out_amount),
        );

        // All 9 inputs echoed as pass-through post-states, in declared order.
        assert_eq!(post_states.len(), 9, "one post-state per input, no clock");
        assert_eq!(post_states[8].account().program_owner, NATIVE_ID, "user_native last");
    }

    /// Same path with WLEZ as the pool's token A (input is token B). The
    /// orientation pick must put the credited input holding in `amm_a_b` and
    /// the unchanged WLEZ holding in `amm_a_a`, and the `assert_eq!` guard
    /// that "the pool's non-input side must be WLEZ" must hold. Locks the
    /// `in_is_token_a == false` branch.
    #[test]
    fn native_out_wlez_token_a_reconciles_chained_call_pre_states() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let user_in = holding(id(60), def_b(), swap_in); // input == token B
        let a_in = holding(id(61), def_b(), 0);
        let a_wlez = holding(id(62), def_a(), 0); // WLEZ side (token A)
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let wlez_definition = wlez_definition(def_a()); // WLEZ == pool token A
        let wlez_vault = wlez_vault(id(63));
        let user_native = native_account(id(64), 0);

        // Input is token B: reserve_in == reserve_b, reserve_out == reserve_a.
        let out_amount = amm_exact_input_out(RESERVE_B0, RESERVE_A0, FEE, swap_in);
        assert!(out_amount > 0, "fixture must produce nonzero output");

        let (_post_states, calls) = private_swap_native_out(
            user_in.clone(),
            a_in.clone(),
            a_wlez.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            wlez_definition.clone(),
            wlez_vault.clone(),
            user_native.clone(),
            swap_in,
            out_amount,
            def_b(), // token_in == token B
            FEE,
            u64::MAX,
        );

        assert_eq!(calls.len(), 3);
        // AMM call: amm_a_a is the unchanged WLEZ side, amm_a_b is the
        // input side credited by the deshield.
        assert_eq!(calls[1].pre_states[3].account_id, a_wlez.account_id, "amm_a_a == WLEZ side");
        assert_eq!(balance_of(&calls[1].pre_states[3]), 0, "WLEZ side unchanged at swap time");
        assert_eq!(calls[1].pre_states[4].account_id, a_in.account_id, "amm_a_b == input side");
        assert_eq!(
            balance_of(&calls[1].pre_states[4]),
            swap_in,
            "input-side A holding (token B) must carry the deshield credit"
        );
        // Unwrap still consumes out_amount from the WLEZ-side holding.
        assert_eq!(calls[2].pre_states[0].account_id, a_wlez.account_id);
        assert_eq!(balance_of(&calls[2].pre_states[0]), out_amount);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn native_out_rejects_min_out_above_computed() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = private_swap_native_out(
            holding(id(60), def_a(), swap_in),
            holding(id(61), def_a(), 0),
            holding(id(62), def_b(), 0),
            pool,
            holding(vault_a_id(), def_a(), RESERVE_A0),
            holding(vault_b_id(), def_b(), RESERVE_B0),
            wlez_definition(def_b()),
            wlez_vault(id(63)),
            native_account(id(64), 0),
            swap_in,
            out_amount + 1, // demand more than achievable -> slippage panic
            def_a(),
            FEE,
            u64::MAX,
        );
    }

    /// Native-out guards against unwrapping an unrelated token: if the pool's
    /// non-input side is NOT WLEZ, the handler must panic. Here WLEZ is
    /// declared as a third (def_c) id absent from the pool, so the
    /// "non-input side must be WLEZ" assertion fires.
    #[test]
    #[should_panic(expected = "non-input side must be WLEZ")]
    fn native_out_rejects_non_wlez_output_side() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = private_swap_native_out(
            holding(id(60), def_a(), swap_in),
            holding(id(61), def_a(), 0),
            holding(id(62), def_b(), 0),
            pool,
            holding(vault_a_id(), def_a(), RESERVE_A0),
            holding(vault_b_id(), def_b(), RESERVE_B0),
            wlez_definition(id(99)), // WLEZ def id not a token of this pool
            wlez_vault(id(63)),
            native_account(id(64), 0),
            swap_in,
            out_amount,
            def_a(),
            FEE,
            u64::MAX,
        );
    }

    fn decode_amm(call: &ChainedCall) -> AmmInstruction {
        risc0_zkvm::serde::from_slice(&call.instruction_data).expect("amm instruction decode")
    }
}
