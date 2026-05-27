//! WLEZ end-to-end integration tests under the zkVM (RISC0_DEV_MODE=1).
//!
//! Each test exercises the full Initialize/Wrap/Unwrap circuit:
//! `state.transition_from_public_transaction` deploys the program on a
//! fresh `V03State`, dispatches into the guest binary, runs all chained
//! calls (token::Mint, token::Burn, authenticated_transfer::transfer),
//! and asserts the resulting on-chain state.
//!
//! Pure-function shape tests live in `programs/wlez/src/tests.rs`. This
//! file pins the dynamic behaviour:
//!   - The chained native transfer + chained Mint compose correctly under
//!     `validate_execution` (no panic, balances conserved).
//!   - Vault PDA authorisation works for the native release in Unwrap.
//!   - Wrap/Unwrap is a round-trip — user's native balance is fully
//!     restored after a Wrap→Unwrap cycle, definition.total_supply ends
//!     at zero, vault.balance ends at zero.

use nssa::{
    program_deployment_transaction::{self, ProgramDeploymentTransaction},
    public_transaction, PrivateKey, PublicKey, PublicTransaction, V03State,
};
use nssa_core::{
    account::{Account, AccountId, Data, Nonce},
    program::ProgramId,
};
use token_core::{TokenDefinition, TokenHolding};
use wlez_core::{get_wlez_definition_id, get_wlez_vault_id, Instruction, WLEZ_NAME};

const USER_INITIAL_NATIVE: u128 = 10_000;

// Reusable keys / ids.
fn user_key() -> PrivateKey {
    PrivateKey::try_new([0x11; 32]).expect("valid private key")
}
fn user_id() -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(&user_key()))
}

fn ref_def_key() -> PrivateKey {
    PrivateKey::try_new([0x22; 32]).expect("valid private key")
}
fn ref_def_id() -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(&ref_def_key()))
}

fn user_holding_key() -> PrivateKey {
    PrivateKey::try_new([0x33; 32]).expect("valid private key")
}
fn user_holding_id() -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(&user_holding_key()))
}

fn token_program_id() -> ProgramId {
    token_methods::TOKEN_ID
}
fn wlez_program_id() -> ProgramId {
    wlez_methods::WLEZ_ID
}

fn vault_id() -> AccountId {
    get_wlez_vault_id(&wlez_program_id())
}
fn wlez_def_id() -> AccountId {
    get_wlez_definition_id(&wlez_program_id())
}

fn deploy_programs(state: &mut V03State) {
    for elf in [token_methods::TOKEN_ELF, wlez_methods::WLEZ_ELF] {
        let msg = program_deployment_transaction::Message::new(elf.to_vec());
        state
            .transition_from_program_deployment_transaction(&ProgramDeploymentTransaction::new(msg))
            .expect("program deployment must succeed");
    }
}

// Reference token definition (just a stand-in token program account so
// Initialize can read `token_program_id` from its `program_owner` field).
fn reference_token_definition_account() -> Account {
    Account {
        program_owner: token_program_id(),
        balance: 0,
        data: Data::from(&TokenDefinition::Fungible {
            name: "REF".to_string(),
            total_supply: 1_000_000,
            metadata_id: None,
        }),
        nonce: Nonce(0),
    }
}

// Pre-initialised WLEZ definition (after a successful Initialize run).
fn wlez_definition_after_init() -> Account {
    Account {
        program_owner: token_program_id(),
        balance: 0,
        data: Data::from(&TokenDefinition::Fungible {
            name: WLEZ_NAME.to_string(),
            total_supply: 0,
            metadata_id: None,
        }),
        nonce: Nonce(0),
    }
}

// Pre-initialised user WLEZ holding (after a `token::InitializeAccount`
// at the user-keypair-derived id, holding 0 WLEZ).
fn user_holding_init_with(balance: u128) -> Account {
    Account {
        program_owner: token_program_id(),
        balance: 0,
        data: Data::from(&TokenHolding::Fungible {
            definition_id: wlez_def_id(),
            balance,
        }),
        nonce: Nonce(0),
    }
}

// Pre-claimed vault (after a successful Initialize run).
fn vault_account_with(balance: u128) -> Account {
    Account {
        program_owner: wlez_program_id(),
        balance,
        data: Data::default(),
        nonce: Nonce(0),
    }
}

