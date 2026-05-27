//! State machine for the psychopomp-registry LEZ program.
//!
//! README §Components > "psychopomp-registry" specifies:
//! - Per-operator state: pubkey, attestation root, allowed MRENCLAVE list,
//!   hardware class, stake bond, reputation, active status, unbonding timer.
//! - Instructions: Register, UpdateMeasurements, Unbond, Withdraw.
//!
//! This crate is the pure-Rust state machine + types. A LEZ guest binary
//! (under `psychopomp-registry/methods/guest`, Phase-1) wraps this crate and
//! plumbs it into nssa_core::account::AccountWithMetadata I/O. The state
//! machine itself is deterministic + testable without the zkVM.

use borsh::{BorshDeserialize, BorshSerialize};
use psychopomp_hwclass::HwClass;
use serde::{Deserialize, Serialize};

/// PDA seed for the per-operator state account.
///
/// `operator_state_seed = sha256(b"psychopomp-registry/operator/" || operator_pk)`.
/// The full AccountId is `AccountId::for_public_pda(&registry_program_id, &PdaSeed::new(seed))`.
/// Pure-Rust so the LEZ guest can call this; the host-side `operator_state_pda`
/// helper (gated on the `host` feature) wraps it into an `AccountId`.
pub fn operator_state_seed(operator_pk: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"psychopomp-registry/operator/");
    h.update(operator_pk);
    h.finalize().into()
}

#[cfg(feature = "host")]
pub fn operator_state_pda(
    registry_program_id: &nssa_core::program::ProgramId,
    operator_pk: &[u8; 32],
) -> nssa_core::account::AccountId {
    let seed = operator_state_seed(operator_pk);
    nssa_core::account::AccountId::for_public_pda(
        registry_program_id,
        &nssa_core::program::PdaSeed::new(seed),
    )
}

/// Network-governed parameters. Tunable via governance proposal at runtime.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct RegistryParams {
    /// Minimum bond required at Register time (LEZ).
    pub min_stake: u128,
    /// `K` in `S >= K * R_epoch` — stake-to-revenue multiplier (initial: 100).
    pub stake_to_revenue_k: u32,
    /// Unbonding cooldown in epochs (initial: 2 weeks of epochs).
    pub unbond_epochs: u64,
    /// Maximum allowed MRENCLAVE entries per operator (anti-churn).
    pub max_measurements: usize,
}

impl Default for RegistryParams {
    fn default() -> Self {
        Self {
            min_stake: 1_000_000_000_000_000_000, // 1 LEZ in atto
            stake_to_revenue_k: 100,
            unbond_epochs: 14 * 24 * 6, // ~14d at 10-min epochs
            max_measurements: 8,
        }
    }
}

