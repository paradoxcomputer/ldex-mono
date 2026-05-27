//! On-chain integration for psychopomp. Wraps the LEZ sequencer RPC for:
//!   - reading an operator's `OperatorState` from the psychopomp-registry
//!     program account (chain-discovery validation),
//!   - querying registered program IDs,
//!   - (Phase-1+) posting registry/escrow instructions.

use borsh::BorshDeserialize;
use nssa_core::account::AccountId;
use nssa_core::program::ProgramId;
use psychopomp_escrow_core::{job_state_pda, Instruction as EInstr, JobFilter, JobState};
use psychopomp_hwclass::HwClass;
use psychopomp_registry_core::{operator_state_pda, FaultKind, Instruction as RInstr, OperatorState, OperatorStatus};
use sequencer_service_rpc::{RpcClient as _, SequencerClient, SequencerClientBuilder};
use std::collections::BTreeMap;
use std::path::Path;

const RPC_BODY_MAX_BYTES: u32 = 64 * 1024 * 1024;

#[derive(thiserror::Error, Debug)]
pub enum ChainError {
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("borsh: {0}")]
    Borsh(#[from] std::io::Error),
    #[error("account_id parse: {0}")]
    AccountId(String),
}

pub struct PsychopompChain {
    client: SequencerClient,
    pub endpoint: String,
}

impl PsychopompChain {
    pub fn connect(endpoint: &str) -> Result<Self, ChainError> {
        let client = SequencerClientBuilder::default()
            .max_request_size(RPC_BODY_MAX_BYTES)
            .max_response_size(RPC_BODY_MAX_BYTES)
            .build(endpoint)
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
        })
    }

    /// List program-id registrations the sequencer knows about. Useful for
    /// quickly checking that the psychopomp-registry + psychopomp-escrow
    /// programs are deployed on this network.
    pub async fn list_program_ids(&self) -> Result<BTreeMap<String, ProgramId>, ChainError> {
        self.client
            .get_program_ids()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))
    }

    /// Read a single operator's OperatorState slot from chain. Returns None
    /// if the account doesn't exist or has empty data.
    pub async fn get_operator_state(
        &self,
        operator_account_id: AccountId,
    ) -> Result<Option<OperatorState>, ChainError> {
        let account = match self.client.get_account(operator_account_id).await {
            Ok(a) => a,
            Err(_) => return Ok(None),
        };
        let bytes = account.data.as_ref();
        if bytes.is_empty() {
            return Ok(None);
        }
        let st = OperatorState::try_from_slice(bytes)?;
        Ok(Some(st))
    }

    /// Read a job's on-chain state from the deployed escrow program.
    /// Returns None for an empty slot.
    pub async fn get_job_state(
        &self,
        job_account_id: AccountId,
    ) -> Result<Option<JobState>, ChainError> {
        let account = match self.client.get_account(job_account_id).await {
            Ok(a) => a,
            Err(_) => return Ok(None),
        };
        let bytes = account.data.as_ref();
        if bytes.is_empty() {
            return Ok(None);
        }
        let st = JobState::try_from_slice(bytes)?;
        Ok(Some(st))
    }

    /// Filter a candidate set of operator account-ids: keep only those whose
    /// on-chain state is Active. Used as a chain-side validation layer on top
    /// of the file-based discovery in `psychopomp-client::discovery`.
    pub async fn keep_active(
        &self,
        candidates: Vec<AccountId>,
    ) -> Result<Vec<(AccountId, OperatorState)>, ChainError> {
        let mut out = Vec::with_capacity(candidates.len());
        for acc in candidates {
            if let Some(st) = self.get_operator_state(acc).await? {
                if matches!(st.status, OperatorStatus::Active) {
                    out.push((acc, st));
                }
            }
        }
        Ok(out)
    }
}

pub fn parse_account_id(s: &str) -> Result<AccountId, ChainError> {
    s.parse::<AccountId>().map_err(|e| ChainError::AccountId(e.to_string()))
}

/// Returns the on-chain account-id slot for a given operator pubkey under
/// the deployed registry program. Caller can then read its `OperatorState`
/// via `get_operator_state(pda)`.
pub fn operator_pda(registry_program_id: &ProgramId, operator_pk: &[u8; 32]) -> AccountId {
    operator_state_pda(registry_program_id, operator_pk)
}

/// Returns the on-chain account-id slot for a given escrow job_id.
pub fn job_pda(escrow_program_id: &ProgramId, job_id: &[u8; 32]) -> AccountId {
    job_state_pda(escrow_program_id, job_id)
}

