//! Account-A private-swap router logic.
//!
//! Emits, as one chained-call tree under a single privacy-preserving
//! proof: (1) deshield — token `Transfer` from the user's
//! circuit-deshielded private input holding into fresh public account A;
//! (2) AMM `SwapExactInput` from A in the public pool; (3) re-shield —
//! token `Transfer` of A's output back to the user's private holding.
//!
//! The re-shield amount is the AMM constant-product output, which is not
//! observable before the swap runs (the whole tree is emitted up front).
//! We therefore **recompute it from the pool reserves the router sees in
//! pre-state, byte-for-byte mirroring `amm/src/swap.rs::swap_logic`**.
//! This duplicate-math coupling is intentional and is the core reason the
//! routerless `PrivateOwned` mode (design.md §5.10 "Private") is the
//! recommended path — this router exists only for verbatim RFP AC #4.

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
/// prior chained calls in the same proof — the LEZ framework
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
/// vault_a, vault_b, user_holding_out]`. **No clock account** — the
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
    // `amm/src/swap.rs` derives the token program from a vault's owner) —
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
    //     a_holding_a/b here must reflect the post-deshield balances —
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
            // No clock account — see `SwapExactInputCircuit` doc on
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
    // exactly — so explicitly echo all 8 inputs unchanged here. (Don't
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
        // No clock — see chained-call comment above.
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
///   - atomic — wrap+swap either both land or neither does (no
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
    // No clock — the chained AMM call uses `SwapExactInputCircuit`
    // (no TWAP oracle update), so the proof's pre-state set is
    // CLOCK_01-free and won't drift during slow CPU proving.
    swap_amount_in: u128,
    min_amount_out: u128,
    fees: u128,
    deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // Slippage / sanity guards — same shape as `private_swap`.
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

    // (1) WLEZ::Wrap — user_native → vault drains `swap_amount_in`; mint
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
            // No clock — `SwapExactInputCircuit` doesn't tick the
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
        // No clock — see chained-call comment above.
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
    // No clock — chained AMM uses SwapExactInputCircuit (no oracle).
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
            // No clock — see `private_swap` doc for the rationale.
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
        // No clock — see chained-call comment above.
    ];

    // `shift_native_balance` is used only by the host-side FFI plumbing
    // that mirrors the running native diff for `ldex_*` callers; the
    // chained Wrap/Unwrap inside the proof needs no such mirroring (it
    // already operates on the running pre_states). Silence unused warning
    // when the FFI path is not compiled in.
    let _ = shift_native_balance;

    (post_states, chained_calls)
}
