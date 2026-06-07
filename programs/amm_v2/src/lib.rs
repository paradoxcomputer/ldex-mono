//! amm_v2 combined private-swap program - orchestration + AMM math
//! inlined into one chained-call program, for all three disposable-
//! swap variants (token↔token, LEZ→token, token→LEZ).
//!
//! amm_v2's standard ops (NewDefinition, AddLiquidity, RemoveLiquidity,
//! SwapExactInput, SwapExactInputCircuit) delegate to the canonical
//! `amm_program` crate parameterised by amm_v2's `self_program_id` -
//! so pools/vaults/LP-tokens derive under amm_v2 and amm_v2 owns them.
//!
//! The DisposableSwap / DisposableSwapNativeIn / DisposableSwapNativeOut
//! variants are the "combined inner program" approach: amm_v2 is the
//! top-level chained call from the upstream privacy circuit, inlines
//! the AMM math + reserve updates, and chains only the necessary
//! token::Transfer (and WLEZ::Wrap/Unwrap for native variants).

pub use amm_v2_core as core;

use amm_core::{
    amm_exact_input_out, apply_swap_to_pool_def, assert_supported_fee_tier,
    compute_liquidity_token_pda, compute_liquidity_token_pda_seed,
    compute_lp_lock_holding_pda, compute_lp_lock_holding_pda_seed, compute_pool_pda,
    compute_pool_pda_seed, compute_vault_pda, compute_vault_pda_seed,
    MINIMUM_LIQUIDITY, PoolDefinition,
};
use nssa_core::{
    account::{Account, AccountId, AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall, Claim, ProgramId},
};
use std::num::NonZeroU128;
use token_core::{Instruction as TokenInstruction, TokenDefinition, TokenHolding};
use wlez_core::Instruction as WlezInstruction;

/// Apply a signed balance delta to a Fungible token holding (same
/// shift_balance pattern as `private_swap_router::shift_balance` -
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

/// Apply the same pool-reserve update as `SwapExactInputCircuit` (no
/// oracle update - drift-free pre-state set). Returns the updated
/// `PoolDefinition` ready to be re-serialised into `pool.account.data`.
/// Thin wrapper over the shared `amm_core::apply_swap_to_pool_def` (single
/// source of truth for the reserve/fee/accumulator math).
fn pool_post_def(
    pool_def: &PoolDefinition,
    deposit_a: u128,
    withdraw_a: u128,
    deposit_b: u128,
    withdraw_b: u128,
) -> PoolDefinition {
    apply_swap_to_pool_def(pool_def.clone(), deposit_a, withdraw_a, deposit_b, withdraw_b)
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

    // 1) WLEZ::Wrap - drains user_native, mints WLEZ into a_wlez_holding.
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

    // 4) WLEZ::Unwrap - burns a_wlez_holding by out_amount, releases
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
/// their deterministic `ATA(owner, lp_def)` (RFP Func #8 - LP holding
/// side). Token deposits go via canonical `token::Transfer` from the
/// user's keypair `user_holding_a/b` (the ATA `Transfer` chained call
/// requires the recipient to be an already-initialised TokenHolding,
/// which the brand-new vaults are not - only `token::Transfer` with a
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

    // Pool post-state (amm_v2 skips on-chain oracle - block_ts_last=0).
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
        // Pin the ATA program at creation so every later ATA-routed op asserts
        // against it (prevents the no-op-substitute drain).
        ata_program_id,
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

    // Chained calls - same shape as canonical NewDefinition: the two
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
    // Create - `call_lp_def` already claimed lp_def via `Claim::Pda`,
    // and ATA::Create returns `AccountPostState::new(token_def…)`
    // with no claim of its own. Leaving `is_authorized=true` on this
    // call's input drives `InconsistentAccountAuthorization` against
    // the merged post-state (input says authorised, no chained-call
    // post-state actually claims it here). Owner is touched ONLY by
    // this chained call - the token::Transfer drains above don't
    // carry owner - so the owner authorisation stays consistent.
    let mut lp_def_for_ata_create = pool_lp_after_lock.clone();
    lp_def_for_ata_create.is_authorized = false;
    let call_ata_lp_create = ChainedCall::new(
        ata_program_id,
        vec![owner.clone(), lp_def_for_ata_create, ata_lp.clone()],
        &ata_core::Instruction::Create,
    );

    // After ata::Create, ata_lp is a Fungible TokenHolding owned by
    // the token program - token::Mint can credit it.
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
    // SECURITY: the ATA program must match the one pinned at pool creation. The
    // ATA address checks below bind ata_in/out to this id, but binding alone is
    // not enough - a substitute program the attacker controls could no-op the
    // burn/return while the vault still pays out. Pinning closes that.
    assert_eq!(
        ata_program_id, pool_def_data.ata_program_id,
        "ata_program_id does not match the program pinned at pool creation"
    );

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
    assert!(
        pool_def_data.liquidity_pool_supply > MINIMUM_LIQUIDITY,
        "Pool only contains locked liquidity"
    );
    let unlocked = pool_def_data
        .liquidity_pool_supply
        .checked_sub(MINIMUM_LIQUIDITY)
        .expect("liquidity_pool_supply - MINIMUM_LIQUIDITY underflow");
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
        liquidity_pool_supply: pool_def_data
            .liquidity_pool_supply
            .checked_sub(remove_amount)
            .expect("liquidity_pool_supply - remove_amount underflow"),
        reserve_a: pool_def_data
            .reserve_a
            .checked_sub(withdraw_a)
            .expect("reserve_a - withdraw_a underflow"),
        reserve_b: pool_def_data
            .reserve_b
            .checked_sub(withdraw_b)
            .expect("reserve_b - withdraw_b underflow"),
        ..pool_def_data.clone()
    };
    let mut pool_post = pool.account.clone();
    pool_post.data = Data::from(&pool_post_def);

    let token_program_id = vault_a.account.program_owner;

    // 1) ata::Burn - drain ata_lp by remove_amount; owner authorises.
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

