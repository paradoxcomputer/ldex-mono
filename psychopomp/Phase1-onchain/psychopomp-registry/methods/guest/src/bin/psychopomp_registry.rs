//! psychopomp-registry LEZ guest.
//!
//! Instructions wrap `psychopomp-registry-core::Instruction`. Each handler
//! takes one `AccountWithMetadata` (the operator's per-pubkey state slot)
//! and the instruction args; the bridge crate handles the borsh round-trip.

#![no_main]

use spel_framework::prelude::*;
use spel_framework::context::ProgramContext;
use nssa_core::account::AccountWithMetadata;
use nssa_core::program::DEFAULT_PROGRAM_ID;
use psychopomp_registry_core::{FaultKind, Instruction as RInstr, RegistryParams};
use psychopomp_hwclass::HwClass;

risc0_zkvm::guest::entry!(main);

#[lez_program(instruction = "psychopomp_registry_core::Instruction")]
mod psychopomp_registry {
    #[allow(unused_imports)]
    use super::*;

    /// Register a new operator. `slot` is the (currently empty) PDA-owned
    /// per-operator state account; `_funder` is any wallet-controlled
    /// public account that signs the tx (Phase-1 stub: no stake transfer
    /// yet; future versions will chain-call auth_transfer to lock stake
    /// from funder into a registry-owned bond vault).
    #[instruction]
    pub fn register(
        slot: AccountWithMetadata,
        _funder: AccountWithMetadata,
        operator_pk: [u8; 32],
        attestation_root: [u8; 32],
        measurements: Vec<[u8; 32]>,
        hw_class: HwClass,
        stake: u128,
    ) -> SpelResult {
        let instr = RInstr::Register {
            operator_pk,
            attestation_root,
            measurements,
            hw_class,
            stake,
        };
        let post = psychopomp_registry_program::apply_to_account(
            slot,
            &instr,
            0, // epoch_now: filled in by LEZ runtime; placeholder for Phase-1
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![post], vec![]))
    }

    #[instruction]
    pub fn update_measurements(
        slot: AccountWithMetadata,
        operator_pk: [u8; 32],
        measurements: Vec<[u8; 32]>,
        additional_stake: u128,
    ) -> SpelResult {
        let instr = RInstr::UpdateMeasurements {
            operator_pk,
            measurements,
            additional_stake,
        };
        let post = psychopomp_registry_program::apply_to_account(
            slot,
            &instr,
            0,
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![post], vec![]))
    }

    #[instruction]
    pub fn unbond(slot: AccountWithMetadata, operator_pk: [u8; 32]) -> SpelResult {
        let instr = RInstr::Unbond { operator_pk };
        let post = psychopomp_registry_program::apply_to_account(
            slot,
            &instr,
            0,
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![post], vec![]))
    }

    #[instruction]
    pub fn withdraw(slot: AccountWithMetadata, operator_pk: [u8; 32]) -> SpelResult {
        let instr = RInstr::Withdraw { operator_pk };
        let post = psychopomp_registry_program::apply_to_account(
            slot,
            &instr,
            0,
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![post], vec![]))
    }

    /// Called BY ANOTHER PROGRAM (in practice psychopomp-escrow) as a
    /// chained call. Direct user invocation is rejected - anyone could
    /// otherwise inflate a competitor's failure count. Phase-1 gate is
    /// "must be chain-called by some program"; Phase-2 will pin the
    /// escrow's ProgramId explicitly after deploy.
    #[instruction]
    pub fn record_settlement(
        ctx: ProgramContext,
        slot: AccountWithMetadata,
        operator_pk: [u8; 32],
        success: bool,
        fault_kind: Option<FaultKind>,
    ) -> SpelResult {
        if ctx.caller_program_id == DEFAULT_PROGRAM_ID {
            return Err(SpelError::custom(
                2,
                "record_settlement: caller_program_id is DEFAULT (direct user call rejected); must be chain-called",
            ));
        }
        let instr = RInstr::RecordSettlement {
            operator_pk,
            success,
            fault_kind,
        };
        let post = psychopomp_registry_program::apply_to_account(
            slot,
            &instr,
            0,
            &RegistryParams::default(),
        )
        .map_err(|e| SpelError::custom(1, e.to_string()))?;
        Ok(spel_framework::SpelOutput::execute(vec![post], vec![]))
    }
}
