//! RFP Func #8 — `AddLiquidity` with the user side using Associated Token
//! Accounts. Both deposit legs go through `ata::Transfer` (owner-authorised);
//! the LP mint into the user's ATA-LP uses the existing PDA-authorised
//! `token::Mint` since `Mint`'s recipient is just a Fungible token holding.
//!
//! Math is byte-for-byte the same as `amm/src/add.rs::add_liquidity` (LP
//! supply / reserve update + oracle accumulator); the only delta is the
//! deposit-leg chained calls.

use std::num::NonZeroU128;

use amm_core::{
    assert_supported_fee_tier, compute_liquidity_token_pda_seed, read_vault_fungible_balances,
    PoolDefinition,
};
use nssa_core::{
    account::{AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall, ProgramId},
};

#[expect(clippy::too_many_arguments, reason = "fixed protocol account list")]
#[must_use]
pub fn add_liquidity_ata(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    pool_definition_lp: AccountWithMetadata,
    owner: AccountWithMetadata,
    ata_a: AccountWithMetadata,
    ata_b: AccountWithMetadata,
    ata_lp: AccountWithMetadata,
    min_amount_liquidity: NonZeroU128,
    max_amount_to_add_token_a: u128,
    max_amount_to_add_token_b: u128,
    ata_program_id: ProgramId,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def_data = PoolDefinition::try_from(&pool.account.data)
        .expect("Add liquidity (ATA): valid Pool Definition expected");
    assert_supported_fee_tier(pool_def_data.fees);
    assert_eq!(vault_a.account_id, pool_def_data.vault_a_id, "Vault A was not provided");
    assert_eq!(vault_b.account_id, pool_def_data.vault_b_id, "Vault B was not provided");
    // SECURITY: the ATA program must match the one pinned at pool creation, else
    // a no-op substitute would skip the real deposit `token::Transfer`s while the
    // LP is still minted to the user (see swap_ata for the full rationale).
    assert_eq!(
        ata_program_id, pool_def_data.ata_program_id,
        "ata_program_id does not match the program pinned at pool creation"
    );
    assert_eq!(
        pool_def_data.liquidity_pool_id, pool_definition_lp.account_id,
        "LP definition mismatch"
    );

    let token_program_id = vault_a.account.program_owner;
    assert_eq!(
        ata_a.account.program_owner, token_program_id,
        "ATA A must be owned by the vault's Token Program"
    );
    assert_eq!(
        ata_b.account.program_owner, token_program_id,
        "ATA B must be owned by the vault's Token Program"
    );
    assert_eq!(
        ata_lp.account.program_owner, token_program_id,
        "ATA LP must be owned by the vault's Token Program"
    );
    assert!(
        max_amount_to_add_token_a != 0 && max_amount_to_add_token_b != 0,
        "Both max-balances must be nonzero"
    );

    let (vault_a_balance, vault_b_balance) =
        read_vault_fungible_balances("Add liquidity (ATA)", &vault_a, &vault_b);
    assert!(vault_a_balance >= pool_def_data.reserve_a, "vault_a balance < reserve_a");
    assert!(vault_b_balance >= pool_def_data.reserve_b, "vault_b balance < reserve_b");

    assert!(pool_def_data.reserve_a != 0, "Reserves must be nonzero");
    assert!(pool_def_data.reserve_b != 0, "Reserves must be nonzero");
    let ideal_a = pool_def_data.reserve_a
        .checked_mul(max_amount_to_add_token_b).expect("reserve_a * max_b overflows u128")
        / pool_def_data.reserve_b;
    let ideal_b = pool_def_data.reserve_b
        .checked_mul(max_amount_to_add_token_a).expect("reserve_b * max_a overflows u128")
        / pool_def_data.reserve_a;
    let actual_amount_a = if ideal_a > max_amount_to_add_token_a { max_amount_to_add_token_a } else { ideal_a };
    let actual_amount_b = if ideal_b > max_amount_to_add_token_b { max_amount_to_add_token_b } else { ideal_b };
    assert!(actual_amount_a != 0, "A trade amount is 0");
    assert!(actual_amount_b != 0, "A trade amount is 0");

    let delta_lp = std::cmp::min(
        pool_def_data.liquidity_pool_supply
            .checked_mul(actual_amount_a).expect("lp * a overflows u128")
            / pool_def_data.reserve_a,
        pool_def_data.liquidity_pool_supply
            .checked_mul(actual_amount_b).expect("lp * b overflows u128")
            / pool_def_data.reserve_b,
    );
    assert!(delta_lp != 0, "Payable LP must be nonzero");
    assert!(delta_lp >= min_amount_liquidity.get(), "Payable LP < min provided");

    // Pool post-state — mirror add::add_liquidity's update.
    let mut pool_post = pool.account.clone();
    let oracle_pre = {
        let mut o = pool_def_data.clone();
        o.oracle_update(clock_ts);
        o
    };
    let mut pool_post_def = PoolDefinition {
        liquidity_pool_supply: pool_def_data.liquidity_pool_supply
            .checked_add(delta_lp).expect("lp + delta overflows u128"),
        reserve_a: pool_def_data.reserve_a
            .checked_add(actual_amount_a).expect("reserve_a + a overflows u128"),
        reserve_b: pool_def_data.reserve_b
            .checked_add(actual_amount_b).expect("reserve_b + b overflows u128"),
        ..pool_def_data
    };
    pool_post_def.price_a_cum_last = oracle_pre.price_a_cum_last;
    pool_post_def.price_b_cum_last = oracle_pre.price_b_cum_last;
    pool_post_def.block_ts_last = oracle_pre.block_ts_last;
    pool_post_def.obs = oracle_pre.obs;
    pool_post.data = Data::from(&pool_post_def);

    // Chained calls:
    //   (1) ata::Transfer { owner, ATA_A, vault_a, actual_amount_a }
    //   (2) ata::Transfer { owner, ATA_B, vault_b, actual_amount_b }
    //   (3) token::Mint   { lp_def(PDA), ATA_LP, delta_lp }
    let mut owner_auth = owner.clone();
    owner_auth.is_authorized = true;
    let call_a = ChainedCall::new(
        ata_program_id,
        vec![owner_auth.clone(), ata_a.clone(), vault_a.clone()],
        &ata_core::Instruction::Transfer { amount: actual_amount_a },
    );
    let call_b = ChainedCall::new(
        ata_program_id,
        vec![owner_auth, ata_b.clone(), vault_b.clone()],
        &ata_core::Instruction::Transfer { amount: actual_amount_b },
    );
    let mut lp_def_auth = pool_definition_lp.clone();
    lp_def_auth.is_authorized = true;
    let call_lp = ChainedCall::new(
        token_program_id,
        vec![lp_def_auth, ata_lp.clone()],
        &token_core::Instruction::Mint { amount_to_mint: delta_lp },
    )
    .with_pda_seeds(vec![compute_liquidity_token_pda_seed(pool.account_id)]);

    let chained_calls = vec![call_lp, call_b, call_a];

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
