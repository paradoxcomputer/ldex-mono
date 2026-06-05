//! RFP Func #8 — AMM swap with the user side using **Associated Token
//! Accounts (ATAs)** instead of keypair token holdings.
//!
//! ATAs are deterministic per `(owner, definition)` (see `ata_core`); the
//! ATA program owns them and authorises spends via PDA seeds when the
//! account's named **owner** signs the outer tx. So the input leg of the
//! swap is dispatched through the ATA program (which internally chains a
//! token::Transfer from the ATA-PDA to the vault); the output leg is the
//! same vault-PDA-authorised token::Transfer used by the keypair-side
//! `swap_exact_input`, depositing into the recipient ATA (which is, at
//! the token layer, just a Fungible token holding).
//!
//! This keeps the original keypair-side path (`swap::swap_exact_input`)
//! intact for backward compatibility while letting clients route trades
//! exclusively through ATAs.

use amm_core::{
    apply_swap_to_pool_def, assert_supported_fee_tier, compute_vault_pda_seed,
    read_vault_fungible_balances, FEE_BPS_DENOMINATOR, MINIMUM_LIQUIDITY, PoolDefinition,
};
use nssa_core::{
    account::{AccountId, AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall, ProgramId},
};

/// Swap `swap_amount_in` of `token_in_id` from the user's ATA into the
/// pool. The output goes into the user's other-side ATA. Symmetric for
/// both directions; the AMM constant-product price + fee mirror
/// `swap::swap_exact_input` exactly.
///
/// Account order:
/// `[pool, vault_a, vault_b, owner, ata_a, ata_b, clock]`
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn swap_exact_input_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    owner: AccountWithMetadata,
    ata_a: AccountWithMetadata,
    ata_b: AccountWithMetadata,
    swap_amount_in: u128,
    min_amount_out: u128,
    token_in_id: AccountId,
    ata_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // Reuse swap.rs's setup validation: same pool/vault checks.
    let pool_def_data = PoolDefinition::try_from(&pool.account.data)
        .expect("AMM Program expects a valid Pool Definition Account");
    assert_supported_fee_tier(pool_def_data.fees);
    assert!(
        pool_def_data.liquidity_pool_supply >= MINIMUM_LIQUIDITY,
        "Pool liquidity supply is below minimum liquidity"
    );
    assert_eq!(vault_a.account_id, pool_def_data.vault_a_id, "Vault A was not provided");
    assert_eq!(vault_b.account_id, pool_def_data.vault_b_id, "Vault B was not provided");
    let _ = read_vault_fungible_balances("Validate ATA swap setup", &vault_a, &vault_b);

    // The ATA program id MUST come from the instruction (we cannot read
    // it from ata_*.account.program_owner — ATAs are token holdings owned
    // by the token program; the ATA program holds PDA authority via the
    // ATA seed derivation, not storage ownership). Both ATAs share the
    // same underlying token program owner, which the AMM derives from
    // the vault as usual.
    let _ = (&ata_a, &ata_b);

    // ATA address validation is enforced by the ATA program's own
    // `verify_ata_and_get_seed` when the chained `ata::Transfer` runs
    // (it derives the seed from owner+definition and asserts the
    // passed ATA matches). The AMM only needs to know which token each
    // ATA holds, which it learns from the chained Transfer's effect.
    let def_a = pool_def_data.definition_token_a_id;
    let def_b = pool_def_data.definition_token_b_id;

    // Pick the input/output legs by direction.
    let (ata_in, ata_out, vault_in, vault_out, reserve_in, reserve_out) =
        if token_in_id == def_a {
            (ata_a.clone(), ata_b.clone(), vault_a.clone(), vault_b.clone(),
             pool_def_data.reserve_a, pool_def_data.reserve_b)
        } else if token_in_id == def_b {
            (ata_b.clone(), ata_a.clone(), vault_b.clone(), vault_a.clone(),
             pool_def_data.reserve_b, pool_def_data.reserve_a)
        } else {
            panic!("token_definition_id_in is not a token of this pool");
        };

    // Constant-product math (mirrors swap::swap_logic).
    let effective_in = swap_amount_in
        .checked_mul(FEE_BPS_DENOMINATOR - pool_def_data.fees)
        .expect("swap_amount_in * (FEE_DENOM - fee) overflows u128")
        / FEE_BPS_DENOMINATOR;
    assert!(effective_in != 0, "Effective swap amount should be nonzero");
    let withdraw_amount = reserve_out
        .checked_mul(effective_in)
        .expect("reserve * effective_in overflows u128")
        / reserve_in
            .checked_add(effective_in)
            .expect("reserve_in + effective_in overflows u128");
    assert!(min_amount_out <= withdraw_amount, "Withdraw amount is less than minimal amount out");
    assert!(withdraw_amount != 0, "Withdraw amount should be nonzero");

    // (1) Input leg: ata::Transfer { owner-authorised, ATA→vault }.
    //     Owner signs the outer tx → ata_in is authorised via PDA seed
    //     internally by the ATA program, which chains token::Transfer.
    let mut owner_auth = owner.clone();
    owner_auth.is_authorized = true;
    let mut chained_calls = Vec::with_capacity(2);
    chained_calls.push(ChainedCall::new(
        ata_program_id,
        vec![owner_auth, ata_in.clone(), vault_in.clone()],
        &ata_core::Instruction::Transfer { amount: swap_amount_in },
    ));

    // (2) Output leg: vault → ATA via vault-PDA-authorised token::Transfer
    //     (identical pattern to swap::swap_logic).
    let token_program_id = vault_a.account.program_owner;
    let mut vault_withdraw = vault_out.clone();
    vault_withdraw.is_authorized = true;
    let pda_seed = compute_vault_pda_seed(
        pool.account_id,
        token_core::TokenHolding::try_from(&vault_withdraw.account.data)
            .expect("ATA swap: vault should be a Fungible token holding")
            .definition_id(),
    );
    chained_calls.push(
        ChainedCall::new(
            token_program_id,
            vec![vault_withdraw, ata_out.clone()],
            &token_core::Instruction::Transfer { amount_to_transfer: withdraw_amount },
        )
        .with_pda_seeds(vec![pda_seed]),
    );

    // Post-state: same algebra as swap::create_swap_post_states (pool
    // reserves + oracle ring + lifetime accumulators). We reuse that by
    // computing the deposit/withdraw quartet locally then echoing the
    // other accounts as unchanged passthroughs (validate_execution
    // requires len(pre)==len(post) and unchanged nonces).
    let (deposit_a, withdraw_a, deposit_b, withdraw_b) = if token_in_id == def_a {
        (swap_amount_in, 0u128, 0u128, withdraw_amount)
    } else {
        (0u128, withdraw_amount, swap_amount_in, 0u128)
    };

    // On-chain oracle: integrate pre-swap reserves over [t_last, now].
    let mut oracle_pre = pool_def_data.clone();
    oracle_pre.oracle_update(clock_ts);
    // Reserve + RFP Usability #3 volume/fee accumulators (shared helper —
    // single source of truth), then layer the oracle ring/cum/ts on top.
    let mut pool_post_def =
        apply_swap_to_pool_def(pool_def_data, deposit_a, withdraw_a, deposit_b, withdraw_b);
    pool_post_def.price_a_cum_last = oracle_pre.price_a_cum_last;
    pool_post_def.price_b_cum_last = oracle_pre.price_b_cum_last;
    pool_post_def.block_ts_last = oracle_pre.block_ts_last;
    pool_post_def.obs = oracle_pre.obs;

    let mut pool_post = pool.account.clone();
    pool_post.data = Data::from(&pool_post_def);

    // Order matches the input account vector so validate_execution
    // 1:1 pre↔post mapping holds; balance fields are echoed (chained
    // calls mutate them via the framework's running state_diff).
    let post_states = vec![
        AccountPostState::new(pool_post),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(owner.account),
        AccountPostState::new(ata_a.account),
        AccountPostState::new(ata_b.account),
    ];

    (post_states, chained_calls)
}