#[cfg(test)]
mod tests {
    //! Unit coverage for the amm_v2-specific combined-orchestration handlers,
    //! which are otherwise untested (the canonical public swap path is covered
    //! by `programs/integration_tests/tests/amm_disposable_drift_free.rs`).
    //!
    //! These handlers hand-build chained-call sequences whose correctness hinges
    //! on exact pre-state reconciliation against the running state diff
    //! (`shift_balance` signs), account ordering, vault PDA-seed claims, the
    //! `lp_def → ata::Create → Mint` ordering in `new_definition_ata`, and the
    //! `ata::Burn → token::Burn` supply decrement in `remove_liquidity_ata`. The
    //! asserts below pin each of those so an off-by-one would fail here rather
    //! than only surfacing as a sequencer-side rejection at runtime.

    use super::*;

    const AMM_V2_ID: ProgramId = [9; 8];
    const TOKEN_ID: ProgramId = [7; 8];
    const ATA_ID: ProgramId = [5; 8];
    const WLEZ_ID: ProgramId = [3; 8];
    const NATIVE_ID: ProgramId = [1; 8];

    const FEE: u128 = amm_core::FEE_TIER_BPS_30;
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
            nonce: nssa_core::account::Nonce(0),
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
        risc0_zkvm::serde::from_slice(&call.instruction_data)
            .expect("token instruction decode")
    }

    fn decode_ata(call: &ChainedCall) -> ata_core::Instruction {
        risc0_zkvm::serde::from_slice(&call.instruction_data)
            .expect("ata instruction decode")
    }

    fn decode_wlez(call: &ChainedCall) -> WlezInstruction {
        risc0_zkvm::serde::from_slice(&call.instruction_data)
            .expect("wlez instruction decode")
    }

    fn def_a() -> AccountId {
        id(3)
    }
    fn def_b() -> AccountId {
        id(4)
    }
    fn pool_id() -> AccountId {
        compute_pool_pda(AMM_V2_ID, def_a(), def_b(), FEE)
    }
    fn vault_a_id() -> AccountId {
        compute_vault_pda(AMM_V2_ID, pool_id(), def_a())
    }
    fn vault_b_id() -> AccountId {
        compute_vault_pda(AMM_V2_ID, pool_id(), def_b())
    }
    fn lp_def_id() -> AccountId {
        compute_liquidity_token_pda(AMM_V2_ID, pool_id())
    }
    fn ata_of(owner: AccountId, def: AccountId) -> AccountId {
        ata_core::get_associated_token_account_id(&ATA_ID, &ata_core::compute_ata_seed(owner, def))
    }

    /// A live, amm_v2-owned token↔token pool with the given reserves.
    fn live_pool(reserve_a: u128, reserve_b: u128, supply: u128) -> AccountWithMetadata {
        let def = PoolDefinition {
            definition_token_a_id: def_a(),
            definition_token_b_id: def_b(),
            vault_a_id: vault_a_id(),
            vault_b_id: vault_b_id(),
            liquidity_pool_id: lp_def_id(),
            liquidity_pool_supply: supply,
            reserve_a,
            reserve_b,
            fees: FEE,
            // Pinned at creation; `remove_liquidity_ata` asserts the caller's
            // ata_program_id matches this. Irrelevant to the disposable swaps.
            ata_program_id: ATA_ID,
            ..Default::default()
        };
        let account = Account {
            program_owner: AMM_V2_ID,
            balance: 0,
            data: Data::from(&def),
            nonce: nssa_core::account::Nonce(0),
        };
        AccountWithMetadata::new(account, false, pool_id())
    }

    fn pool_supply(post: &AccountPostState) -> u128 {
        PoolDefinition::try_from(&post.account().data)
            .expect("pool def")
            .liquidity_pool_supply
    }

    // ---- WLEZ-side fixtures (for the native-variant disposable swaps) -----
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
            nonce: nssa_core::account::Nonce(0),
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
            nonce: nssa_core::account::Nonce(0),
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
            nonce: nssa_core::account::Nonce(0),
        };
        AccountWithMetadata::new(account, false, account_id)
    }

    // ---- disposable_swap (token ↔ token) ---------------------------------

    #[test]
    fn disposable_swap_reconciles_chained_call_pre_states() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let user_in = holding(id(40), def_a(), swap_in);
        // Account-A side holdings (intermediaries) and fresh receiver.
        let a_holding_a = holding(id(41), def_a(), 0);
        let a_holding_b = holding(id(42), def_b(), 0);
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let user_out = holding(id(43), def_b(), 0);

        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        assert!(out_amount > 0, "test fixture must produce nonzero output");

        let (post_states, calls) = disposable_swap(
            AMM_V2_ID,
            user_in.clone(),
            a_holding_a.clone(),
            a_holding_b.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            user_out.clone(),
            swap_in,
            out_amount, // min_amount_out == exact out: passes the slippage check
            def_a(),    // token_in == token A
            FEE,
            u64::MAX,
        );

        // Four calls, in order: deshield, vault-in, vault-out, reshield.
        assert_eq!(calls.len(), 4, "deshield/vault-in/vault-out/reshield");

        // 1) Deshield: user_in -> a_in (token A side), full swap_in.
        assert_eq!(calls[0].program_id, TOKEN_ID);
        assert_eq!(calls[0].pre_states[0].account_id, user_in.account_id);
        assert_eq!(calls[0].pre_states[1].account_id, a_holding_a.account_id);
        assert!(calls[0].pda_seeds.is_empty(), "deshield is user-authorised");
        assert!(
            matches!(decode_token(&calls[0]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == swap_in),
        );

        // 2) Vault deposit: a_in pre-state MUST reflect the running diff - it was
        //    credited by `swap_in` in call 1 before being drained here.
        assert_eq!(calls[1].pre_states[0].account_id, a_holding_a.account_id);
        assert_eq!(
            balance_of(&calls[1].pre_states[0]),
            swap_in,
            "a_in pre-state must include the deshield credit (shift_balance +)"
        );
        assert_eq!(calls[1].pre_states[1].account_id, vault_a.account_id);

        // 3) Vault withdraw: vault_out (token B) -> a_out, PDA-authorised.
        assert_eq!(calls[2].pre_states[0].account_id, vault_b.account_id);
        assert!(calls[2].pre_states[0].is_authorized, "vault_out must be authorised");
        assert_eq!(calls[2].pre_states[1].account_id, a_holding_b.account_id);
        assert_eq!(
            calls[2].pda_seeds,
            vec![compute_vault_pda_seed(pool_id(), def_b())],
            "vault_out withdraw must carry the vault_b PDA seed"
        );
        assert!(
            matches!(decode_token(&calls[2]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == out_amount),
        );

        // 4) Reshield: a_out pre-state reflects the running diff (credited by
        //    `out_amount` in call 3) before paying the private receiver.
        assert_eq!(calls[3].pre_states[0].account_id, a_holding_b.account_id);
        assert_eq!(
            balance_of(&calls[3].pre_states[0]),
            out_amount,
            "a_out pre-state must include the AMM-out credit (shift_balance +)"
        );
        assert_eq!(calls[3].pre_states[1].account_id, user_out.account_id);

        // Pool post-state: reserve_in up by swap_in, reserve_out down by out_amount.
        let pool_post = PoolDefinition::try_from(&post_states[3].account().data).expect("pool def");
        assert_eq!(pool_post.reserve_a, RESERVE_A0 + swap_in);
        assert_eq!(pool_post.reserve_b, RESERVE_B0 - out_amount);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn disposable_swap_rejects_below_min_out() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = disposable_swap(
            AMM_V2_ID,
            holding(id(40), def_a(), swap_in),
            holding(id(41), def_a(), 0),
            holding(id(42), def_b(), 0),
            pool,
            holding(vault_a_id(), def_a(), RESERVE_A0),
            holding(vault_b_id(), def_b(), RESERVE_B0),
            holding(id(43), def_b(), 0),
            swap_in,
            out_amount + 1, // demand more than achievable -> slippage panic
            def_a(),
            FEE,
            u64::MAX,
        );
    }

    /// Regression for the FFI vault/holding mis-ordering bug: a caller
    /// who supplies the pair in the REVERSE of the pool's canonical leg
    /// order (token B first) must align the (vault, a-holding) legs before
    /// dispatch - otherwise `vault_a` resolves to vault(token_b) and the
    /// handler panics on `vault_a.account_id == pool_def.vault_a_id`. This
    /// mirrors exactly what `pool_needs_leg_flip` + the per-site swap now do
    /// in `ldex-amm-ffi` (and `ldex_amm_v2_disposable_swap`). The pool's
    /// stored token_a is `def_a()` (create-time order, NOT sorted), so a
    /// caller passing `def_b()` as its "def_a" needs the flip.
    #[test]
    fn disposable_swap_aligns_reversed_caller_leg_order() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        // Swap A -> B, but the caller enumerated the pool as (def_b, def_a).
        let tok_in = def_a();
        let caller_def_a = def_b(); // reversed: caller's "A" is the pool's B

        // What `prep` would derive from the caller's reversed order
        // (caller_def_a = def_b(), caller_def_b = def_a()):
        //   vault_a = vault(caller_def_a) = vault_b_id (the pool's token B)
        //   vault_b = vault(caller_def_b) = vault_a_id
        // and the paired a-holdings in caller order.
        let prep_vault_a = holding(vault_b_id(), def_b(), RESERVE_B0);
        let prep_vault_b = holding(vault_a_id(), def_a(), RESERVE_A0);
        let prep_a_a = holding(id(41), def_b(), 0); // a-holding for caller's def_a == def_b()
        let prep_a_b = holding(id(42), def_a(), 0); // a-holding for caller's def_b == def_a()

        // Apply the fix's rule: the pool's token_a is NOT caller_def_a, so flip.
        let flip = pool_def_token_a(&pool) != caller_def_a;
        assert!(flip, "fixture must exercise the reversed-order (flip) branch");
        let (vault_a, vault_b, a_a, a_b) = if flip {
            (prep_vault_b, prep_vault_a, prep_a_b, prep_a_a)
        } else {
            (prep_vault_a, prep_vault_b, prep_a_a, prep_a_b)
        };

        // user_in (private source, token A) and user_out (token B receiver).
        let user_in = holding(id(40), def_a(), swap_in);
        let user_out = holding(id(43), def_b(), 0);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);

        // With the aligned legs the handler accepts the call (no vault-id
        // panic) and produces the correct A->B swap.
        let (post_states, calls) = disposable_swap(
            AMM_V2_ID,
            user_in,
            a_a.clone(),
            a_b.clone(),
            pool,
            vault_a.clone(),
            vault_b.clone(),
            user_out,
            swap_in,
            out_amount,
            tok_in,
            FEE,
            u64::MAX,
        );

        assert_eq!(calls.len(), 4);
        // Deposit leg drains the token-A a-holding (now slot a_a) into the
        // token-A vault (now slot vault_a == vault_a_id).
        assert_eq!(calls[1].pre_states[0].account_id, a_a.account_id);
        assert_eq!(calls[1].pre_states[1].account_id, vault_a_id());
        // Withdraw leg pays out of the token-B vault.
        assert_eq!(calls[2].pre_states[0].account_id, vault_b_id());
        assert!(calls[2].pre_states[0].is_authorized);
        let pool_post =
            PoolDefinition::try_from(&post_states[3].account().data).expect("pool def");
        assert_eq!(pool_post.reserve_a, RESERVE_A0 + swap_in);
        assert_eq!(pool_post.reserve_b, RESERVE_B0 - out_amount);
    }

    /// Mirror of `pool_needs_leg_flip`'s on-chain read, for the test above:
    /// the pool's stored canonical token-A definition id.
    fn pool_def_token_a(pool: &AccountWithMetadata) -> AccountId {
        PoolDefinition::try_from(&pool.account.data)
            .expect("pool def")
            .definition_token_a_id
    }

    // ---- disposable_swap_native_in (LEZ -> token) ------------------------

    #[test]
    fn disposable_swap_native_in_reconciles_chained_call_pre_states() {
        // WLEZ is token A of the pool; the output token is token B.
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let user_native = native_account(id(50), swap_in);
        let wlez_vault = wlez_vault(id(51));
        let wlez_definition = wlez_definition(def_a());
        // Account-A-side WLEZ holding (intermediary) + output holding + receiver.
        let a_wlez = holding(id(52), def_a(), 0);
        let a_out = holding(id(53), def_b(), 0);
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let user_out = holding(id(54), def_b(), 0);

        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        assert!(out_amount > 0, "test fixture must produce nonzero output");

        let (post_states, calls) = disposable_swap_native_in(
            AMM_V2_ID,
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
            out_amount, // min_amount_out == exact out: passes the slippage check
            FEE,
            u64::MAX,
        );

        // Four calls, in order: Wrap, vault-in, vault-out, reshield.
        assert_eq!(calls.len(), 4, "Wrap/vault-in/vault-out/reshield");

        // 1) WLEZ::Wrap - account order per wlez_core::Instruction::Wrap docs:
        //    [user_native, vault, definition, user(=a_wlez) holding].
        assert_eq!(calls[0].program_id, WLEZ_ID, "Wrap runs on the WLEZ program");
        assert_eq!(calls[0].pre_states[0].account_id, user_native.account_id);
        assert_eq!(calls[0].pre_states[1].account_id, wlez_vault.account_id);
        assert_eq!(calls[0].pre_states[2].account_id, wlez_definition.account_id);
        assert_eq!(calls[0].pre_states[3].account_id, a_wlez.account_id);
        assert!(calls[0].pda_seeds.is_empty(), "Wrap carries no seed at this level");
        assert!(
            matches!(decode_wlez(&calls[0]), WlezInstruction::Wrap { amount } if amount == swap_in),
        );

        // 2) Vault deposit: a_wlez pre-state MUST reflect the running diff - it
        //    was credited by `swap_in` by the Wrap mint before being drained.
        assert_eq!(calls[1].program_id, TOKEN_ID);
        assert_eq!(calls[1].pre_states[0].account_id, a_wlez.account_id);
        assert_eq!(
            balance_of(&calls[1].pre_states[0]),
            swap_in,
            "a_wlez pre-state must include the Wrap-mint credit (shift_balance +)"
        );
        assert_eq!(calls[1].pre_states[1].account_id, vault_a.account_id);
        assert!(
            matches!(decode_token(&calls[1]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == swap_in),
        );

        // 3) Vault withdraw: vault_out (token B) -> a_out, PDA-authorised.
        assert_eq!(calls[2].pre_states[0].account_id, vault_b.account_id);
        assert!(calls[2].pre_states[0].is_authorized, "vault_out must be authorised");
        assert_eq!(calls[2].pre_states[1].account_id, a_out.account_id);
        assert_eq!(
            calls[2].pda_seeds,
            vec![compute_vault_pda_seed(pool_id(), def_b())],
            "vault_out withdraw must carry the vault_b PDA seed"
        );
        assert!(
            matches!(decode_token(&calls[2]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == out_amount),
        );

        // 4) Reshield: a_out pre-state reflects the running diff (credited by
        //    `out_amount` in call 3) before paying the private receiver.
        assert_eq!(calls[3].pre_states[0].account_id, a_out.account_id);
        assert_eq!(
            balance_of(&calls[3].pre_states[0]),
            out_amount,
            "a_out pre-state must include the AMM-out credit (shift_balance +)"
        );
        assert_eq!(calls[3].pre_states[1].account_id, user_out.account_id);

        // Pool post-state: WLEZ side (reserve_a) up by swap_in, out side down.
        let pool_post = PoolDefinition::try_from(&post_states[5].account().data).expect("pool def");
        assert_eq!(pool_post.reserve_a, RESERVE_A0 + swap_in);
        assert_eq!(pool_post.reserve_b, RESERVE_B0 - out_amount);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn disposable_swap_native_in_rejects_below_min_out() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = disposable_swap_native_in(
            AMM_V2_ID,
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

    // ---- disposable_swap_native_out (token -> LEZ) -----------------------

    #[test]
    fn disposable_swap_native_out_reconciles_chained_call_pre_states() {
        // Input is token A (non-WLEZ); WLEZ is token B (the output side).
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let user_in = holding(id(60), def_a(), swap_in);
        let a_in = holding(id(61), def_a(), 0);
        // a_wlez receives the AMM-out (WLEZ side) before being unwrapped.
        let a_wlez = holding(id(62), def_b(), 0);
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);
        let wlez_definition = wlez_definition(def_b());
        let wlez_vault = wlez_vault(id(63));
        let user_native = native_account(id(64), 0);

        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        assert!(out_amount > 0, "test fixture must produce nonzero output");

        let (post_states, calls) = disposable_swap_native_out(
            AMM_V2_ID,
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
            out_amount, // min_amount_out == exact out: passes the slippage check
            def_a(),    // token_in == token A (non-WLEZ side)
            FEE,
            u64::MAX,
        );

        // Four calls, in order: deshield, vault-in, vault-out, Unwrap.
        assert_eq!(calls.len(), 4, "deshield/vault-in/vault-out/Unwrap");

        // 1) Deshield: user_in -> a_in (token A side), full swap_in.
        assert_eq!(calls[0].program_id, TOKEN_ID);
        assert_eq!(calls[0].pre_states[0].account_id, user_in.account_id);
        assert_eq!(calls[0].pre_states[1].account_id, a_in.account_id);
        assert!(calls[0].pda_seeds.is_empty(), "deshield is user-authorised");
        assert!(
            matches!(decode_token(&calls[0]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == swap_in),
        );

        // 2) Vault deposit: a_in pre-state MUST reflect the running diff - it
        //    was credited by `swap_in` in call 1 before being drained here.
        assert_eq!(calls[1].pre_states[0].account_id, a_in.account_id);
        assert_eq!(
            balance_of(&calls[1].pre_states[0]),
            swap_in,
            "a_in pre-state must include the deshield credit (shift_balance +)"
        );
        assert_eq!(calls[1].pre_states[1].account_id, vault_a.account_id);

        // 3) Vault withdraw: vault_out (token B, WLEZ side) -> a_wlez, PDA-auth.
        assert_eq!(calls[2].pre_states[0].account_id, vault_b.account_id);
        assert!(calls[2].pre_states[0].is_authorized, "vault_out must be authorised");
        assert_eq!(calls[2].pre_states[1].account_id, a_wlez.account_id);
        assert_eq!(
            calls[2].pda_seeds,
            vec![compute_vault_pda_seed(pool_id(), def_b())],
            "vault_out withdraw must carry the vault_b PDA seed"
        );
        assert!(
            matches!(decode_token(&calls[2]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == out_amount),
        );

        // 4) WLEZ::Unwrap - account order per wlez_core::Instruction::Unwrap
        //    docs: [user(=a_wlez) holding, definition, vault, user_native].
        //    a_wlez pre-state reflects the running diff (credited by `out_amount`
        //    in call 3) before the burn.
        assert_eq!(calls[3].program_id, WLEZ_ID, "Unwrap runs on the WLEZ program");
        assert_eq!(calls[3].pre_states[0].account_id, a_wlez.account_id);
        assert_eq!(
            balance_of(&calls[3].pre_states[0]),
            out_amount,
            "a_wlez pre-state must include the AMM-out credit (shift_balance +)"
        );
        assert_eq!(calls[3].pre_states[1].account_id, wlez_definition.account_id);
        assert_eq!(calls[3].pre_states[2].account_id, wlez_vault.account_id);
        assert_eq!(calls[3].pre_states[3].account_id, user_native.account_id);
        assert!(
            matches!(decode_wlez(&calls[3]), WlezInstruction::Unwrap { amount } if amount == out_amount),
        );

        // Pool post-state: input side (reserve_a) up by swap_in, WLEZ side down.
        let pool_post = PoolDefinition::try_from(&post_states[3].account().data).expect("pool def");
        assert_eq!(pool_post.reserve_a, RESERVE_A0 + swap_in);
        assert_eq!(pool_post.reserve_b, RESERVE_B0 - out_amount);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn disposable_swap_native_out_rejects_below_min_out() {
        let swap_in = 1_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, 100_000);
        let out_amount = amm_exact_input_out(RESERVE_A0, RESERVE_B0, FEE, swap_in);
        let _ = disposable_swap_native_out(
            AMM_V2_ID,
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

    // ---- new_definition_ata -> remove_liquidity_ata round-trip -----------

    /// Build the account list `new_definition_ata` expects, run it, and return
    /// `(post_states, calls, owner_id)`.
    fn run_new_definition_ata(
        token_a_amount: NonZeroU128,
        token_b_amount: NonZeroU128,
    ) -> (Vec<AccountPostState>, Vec<ChainedCall>, AccountId) {
        let owner_id = id(20);
        let pool = AccountWithMetadata::new(Account::default(), false, pool_id());
        let vault_a = holding(vault_a_id(), def_a(), 0);
        let vault_b = holding(vault_b_id(), def_b(), 0);
        let pool_lp = AccountWithMetadata::new(
            Account::default(),
            false,
            lp_def_id(),
        );
        let lp_lock = AccountWithMetadata::new(
            Account::default(),
            false,
            compute_lp_lock_holding_pda(AMM_V2_ID, pool_id()),
        );
        let owner = AccountWithMetadata::new(Account::default(), false, owner_id);
        // user_holding_a/b must sign (token::Transfer drains).
        let mut user_a = holding(id(21), def_a(), token_a_amount.get());
        user_a.is_authorized = true;
        let mut user_b = holding(id(22), def_b(), token_b_amount.get());
        user_b.is_authorized = true;
        let ata_lp = AccountWithMetadata::new(
            Account::default(),
            false,
            ata_of(owner_id, lp_def_id()),
        );

        let (post, calls) = new_definition_ata(
            pool,
            vault_a,
            vault_b,
            pool_lp,
            lp_lock,
            owner,
            user_a,
            user_b,
            ata_lp,
            token_a_amount,
            token_b_amount,
            FEE,
            AMM_V2_ID,
            ATA_ID,
        );
        (post, calls, owner_id)
    }

    #[test]
    fn new_definition_then_remove_keeps_lp_supply_in_lockstep() {
        let a_amt = NonZeroU128::new(RESERVE_A0).unwrap();
        let b_amt = NonZeroU128::new(RESERVE_B0).unwrap();
        let initial_lp = (RESERVE_A0 * RESERVE_B0).isqrt();
        assert!(initial_lp > MINIMUM_LIQUIDITY, "fixture sanity");
        let user_lp = initial_lp - MINIMUM_LIQUIDITY;

        let (post, calls, owner_id) = run_new_definition_ata(a_amt, b_amt);

        // Pool post-state records the full initial LP supply.
        assert_eq!(pool_supply(&post[0]), initial_lp, "pool liquidity_pool_supply");
        assert!(post[0].required_claim().is_some(), "pool claimed via PDA");

        // Chained calls in the exact order the comments mandate:
        //   [lp_def, ata::Create, Mint(user), token_b, token_a].
        assert_eq!(calls.len(), 5);
        // call 0: NewFungibleDefinition locks MINIMUM_LIQUIDITY into the LP def.
        match decode_token(&calls[0]) {
            TokenInstruction::NewFungibleDefinition { total_supply, .. } => {
                assert_eq!(total_supply, MINIMUM_LIQUIDITY, "locked minimum liquidity");
            }
            _ => panic!("call 0 must be NewFungibleDefinition"),
        }
        // call 1: ata::Create(owner, lp_def, ata_lp) - BEFORE the Mint, so the
        // ATA exists as a Fungible holding tied to lp_def when Mint runs.
        assert_eq!(calls[1].program_id, ATA_ID);
        assert!(matches!(decode_ata(&calls[1]), ata_core::Instruction::Create));
        assert_eq!(calls[1].pre_states[0].account_id, owner_id);
        assert_eq!(calls[1].pre_states[2].account_id, ata_of(owner_id, lp_def_id()));
        // call 2: Mint user's share into the freshly-created ATA.
        match decode_token(&calls[2]) {
            TokenInstruction::Mint { amount_to_mint } => {
                assert_eq!(amount_to_mint, user_lp, "user share == initial_lp - lock");
            }
            _ => panic!("call 2 must be Mint"),
        }
        assert_eq!(
            calls[2].pda_seeds,
            vec![compute_liquidity_token_pda_seed(pool_id())],
            "Mint authorised by lp_def PDA seed"
        );
        // Lockstep invariant: locked supply + user-minted share == pool supply.
        assert_eq!(MINIMUM_LIQUIDITY + user_lp, initial_lp);

        // ---- Now remove the user's whole LP position back out. ----
        let pool_after_new = AccountWithMetadata::new(post[0].account().clone(), false, pool_id());
        let mut owner = AccountWithMetadata::new(Account::default(), false, owner_id);
        owner.is_authorized = true;
        let ata_a = holding(ata_of(owner_id, def_a()), def_a(), 0);
        let ata_b = holding(ata_of(owner_id, def_b()), def_b(), 0);
        // The user's LP ATA now holds `user_lp` (minted above).
        let ata_lp = holding(ata_of(owner_id, lp_def_id()), lp_def_id(), user_lp);
        let pool_lp = AccountWithMetadata::new(Account::default(), false, lp_def_id());
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);

        let remove = NonZeroU128::new(user_lp).unwrap();
        let (rpost, rcalls) = remove_liquidity_ata(
            pool_after_new,
            vault_a,
            vault_b,
            pool_lp,
            owner,
            ata_a,
            ata_b,
            ata_lp,
            remove,
            1,
            1,
            ATA_ID,
        );

        // Pool supply drops by exactly the removed amount - in lockstep with the
        // LP burned below (no drift between pool bookkeeping and LP token).
        assert_eq!(
            pool_supply(&rpost[0]),
            initial_lp - user_lp,
            "pool supply must drop by the removed LP amount"
        );

        // Calls: [ata::Burn, token::Transfer(vault_a), token::Transfer(vault_b)].
        assert_eq!(rcalls.len(), 3);
        match decode_ata(&rcalls[0]) {
            ata_core::Instruction::Burn { amount } => {
                assert_eq!(amount, user_lp, "ata::Burn decrements LP supply by removed amount");
            }
            _ => panic!("call 0 must be ata::Burn"),
        }
        // Burn account order: [owner(auth), ata_lp, lp_def].
        assert!(rcalls[0].pre_states[0].is_authorized, "owner authorises burn");
        assert_eq!(rcalls[0].pre_states[1].account_id, ata_of(owner_id, lp_def_id()));
        assert_eq!(rcalls[0].pre_states[2].account_id, lp_def_id());

        // Proportional withdrawal returns the full reserves for a full burn of
        // the unlocked supply share; assert it lands in the user's ATAs.
        let expected_a = RESERVE_A0 * user_lp / initial_lp;
        let expected_b = RESERVE_B0 * user_lp / initial_lp;
        assert_eq!(rcalls[1].pre_states[1].account_id, ata_of(owner_id, def_a()));
        assert!(
            matches!(decode_token(&rcalls[1]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == expected_a),
        );
        assert_eq!(rcalls[2].pre_states[1].account_id, ata_of(owner_id, def_b()));
        assert!(
            matches!(decode_token(&rcalls[2]), TokenInstruction::Transfer { amount_to_transfer } if amount_to_transfer == expected_b),
        );
        assert_eq!(rcalls[1].pda_seeds, vec![compute_vault_pda_seed(pool_id(), def_a())]);
        assert_eq!(rcalls[2].pda_seeds, vec![compute_vault_pda_seed(pool_id(), def_b())]);

        // Pool reserves drop by the amounts returned.
        let pool_post = PoolDefinition::try_from(&rpost[0].account().data).expect("pool def");
        assert_eq!(pool_post.reserve_a, RESERVE_A0 - expected_a);
        assert_eq!(pool_post.reserve_b, RESERVE_B0 - expected_b);
    }

    #[test]
    #[should_panic(expected = "Initial liquidity must exceed minimum liquidity lock")]
    fn new_definition_ata_rejects_below_minimum_liquidity() {
        // isqrt(amounts) <= MINIMUM_LIQUIDITY must be rejected.
        let small = NonZeroU128::new(10).unwrap();
        let _ = run_new_definition_ata(small, small);
    }

    #[test]
    #[should_panic(expected = "Cannot remove locked minimum liquidity")]
    fn remove_liquidity_ata_rejects_removing_locked_minimum() {
        let owner_id = id(20);
        let supply = 5_000u128;
        let pool = live_pool(RESERVE_A0, RESERVE_B0, supply);
        let mut owner = AccountWithMetadata::new(Account::default(), false, owner_id);
        owner.is_authorized = true;
        let ata_a = holding(ata_of(owner_id, def_a()), def_a(), 0);
        let ata_b = holding(ata_of(owner_id, def_b()), def_b(), 0);
        // User holds the entire supply, but `unlocked = supply - MINIMUM_LIQUIDITY`
        // is the cap; removing the full supply must be rejected.
        let ata_lp = holding(ata_of(owner_id, lp_def_id()), lp_def_id(), supply);
        let pool_lp = AccountWithMetadata::new(Account::default(), false, lp_def_id());
        let vault_a = holding(vault_a_id(), def_a(), RESERVE_A0);
        let vault_b = holding(vault_b_id(), def_b(), RESERVE_B0);

        let remove = NonZeroU128::new(supply).unwrap();
        let _ = remove_liquidity_ata(
            pool,
            vault_a,
            vault_b,
            pool_lp,
            owner,
            ata_a,
            ata_b,
            ata_lp,
            remove,
            1,
            1,
            ATA_ID,
        );
    }
}