/// Common path for "PublicTransaction { account_ids = <accounts>, signers = <signers> }".
async fn submit_tx<I: serde::Serialize>(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    program_id: ProgramId,
    accounts: Vec<AccountId>,
    signer_acc: AccountId,
    instruction: I,
) -> Result<common::HashType, ChainError> {
    use wallet::WalletCore;
    let wallet = WalletCore::new_update_chain(
        wallet_config_path.to_path_buf(),
        wallet_storage_path.to_path_buf(),
        None,
    )
    .map_err(|e| ChainError::Rpc(format!("wallet init: {e}")))?;
    let nonces = wallet
        .get_accounts_nonces(vec![signer_acc])
        .await
        .map_err(|e| ChainError::Rpc(format!("nonce fetch: {e}")))?;
    let signing_key = wallet
        .storage()
        .user_data
        .get_pub_account_signing_key(signer_acc)
        .ok_or_else(|| ChainError::Rpc(format!("signing key not in wallet: {signer_acc:?}")))?;
    let message = nssa::public_transaction::Message::try_new(
        program_id,
        accounts,
        nonces,
        instruction,
    )
    .map_err(|e| ChainError::Rpc(format!("Message::try_new: {e}")))?;
    let witness_set = nssa::public_transaction::WitnessSet::for_message(&message, &[signing_key]);
    let tx = nssa::PublicTransaction::new(message, witness_set);
    wallet
        .sequencer_client
        .send_transaction(common::transaction::NSSATransaction::Public(tx))
        .await
        .map_err(|e| ChainError::Rpc(format!("send_transaction: {e}")))
}

/// Compat wrapper for callers still expecting the [slot, signer] shape.
async fn submit_2acc<I: serde::Serialize>(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    program_id: ProgramId,
    slot: AccountId,
    signer_acc: AccountId,
    instruction: I,
) -> Result<common::HashType, ChainError> {
    submit_tx(
        wallet_config_path, wallet_storage_path, program_id,
        vec![slot, signer_acc], signer_acc, instruction,
    ).await
}

/// Client posts a new job.
#[allow(clippy::too_many_arguments)]
pub async fn submit_post(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    escrow_program_id: ProgramId,
    funder_account: AccountId,
    job_id: [u8; 32],
    client_pk: [u8; 32],
    ciphertext_hash: [u8; 32],
    filter: JobFilter,
    max_bid: u128,
    deadline_epoch: u64,
) -> Result<(common::HashType, AccountId), ChainError> {
    let pda = job_state_pda(&escrow_program_id, &job_id);
    let instr = EInstr::Post {
        job_id,
        client_pk,
        ciphertext_hash,
        filter,
        max_bid,
        deadline_epoch,
    };
    let hash = submit_2acc(
        wallet_config_path,
        wallet_storage_path,
        escrow_program_id,
        pda,
        funder_account,
        instr,
    )
    .await?;
    Ok((hash, pda))
}

/// Operator accepts an open job.
#[allow(clippy::too_many_arguments)]
pub async fn submit_accept(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    escrow_program_id: ProgramId,
    operator_funding_account: AccountId,
    job_id: [u8; 32],
    operator_pk: [u8; 32],
    operator_hw_class: HwClass,
    operator_mrenclave: [u8; 32],
) -> Result<(common::HashType, AccountId), ChainError> {
    let pda = job_state_pda(&escrow_program_id, &job_id);
    let instr = EInstr::Accept {
        job_id,
        operator_pk,
        operator_hw_class,
        operator_mrenclave,
    };
    let hash = submit_2acc(
        wallet_config_path,
        wallet_storage_path,
        escrow_program_id,
        pda,
        operator_funding_account,
        instr,
    )
    .await?;
    Ok((hash, pda))
}

/// Operator delivers + settles. Requires the operator's on-chain
/// `OperatorState` slot as a pre-state so the escrow can chain a
/// `record_settlement` call into the registry.
#[allow(clippy::too_many_arguments)]
pub async fn submit_settle(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    escrow_program_id: ProgramId,
    operator_funding_account: AccountId,
    operator_state_slot: AccountId,
    job_id: [u8; 32],
    operator_pk: [u8; 32],
    wall_clock_ms: u64,
) -> Result<(common::HashType, AccountId), ChainError> {
    let pda = job_state_pda(&escrow_program_id, &job_id);
    let instr = EInstr::Settle {
        job_id,
        operator_pk,
        wall_clock_ms,
    };
    let hash = submit_tx(
        wallet_config_path,
        wallet_storage_path,
        escrow_program_id,
        vec![pda, operator_funding_account, operator_state_slot],
        operator_funding_account,
        instr,
    )
    .await?;
    Ok((hash, pda))
}

