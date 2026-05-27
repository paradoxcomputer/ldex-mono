//! State machine for the psychopomp-escrow LEZ program.
//!
//! README §Components > "psychopomp-escrow" specifies:
//! - Per-job state: client_pk, ciphertext_hash, filter, max_bid, escrow,
//!   deadline, status: Open|Awarded|Settled|Refunded.
//! - Instructions: Post, Accept, Settle (with stark + attestation), Fault.
//!
//! Pure-Rust state machine. The LEZ guest wrapper (Phase-1) plumbs
//! `Settle.stark` through `risc0_zkvm::Receipt::verify(IMAGE_ID)` and
//! `Settle.attestation` through `psychopomp_attest::Verifier_` before
//! invoking `apply()`.

use borsh::{BorshDeserialize, BorshSerialize};
use psychopomp_registry_core::{FaultKind, RegistryParams};
use psychopomp_hwclass::HwClass;
use serde::{Deserialize, Serialize};

/// PDA seed for the per-job state account.
/// `job_state_seed = sha256(b"psychopomp-escrow/job/" || job_id)`.
pub fn job_state_seed(job_id: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"psychopomp-escrow/job/");
    h.update(job_id);
    h.finalize().into()
}

#[cfg(feature = "host")]
pub fn job_state_pda(
    escrow_program_id: &nssa_core::program::ProgramId,
    job_id: &[u8; 32],
) -> nssa_core::account::AccountId {
    let seed = job_state_seed(job_id);
    nssa_core::account::AccountId::for_public_pda(
        escrow_program_id,
        &nssa_core::program::PdaSeed::new(seed),
    )
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct EscrowParams {
    /// Per-job operator stake = `alpha * max_bid`. Initial α=10 per README.
    pub alpha: u32,
    /// Slashed-bond split: portion burnt (rest goes to honest operators
    /// pro-rata). 0..=10000 (basis points). Initial 5000.
    pub burn_bps: u16,
}

impl Default for EscrowParams {
    fn default() -> Self {
        Self {
            alpha: 10,
            burn_bps: 5000,
        }
    }
}

/// Constraints clients attach to the job: which hardware classes are
/// acceptable, which MRENCLAVE measurements (whitelist), etc. Operators that
/// don't satisfy the filter can't legitimately Accept.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct JobFilter {
    pub accepted_hw_classes: Vec<HwClass>,
    pub accepted_mrenclaves: Vec<[u8; 32]>,
}

