//! amm_v2 combined private-swap program — orchestration + AMM math
//! inlined into one chained-call program, for all three disposable-
//! swap variants (token↔token, LEZ→token, token→LEZ).
//!
//! amm_v2's standard ops (NewDefinition, AddLiquidity, RemoveLiquidity,
//! SwapExactInput, SwapExactInputCircuit) delegate to the canonical
//! `amm_program` crate parameterised by amm_v2's `self_program_id` —
//! so pools/vaults/LP-tokens derive under amm_v2 and amm_v2 owns them.
//!
//! The DisposableSwap / DisposableSwapNativeIn / DisposableSwapNativeOut
//! variants are the "combined inner program" approach: amm_v2 is the
//! top-level chained call from the upstream privacy circuit, inlines
//! the AMM math + reserve updates, and chains only the necessary
//! token::Transfer (and WLEZ::Wrap/Unwrap for native variants).

pub use amm_v2_core as core;

use amm_core::{
    assert_supported_fee_tier, compute_liquidity_token_pda,
    compute_liquidity_token_pda_seed, compute_lp_lock_holding_pda,
    compute_lp_lock_holding_pda_seed, compute_pool_pda,
    compute_pool_pda_seed, compute_vault_pda, compute_vault_pda_seed,
    FEE_BPS_DENOMINATOR, MINIMUM_LIQUIDITY, PoolDefinition,
};
use nssa_core::{
    account::{Account, AccountId, AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall, Claim, ProgramId},
};
use std::num::NonZeroU128;
use token_core::{Instruction as TokenInstruction, TokenDefinition, TokenHolding};
use wlez_core::Instruction as WlezInstruction;

/// Apply a signed balance delta to a Fungible token holding (same
/// shift_balance pattern as `private_swap_router::shift_balance` —
/// used to construct each chained call's pre-state reflecting the
/// running state diff from prior chained calls in the same proof).
fn shift_balance(
    awm: &AccountWithMetadata,
    delta_pos: u128,
    sign_pos: bool,
) -> AccountWithMetadata {
    let mut out = awm.clone();
    let mut h = TokenHolding::try_from(&out.account.data)
        .expect("amm_v2: chained-call pre-state must be an initialized Fungible token holding");
    match &mut h {
        TokenHolding::Fungible { balance, .. } => {
            *balance = if sign_pos {
                balance
                    .checked_add(delta_pos)
                    .expect("amm_v2: balance overflow on shift")
            } else {
                balance
                    .checked_sub(delta_pos)
                    .expect("amm_v2: insufficient balance on shift")
            };
        }
        _ => panic!("amm_v2: chained-call pre-state must be Fungible"),
    }
    out.account.data = Data::from(&h);
    out
}

/// AMM constant-product swap math (mirror of `amm/src/swap.rs::swap_logic`,
/// floor-rounded with fee-adjusted constant product). Common to all
/// three disposable variants.
fn amm_exact_input_out(
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u128,
    swap_amount_in: u128,
) -> u128 {
    let effective_in = swap_amount_in
        .checked_mul(FEE_BPS_DENOMINATOR - fee_bps)
        .expect("effective_in mul overflow")
        / FEE_BPS_DENOMINATOR;
    assert!(effective_in != 0, "effective input is zero — increase swap_amount_in");
    reserve_out
        .checked_mul(effective_in)
        .expect("reserve_out * effective_in overflow")
        / reserve_in
            .checked_add(effective_in)
            .expect("reserve_in + effective_in overflow")
}