/// RFP Func #8 — `SwapExactOutput` with the user side using ATAs.
/// Symmetric to `swap_exact_input_ata`; the math mirrors
/// `swap::swap_exact_output` / `exact_output_swap_logic` byte-for-byte.
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn swap_exact_output_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    owner: AccountWithMetadata,
    ata_a: AccountWithMetadata,
    ata_b: AccountWithMetadata,
    exact_amount_out: u128,
    max_amount_in: u128,
    token_in_id: AccountId,
    _ata_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def_data = PoolDefinition::try_from(&pool.account.data)
        .expect("AMM Program expects a valid Pool Definition Account");
    assert_supported_fee_tier(pool_def_data.fees);
    assert!(
        pool_def_data.liquidity_pool_supply >= MINIMUM_LIQUIDITY,
        "Pool liquidity supply is below minimum liquidity"
    );
    assert_eq!(vault_a.account_id, pool_def_data.vault_a_id, "Vault A was not provided");
    assert_eq!(vault_b.account_id, pool_def_data.vault_b_id, "Vault B was not provided");
    let _ = read_vault_fungible_balances("Validate ATA swap setup", &vault_a, &vault_b);

    let def_a = pool_def_data.definition_token_a_id;
    let def_b = pool_def_data.definition_token_b_id;
    let (ata_in, ata_out, vault_in, vault_out, reserve_in, reserve_out) =
        if token_in_id == def_a {
            (ata_a.clone(), ata_b.clone(), vault_a.clone(), vault_b.clone(),
             pool_def_data.reserve_a, pool_def_data.reserve_b)
        } else if token_in_id == def_b {
            (ata_b.clone(), ata_a.clone(), vault_b.clone(), vault_a.clone(),
             pool_def_data.reserve_b, pool_def_data.reserve_a)
        } else {
            panic!("token_definition_id_in is not a token of this pool");
        };

    assert_ne!(exact_amount_out, 0, "Exact amount out must be nonzero");
    assert!(exact_amount_out < reserve_out, "Exact amount out exceeds reserve");
    let effective_in_min = reserve_in
        .checked_mul(exact_amount_out).expect("reserve * out overflows u128")
        .div_ceil(reserve_out.checked_sub(exact_amount_out)
            .expect("reserve_out - amount_out underflows"));
    let fee_mul = FEE_BPS_DENOMINATOR
        .checked_sub(pool_def_data.fees).expect("fee_bps exceeds fee denominator");
    let deposit_amount = effective_in_min
        .checked_mul(FEE_BPS_DENOMINATOR).expect("eff_in * FEE_DENOM overflows u128")
        .div_ceil(fee_mul);
    assert!(deposit_amount <= max_amount_in, "Required input exceeds maximum amount in");

    let ata_program_id = ata_a.account.program_owner; // unused as dispatch — ATA addr is the recipient; we dispatch ata::Transfer via the explicit `ata_program_id` arg below.
    let _ = ata_program_id;
    let mut owner_auth = owner.clone();
    owner_auth.is_authorized = true;
    let mut chained_calls = Vec::with_capacity(2);
    chained_calls.push(ChainedCall::new(
        _ata_program_id,
        vec![owner_auth, ata_in.clone(), vault_in.clone()],
        &ata_core::Instruction::Transfer { amount: deposit_amount },
    ));

    let token_program_id = vault_a.account.program_owner;
    let mut vault_withdraw = vault_out.clone();
    vault_withdraw.is_authorized = true;
    let pda_seed = compute_vault_pda_seed(
        pool.account_id,
        token_core::TokenHolding::try_from(&vault_withdraw.account.data)
            .expect("ATA swap_out: vault must be Fungible").definition_id(),
    );
    chained_calls.push(
        ChainedCall::new(
            token_program_id,
            vec![vault_withdraw, ata_out.clone()],
            &token_core::Instruction::Transfer { amount_to_transfer: exact_amount_out },
        )
        .with_pda_seeds(vec![pda_seed]),
    );

    // Post-state algebra (mirrors create_swap_post_states).
    let (deposit_a, withdraw_a, deposit_b, withdraw_b) = if token_in_id == def_a {
        (deposit_amount, 0u128, 0u128, exact_amount_out)
    } else {
        (0u128, exact_amount_out, deposit_amount, 0u128)
    };
    let mut oracle_pre = pool_def_data.clone();
    oracle_pre.oracle_update(clock_ts);
    // Reserve + RFP Usability #3 volume/fee accumulators (shared helper —
    // single source of truth), then layer the oracle ring/cum/ts on top.
    let mut pool_post_def =
        apply_swap_to_pool_def(pool_def_data, deposit_a, withdraw_a, deposit_b, withdraw_b);
    pool_post_def.price_a_cum_last = oracle_pre.price_a_cum_last;
    pool_post_def.price_b_cum_last = oracle_pre.price_b_cum_last;
    pool_post_def.block_ts_last = oracle_pre.block_ts_last;
    pool_post_def.obs = oracle_pre.obs;
    let mut pool_post = pool.account.clone();
    pool_post.data = Data::from(&pool_post_def);

    let post_states = vec![
        AccountPostState::new(pool_post),
        AccountPostState::new(vault_a.account),
        AccountPostState::new(vault_b.account),
        AccountPostState::new(owner.account),
        AccountPostState::new(ata_a.account),
        AccountPostState::new(ata_b.account),
    ];
    (post_states, chained_calls)
}