/// Persisted per-operator state held by the registry program.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct OperatorState {
    /// 32-byte ed25519 pubkey identifying the operator. Signs registry updates.
    pub operator_pk: [u8; 32],
    /// Long-term attestation-root pubkey. Signs ephemeral per-session keys.
    pub attestation_root: [u8; 32],
    pub measurements: Vec<[u8; 32]>,
    pub hw_class: HwClass,
    pub stake: u128,
    pub reputation: Reputation,
    pub status: OperatorStatus,
    /// Epoch number at which Unbond was called. None until unbonded.
    pub unbond_started_epoch: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct Reputation {
    pub successes: u64,
    pub liveness_faults: u64,
    pub correctness_faults: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum OperatorStatus {
    Active = 0,
    Unbonding = 1,
    Withdrawn = 2,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Instruction {
    Register {
        operator_pk: [u8; 32],
        attestation_root: [u8; 32],
        measurements: Vec<[u8; 32]>,
        hw_class: HwClass,
        stake: u128,
    },
    UpdateMeasurements {
        operator_pk: [u8; 32],
        measurements: Vec<[u8; 32]>,
        /// Top-up to cover the cooldown penalty for changing measurements
        /// without going through full unbond/rebond (initial proposal in
        /// README: re-stake + cooldown).
        additional_stake: u128,
    },
    Unbond {
        operator_pk: [u8; 32],
    },
    Withdraw {
        operator_pk: [u8; 32],
    },
    /// Settlement program calls this on a verified completed job. The
    /// reputation update isn't part of the user-callable surface; gated by
    /// caller-program-id check in the LEZ wrapper.
    RecordSettlement {
        operator_pk: [u8; 32],
        success: bool,
        fault_kind: Option<FaultKind>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum FaultKind {
    Liveness = 0,
    Correctness = 1,
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum RegistryError {
    #[error("operator already registered")]
    AlreadyRegistered,
    #[error("operator not found")]
    NotFound,
    #[error("stake {stake} < min {min}")]
    StakeBelowMin { stake: u128, min: u128 },
    #[error("too many measurements: {count} > max {max}")]
    TooManyMeasurements { count: usize, max: usize },
    #[error("operator must be Active for this op")]
    NotActive,
    #[error("operator not unbonded yet")]
    NotUnbonded,
    #[error("unbond cooldown not complete: {remaining} epochs left")]
    CooldownPending { remaining: u64 },
}

/// Apply one instruction to the registry's per-operator slot. Returns the
/// new state (or new entry) on success. Caller is responsible for
/// authorization (ed25519 sig over the instruction by `operator_pk`) and for
/// persisting the result back to the account.
pub fn apply(
    current: Option<&OperatorState>,
    instr: &Instruction,
    epoch_now: u64,
    params: &RegistryParams,
) -> Result<OperatorState, RegistryError> {
    match instr {
        Instruction::Register {
            operator_pk,
            attestation_root,
            measurements,
            hw_class,
            stake,
        } => {
            if current.is_some() {
                return Err(RegistryError::AlreadyRegistered);
            }
            if *stake < params.min_stake {
                return Err(RegistryError::StakeBelowMin { stake: *stake, min: params.min_stake });
            }
            if measurements.len() > params.max_measurements {
                return Err(RegistryError::TooManyMeasurements {
                    count: measurements.len(),
                    max: params.max_measurements,
                });
            }
            Ok(OperatorState {
                operator_pk: *operator_pk,
                attestation_root: *attestation_root,
                measurements: measurements.clone(),
                hw_class: *hw_class,
                stake: *stake,
                reputation: Reputation::default(),
                status: OperatorStatus::Active,
                unbond_started_epoch: None,
            })
        }
        Instruction::UpdateMeasurements {
            operator_pk,
            measurements,
            additional_stake,
        } => {
            let mut s = current.ok_or(RegistryError::NotFound)?.clone();
            if s.operator_pk != *operator_pk {
                return Err(RegistryError::NotFound);
            }
            if s.status != OperatorStatus::Active {
                return Err(RegistryError::NotActive);
            }
            if measurements.len() > params.max_measurements {
                return Err(RegistryError::TooManyMeasurements {
                    count: measurements.len(),
                    max: params.max_measurements,
                });
            }
            s.measurements = measurements.clone();
            s.stake = s.stake.saturating_add(*additional_stake);
            Ok(s)
        }
        Instruction::Unbond { operator_pk } => {
            let mut s = current.ok_or(RegistryError::NotFound)?.clone();
            if s.operator_pk != *operator_pk {
                return Err(RegistryError::NotFound);
            }
            if s.status != OperatorStatus::Active {
                return Err(RegistryError::NotActive);
            }
            s.status = OperatorStatus::Unbonding;
            s.unbond_started_epoch = Some(epoch_now);
            Ok(s)
        }
        Instruction::Withdraw { operator_pk } => {
            let s = current.ok_or(RegistryError::NotFound)?;
            if s.operator_pk != *operator_pk {
                return Err(RegistryError::NotFound);
            }
            if s.status != OperatorStatus::Unbonding {
                return Err(RegistryError::NotUnbonded);
            }
            let started = s.unbond_started_epoch.ok_or(RegistryError::NotUnbonded)?;
            let elapsed = epoch_now.saturating_sub(started);
            if elapsed < params.unbond_epochs {
                return Err(RegistryError::CooldownPending {
                    remaining: params.unbond_epochs - elapsed,
                });
            }
            // Stake transfer back to the operator is the caller's job (the
            // LEZ wrapper). We return Withdrawn so the slot can be reaped.
            let mut s = s.clone();
            s.status = OperatorStatus::Withdrawn;
            s.stake = 0;
            Ok(s)
        }
        Instruction::RecordSettlement {
            operator_pk,
            success,
            fault_kind,
        } => {
            let mut s = current.ok_or(RegistryError::NotFound)?.clone();
            if s.operator_pk != *operator_pk {
                return Err(RegistryError::NotFound);
            }
            if *success {
                s.reputation.successes = s.reputation.successes.saturating_add(1);
            } else {
                match fault_kind {
                    Some(FaultKind::Liveness) => {
                        s.reputation.liveness_faults =
                            s.reputation.liveness_faults.saturating_add(1)
                    }
                    Some(FaultKind::Correctness) => {
                        s.reputation.correctness_faults =
                            s.reputation.correctness_faults.saturating_add(1)
                    }
                    None => {}
                }
            }
            Ok(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psychopomp_hwclass::HwClass;

    fn pk(b: u8) -> [u8; 32] { [b; 32] }

    fn register(stake: u128) -> Instruction {
        Instruction::Register {
            operator_pk: pk(1),
            attestation_root: pk(2),
            measurements: vec![pk(3)],
            hw_class: HwClass::H100CC,
            stake,
        }
    }

    #[test]
    fn happy_register() {
        let s = apply(None, &register(2_000_000_000_000_000_000), 100, &RegistryParams::default()).unwrap();
        assert_eq!(s.status, OperatorStatus::Active);
        assert_eq!(s.stake, 2_000_000_000_000_000_000);
    }

    #[test]
    fn rejects_double_register() {
        let p = RegistryParams::default();
        let s = apply(None, &register(p.min_stake), 0, &p).unwrap();
        let err = apply(Some(&s), &register(p.min_stake), 0, &p).unwrap_err();
        assert_eq!(err, RegistryError::AlreadyRegistered);
    }

    #[test]
    fn rejects_low_stake() {
        let p = RegistryParams::default();
        let err = apply(None, &register(1), 0, &p).unwrap_err();
        assert!(matches!(err, RegistryError::StakeBelowMin { .. }));
    }

    #[test]
    fn unbond_then_withdraw_after_cooldown() {
        let p = RegistryParams { unbond_epochs: 100, ..Default::default() };
        let s = apply(None, &register(p.min_stake), 0, &p).unwrap();
        let unbonded = apply(Some(&s), &Instruction::Unbond { operator_pk: pk(1) }, 50, &p).unwrap();
        assert_eq!(unbonded.status, OperatorStatus::Unbonding);
        // 99 epochs elapsed → still pending
        let err = apply(Some(&unbonded), &Instruction::Withdraw { operator_pk: pk(1) }, 149, &p).unwrap_err();
        assert!(matches!(err, RegistryError::CooldownPending { .. }));
        // 100 epochs elapsed → can withdraw
        let w = apply(Some(&unbonded), &Instruction::Withdraw { operator_pk: pk(1) }, 150, &p).unwrap();
        assert_eq!(w.status, OperatorStatus::Withdrawn);
        assert_eq!(w.stake, 0);
    }

    #[test]
    fn reputation_increments() {
        let p = RegistryParams::default();
        let s = apply(None, &register(p.min_stake), 0, &p).unwrap();
        let s = apply(
            Some(&s),
            &Instruction::RecordSettlement {
                operator_pk: pk(1),
                success: true,
                fault_kind: None,
            },
            0,
            &p,
        )
        .unwrap();
        assert_eq!(s.reputation.successes, 1);
        let s = apply(
            Some(&s),
            &Instruction::RecordSettlement {
                operator_pk: pk(1),
                success: false,
                fault_kind: Some(FaultKind::Liveness),
            },
            0,
            &p,
        )
        .unwrap();
        assert_eq!(s.reputation.liveness_faults, 1);
    }
}
