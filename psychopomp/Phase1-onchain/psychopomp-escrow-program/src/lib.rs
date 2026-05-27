//! Bridge crate: wraps `psychopomp-escrow-core::apply` in nssa account I/O.
//!
//! The guest binary consumes one instruction + one `AccountWithMetadata`
//! representing the per-job escrow slot (PDA-owned). We borsh-decode, apply,
//! re-encode the new `JobState` (or empty Data if the slot should be reaped).
//!
//! Balance-delta application (transferring LEZ between client, operator, and
//! the burn sink) is the GUEST's job once it has the `Vec<BalanceDelta>`
//! returned here — see the methods/guest/ binary for the chained-call
//! plumbing that does that.

use borsh::BorshDeserialize;
use nssa_core::account::{Account, AccountWithMetadata};
use nssa_core::program::{AccountPostState, Claim, PdaSeed};
use psychopomp_escrow_core::{apply, job_state_seed, ApplyOutput, EscrowError, EscrowParams, Instruction, JobState};
use psychopomp_registry_core::RegistryParams;

pub use psychopomp_escrow_core::BalanceDelta;

#[derive(thiserror::Error, Debug)]
pub enum BridgeError {
    #[error("borsh: {0}")]
    Borsh(#[from] std::io::Error),
    #[error("escrow: {0}")]
    Escrow(#[from] EscrowError),
    #[error("data too big: {0}")]
    DataTooBig(String),
}

pub struct ApplyBundle {
    pub post_state: AccountPostState,
    pub deltas: Vec<BalanceDelta>,
    /// The operator that this state-transition implicates (for chained
    /// `record_settlement` calls). Set on Settle and Fault; None elsewhere.
    pub operator_pk: Option<[u8; 32]>,
}

pub fn apply_to_account(
    slot: AccountWithMetadata,
    instr: &Instruction,
    epoch_now: u64,
    escrow_params: &EscrowParams,
    registry_params: &RegistryParams,
) -> Result<ApplyBundle, BridgeError> {
    let mut account: Account = slot.account.clone();
    let bytes = account.data.as_ref();
    let current: Option<JobState> = if bytes.is_empty() {
        None
    } else {
        Some(JobState::try_from_slice(bytes)?)
    };
    let was_empty = current.is_none();
    // Snapshot the operator_pk BEFORE applying — Fault zeroes the awarded
    // state, so we need to look at the prior status to know who to slash.
    let prior_operator_pk: Option<[u8; 32]> = current
        .as_ref()
        .and_then(|s| match &s.status {
            psychopomp_escrow_core::Status::Awarded { operator_pk, .. } => Some(*operator_pk),
            _ => None,
        });
    let ApplyOutput { new_state, deltas } =
        apply(current.as_ref(), instr, epoch_now, escrow_params, registry_params)?;
    // After applying, surface the operator_pk for chained-call use. Settle
    // sees it in the new Settled status; Fault preserves it from prior.
    let post_operator_pk: Option<[u8; 32]> = match &new_state {
        Some(s) => match &s.status {
            psychopomp_escrow_core::Status::Settled { operator_pk, .. } => Some(*operator_pk),
            psychopomp_escrow_core::Status::Refunded { .. } => prior_operator_pk,
            _ => None,
        },
        None => None,
    };
    let (encoded, new_job_id) = match &new_state {
        Some(s) => (borsh::to_vec(s)?, Some(s.job_id)),
        None => (Vec::new(), None),
    };
    account.data = encoded
        .try_into()
        .map_err(|e: nssa_core::account::data::DataTooBigError| BridgeError::DataTooBig(e.to_string()))?;
    // On Post (slot was empty), claim the PDA. Other instructions pass-through.
    let post_state = if was_empty {
        if let Some(jid) = new_job_id {
            let seed = PdaSeed::new(job_state_seed(&jid));
            AccountPostState::new_claimed(account, Claim::Pda(seed))
        } else {
            AccountPostState::new(account)
        }
    } else {
        AccountPostState::new(account)
    };
    Ok(ApplyBundle {
        post_state,
        deltas,
        operator_pk: post_operator_pk,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nssa_core::account::AccountId;
    use psychopomp_escrow_core::{JobFilter, Status};
    use psychopomp_hwclass::HwClass;

    fn empty_slot() -> AccountWithMetadata {
        AccountWithMetadata::new(Account::default(), true, AccountId::new([1u8; 32]))
    }

    #[test]
    fn post_then_accept_round_trip() {
        let ep = EscrowParams::default();
        let rp = RegistryParams::default();
        let post = Instruction::Post {
            job_id: [42u8; 32],
            client_pk: [1u8; 32],
            ciphertext_hash: [2u8; 32],
            filter: JobFilter {
                accepted_hw_classes: vec![HwClass::H100CC],
                accepted_mrenclaves: vec![],
            },
            max_bid: 1000,
            deadline_epoch: 100,
        };
        let bundle = apply_to_account(empty_slot(), &post, 0, &ep, &rp).unwrap();
        assert_eq!(bundle.deltas.len(), 1);
        let state_bytes = bundle.post_state.account().data.as_ref().to_vec();
        let st: JobState = JobState::try_from_slice(&state_bytes).unwrap();
        assert_eq!(st.status, Status::Open);

        // Continuation: Accept on the next iteration.
        let acc = Account {
            data: state_bytes.try_into().unwrap(),
            ..Default::default()
        };
        let slot = AccountWithMetadata::new(acc, true, AccountId::new([1u8; 32]));
        let accept = Instruction::Accept {
            job_id: [42u8; 32],
            operator_pk: [7u8; 32],
            operator_hw_class: HwClass::H100CC,
            operator_mrenclave: [9u8; 32],
        };
        let bundle = apply_to_account(slot, &accept, 5, &ep, &rp).unwrap();
        assert!(matches!(
            JobState::try_from_slice(bundle.post_state.account().data.as_ref())
                .unwrap()
                .status,
            Status::Awarded { .. }
        ));
        assert!(matches!(bundle.deltas[0], BalanceDelta::LockOperatorStake { .. }));
    }
}