// Build a state that's already past Initialize: vault claimed, WLEZ
// definition created, user has a 0-balance WLEZ holding, user_native
// pre-funded with `USER_INITIAL_NATIVE` LEZ. Skipping Initialize via
// `force_insert_account` is the pragmatic way to test Wrap/Unwrap
// without also re-deriving the bootstrap admin's signer plumbing — the
// shape of Initialize is exercised by the unit tests in
// `programs/wlez/src/tests.rs`.
fn state_post_initialize() -> V03State {
    let mut state =
        V03State::new_with_genesis_accounts(&[(user_id(), USER_INITIAL_NATIVE)], vec![], 0);
    deploy_programs(&mut state);
    state.force_insert_account(vault_id(), vault_account_with(0));
    state.force_insert_account(wlez_def_id(), wlez_definition_after_init());
    state.force_insert_account(user_holding_id(), user_holding_init_with(0));
    state.force_insert_account(ref_def_id(), reference_token_definition_account());
    state
}

fn current_nonce(state: &V03State, id: AccountId) -> Nonce {
    state.get_account_by_id(id).nonce
}

fn token_definition_supply(state: &V03State, id: AccountId) -> u128 {
    match TokenDefinition::try_from(&state.get_account_by_id(id).data).unwrap() {
        TokenDefinition::Fungible { total_supply, .. } => total_supply,
        _ => panic!("expected fungible definition"),
    }
}
fn token_holding_balance(state: &V03State, id: AccountId) -> u128 {
    match TokenHolding::try_from(&state.get_account_by_id(id).data).unwrap() {
        TokenHolding::Fungible { balance, .. } => balance,
        _ => panic!("expected fungible holding"),
    }
}

// ── Initialize ──────────────────────────────────────────────────────

// Pins the live dispatch shape: the framework instantiates the guest
// with one AccountWithMetadata per Message.accounts entry, so the
// guest's `initialize` arg count MUST match the FFI's Message account
// count (5: vault, definition, init_holding, reference_token_def,
// payer). A mismatch silently rejects the tx at the sequencer — rc=0
// from the FFI but vault/def stay uninitialised on chain. This test
// exercises a real Initialize through the zkVM so a future regression
// surfaces immediately rather than during a live bootstrap.
#[test]
fn wlez_initialize_creates_definition_and_claims_vault() {
    // Genesis-fund the payer (USER) so it has a non-default native
    // account that can sign the Initialize tx.
    let mut state = V03State::new_with_genesis_accounts(
        &[(user_id(), USER_INITIAL_NATIVE)], vec![], 0);
    deploy_programs(&mut state);
    // Pre-insert the reference token def (any token-program-owned
    // definition will do — Initialize reads program_owner from it to
    // find the token program id).
    state.force_insert_account(ref_def_id(), reference_token_definition_account());
    // Derive the PDA-init-holding id so we can reference it in the message.
    let init_holding_id = wlez_core::get_wlez_init_holding_id(&wlez_program_id());

    // Print all the account ids so the ModifiedProgramOwner error
    // message can be cross-referenced with what's in the message.
    eprintln!("test ids:");
    eprintln!("  vault          = {:?}", vault_id());
    eprintln!("  wlez_def       = {:?}", wlez_def_id());
    eprintln!("  init_holding   = {:?}", init_holding_id);
    eprintln!("  ref_def        = {:?}", ref_def_id());
    eprintln!("  user (payer)   = {:?}", user_id());
    let message = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![
            vault_id(),
            wlez_def_id(),
            init_holding_id,
            ref_def_id(),
            user_id(),    // payer / signer
        ],
        vec![current_nonce(&state, user_id())],
        Instruction::Initialize,
    )
    .unwrap();
    let witness = public_transaction::WitnessSet::for_message(&message, &[&user_key()]);
    let tx = PublicTransaction::new(message, witness);
    state
        .transition_from_public_transaction(&tx, 0, 0)
        .expect("Initialize must succeed on the zkVM");

    // Vault is now claimed by the WLEZ program with 0 balance.
    let vault_acct = state.get_account_by_id(vault_id());
    assert_eq!(vault_acct.program_owner, wlez_program_id(),
        "vault must be owned by the WLEZ program after Initialize");
    assert_eq!(vault_acct.balance, 0);

    // Definition is a TokenDefinition::Fungible{name:WLEZ, total_supply:0}.
    let def_acct = state.get_account_by_id(wlez_def_id());
    assert_eq!(def_acct.program_owner, token_program_id(),
        "definition must be owned by the token program after Initialize");
    match TokenDefinition::try_from(&def_acct.data)
        .expect("definition must hold a valid TokenDefinition after Initialize") {
        TokenDefinition::Fungible { name, total_supply, .. } => {
            assert_eq!(name, WLEZ_NAME, "definition name must be \"WLEZ\"");
            assert_eq!(total_supply, 0, "supply must start at 0");
        }
        _ => panic!("expected fungible WLEZ definition"),
    }

    // Init-holding is a TokenHolding::Fungible{def: wlez_def_id, balance: 0}.
    let ih_acct = state.get_account_by_id(init_holding_id);
    assert_eq!(ih_acct.program_owner, token_program_id());
    match TokenHolding::try_from(&ih_acct.data)
        .expect("init_holding must be a valid TokenHolding after Initialize") {
        TokenHolding::Fungible { definition_id, balance } => {
            assert_eq!(definition_id, wlez_def_id());
            assert_eq!(balance, 0);
        }
        _ => panic!("expected fungible init_holding"),
    }
}

