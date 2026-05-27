use amm_core::{
    assert_supported_fee_tier, read_vault_fungible_balances, PoolDefinition, MINIMUM_LIQUIDITY,
};
use nssa_core::{
    account::{AccountWithMetadata, Data},
    program::{AccountPostState, ChainedCall},
};

pub fn sync_reserves(
    pool: AccountWithMetadata,
    vault_a: AccountWithMetadata,
    vault_b: AccountWithMetadata,
    clock_ts: i64,
) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
    let pool_def_data = PoolDefinition::try_from(&pool.account.data)
        .expect("Sync reserves: AMM Program expects a valid Pool Definition Account");
    assert_supported_fee_tier(pool_def_data.fees);

    assert!(
        pool_def_data.liquidity_pool_supply >= MINIMUM_LIQUIDITY,
        "Pool liquidity supply is below minimum liquidity"
    );
    assert_eq!(
        vault_a.account_id, pool_def_data.vault_a_id,
        "Vault A was not provided"
    );
    assert_eq!(
        vault_b.account_id, pool_def_data.vault_b_id,
        "Vault B was not provided"
    );

    let (vault_a_balance, vault_b_balance) =
        read_vault_fungible_balances("Sync reserves", &vault_a, &vault_b);
    assert!(
        vault_a_balance >= pool_def_data.reserve_a,
        "Sync reserves: vault A balance is less than its reserve"
    );
    assert!(
        vault_b_balance >= pool_def_data.reserve_b,
        "Sync reserves: vault B balance is less than its reserve"
    );

    let mut pool_post = pool.account.clone();
    // On-chain price oracle (§5.11③): accumulate at pre-sync reserves.
    let oracle_pre = {
        let mut o = pool_def_data.clone();
        o.oracle_update(clock_ts);
        o
    };
    let mut pool_post_definition = PoolDefinition {
        reserve_a: vault_a_balance,
        reserve_b: vault_b_balance,
        ..pool_def_data
    };
    pool_post_definition.price_a_cum_last = oracle_pre.price_a_cum_last;
    pool_post_definition.price_b_cum_last = oracle_pre.price_b_cum_last;
    pool_post_definition.block_ts_last = oracle_pre.block_ts_last;
    pool_post_definition.obs = oracle_pre.obs;
    pool_post.data = Data::from(&pool_post_definition);

    (
        vec![
            AccountPostState::new(pool_post),
            AccountPostState::new(vault_a.account.clone()),
            AccountPostState::new(vault_b.account.clone()),
        ],
        Vec::new(),
    )
}
