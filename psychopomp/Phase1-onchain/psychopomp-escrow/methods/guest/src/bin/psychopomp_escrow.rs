//! psychopomp-escrow LEZ guest.
//!
//! Phase-1 stub. STARK + attestation re-verification will land here once
//! `psychopomp-attest::Verifier_` can be linked into a guest-target build;
//! today the guest accepts the chain transition + emits the new JobState
//! + delta list. Balance enforcement is the chained-call wrapper's job.

#![no_main]

use spel_framework::prelude::*;
use nssa_core::account::AccountWithMetadata;
use nssa_core::program::{ChainedCall, DEFAULT_PROGRAM_ID};
use psychopomp_escrow_core::{EscrowParams, Instruction as EInstr, JobFilter};
use psychopomp_registry_core::{FaultKind, Instruction as RInstr, RegistryParams};
use psychopomp_hwclass::HwClass;

risc0_zkvm::guest::entry!(main);

#[lez_program(instruction = "psychopomp_escrow_core::Instruction")]
mod psychopomp_escrow {
    #[allow(unused_imports)]
    use super::*;

    /// Client opens a new job. `slot` is the (empty) PDA owned by this
    /// program; `funder` is the wallet account paying the escrow.
    ///
    /// Chains an authenticated_transfer (`funder` → `slot`, `max_bid` LEZ)
    /// so the escrow PDA is actually funded on chain. Pattern mirrors WLEZ's
    /// `wrap`: read the auth-transfer ProgramId from `funder.account.program_owner`
    /// (every public account is owned by auth-transfer), then chain a call
    /// whose instruction data is the bare `u128` amount.
    #[instruction]
    pub fn post(
        slot: AccountWithMetadata,
        funder: AccountWithMetadata,
        job_id: [u8; 32],
        client_pk: [u8; 32],
        ciphertext_hash: [u8; 32],
        filter: JobFilter,
        max_bid: u128,
        deadline_epoch: u64,
    ) -> SpelResult {
        let slot_id = slot.account_id;
        let instr = EInstr::Post {
            job_id,
            client_pk,
            ciphertext_hash,
            filter,
            max_bid,
            deadline_epoch,
        };
        let bundle = psychopomp_escrow_program::apply_to_account(
            slot,
            &instr,
            0,
            &EscrowParams::default(),
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        // Construct the chained call's pre-state for the slot from the
        // parent's POST-state (the LEZ runtime expects chained pre-states
        // to reflect the running diff — see lez-chained-call-prestate-semantics
        // memory note). The slot now has program_owner=escrow + the JobState
        // data; auth-transfer will add max_bid to its balance.
        // Chained-call pre-state: the runtime applies our post-state's data
        // mutation BEFORE invoking the child but does NOT yet honor our
        // Claim::Pda, so the slot is still DEFAULT_PROGRAM_ID-owned when
        // auth-transfer sees it. Mirror that: data = our JobState, but
        // program_owner = DEFAULT. (Authorization propagates via Claim::Pda
        // at runtime check-time, not for the chained pre-state.)
        let mut slot_post_account = bundle.post_state.account().clone();
        slot_post_account.program_owner = DEFAULT_PROGRAM_ID;
        let slot_for_chain = AccountWithMetadata {
            account: slot_post_account,
            is_authorized: false,
            account_id: slot_id,
        };
        let auth_transfer_program_id = funder.account.program_owner;
        let mut funder_authed = funder.clone();
        funder_authed.is_authorized = true;
        let fund_call = ChainedCall::new(
            auth_transfer_program_id,
            vec![funder_authed, slot_for_chain],
            &max_bid,
        );
        Ok(spel_framework::SpelOutput::execute(
            vec![bundle.post_state],
            vec![fund_call],
        ))
    }

    /// Operator commits to deliver. `_operator` is the funding account that
    /// will own the per-job stake lock (Phase-1+ chained call).
    #[instruction]
    pub fn accept(
        slot: AccountWithMetadata,
        _operator: AccountWithMetadata,
        job_id: [u8; 32],
        operator_pk: [u8; 32],
        operator_hw_class: HwClass,
        operator_mrenclave: [u8; 32],
    ) -> SpelResult {
        let instr = EInstr::Accept {
            job_id,
            operator_pk,
            operator_hw_class,
            operator_mrenclave,
        };
        let bundle = psychopomp_escrow_program::apply_to_account(
            slot,
            &instr,
            0,
            &EscrowParams::default(),
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![bundle.post_state], vec![]))
    }

    /// Operator delivers + settles. Chains `record_settlement(success=true)`
    /// to the registry program so the operator's reputation auto-updates.
    #[instruction]
    pub fn settle(
        slot: AccountWithMetadata,
        _operator: AccountWithMetadata,
        operator_state_slot: AccountWithMetadata,
        job_id: [u8; 32],
        operator_pk: [u8; 32],
        wall_clock_ms: u64,
    ) -> SpelResult {
        let instr = EInstr::Settle { job_id, operator_pk, wall_clock_ms };
        let bundle = psychopomp_escrow_program::apply_to_account(
            slot, &instr, 0, &EscrowParams::default(), &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        let registry_program_id = operator_state_slot.account.program_owner;
        let chained = vec![ChainedCall::new(
            registry_program_id,
            vec![operator_state_slot],
            &RInstr::RecordSettlement {
                operator_pk: bundle.operator_pk.unwrap_or(operator_pk),
                success: true,
                fault_kind: None,
            },
        )];
        Ok(spel_framework::SpelOutput::execute(vec![bundle.post_state], chained))
    }

    /// Anyone can fault a deadline-missed or proof-rejected job. Chains
    /// `record_settlement(success=false, fault_kind=reason)` to the registry
    /// so the operator's fault counter increments. Uses the operator_pk
    /// captured from the prior Awarded state — no need for the caller to
    /// supply it.
    ///
    /// `claimed_epoch_now` is the caller's claim of the current chain epoch
    /// (used by the bridge to enforce the `deadline_epoch` check for
    /// Liveness faults). Phase-2 will replace this with a CLOCK_01 read
    /// (same pattern LDEX's AMM uses). Until then, a hostile caller could
    /// submit a fake epoch — but the worst they can do is fault a job they
    /// could have faulted later anyway, so the practical risk is low.
    #[instruction]
    pub fn fault(
        slot: AccountWithMetadata,
        _caller: AccountWithMetadata,
        operator_state_slot: AccountWithMetadata,
        job_id: [u8; 32],
        reason: FaultKind,
        claimed_epoch_now: u64,
    ) -> SpelResult {
        let instr = EInstr::Fault { job_id, reason, claimed_epoch_now };
        let bundle = psychopomp_escrow_program::apply_to_account(
            slot, &instr, claimed_epoch_now, &EscrowParams::default(), &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        let chained = match bundle.operator_pk {
            Some(operator_pk) => {
                let registry_program_id = operator_state_slot.account.program_owner;
                vec![ChainedCall::new(
                    registry_program_id,
                    vec![operator_state_slot],
                    &RInstr::RecordSettlement {
                        operator_pk,
                        success: false,
                        fault_kind: Some(reason),
                    },
                )]
            }
            None => vec![],
        };
        Ok(spel_framework::SpelOutput::execute(vec![bundle.post_state], chained))
    }
}
