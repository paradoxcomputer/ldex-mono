use std::num::NonZeroU128;

use amm_core::{
    assert_supported_fee_tier, compute_liquidity_token_pda, compute_liquidity_token_pda_seed,
    compute_lp_lock_holding_pda, compute_lp_lock_holding_pda_seed, compute_pool_pda,
    compute_pool_pda_seed, compute_vault_pda, compute_vault_pda_seed, PoolDefinition,
    MINIMUM_LIQUIDITY,
};
use nssa_core::{
    account::{Account, AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall, Claim, ProgramId},
};
use token_core::TokenDefinition;

/// Initializes a new Pool with a keypair-only user side - no ATA program is
/// pinned, so the pool's ATA-routed ops are fail-closed (rejected). For an
/// ATA-routable pool use [`new_definition_ata`].
#[expect(clippy::too_many_arguments, reason = "TODO: Fix later")]
pub fn new_definition(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    lp_lock_holding: AccountWithMetadata,
    user_holding_a: AccountWithMetadata,
    user_holding_b: AccountWithMetadata,
    user_holding_lp: AccountWithMetadata,
    token_a_amount: NonZeroU128,
    token_b_amount: NonZeroU128,
    fees: u128,
    amm_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // No ATA program pinned (fail-closed).
    new_definition_impl(
        pool,
        vault_a,
        vault_b,
        pool_definition_lp,
        lp_lock_holding,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        token_a_amount,
        token_b_amount,
        fees,
        amm_program_id,
        ProgramId::default(),
        clock_ts,
    )
}

/// Like [`new_definition`], but pins `ata_program_id` into the pool so the
/// ATA-routed ops (`swap_exact_input_ata` / `swap_exact_output_ata` /
/// `add_liquidity_ata`) can assert the submitter's program matches the one
/// fixed at creation. Mirrors `amm_v2::new_definition_ata`; the deposit /
/// LP-lock / LP-mint legs are identical to [`new_definition`].
#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
pub fn new_definition_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    lp_lock_holding: AccountWithMetadata,
    user_holding_a: AccountWithMetadata,
    user_holding_b: AccountWithMetadata,
    user_holding_lp: AccountWithMetadata,
    token_a_amount: NonZeroU128,
    token_b_amount: NonZeroU128,
    fees: u128,
    amm_program_id: ProgramId,
    ata_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    new_definition_impl(
        pool,
        vault_a,
        vault_b,
        pool_definition_lp,
        lp_lock_holding,
        user_holding_a,
        user_holding_b,
        user_holding_lp,
        token_a_amount,
        token_b_amount,
        fees,
        amm_program_id,
        ata_program_id,
        clock_ts,
    )
}