// ── Wrap ────────────────────────────────────────────────────────────

#[test]
fn wlez_wrap_locks_native_and_mints_wlez() {
    let mut state = state_post_initialize();

    let amount = 750_u128;
    let message = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![user_id(), vault_id(), wlez_def_id(), user_holding_id()],
        vec![current_nonce(&state, user_id())],
        Instruction::Wrap { amount },
    )
    .unwrap();
    let witness = public_transaction::WitnessSet::for_message(&message, &[&user_key()]);
    let tx = PublicTransaction::new(message, witness);
    state
        .transition_from_public_transaction(&tx, 0, 0)
        .expect("wrap must succeed");

    // user_native lost `amount` LEZ
    assert_eq!(
        state.get_account_by_id(user_id()).balance,
        USER_INITIAL_NATIVE - amount,
        "user native balance must decrease by wrap amount"
    );
    // vault gained `amount` LEZ
    assert_eq!(
        state.get_account_by_id(vault_id()).balance,
        amount,
        "vault balance must increase by wrap amount"
    );
    // WLEZ definition's total_supply grew
    assert_eq!(
        token_definition_supply(&state, wlez_def_id()),
        amount,
        "WLEZ total_supply must increase by wrap amount"
    );
    // User holding now has `amount` WLEZ
    assert_eq!(
        token_holding_balance(&state, user_holding_id()),
        amount,
        "user WLEZ holding must increase by wrap amount"
    );
    // **Conservation invariant**: vault.balance == definition.total_supply
    assert_eq!(
        state.get_account_by_id(vault_id()).balance,
        token_definition_supply(&state, wlez_def_id()),
        "vault must always equal total WLEZ supply"
    );
}

// ── Unwrap ──────────────────────────────────────────────────────────

#[test]
fn wlez_unwrap_burns_wlez_and_releases_native() {
    // Start from a state where the user has already wrapped 1000 LEZ.
    let mut state = state_post_initialize();
    state.force_insert_account(vault_id(), vault_account_with(1000));
    // Re-set the definition with total_supply=1000 to match the wrapped amount.
    let mut def_with_supply = wlez_definition_after_init();
    def_with_supply.data = Data::from(&TokenDefinition::Fungible {
        name: WLEZ_NAME.to_string(),
        total_supply: 1000,
        metadata_id: None,
    });
    state.force_insert_account(wlez_def_id(), def_with_supply);
    state.force_insert_account(user_holding_id(), user_holding_init_with(1000));

    let amount = 400_u128;
    let message = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![user_holding_id(), wlez_def_id(), vault_id(), user_id()],
        vec![current_nonce(&state, user_holding_id())],
        Instruction::Unwrap { amount },
    )
    .unwrap();
    let witness = public_transaction::WitnessSet::for_message(&message, &[&user_holding_key()]);
    let tx = PublicTransaction::new(message, witness);
    state
        .transition_from_public_transaction(&tx, 0, 0)
        .expect("unwrap must succeed");

    // user_holding lost `amount` WLEZ
    assert_eq!(
        token_holding_balance(&state, user_holding_id()),
        1000 - amount,
        "user WLEZ holding must decrease by unwrap amount"
    );
    // Definition's total_supply shrank
    assert_eq!(
        token_definition_supply(&state, wlez_def_id()),
        1000 - amount,
        "WLEZ total_supply must decrease by unwrap amount"
    );
    // Vault released `amount` LEZ
    assert_eq!(
        state.get_account_by_id(vault_id()).balance,
        1000 - amount,
        "vault balance must decrease by unwrap amount"
    );
    // user_native gained `amount` LEZ (started at USER_INITIAL_NATIVE).
    assert_eq!(
        state.get_account_by_id(user_id()).balance,
        USER_INITIAL_NATIVE + amount,
        "user native balance must increase by unwrap amount"
    );
    // Conservation still holds.
    assert_eq!(
        state.get_account_by_id(vault_id()).balance,
        token_definition_supply(&state, wlez_def_id()),
    );
}

