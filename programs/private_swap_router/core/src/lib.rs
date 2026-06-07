//! Core types for the LDEX **account-A private-swap router**.
//!
//! This program exists only to satisfy the *verbatim* RFP-004 Privacy
//! AC #4 wording - a **fresh single-use public account A** that literally
//! appears on-chain per operation (deshield → A swaps publicly →
//! re-shield). It is the **weaker** of the two privacy options the
//! mini-app ships; the recommended path is the routerless `PrivateOwned`
//! mode (design.md §5.10 "Private"), which exposes no public address at
//! all. See `docs/design.md` §5.2 for the full rationale.
//!
//! Atomicity is structural: the whole deshield→swap→re-shield tree is one
//! privacy-preserving transaction / one proof - it applies fully or not
//! at all, so the re-shield is physically inseparable from the deshield
//! (RFP Privacy AC #1, Usability #1/#7).

use nssa_core::account::AccountId;
use serde::{Deserialize, Serialize};

/// Private-swap router instruction.
#[derive(Serialize, Deserialize)]
pub enum Instruction {
    /// Deshield the input token from the user's (circuit-deshielded)
    /// private holding into the fresh public account A, run an AMM
    /// `SwapExactInput` from A in the public pool, then re-shield A's
    /// output back to the user's private holding - all in one
    /// privacy-preserving transaction.
    ///
    /// Required accounts (exact order; see guest entrypoint):
    /// 1. `user_holding_in`  - user's private input-token holding
    ///    (`PrivateOwned`; deshielded into the program by the circuit).
    /// 2. `a_holding_a`      - account A's Token-A holding (fresh, public).
    /// 3. `a_holding_b`      - account A's Token-B holding (fresh, public).
    /// 4. `pool`             - AMM pool (public).
    /// 5. `vault_a`          - pool vault for Token A (public).
    /// 6. `vault_b`          - pool vault for Token B (public).
    /// 7. `user_holding_out` - user's private output-token holding
    ///    (`PrivateOwned`; re-shielded by the circuit from its post-state).
    PrivateSwap {
        /// Exact input amount to deshield + swap.
        swap_amount_in: u128,
        /// Minimum acceptable output (slippage guard; forwarded to AMM).
        min_amount_out: u128,
        /// Which pool token is the input (selects A→B vs B→A).
        token_definition_id_in: AccountId,
        /// Pool fee tier in bps (must match the targeted pool).
        fees: u128,
        /// Unix-ms timestamp after which the whole tx is invalid.
        deadline: u64,
    },

    /// Batched native-LEZ-in swap. In one privacy-preserving transaction:
    ///   (1) WLEZ::Wrap - locks `swap_amount_in` native LEZ from the user's
    ///       public native account into the WLEZ vault and mints WLEZ into
    ///       account A's WLEZ holding;
    ///   (2) AMM::SwapExactInput - A trades the freshly-minted WLEZ for the
    ///       other side of the target pool;
    ///   (3) Token::Transfer - re-shields A's output back to the user's
    ///       private holding (`PrivateOwned`).
    ///
    /// Replaces the two-tx flow `wrap → private_swap` for native input,
    /// saving one block wait (~10 s) and one tx-submit roundtrip, and
    /// giving wrap+swap atomicity (no stuck-WLEZ failure mode if the
    /// second tx reverts).
    ///
    /// Required accounts (exact order; see guest entrypoint):
    /// 0. `user_native`       - user's public native LEZ account (signer).
    /// 1. `wlez_vault`        - WLEZ vault PDA. Mutated by chained Wrap.
    /// 2. `wlez_definition`   - WLEZ token-definition PDA. Mutated by Wrap.
    /// 3. `a_wlez_holding`    - A's WLEZ holding (public, pre-initialised
    ///                          for the WLEZ definition; receives mint).
    /// 4. `a_holding_out`     - A's output-token holding (public,
    ///                          pre-initialised for the pool's other side).
    /// 5. `pool`              - AMM pool (public).
    /// 6. `vault_a`           - pool vault for token A.
    /// 7. `vault_b`           - pool vault for token B.
    /// 8. `user_holding_out`  - user's private output-token holding
    ///                          (`PrivateOwned`; re-shielded by the
    ///                          circuit from its post-state).
    /// 9. `clock`             - on-chain clock read-only (AMM oracle).
    PrivateSwapNativeIn {
        /// Native LEZ to wrap+swap.
        swap_amount_in: u128,
        /// Minimum acceptable output (slippage guard; forwarded to AMM).
        min_amount_out: u128,
        /// Pool fee tier in bps (must match the targeted pool).
        fees: u128,
        /// Unix-ms timestamp after which the whole tx is invalid.
        deadline: u64,
    },

    /// Batched native-LEZ-out swap. Mirror of `PrivateSwapNativeIn`:
    ///   (1) Token::Transfer - deshields the user's private input holding
    ///       into account A's input-token holding;
    ///   (2) AMM::SwapExactInput - A trades the input for WLEZ on the pool;
    ///   (3) WLEZ::Unwrap - burns A's WLEZ and releases the equivalent
    ///       native LEZ from the vault to the user's public native account.
    ///
    /// Required accounts (exact order; see guest entrypoint):
    /// 0. `user_holding_in`   - user's private input holding (`PrivateOwned`).
    /// 1. `a_holding_in`      - A's input-token holding (public, pre-init).
    /// 2. `a_wlez_holding`    - A's WLEZ holding (public, pre-init for
    ///                          the WLEZ definition; receives AMM output).
    /// 3. `pool`              - AMM pool (public).
    /// 4. `vault_a`           - pool vault for token A.
    /// 5. `vault_b`           - pool vault for token B.
    /// 6. `wlez_definition`   - WLEZ token-definition PDA. Mutated by Unwrap.
    /// 7. `wlez_vault`        - WLEZ vault PDA. Mutated by Unwrap.
    /// 8. `user_native`       - user's public native LEZ account (recipient).
    /// 9. `clock`             - on-chain clock read-only (AMM oracle).
    PrivateSwapNativeOut {
        /// Input token to deshield + swap.
        swap_amount_in: u128,
        /// Minimum acceptable native LEZ output (slippage guard).
        min_amount_out: u128,
        /// Which pool token is the input. The OUTPUT side must be the
        /// WLEZ definition (caller assertion); we cannot infer direction
        /// from the WLEZ id alone if the user supplied an unrelated pool.
        token_definition_id_in: AccountId,
        /// Pool fee tier in bps (must match the targeted pool).
        fees: u128,
        /// Unix-ms timestamp after which the whole tx is invalid.
        deadline: u64,
    },
}
