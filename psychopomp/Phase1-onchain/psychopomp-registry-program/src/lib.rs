//! Bridge crate: wraps `psychopomp-registry-core::apply` in nssa account I/O.
//!
//! The guest binary (under `../psychopomp-registry/methods/guest`) consumes
//! one instruction-shaped input and one `AccountWithMetadata` representing
//! the operator's per-pubkey state slot. We borsh-decode it, apply the
//! state transition, borsh-encode the new state into `Data`, and emit a
//! single `AccountPostState`.
//!
//! What this crate intentionally does NOT do:
//!   - LEZ balance transfers (stake locking). The guest wrapper passes
//!     additional accounts representing the operator's bond, and the
//!     callsite there performs balance arithmetic + authority checks.
//!   - Ed25519 signature verification of the operator's `operator_pk`.
//!     The LEZ runtime authenticates via `AccountWithMetadata::is_authorized`.

use borsh::BorshDeserialize;
use nssa_core::account::{Account, AccountWithMetadata};
use nssa_core::program::{AccountPostState, Claim, PdaSeed};
use psychopomp_registry_core::{
    apply, operator_state_seed, Instruction, OperatorState, RegistryError, RegistryParams,
};

#[derive(thiserror::Error, Debug)]
pub enum BridgeError {
    #[error("borsh: {0}")]
    Borsh(#[from] std::io::Error),
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("data too big: {0}")]
    DataTooBig(String),
    #[error("operator slot must be authorized for self-state mutations")]
    NotAuthorized,
}

/// Apply a registry instruction to a single operator-state slot.
///
/// `slot` is the `AccountWithMetadata` representing the operator's PDA-owned
/// state account. On `Register` it must be empty; on every other op it must
/// borsh-deserialize to a valid `OperatorState`.
pub fn apply_to_account(
    slot: AccountWithMetadata,
    instr: &Instruction,
    epoch_now: u64,
    params: &RegistryParams,
) -> Result<AccountPostState, BridgeError> {
    let mut account: Account = slot.account.clone();
    let bytes = account.data.as_ref();
    let current: Option<OperatorState> = if bytes.is_empty() {
        None
    } else {
        Some(OperatorState::try_from_slice(bytes)?)
    };
    // Mutating an existing operator slot generally requires authorization
    // (the operator signed this tx). The exception is RecordSettlement,
    // which is meant to be chain-called by the escrow program and gated by
    // caller_program_id at the guest layer (see the LEZ guest).
    let is_record_settlement = matches!(instr, Instruction::RecordSettlement { .. });
    if current.is_some() && !slot.is_authorized && !is_record_settlement {
        return Err(BridgeError::NotAuthorized);
    }
    let new_state = apply(current.as_ref(), instr, epoch_now, params)?;
    let encoded = borsh::to_vec(&new_state)?;
    account.data = encoded
        .try_into()
        .map_err(|e: nssa_core::account::data::DataTooBigError| BridgeError::DataTooBig(e.to_string()))?;
    // On Register (current was None), the per-operator slot is a brand-new
    // PDA owned by this program — emit a Claim::Pda so the runtime
    // allocates it under (registry_program_id, sha256(...)). On every other
    // op, the slot already exists and the post-state passes through with no
    // claim. The PDA seed scheme matches `operator_state_seed(operator_pk)`
    // so chain-side derivation matches client-side derivation.
    if current.is_none() {
        let seed = PdaSeed::new(operator_state_seed(&new_state.operator_pk));
        Ok(AccountPostState::new_claimed(account, Claim::Pda(seed)))
    } else {
        Ok(AccountPostState::new(account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nssa_core::account::AccountId;
    use psychopomp_hwclass::HwClass;

    fn empty_slot(authorized: bool) -> AccountWithMetadata {
        AccountWithMetadata::new(Account::default(), authorized, AccountId::new([1u8; 32]))
    }

    #[test]
    fn round_trip_register_then_unbond() {
        let p = RegistryParams::default();
        let register = Instruction::Register {
            operator_pk: [9u8; 32],
            attestation_root: [8u8; 32],
            measurements: vec![[7u8; 32]],
            hw_class: HwClass::H100CC,
            stake: p.min_stake,
        };
        let post = apply_to_account(empty_slot(false), &register, 0, &p).unwrap();
        let state_bytes = post.account().data.as_ref().to_vec();
        let st: OperatorState = OperatorState::try_from_slice(&state_bytes).unwrap();
        assert_eq!(st.operator_pk, [9u8; 32]);

        // Build next slot from the post-state.
        let acc = Account {
            data: state_bytes.try_into().unwrap(),
            ..Default::default()
        };
        let next_slot = AccountWithMetadata::new(acc, true, AccountId::new([1u8; 32]));
        let post = apply_to_account(
            next_slot,
            &Instruction::Unbond { operator_pk: [9u8; 32] },
            1,
            &p,
        )
        .unwrap();
        let after = OperatorState::try_from_slice(post.account().data.as_ref()).unwrap();
        assert!(matches!(after.status, psychopomp_registry_core::OperatorStatus::Unbonding));
    }

    #[test]
    fn unauthorized_mutation_rejected() {
        let p = RegistryParams::default();
        // First, register on an authorized empty slot
        let register = Instruction::Register {
            operator_pk: [9u8; 32],
            attestation_root: [8u8; 32],
            measurements: vec![[7u8; 32]],
            hw_class: HwClass::H100CC,
            stake: p.min_stake,
        };
        let post = apply_to_account(empty_slot(true), &register, 0, &p).unwrap();
        let state_bytes = post.account().data.as_ref().to_vec();
        // Then try to Unbond on an UN-authorized slot
        let acc = Account {
            data: state_bytes.try_into().unwrap(),
            ..Default::default()
        };
        let slot = AccountWithMetadata::new(acc, false, AccountId::new([1u8; 32]));
        let err = apply_to_account(slot, &Instruction::Unbond { operator_pk: [9u8; 32] }, 1, &p)
            .unwrap_err();
        assert!(matches!(err, BridgeError::NotAuthorized));
    }
}