// ── Round-trip ──────────────────────────────────────────────────────

#[test]
fn wlez_wrap_then_unwrap_restores_native_balance() {
    let mut state = state_post_initialize();
    let amount = 1234_u128;

    // Step 1: wrap.
    let wrap_msg = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![user_id(), vault_id(), wlez_def_id(), user_holding_id()],
        vec![current_nonce(&state, user_id())],
        Instruction::Wrap { amount },
    )
    .unwrap();
    let wrap_witness = public_transaction::WitnessSet::for_message(&wrap_msg, &[&user_key()]);
    state
        .transition_from_public_transaction(&PublicTransaction::new(wrap_msg, wrap_witness), 0, 0)
        .expect("wrap must succeed");

    // Step 2: unwrap the full amount back.
    let unwrap_msg = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![user_holding_id(), wlez_def_id(), vault_id(), user_id()],
        vec![current_nonce(&state, user_holding_id())],
        Instruction::Unwrap { amount },
    )
    .unwrap();
    let unwrap_witness =
        public_transaction::WitnessSet::for_message(&unwrap_msg, &[&user_holding_key()]);
    state
        .transition_from_public_transaction(
            &PublicTransaction::new(unwrap_msg, unwrap_witness),
            0,
            0,
        )
        .expect("unwrap must succeed");

    // User's native balance is back to where it started — 1:1 wrap means
    // no value leaks, and there are no fees on the wrap/unwrap path.
    assert_eq!(
        state.get_account_by_id(user_id()).balance,
        USER_INITIAL_NATIVE,
        "round-trip must restore the original native balance exactly"
    );
    assert_eq!(
        token_holding_balance(&state, user_holding_id()),
        0,
        "all WLEZ must have been burned"
    );
    assert_eq!(
        token_definition_supply(&state, wlez_def_id()),
        0,
        "WLEZ supply must be back to zero"
    );
    assert_eq!(
        state.get_account_by_id(vault_id()).balance,
        0,
        "vault must be empty"
    );
}

// ── Negative cases ──────────────────────────────────────────────────

#[test]
fn wlez_unwrap_rejects_when_vault_under_collateralised() {
    // This is the safety net the unit tests already pin at the
    // pure-function level — re-pinning at the zkVM level confirms the
    // panic propagates as a failed tx (rather than e.g. silently
    // succeeding and breaking the conservation invariant).
    let mut state = state_post_initialize();
    state.force_insert_account(vault_id(), vault_account_with(100));
    let mut def = wlez_definition_after_init();
    def.data = Data::from(&TokenDefinition::Fungible {
        name: WLEZ_NAME.to_string(),
        total_supply: 100,
        metadata_id: None,
    });
    state.force_insert_account(wlez_def_id(), def);
    state.force_insert_account(user_holding_id(), user_holding_init_with(100));

    let amount = 500_u128; // > vault.balance
    let message = public_transaction::Message::try_new(
        wlez_program_id(),
        vec![user_holding_id(), wlez_def_id(), vault_id(), user_id()],
        vec![current_nonce(&state, user_holding_id())],
        Instruction::Unwrap { amount },
    )
    .unwrap();
    let witness = public_transaction::WitnessSet::for_message(&message, &[&user_holding_key()]);
    let tx = PublicTransaction::new(message, witness);
    let result = state.transition_from_public_transaction(&tx, 0, 0);
    assert!(
        result.is_err(),
        "Unwrap with amount > vault must fail at the zkVM level"
    );

    // State must be unchanged after the failed tx.
    assert_eq!(state.get_account_by_id(vault_id()).balance, 100);
    assert_eq!(token_definition_supply(&state, wlez_def_id()), 100);
    assert_eq!(token_holding_balance(&state, user_holding_id()), 100);
    assert_eq!(state.get_account_by_id(user_id()).balance, USER_INITIAL_NATIVE);
}