#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
fn new_definition_impl(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    lp_lock_holding: AccountWithMetadata,
    user_holding_a: AccountWithMetadata,
    user_holding_b: AccountWithMetadata,
    user_holding_lp: AccountWithMetadata,
    token_a_amount: NonZeroU128,
    token_b_amount: NonZeroU128,
    fees: u128,
    amm_program_id: ProgramId,
    ata_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    // Validate the fee tier before it is used to derive the (fee-tier-aware)
    // pool PDA, so an unsupported tier is rejected with a clear message
    // rather than surfacing as a PDA mismatch.
    assert_supported_fee_tier(fees);

    let definition_token_a_id = token_core::TokenHolding::try_from(&user_holding_a.account.data)
        .expect("New definition: AMM Program expects valid Token Holding account for Token A")
        .definition_id();
    let definition_token_b_id = token_core::TokenHolding::try_from(&user_holding_b.account.data)
        .expect("New definition: AMM Program expects valid Token Holding account for Token B")
        .definition_id();

    let token_program = user_holding_a.account.program_owner;

    // both instances of the same token program
    assert_eq!(
        user_holding_b.account.program_owner, token_program,
        "User Token holdings must use the same Token Program"
    );
    // Verify token_a and token_b are different
    assert!(
        definition_token_a_id != definition_token_b_id,
        "Cannot set up a swap for a token with itself"
    );
    assert_eq!(
        pool.account_id,
        compute_pool_pda(amm_program_id, definition_token_a_id, definition_token_b_id, fees),
        "Pool Definition Account ID does not match PDA"
    );
    assert_eq!(
        vault_a.account_id,
        compute_vault_pda(amm_program_id, pool.account_id, definition_token_a_id),
        "Vault ID does not match PDA"
    );
    assert_eq!(
        vault_b.account_id,
        compute_vault_pda(amm_program_id, pool.account_id, definition_token_b_id),
        "Vault ID does not match PDA"
    );
    assert_eq!(
        pool_definition_lp.account_id,
        compute_liquidity_token_pda(amm_program_id, pool.account_id),
        "Liquidity pool Token Definition Account ID does not match PDA"
    );
    assert_eq!(
        lp_lock_holding.account_id,
        compute_lp_lock_holding_pda(amm_program_id, pool.account_id),
        "LP lock holding Account ID does not match PDA"
    );

    // Assert that pool is uninitialized (hard precondition)
    assert_eq!(
        pool.account,
        Account::default(),
        "Pool account must be uninitialized"
    );
    assert!(
        user_holding_lp.account != Account::default() || user_holding_lp.is_authorized,
        "Fresh user LP holding requires user authorization"
    );

    // LP Token minting calculation
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

    // Update pool account
    let mut pool_post = pool.account.clone();
    let pool_post_definition = PoolDefinition {
        definition_token_a_id,
        definition_token_b_id,
        vault_a_id: vault_a.account_id,
        vault_b_id: vault_b.account_id,
        liquidity_pool_id: pool_definition_lp.account_id,
        liquidity_pool_supply: initial_lp,
        reserve_a: token_a_amount.into(),
        reserve_b: token_b_amount.into(),
        fees,
        // Pinned at pool creation. `new_definition` (NewDefinition) passes a
        // default/zero pin - a keypair-only pool whose ATA-routed ops are
        // fail-closed (rejected). `new_definition_ata` (NewDefinitionAta)
        // passes the submitter-supplied ATA program so those ops can match.
        ata_program_id,
        // On-chain price oracle (§5.11③): seed the clock baseline at
        // pool creation; accumulation starts on the first mutating tx.
        price_a_cum_last: 0,
        price_b_cum_last: 0,
        block_ts_last: clock_ts,
        obs: Vec::new(),
        // Lifetime aggregate counters (RFP Usability #3) - start at zero.
        cum_volume_a: 0,
        cum_volume_b: 0,
        cum_fees_a: 0,
        cum_fees_b: 0,
    };

    pool_post.data = Data::from(&pool_post_definition);
    let pool_post: AccountPostState = AccountPostState::new_claimed(
        pool_post.clone(),
        Claim::Pda(compute_pool_pda_seed(
            definition_token_a_id,
            definition_token_b_id,
            fees,
        )),
    );

    let token_program_id = user_holding_a.account.program_owner;

    // Chain call for Token A (user_holding_a -> Vault_A)
    let mut vault_a_authorized = vault_a.clone();
    vault_a_authorized.is_authorized = true;
    let call_token_a = ChainedCall::new(
        token_program_id,
        vec![user_holding_a.clone(), vault_a_authorized],
        &token_core::Instruction::Transfer {
            amount_to_transfer: token_a_amount.into(),
        },
    )
    .with_pda_seeds(vec![compute_vault_pda_seed(
        pool.account_id,
        definition_token_a_id,
    )]);
    // Chain call for Token B (user_holding_b -> Vault_B)
    let mut vault_b_authorized = vault_b.clone();
    vault_b_authorized.is_authorized = true;
    let call_token_b = ChainedCall::new(
        token_program_id,
        vec![user_holding_b.clone(), vault_b_authorized],
        &token_core::Instruction::Transfer {
            amount_to_transfer: token_b_amount.into(),
        },
    )
    .with_pda_seeds(vec![compute_vault_pda_seed(
        pool.account_id,
        definition_token_b_id,
    )]);

    // Chain call for liquidity token lock holding
    let mut pool_lp_auth = pool_definition_lp.clone();
    pool_lp_auth.is_authorized = true;
    let mut lp_lock_holding_auth = lp_lock_holding.clone();
    lp_lock_holding_auth.is_authorized = true;

    let call_token_lp_lock = ChainedCall::new(
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

    let mut pool_lp_after_lock = pool_lp_auth.clone();
    pool_lp_after_lock.account.program_owner = token_program_id;
    pool_lp_after_lock.account.data = Data::from(&TokenDefinition::Fungible {
        name: String::from("LP Token"),
        total_supply: MINIMUM_LIQUIDITY,
        metadata_id: None,
    });

    let call_token_lp_user = ChainedCall::new(
        token_program_id,
        vec![pool_lp_after_lock, user_holding_lp.clone()],
        &token_core::Instruction::Mint {
            amount_to_mint: user_lp,
        },
    )
    .with_pda_seeds(vec![compute_liquidity_token_pda_seed(pool.account_id)]);

    let chained_calls = vec![
        call_token_lp_lock,
        call_token_lp_user,
        call_token_b,
        call_token_a,
    ];

    let post_states = vec![
        pool_post.clone(),
        AccountPostState::new(vault_a.account.clone()),
        AccountPostState::new(vault_b.account.clone()),
        AccountPostState::new(pool_definition_lp.account.clone()),
        AccountPostState::new(lp_lock_holding.account.clone()),
        AccountPostState::new(user_holding_a.account.clone()),
        AccountPostState::new(user_holding_b.account.clone()),
        AccountPostState::new(user_holding_lp.account.clone()),
    ];

    (post_states, chained_calls)
}