/// Apply the same pool-reserve update as `SwapExactInputCircuit` (no
/// oracle update — drift-free pre-state set). Returns the updated
/// `PoolDefinition` ready to be re-serialised into `pool.account.data`.
fn pool_post_def(
    pool_def: &PoolDefinition,
    deposit_a: u128,
    withdraw_a: u128,
    deposit_b: u128,
    withdraw_b: u128,
) -> PoolDefinition {
    let fee_a = if deposit_a > 0 {
        deposit_a - (deposit_a * (FEE_BPS_DENOMINATOR - pool_def.fees) / FEE_BPS_DENOMINATOR)
    } else {
        0
    };
    let fee_b = if deposit_b > 0 {
        deposit_b - (deposit_b * (FEE_BPS_DENOMINATOR - pool_def.fees) / FEE_BPS_DENOMINATOR)
    } else {
        0
    };
    let new_reserve_a = pool_def
        .reserve_a
        .checked_add(deposit_a)
        .expect("reserve_a + deposit_a overflow")
        .checked_sub(withdraw_a)
        .expect("reserve_a underflow");
    let new_reserve_b = pool_def
        .reserve_b
        .checked_add(deposit_b)
        .expect("reserve_b + deposit_b overflow")
        .checked_sub(withdraw_b)
        .expect("reserve_b underflow");
    PoolDefinition {
        reserve_a: new_reserve_a,
        reserve_b: new_reserve_b,
        cum_volume_a: pool_def
            .cum_volume_a
            .saturating_add(deposit_a)
            .saturating_add(withdraw_a),
        cum_volume_b: pool_def
            .cum_volume_b
            .saturating_add(deposit_b)
            .saturating_add(withdraw_b),
        cum_fees_a: pool_def.cum_fees_a.saturating_add(fee_a),
        cum_fees_b: pool_def.cum_fees_b.saturating_add(fee_b),
        ..pool_def.clone()
    }
}