impl JobFilter {
    pub fn matches(&self, hw: HwClass, mrenclave: &[u8; 32]) -> bool {
        let hw_ok = self.accepted_hw_classes.is_empty()
            || self.accepted_hw_classes.contains(&hw);
        let m_ok = self.accepted_mrenclaves.is_empty()
            || self.accepted_mrenclaves.contains(mrenclave);
        hw_ok && m_ok
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct JobState {
    pub job_id: [u8; 32],
    pub client_pk: [u8; 32],
    pub ciphertext_hash: [u8; 32],
    pub filter: JobFilter,
    pub max_bid: u128,
    pub escrow: u128,
    pub deadline_epoch: u64,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Status {
    Open,
    Awarded {
        operator_pk: [u8; 32],
        operator_locked_stake: u128,
        accepted_epoch: u64,
    },
    Settled {
        operator_pk: [u8; 32],
        wall_clock_ms: u64,
    },
    Refunded {
        reason: FaultKind,
    },
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Instruction {
    /// Client posts a new job. The runtime locks `max_bid` from the client's
    /// account and creates a new escrow slot.
    Post {
        job_id: [u8; 32],
        client_pk: [u8; 32],
        ciphertext_hash: [u8; 32],
        filter: JobFilter,
        max_bid: u128,
        deadline_epoch: u64,
    },
    /// Operator commits to deliver. Runtime locks `alpha * max_bid` from the
    /// operator's bond on top of the per-operator base stake.
    Accept {
        job_id: [u8; 32],
        operator_pk: [u8; 32],
        operator_hw_class: HwClass,
        operator_mrenclave: [u8; 32],
    },
    /// Operator delivers `{stark, attestation, binding}`. The LEZ wrapper
    /// has already verified the stark + attestation; we just bookkeep.
    Settle {
        job_id: [u8; 32],
        operator_pk: [u8; 32],
        wall_clock_ms: u64,
    },
    /// Anyone can call after deadline OR on attestation/STARK rejection.
    /// `claimed_epoch_now` is the caller's epoch claim (Phase-2 will read
    /// from CLOCK_01 in the guest).
    Fault {
        job_id: [u8; 32],
        reason: FaultKind,
        claimed_epoch_now: u64,
    },
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum EscrowError {
    #[error("job exists")]
    JobExists,
    #[error("job not found")]
    NotFound,
    #[error("status not Open")]
    NotOpen,
    #[error("status not Awarded")]
    NotAwarded,
    #[error("operator does not match filter")]
    FilterMismatch,
    #[error("operator does not match awarded operator")]
    WrongOperator,
    #[error("deadline not yet reached")]
    DeadlineNotReached,
    #[error("deadline already past at Post time")]
    DeadlineInPast,
    #[error("max_bid is zero")]
    ZeroBid,
}

/// Apply one instruction to a job slot. Returns (new state, balance delta
/// instructions) — the LEZ wrapper applies the balance deltas to client and
/// operator accounts. None means delete the slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BalanceDelta {
    /// Lock `amount` from `from_pk` into the escrow.
    Lock { from_pk: [u8; 32], amount: u128 },
    /// Refund escrowed `amount` to `to_pk`.
    Refund { to_pk: [u8; 32], amount: u128 },
    /// Pay operator the bid amount from escrow.
    Pay { to_pk: [u8; 32], amount: u128 },
    /// Slash operator's locked per-job stake — split per `burn_bps`.
    Slash { operator_pk: [u8; 32], amount: u128, burn_bps: u16 },
    /// Lock operator's per-job stake from their bond.
    LockOperatorStake { operator_pk: [u8; 32], amount: u128 },
    /// Release operator's per-job stake back to their bond.
    ReleaseOperatorStake { operator_pk: [u8; 32], amount: u128 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutput {
    pub new_state: Option<JobState>,
    pub deltas: Vec<BalanceDelta>,
}

pub fn apply(
    current: Option<&JobState>,
    instr: &Instruction,
    epoch_now: u64,
    escrow_params: &EscrowParams,
    _registry_params: &RegistryParams,
) -> Result<ApplyOutput, EscrowError> {
    match instr {
        Instruction::Post {
            job_id,
            client_pk,
            ciphertext_hash,
            filter,
            max_bid,
            deadline_epoch,
        } => {
            if current.is_some() {
                return Err(EscrowError::JobExists);
            }
            if *max_bid == 0 {
                return Err(EscrowError::ZeroBid);
            }
            if *deadline_epoch <= epoch_now {
                return Err(EscrowError::DeadlineInPast);
            }
            let state = JobState {
                job_id: *job_id,
                client_pk: *client_pk,
                ciphertext_hash: *ciphertext_hash,
                filter: filter.clone(),
                max_bid: *max_bid,
                escrow: *max_bid,
                deadline_epoch: *deadline_epoch,
                status: Status::Open,
            };
            Ok(ApplyOutput {
                new_state: Some(state),
                deltas: vec![BalanceDelta::Lock {
                    from_pk: *client_pk,
                    amount: *max_bid,
                }],
            })
        }
        Instruction::Accept {
            job_id,
            operator_pk,
            operator_hw_class,
            operator_mrenclave,
        } => {
            let mut s = current.ok_or(EscrowError::NotFound)?.clone();
            if s.job_id != *job_id {
                return Err(EscrowError::NotFound);
            }
            if s.status != Status::Open {
                return Err(EscrowError::NotOpen);
            }
            if !s.filter.matches(*operator_hw_class, operator_mrenclave) {
                return Err(EscrowError::FilterMismatch);
            }
            let locked_stake = s.max_bid.saturating_mul(escrow_params.alpha as u128);
            s.status = Status::Awarded {
                operator_pk: *operator_pk,
                operator_locked_stake: locked_stake,
                accepted_epoch: epoch_now,
            };
            Ok(ApplyOutput {
                new_state: Some(s),
                deltas: vec![BalanceDelta::LockOperatorStake {
                    operator_pk: *operator_pk,
                    amount: locked_stake,
                }],
            })
        }
        Instruction::Settle {
            job_id,
            operator_pk,
            wall_clock_ms,
        } => {
            let mut s = current.ok_or(EscrowError::NotFound)?.clone();
            if s.job_id != *job_id {
                return Err(EscrowError::NotFound);
            }
            let (awarded_op, locked_stake) = match &s.status {
                Status::Awarded { operator_pk: op, operator_locked_stake, .. } => {
                    (*op, *operator_locked_stake)
                }
                _ => return Err(EscrowError::NotAwarded),
            };
            if awarded_op != *operator_pk {
                return Err(EscrowError::WrongOperator);
            }
            let bid = s.max_bid;
            s.escrow = 0;
            s.status = Status::Settled {
                operator_pk: *operator_pk,
                wall_clock_ms: *wall_clock_ms,
            };
            Ok(ApplyOutput {
                new_state: Some(s),
                deltas: vec![
                    BalanceDelta::Pay {
                        to_pk: *operator_pk,
                        amount: bid,
                    },
                    BalanceDelta::ReleaseOperatorStake {
                        operator_pk: *operator_pk,
                        amount: locked_stake,
                    },
                ],
            })
        }
        Instruction::Fault { job_id, reason, claimed_epoch_now: _ } => {
            let s = current.ok_or(EscrowError::NotFound)?;
            if s.job_id != *job_id {
                return Err(EscrowError::NotFound);
            }
            // Two valid Fault contexts:
            //   1) Awarded but past deadline → liveness fault.
            //   2) Awarded with rejected proof → correctness fault (caller has
            //      already done the verification off-chain).
            // Open or terminal states cannot be faulted.
            let (awarded_op, locked_stake) = match &s.status {
                Status::Awarded { operator_pk, operator_locked_stake, .. } => {
                    (*operator_pk, *operator_locked_stake)
                }
                Status::Open => return Err(EscrowError::NotAwarded),
                Status::Settled { .. } | Status::Refunded { .. } => return Err(EscrowError::NotAwarded),
            };
            if matches!(reason, FaultKind::Liveness) && epoch_now < s.deadline_epoch {
                return Err(EscrowError::DeadlineNotReached);
            }
            let mut s = s.clone();
            let refund_amt = s.escrow;
            s.escrow = 0;
            s.status = Status::Refunded { reason: *reason };
            Ok(ApplyOutput {
                new_state: Some(s),
                deltas: vec![
                    BalanceDelta::Refund {
                        to_pk: current.unwrap().client_pk,
                        amount: refund_amt,
                    },
                    BalanceDelta::Slash {
                        operator_pk: awarded_op,
                        amount: locked_stake,
                        burn_bps: escrow_params.burn_bps,
                    },
                ],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: u8) -> [u8; 32] { [x; 32] }

    fn post(deadline: u64, max_bid: u128) -> Instruction {
        Instruction::Post {
            job_id: b(0xA),
            client_pk: b(1),
            ciphertext_hash: b(2),
            filter: JobFilter {
                accepted_hw_classes: vec![HwClass::H100CC],
                accepted_mrenclaves: vec![],
            },
            max_bid,
            deadline_epoch: deadline,
        }
    }

    #[test]
    fn happy_post_accept_settle() {
        let ep = EscrowParams::default();
        let rp = RegistryParams::default();
        let out = apply(None, &post(100, 1000), 0, &ep, &rp).unwrap();
        let s = out.new_state.unwrap();
        assert_eq!(s.status, Status::Open);
        assert_eq!(s.escrow, 1000);

        let out = apply(
            Some(&s),
            &Instruction::Accept {
                job_id: b(0xA),
                operator_pk: b(7),
                operator_hw_class: HwClass::H100CC,
                operator_mrenclave: b(9),
            },
            10,
            &ep,
            &rp,
        )
        .unwrap();
        let s = out.new_state.unwrap();
        assert!(matches!(s.status, Status::Awarded { .. }));

        let out = apply(
            Some(&s),
            &Instruction::Settle {
                job_id: b(0xA),
                operator_pk: b(7),
                wall_clock_ms: 1234,
            },
            20,
            &ep,
            &rp,
        )
        .unwrap();
        let s = out.new_state.unwrap();
        assert!(matches!(s.status, Status::Settled { .. }));
        assert!(matches!(out.deltas[0], BalanceDelta::Pay { .. }));
    }

    #[test]
    fn liveness_fault_after_deadline() {
        let ep = EscrowParams::default();
        let rp = RegistryParams::default();
        let s = apply(None, &post(100, 1000), 0, &ep, &rp).unwrap().new_state.unwrap();
        let s = apply(
            Some(&s),
            &Instruction::Accept {
                job_id: b(0xA),
                operator_pk: b(7),
                operator_hw_class: HwClass::H100CC,
                operator_mrenclave: b(9),
            },
            10, &ep, &rp,
        ).unwrap().new_state.unwrap();
        // Try to fault before deadline → rejected
        let err = apply(
            Some(&s),
            &Instruction::Fault { job_id: b(0xA), reason: FaultKind::Liveness, claimed_epoch_now: 0 },
            50, &ep, &rp,
        ).unwrap_err();
        assert_eq!(err, EscrowError::DeadlineNotReached);
        // Past deadline → fault accepted
        let out = apply(
            Some(&s),
            &Instruction::Fault { job_id: b(0xA), reason: FaultKind::Liveness, claimed_epoch_now: 0 },
            101, &ep, &rp,
        ).unwrap();
        let s = out.new_state.unwrap();
        assert!(matches!(s.status, Status::Refunded { reason: FaultKind::Liveness }));
        assert!(out.deltas.iter().any(|d| matches!(d, BalanceDelta::Refund { .. })));
        assert!(out.deltas.iter().any(|d| matches!(d, BalanceDelta::Slash { .. })));
    }

    #[test]
    fn correctness_fault_any_time_after_award() {
        let ep = EscrowParams::default();
        let rp = RegistryParams::default();
        let s = apply(None, &post(100, 1000), 0, &ep, &rp).unwrap().new_state.unwrap();
        let s = apply(
            Some(&s),
            &Instruction::Accept {
                job_id: b(0xA), operator_pk: b(7),
                operator_hw_class: HwClass::H100CC, operator_mrenclave: b(9),
            }, 10, &ep, &rp,
        ).unwrap().new_state.unwrap();
        // Correctness fault is OK pre-deadline (caller did the off-chain reject)
        let out = apply(
            Some(&s),
            &Instruction::Fault { job_id: b(0xA), reason: FaultKind::Correctness, claimed_epoch_now: 0 },
            50, &ep, &rp,
        ).unwrap();
        assert!(matches!(out.new_state.unwrap().status, Status::Refunded { reason: FaultKind::Correctness }));
    }

    #[test]
    fn filter_mismatch_rejects_accept() {
        let ep = EscrowParams::default();
        let rp = RegistryParams::default();
        let s = apply(None, &post(100, 1000), 0, &ep, &rp).unwrap().new_state.unwrap();
        let err = apply(
            Some(&s),
            &Instruction::Accept {
                job_id: b(0xA), operator_pk: b(7),
                operator_hw_class: HwClass::MI300SEV, operator_mrenclave: b(9),
            }, 10, &ep, &rp,
        ).unwrap_err();
        assert_eq!(err, EscrowError::FilterMismatch);
    }
}