/// Anyone keeper-faults a missed-deadline or rejected-proof job. Same
/// 3-account shape as settle; the escrow chains
/// `record_settlement(success=false, fault_kind=reason)` to the registry.
///
/// `claimed_epoch_now` is the caller's claim of the current chain epoch
/// (Phase-2 will read it from CLOCK_01 inside the guest instead).
#[allow(clippy::too_many_arguments)]
pub async fn submit_fault(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    escrow_program_id: ProgramId,
    caller_account: AccountId,
    operator_state_slot: AccountId,
    job_id: [u8; 32],
    reason: FaultKind,
    claimed_epoch_now: u64,
) -> Result<(common::HashType, AccountId), ChainError> {
    let pda = job_state_pda(&escrow_program_id, &job_id);
    let instr = EInstr::Fault { job_id, reason, claimed_epoch_now };
    let hash = submit_tx(
        wallet_config_path,
        wallet_storage_path,
        escrow_program_id,
        vec![pda, caller_account, operator_state_slot],
        caller_account,
        instr,
    )
    .await?;
    Ok((hash, pda))
}

/// Submit a `Register` transaction.
#[allow(clippy::too_many_arguments)]
///
/// Builds a `PublicTransaction { account_ids = [operator_slot_pda, funder] }`
/// invoking `psychopomp-registry::register(...)`, signs with `funder_account`'s
/// key, posts via the sequencer RPC, and returns the resulting tx hash plus
/// the operator-slot PDA so the caller can read state back.
///
/// The `funder_account` is any wallet-managed public account whose
/// `SigningKey` is available — it pays for the slot creation and signs the
/// authorization.
pub async fn submit_register(
    wallet_config_path: &Path,
    wallet_storage_path: &Path,
    registry_program_id: ProgramId,
    funder_account: AccountId,
    operator_pk: [u8; 32],
    attestation_root: [u8; 32],
    measurements: Vec<[u8; 32]>,
    hw_class: HwClass,
    stake: u128,
) -> Result<(common::HashType, AccountId), ChainError> {
    use wallet::WalletCore;

    let wallet =
        WalletCore::new_update_chain(wallet_config_path.to_path_buf(), wallet_storage_path.to_path_buf(), None)
            .map_err(|e| ChainError::Rpc(format!("wallet init: {e}")))?;

    let pda = operator_state_pda(&registry_program_id, &operator_pk);

    // Get nonce + signing key for the funder.
    let nonces = wallet
        .get_accounts_nonces(vec![funder_account])
        .await
        .map_err(|e| ChainError::Rpc(format!("nonce fetch: {e}")))?;
    let funder_key = wallet
        .storage()
        .user_data
        .get_pub_account_signing_key(funder_account)
        .ok_or_else(|| ChainError::Rpc(format!("funder signing key not in wallet: {funder_account:?}")))?;

    let instruction = RInstr::Register {
        operator_pk,
        attestation_root,
        measurements,
        hw_class,
        stake,
    };

    let message = nssa::public_transaction::Message::try_new(
        registry_program_id,
        vec![pda, funder_account],
        nonces,
        instruction,
    )
    .map_err(|e| ChainError::Rpc(format!("Message::try_new: {e}")))?;
    let witness_set = nssa::public_transaction::WitnessSet::for_message(&message, &[funder_key]);
    let tx = nssa::PublicTransaction::new(message, witness_set);

    let hash = wallet
        .sequencer_client
        .send_transaction(common::transaction::NSSATransaction::Public(tx))
        .await
        .map_err(|e| ChainError::Rpc(format!("send_transaction: {e}")))?;
    Ok((hash, pda))
}

/// Hex-encode a ProgramId (`[u32;8]`) as a 64-char lowercase string. Matches
/// the format used in the wallet's `chain-info` output.
pub fn program_id_hex(id: &ProgramId) -> String {
    let mut s = String::with_capacity(64);
    for w in id {
        s.push_str(&format!("{w:08x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_account_id_round_trips_b58() {
        // 32 zeros base58-encoded
        let s = "11111111111111111111111111111111";
        assert!(parse_account_id(s).is_ok());
    }

    #[test]
    fn chain_error_displays_cleanly() {
        let e = ChainError::Rpc("oops".into());
        assert_eq!(format!("{e}"), "rpc: oops");
    }

    /// Live test against the psychopomp sequencer at :3050. Marked `#[ignore]`
    /// so it doesn't run in CI; run explicitly with:
    /// `cargo test -p psychopomp-chain -- --ignored live_sequencer_program_ids`
    #[tokio::test]
    #[ignore]
    async fn live_sequencer_program_ids() {
        let chain = PsychopompChain::connect("http://127.0.0.1:3050/").unwrap();
        let ids = chain.list_program_ids().await.unwrap();
        eprintln!("registered programs on :3050:");
        for (name, id) in &ids {
            eprintln!("  {name:30}  {}", id.iter().fold(String::new(), |mut a, w| { a.push_str(&format!("{w:08x}")); a }));
        }
    }
}