/// Token-pair disposable swap (mode-2 RFP-literal account-A).
///
/// 7-account layout, chains 4× token::Transfer (deshield, vault_in,
/// vault_out, reshield). vs the recursive router+amm path: saves 1
/// chained call (the AMM intermediate).
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn disposable_swap(
    self_program_id: nssa_core::program::ProgramId,
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
    _deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("amm_v2 disposable_swap expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");
    assert_eq!(
        pool.account.program_owner,
        self_program_id,
        "Pool must be owned by amm_v2 (call NewDefinition on amm_v2 first)"
    );
    assert_eq!(
        vault_a.account_id, pool_def.vault_a_id,
        "Vault A account id mismatch with pool"
    );
    assert_eq!(
        vault_b.account_id, pool_def.vault_b_id,
        "Vault B account id mismatch with pool"
    );

    let token_a_id = pool_def.definition_token_a_id;
    let token_b_id = pool_def.definition_token_b_id;
    let token_in_is_a = if token_definition_id_in == token_a_id {
        true
    } else if token_definition_id_in == token_b_id {
        false
    } else {
        panic!("token_definition_id_in is not a token of this pool");
    };
    let (reserve_in, reserve_out) = if token_in_is_a {
        (pool_def.reserve_a, pool_def.reserve_b)
    } else {
        (pool_def.reserve_b, pool_def.reserve_a)
    };
    let (a_in, a_out) = if token_in_is_a {
        (a_holding_a.clone(), a_holding_b.clone())
    } else {
        (a_holding_b.clone(), a_holding_a.clone())
    };
    let (vault_in, vault_out) = if token_in_is_a {
        (vault_a.clone(), vault_b.clone())
    } else {
        (vault_b.clone(), vault_a.clone())
    };

    let out_amount = amm_exact_input_out(reserve_in, reserve_out, fees, swap_amount_in);
    assert!(
        out_amount >= min_amount_out,
        "Computed output below min_amount_out (slippage)"
    );

    let (deposit_a, withdraw_a, deposit_b, withdraw_b) = if token_in_is_a {
        (swap_amount_in, 0u128, 0u128, out_amount)
    } else {
        (0u128, out_amount, swap_amount_in, 0u128)
    };
    let pool_post_d = pool_post_def(&pool_def, deposit_a, withdraw_a, deposit_b, withdraw_b);
    let mut pool_post_account = pool.account.clone();
    pool_post_account.data = Data::from(&pool_post_d);

    let token_program_id = user_holding_in.account.program_owner;

    let mut chained_calls = Vec::with_capacity(4);

    // 1) Deshield: user_holding_in → a_in (user-authorised drain).
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![user_holding_in.clone(), a_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // 2) Pool deposit: a_in (post-deshield) → vault_in (PDA-auth on vault_in).
    let a_in_post_deshield = shift_balance(&a_in, swap_amount_in, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_in_post_deshield, vault_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // 3) Pool withdraw: vault_out (PDA-auth) → a_out.
    let vault_out_seed = compute_vault_pda_seed(
        pool.account_id,
        TokenHolding::try_from(&vault_out.account.data)
            .expect("vault_out must hold a valid TokenHolding")
            .definition_id(),
    );
    let mut vault_out_auth = vault_out.clone();
    vault_out_auth.is_authorized = true;
    chained_calls.push(
        ChainedCall::new(
            token_program_id,
            vec![vault_out_auth, a_out.clone()],
            &TokenInstruction::Transfer {
                amount_to_transfer: out_amount,
            },
        )
        .with_pda_seeds(vec![vault_out_seed]),
    );

    // 4) Reshield: a_out (post-AMM) → user_holding_out (private receiver).
    let a_out_post_amm = shift_balance(&a_out, out_amount, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_out_post_amm, user_holding_out.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: out_amount,
        },
    ));

    let post_states = vec![
        AccountPostState::new(user_holding_in.account),
        AccountPostState::new(a_holding_a.account),
        AccountPostState::new(a_holding_b.account),
        AccountPostState::new(pool_post_account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(user_holding_out.account),
    ];

    (post_states, chained_calls)
}

/// Native-LEZ-INPUT disposable swap (mode-2, LEZ → token).
///
/// 9-account layout. Chains: WLEZ::Wrap, 2× token::Transfer (vault),
/// 1× token::Transfer (reshield). vs recursive router-based path:
/// saves 1 chained call (AMM intermediate gone).
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn disposable_swap_native_in(
    self_program_id: nssa_core::program::ProgramId,
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
    _deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("amm_v2 native_in expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");
    assert_eq!(
        pool.account.program_owner, self_program_id,
        "Pool must be owned by amm_v2"
    );
    assert_eq!(vault_a.account_id, pool_def.vault_a_id, "Vault A id mismatch");
    assert_eq!(vault_b.account_id, pool_def.vault_b_id, "Vault B id mismatch");

    let wlez_def_id = wlez_definition.account_id;
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

    let (deposit_a, withdraw_a, deposit_b, withdraw_b) = if wlez_is_token_a {
        (swap_amount_in, 0u128, 0u128, out_amount)
    } else {
        (0u128, out_amount, swap_amount_in, 0u128)
    };
    let pool_post_d = pool_post_def(&pool_def, deposit_a, withdraw_a, deposit_b, withdraw_b);
    let mut pool_post_account = pool.account.clone();
    pool_post_account.data = Data::from(&pool_post_d);

    let (vault_in, vault_out) = if wlez_is_token_a {
        (vault_a.clone(), vault_b.clone())
    } else {
        (vault_b.clone(), vault_a.clone())
    };

    let wlez_program_id = wlez_vault.account.program_owner;
    let token_program_id = wlez_definition.account.program_owner;

    let mut chained_calls = Vec::with_capacity(4);

    // 1) WLEZ::Wrap — drains user_native, mints WLEZ into a_wlez_holding.
    //    WLEZ internally chains auth_transfer + token::Mint.
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

    // 2) Pool deposit: a_wlez_holding (post-wrap) → vault_in.
    let a_wlez_post_wrap = shift_balance(&a_wlez_holding, swap_amount_in, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_wlez_post_wrap, vault_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // 3) Pool withdraw: vault_out (PDA-auth) → a_holding_out.
    let vault_out_seed = compute_vault_pda_seed(
        pool.account_id,
        TokenHolding::try_from(&vault_out.account.data)
            .expect("vault_out must hold a valid TokenHolding")
            .definition_id(),
    );
    let mut vault_out_auth = vault_out.clone();
    vault_out_auth.is_authorized = true;
    chained_calls.push(
        ChainedCall::new(
            token_program_id,
            vec![vault_out_auth, a_holding_out.clone()],
            &TokenInstruction::Transfer {
                amount_to_transfer: out_amount,
            },
        )
        .with_pda_seeds(vec![vault_out_seed]),
    );

    // 4) Reshield: a_holding_out (post-AMM) → user_holding_out (private).
    let a_out_post_amm = shift_balance(&a_holding_out, out_amount, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_out_post_amm, user_holding_out.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: out_amount,
        },
    ));

    let post_states = vec![
        AccountPostState::new(user_native.account),
        AccountPostState::new(wlez_vault.account),
        AccountPostState::new(wlez_definition.account),
        AccountPostState::new(a_wlez_holding.account),
        AccountPostState::new(a_holding_out.account),
        AccountPostState::new(pool_post_account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(user_holding_out.account),
    ];

    (post_states, chained_calls)
}

/// Native-LEZ-OUTPUT disposable swap (mode-2, token → LEZ).
///
/// 9-account layout. Chains: 1× token::Transfer (deshield), 2× token::
/// Transfer (vault), 1× WLEZ::Unwrap. vs recursive: saves 1 chained
/// call.
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn disposable_swap_native_out(
    self_program_id: nssa_core::program::ProgramId,
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
    _deadline: u64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def = PoolDefinition::try_from(&pool.account.data)
        .expect("amm_v2 native_out expects a valid Pool Definition account");
    assert_eq!(pool_def.fees, fees, "Pool fee tier mismatch");
    assert_eq!(
        pool.account.program_owner, self_program_id,
        "Pool must be owned by amm_v2"
    );
    assert_eq!(vault_a.account_id, pool_def.vault_a_id, "Vault A id mismatch");
    assert_eq!(vault_b.account_id, pool_def.vault_b_id, "Vault B id mismatch");

    let wlez_def_id = wlez_definition.account_id;
    // Input side must be one of pool's tokens; the OTHER side must be WLEZ.
    let (reserve_in, reserve_out, in_is_token_a) =
        if token_definition_id_in == pool_def.definition_token_a_id {
            assert_eq!(
                pool_def.definition_token_b_id, wlez_def_id,
                "NativeOut: pool's non-input side must be WLEZ"
            );
            (pool_def.reserve_a, pool_def.reserve_b, true)
        } else if token_definition_id_in == pool_def.definition_token_b_id {
            assert_eq!(
                pool_def.definition_token_a_id, wlez_def_id,
                "NativeOut: pool's non-input side must be WLEZ"
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

    let (deposit_a, withdraw_a, deposit_b, withdraw_b) = if in_is_token_a {
        (swap_amount_in, 0u128, 0u128, out_amount)
    } else {
        (0u128, out_amount, swap_amount_in, 0u128)
    };
    let pool_post_d = pool_post_def(&pool_def, deposit_a, withdraw_a, deposit_b, withdraw_b);
    let mut pool_post_account = pool.account.clone();
    pool_post_account.data = Data::from(&pool_post_d);

    let (vault_in, vault_out) = if in_is_token_a {
        (vault_a.clone(), vault_b.clone())
    } else {
        (vault_b.clone(), vault_a.clone())
    };

    let wlez_program_id = wlez_vault.account.program_owner;
    let token_program_id = wlez_definition.account.program_owner;

    let mut chained_calls = Vec::with_capacity(4);

    // 1) Deshield: user_holding_in → a_holding_in.
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![user_holding_in.clone(), a_holding_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // 2) Pool deposit: a_holding_in (post-deshield) → vault_in.
    let a_in_post_deshield = shift_balance(&a_holding_in, swap_amount_in, true);
    chained_calls.push(ChainedCall::new(
        token_program_id,
        vec![a_in_post_deshield, vault_in.clone()],
        &TokenInstruction::Transfer {
            amount_to_transfer: swap_amount_in,
        },
    ));

    // 3) Pool withdraw: vault_out (PDA-auth, WLEZ side) → a_wlez_holding.
    let vault_out_seed = compute_vault_pda_seed(
        pool.account_id,
        TokenHolding::try_from(&vault_out.account.data)
            .expect("vault_out must hold a valid TokenHolding")
            .definition_id(),
    );
    let mut vault_out_auth = vault_out.clone();
    vault_out_auth.is_authorized = true;
    chained_calls.push(
        ChainedCall::new(
            token_program_id,
            vec![vault_out_auth, a_wlez_holding.clone()],
            &TokenInstruction::Transfer {
                amount_to_transfer: out_amount,
            },
        )
        .with_pda_seeds(vec![vault_out_seed]),
    );

    // 4) WLEZ::Unwrap — burns a_wlez_holding by out_amount, releases
    //    native LEZ to user_native. The first chained-call pre-state
    //    for a_wlez_holding must reflect the running balance (it was
    //    credited by `out_amount` in step 3).
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

    let post_states = vec![
        AccountPostState::new(user_holding_in.account),
        AccountPostState::new(a_holding_in.account),
        AccountPostState::new(a_wlez_holding.account),
        AccountPostState::new(pool_post_account),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(wlez_definition.account),
        AccountPostState::new(wlez_vault.account),
        AccountPostState::new(user_native.account),
    ];

    (post_states, chained_calls)
}

/// Create a new amm_v2 pool with the user's initial LP minted into
/// their deterministic `ATA(owner, lp_def)` (RFP Func #8 — LP holding
/// side). Token deposits go via canonical `token::Transfer` from the
/// user's keypair `user_holding_a/b` (the ATA `Transfer` chained call
/// requires the recipient to be an already-initialised TokenHolding,
/// which the brand-new vaults are not — only `token::Transfer` with a
/// vault-PDA seed lawfully initialises the vaults via the same flow
/// canonical `new_definition` uses).
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn new_definition_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    lp_lock_holding: AccountWithMetadata,
    owner: AccountWithMetadata,
    user_holding_a: AccountWithMetadata,
    user_holding_b: AccountWithMetadata,
    ata_lp: AccountWithMetadata,
    token_a_amount: NonZeroU128,
    token_b_amount: NonZeroU128,
    fees: u128,
    amm_program_id: ProgramId,
    ata_program_id: ProgramId,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    assert_supported_fee_tier(fees);

    let definition_token_a_id =
        TokenHolding::try_from(&user_holding_a.account.data)
            .expect("new_definition_ata: user_holding_a must be a Fungible TokenHolding")
            .definition_id();
    let definition_token_b_id =
        TokenHolding::try_from(&user_holding_b.account.data)
            .expect("new_definition_ata: user_holding_b must be a Fungible TokenHolding")
            .definition_id();
    assert!(
        definition_token_a_id != definition_token_b_id,
        "Cannot set up a swap for a token with itself"
    );
    let token_program_id = user_holding_a.account.program_owner;
    assert_eq!(
        user_holding_b.account.program_owner, token_program_id,
        "user_holding_a/b token program mismatch"
    );

    // Pool / vault / lp PDA checks under amm_v2's program id.
    assert_eq!(
        pool.account_id,
        compute_pool_pda(amm_program_id, definition_token_a_id, definition_token_b_id, fees),
        "Pool PDA mismatch"
    );
    assert_eq!(
        vault_a.account_id,
        compute_vault_pda(amm_program_id, pool.account_id, definition_token_a_id),
        "Vault A PDA mismatch"
    );
    assert_eq!(
        vault_b.account_id,
        compute_vault_pda(amm_program_id, pool.account_id, definition_token_b_id),
        "Vault B PDA mismatch"
    );
    assert_eq!(
        pool_definition_lp.account_id,
        compute_liquidity_token_pda(amm_program_id, pool.account_id),
        "LP definition PDA mismatch"
    );
    assert_eq!(
        lp_lock_holding.account_id,
        compute_lp_lock_holding_pda(amm_program_id, pool.account_id),
        "LP lock holding PDA mismatch"
    );

    // ATA address check: ata_lp derives from (owner, lp_def).
    assert_eq!(
        ata_lp.account_id,
        ata_core::get_associated_token_account_id(
            &ata_program_id,
            &ata_core::compute_ata_seed(owner.account_id, pool_definition_lp.account_id),
        ),
        "ata_lp must equal ATA(owner, lp_def)"
    );

    assert!(
        user_holding_a.is_authorized && user_holding_b.is_authorized,
        "user_holding_a/b must sign new_definition_ata (token::Transfer drains)"
    );
    assert_eq!(
        pool.account,
        Account::default(),
        "Pool account must be uninitialized"
    );

    let initial_lp = token_a_amount
        .get()
        .checked_mul(token_b_amount.get())
        .expect("token_a * token_b overflows u128")
        .isqrt();
    assert!(
        initial_lp > MINIMUM_LIQUIDITY,
        "Initial liquidity must exceed minimum liquidity lock"
    );
    let user_lp = initial_lp - MINIMUM_LIQUIDITY;

    // Pool post-state (amm_v2 skips on-chain oracle — block_ts_last=0).
    let pool_post_def = PoolDefinition {
        definition_token_a_id,
        definition_token_b_id,
        vault_a_id: vault_a.account_id,
        vault_b_id: vault_b.account_id,
        liquidity_pool_id: pool_definition_lp.account_id,
        liquidity_pool_supply: initial_lp,
        reserve_a: token_a_amount.into(),
        reserve_b: token_b_amount.into(),
        fees,
        price_a_cum_last: 0,
        price_b_cum_last: 0,
        block_ts_last: 0,
        obs: Vec::new(),
        cum_volume_a: 0,
        cum_volume_b: 0,
        cum_fees_a: 0,
        cum_fees_b: 0,
    };
    let mut pool_post = pool.account.clone();
    pool_post.data = Data::from(&pool_post_def);
    let pool_post_state = AccountPostState::new_claimed(
        pool_post,
        Claim::Pda(compute_pool_pda_seed(
            definition_token_a_id, definition_token_b_id, fees,
        )),
    );

    // Chained calls — same shape as canonical NewDefinition: the two
    // user-side deposits go through `token::Transfer` with a vault
    // PDA-seed (the brand-new vaults start default, only token's
    // PDA-claim path lawfully initialises them).
    let mut vault_a_auth = vault_a.clone();
    vault_a_auth.is_authorized = true;
    let call_token_a = ChainedCall::new(
        token_program_id,
        vec![user_holding_a.clone(), vault_a_auth],
        &token_core::Instruction::Transfer { amount_to_transfer: token_a_amount.into() },
    )
    .with_pda_seeds(vec![compute_vault_pda_seed(
        pool.account_id, definition_token_a_id,
    )]);
    let mut vault_b_auth = vault_b.clone();
    vault_b_auth.is_authorized = true;
    let call_token_b = ChainedCall::new(
        token_program_id,
        vec![user_holding_b.clone(), vault_b_auth],
        &token_core::Instruction::Transfer { amount_to_transfer: token_b_amount.into() },
    )
    .with_pda_seeds(vec![compute_vault_pda_seed(
        pool.account_id, definition_token_b_id,
    )]);

    // LP token definition + lock holding (same PDA-auth pattern as canonical).
    let mut pool_lp_auth = pool_definition_lp.clone();
    pool_lp_auth.is_authorized = true;
    let mut lp_lock_holding_auth = lp_lock_holding.clone();
    lp_lock_holding_auth.is_authorized = true;
    let call_lp_def = ChainedCall::new(
        token_program_id,
        vec![pool_lp_auth.clone(), lp_lock_holding_auth],
        &token_core::Instruction::NewFungibleDefinition {
            name: String::from("LP Token"),
            total_supply: MINIMUM_LIQUIDITY,
        },
    )
    .with_pda_seeds(vec![
        compute_liquidity_token_pda_seed(pool.account_id),
        compute_lp_lock_holding_pda_seed(pool.account_id),
    ]);

    // After `call_lp_def`, pool_definition_lp is owned by the token
    // program and holds a Fungible(MINIMUM_LIQUIDITY) definition.
    let mut pool_lp_after_lock = pool_lp_auth.clone();
    pool_lp_after_lock.account.program_owner = token_program_id;
    pool_lp_after_lock.account.data = Data::from(&TokenDefinition::Fungible {
        name: String::from("LP Token"),
        total_supply: MINIMUM_LIQUIDITY,
        metadata_id: None,
    });

    // Chain ata::Create(owner, lp_def, ata_lp) IN-TX so ata_lp is
    // initialised as a Fungible TokenHolding tied to lp_def before
    // the Mint runs. A separate pre-tx `ata_create` can't succeed:
    // the ATA program reads `token_def.account.program_owner` to find
    // the token program to chain into, and lp_def doesn't exist pre-
    // tx (its program_owner is zero → "Unknown program"). After
    // `call_lp_def` the in-circuit running state has lp_def fully
    // initialised, so chaining `ata::Create` here works.
    //
    // Drop `is_authorized` on the lp_def metadata passed to ATA::
    // Create — `call_lp_def` already claimed lp_def via `Claim::Pda`,
    // and ATA::Create returns `AccountPostState::new(token_def…)`
    // with no claim of its own. Leaving `is_authorized=true` on this
    // call's input drives `InconsistentAccountAuthorization` against
    // the merged post-state (input says authorised, no chained-call
    // post-state actually claims it here). Owner is touched ONLY by
    // this chained call — the token::Transfer drains above don't
    // carry owner — so the owner authorisation stays consistent.
    let mut lp_def_for_ata_create = pool_lp_after_lock.clone();
    lp_def_for_ata_create.is_authorized = false;
    let call_ata_lp_create = ChainedCall::new(
        ata_program_id,
        vec![owner.clone(), lp_def_for_ata_create, ata_lp.clone()],
        &ata_core::Instruction::Create,
    );

    // After ata::Create, ata_lp is a Fungible TokenHolding owned by
    // the token program — token::Mint can credit it.
    let mut ata_lp_after_create = ata_lp.clone();
    ata_lp_after_create.account.program_owner = token_program_id;
    ata_lp_after_create.account.data = Data::from(
        &token_core::TokenHolding::Fungible {
            definition_id: pool_definition_lp.account_id,
            balance: 0,
        },
    );

    // Mint user's share into ata_lp (token::Mint, PDA-auth on lp_def).
    let call_lp_user = ChainedCall::new(
        token_program_id,
        vec![pool_lp_after_lock, ata_lp_after_create],
        &token_core::Instruction::Mint { amount_to_mint: user_lp },
    )
    .with_pda_seeds(vec![compute_liquidity_token_pda_seed(pool.account_id)]);

    let chained_calls =
        vec![call_lp_def, call_ata_lp_create, call_lp_user, call_token_b, call_token_a];
    let post_states = vec![
        pool_post_state,
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(pool_definition_lp.account),
        AccountPostState::new(lp_lock_holding.account),
        AccountPostState::new(owner.account),
        AccountPostState::new(user_holding_a.account),
        AccountPostState::new(user_holding_b.account),
        AccountPostState::new(ata_lp.account),
    ];
    (post_states, chained_calls)
}

/// Remove liquidity, user side ATAs. Chains: `ata::Burn` for the LP
/// (owner-authorised via PDA), 2× `token::Transfer` from vault to ATA
/// (vault PDA-auth).
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn remove_liquidity_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    owner: AccountWithMetadata,
    ata_a: AccountWithMetadata,
    ata_b: AccountWithMetadata,
    ata_lp: AccountWithMetadata,
    remove_liquidity_amount: NonZeroU128,
    min_amount_to_remove_token_a: u128,
    min_amount_to_remove_token_b: u128,
    ata_program_id: ProgramId,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let remove_amount: u128 = remove_liquidity_amount.into();
    let pool_def_data = PoolDefinition::try_from(&pool.account.data)
        .expect("remove_liquidity_ata expects a valid Pool Definition");
    assert_supported_fee_tier(pool_def_data.fees);
    assert_eq!(
        pool_def_data.liquidity_pool_id, pool_definition_lp.account_id,
        "LP definition mismatch"
    );
    assert_eq!(vault_a.account_id, pool_def_data.vault_a_id, "Vault A id mismatch");
    assert_eq!(vault_b.account_id, pool_def_data.vault_b_id, "Vault B id mismatch");
    assert!(owner.is_authorized, "owner must sign remove_liquidity_ata");

    // ATA address checks.
    let def_a = pool_def_data.definition_token_a_id;
    let def_b = pool_def_data.definition_token_b_id;
    assert_eq!(
        ata_a.account_id,
        ata_core::get_associated_token_account_id(
            &ata_program_id, &ata_core::compute_ata_seed(owner.account_id, def_a),
        ),
        "ata_a must equal ATA(owner, def_a)"
    );
    assert_eq!(
        ata_b.account_id,
        ata_core::get_associated_token_account_id(
            &ata_program_id, &ata_core::compute_ata_seed(owner.account_id, def_b),
        ),
        "ata_b must equal ATA(owner, def_b)"
    );
    assert_eq!(
        ata_lp.account_id,
        ata_core::get_associated_token_account_id(
            &ata_program_id, &ata_core::compute_ata_seed(owner.account_id, pool_definition_lp.account_id),
        ),
        "ata_lp must equal ATA(owner, lp_def)"
    );

    let ata_lp_holding = TokenHolding::try_from(&ata_lp.account.data)
        .expect("ata_lp must hold a valid Fungible LP TokenHolding");
    let user_lp_balance = match ata_lp_holding {
        TokenHolding::Fungible { balance, .. } => balance,
        _ => panic!("ata_lp must be Fungible"),
    };
    assert!(
        remove_amount <= user_lp_balance,
        "Remove amount exceeds user LP balance"
    );
    let unlocked = pool_def_data.liquidity_pool_supply - MINIMUM_LIQUIDITY;
    assert!(
        remove_amount <= unlocked,
        "Cannot remove locked minimum liquidity"
    );
    assert!(min_amount_to_remove_token_a != 0, "min A must be nonzero");
    assert!(min_amount_to_remove_token_b != 0, "min B must be nonzero");

    let withdraw_a = pool_def_data
        .reserve_a
        .checked_mul(remove_amount)
        .expect("reserve_a * amount overflow")
        / pool_def_data.liquidity_pool_supply;
    let withdraw_b = pool_def_data
        .reserve_b
        .checked_mul(remove_amount)
        .expect("reserve_b * amount overflow")
        / pool_def_data.liquidity_pool_supply;
    assert!(withdraw_a >= min_amount_to_remove_token_a, "Slippage A");
    assert!(withdraw_b >= min_amount_to_remove_token_b, "Slippage B");

    let pool_post_def = PoolDefinition {
        liquidity_pool_supply: pool_def_data.liquidity_pool_supply - remove_amount,
        reserve_a: pool_def_data.reserve_a - withdraw_a,
        reserve_b: pool_def_data.reserve_b - withdraw_b,
        ..pool_def_data.clone()
    };
    let mut pool_post = pool.account.clone();
    pool_post.data = Data::from(&pool_post_def);

    let token_program_id = vault_a.account.program_owner;

    // 1) ata::Burn — drain ata_lp by remove_amount; owner authorises.
    // lp_def passed with is_authorized=false: ATA::Burn returns
    // `AccountPostState::new(token_def.account.clone())` (no claim),
    // and the pool already holds lp_def authorised by the parent
    // call's account list. Mixing `is_authorized=true` here drives
    // `InconsistentAccountAuthorization` (input says authorised, post
    // emits no claim).
    let mut owner_auth = owner.clone();
    owner_auth.is_authorized = true;
    let mut lp_def_unauth = pool_definition_lp.clone();
    lp_def_unauth.is_authorized = false;
    let call_burn = ChainedCall::new(
        ata_program_id,
        vec![owner_auth, ata_lp.clone(), lp_def_unauth],
        &ata_core::Instruction::Burn { amount: remove_amount },
    );

    // 2) token::Transfer(vault_a → ata_a) with vault_a PDA-auth.
    let mut vault_a_auth = vault_a.clone();
    vault_a_auth.is_authorized = true;
    let call_a = ChainedCall::new(
        token_program_id,
        vec![vault_a_auth, ata_a.clone()],
        &token_core::Instruction::Transfer { amount_to_transfer: withdraw_a },
    )
    .with_pda_seeds(vec![amm_core::compute_vault_pda_seed(pool.account_id, def_a)]);

    // 3) token::Transfer(vault_b → ata_b) with vault_b PDA-auth.
    let mut vault_b_auth = vault_b.clone();
    vault_b_auth.is_authorized = true;
    let call_b = ChainedCall::new(
        token_program_id,
        vec![vault_b_auth, ata_b.clone()],
        &token_core::Instruction::Transfer { amount_to_transfer: withdraw_b },
    )
    .with_pda_seeds(vec![amm_core::compute_vault_pda_seed(pool.account_id, def_b)]);

    let chained_calls = vec![call_burn, call_a, call_b];
    let post_states = vec![
        AccountPostState::new(pool_post),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(pool_definition_lp.account),
        AccountPostState::new(owner.account),
        AccountPostState::new(ata_a.account),
        AccountPostState::new(ata_b.account),
        AccountPostState::new(ata_lp.account),
    ];
    (post_states, chained_calls)
}
