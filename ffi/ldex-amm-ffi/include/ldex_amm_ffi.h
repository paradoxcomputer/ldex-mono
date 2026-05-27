/* C ABI for libldex_amm_ffi.so — the LDEX (fee-tier) AMM shim.
 *
 * Hand-written to match ffi/ldex-amm-ffi/src/{lib,submit}.rs exactly.
 * All `*_id` / `out*` params are 32-byte buffers (raw account/program id
 * bytes). `config_path`/`storage_path` are the wallet's
 * wallet_config.json / storage.json (the same files the LEZ wallet CLI
 * uses). All functions return one of the LDEX_AMM_* codes below.
 */
#ifndef LDEX_AMM_FFI_H
#define LDEX_AMM_FFI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Rust u128 over the C ABI == unsigned __int128 (x86-64, same toolchain). */
typedef unsigned __int128 ldex_u128;

enum {
  LDEX_AMM_OK = 0,
  LDEX_AMM_ERR_NULL = 1,    /* null/!=32-byte buffer */
  LDEX_AMM_ERR_WALLET = 2,  /* wallet open / runtime */
  LDEX_AMM_ERR_ACCOUNT = 3, /* on-chain read / token-holding decode */
  LDEX_AMM_ERR_KEY = 4,     /* signing key not in wallet */
  LDEX_AMM_ERR_SUBMIT = 5,  /* tx build / sequencer submit */
  LDEX_AMM_ERR_UTF8 = 6     /* bad string arg */
};

/* --- pure, fee-tier-aware PDA derivations (no I/O) --- */
int32_t ldex_amm_pool_id(const uint8_t *amm_program_id,
                         const uint8_t *token_a_def,
                         const uint8_t *token_b_def, ldex_u128 fees,
                         uint8_t *out);
int32_t ldex_amm_vault_id(const uint8_t *amm_program_id,
                          const uint8_t *pool_id, const uint8_t *token_def,
                          uint8_t *out);
int32_t ldex_amm_lp_definition_id(const uint8_t *amm_program_id,
                                  const uint8_t *pool_id, uint8_t *out);
int32_t ldex_amm_lp_lock_id(const uint8_t *amm_program_id,
                            const uint8_t *pool_id, uint8_t *out);

/* Parse "Public/<b58>" | "Private/<b58>" | "<b58>" | "<64hex>" -> 32 bytes */
int32_t ldex_amm_parse_account_id(const char *s, uint8_t *out);

/* RFP-004 Func #8 — Associated Token Accounts.
 * ldex_ata_id: deterministic ATA address for (owner, mint) =
 *   for_public_pda(ata_pid, sha256(owner || mint)). Pure, no I/O.
 * ldex_ata_create: submit the ATA program's idempotent Create (public
 *   tx) so the (owner,mint) holding exists before token interactions. */
int32_t ldex_ata_id(const uint8_t *ata_program_id, const uint8_t *owner,
                     const uint8_t *token_def, uint8_t *out);
int32_t ldex_ata_create(const char *config_path, const char *storage_path,
                        const uint8_t *ata_program_id, const uint8_t *owner,
                        const uint8_t *token_def, uint8_t *out_tx_hash);
/* RFP-004 Func #8 — `ata::Transfer` as a public tx. Drains `amount`
 * of the underlying token from `sender_ata` (PDA-owned by the ATA
 * program) into `recipient` (any initialised TokenHolding for the
 * same def). Owner signs; the ATA program internally chains
 * `token::Transfer` with the sender's PDA-seed. Primary use:
 * unwrap-from-ATA workflow (move WLEZ from `ATA(USER,WLEZ_DEF)` into
 * `HOLD_W` so WLEZ::Unwrap can authorise the burn). */
int32_t ldex_ata_transfer(const char *config_path, const char *storage_path,
                          const uint8_t *ata_program_id, const uint8_t *owner,
                          const uint8_t *sender_ata, const uint8_t *recipient,
                          ldex_u128 amount, uint8_t *out_tx_hash);

/* --- chain-state reads (JSON into `out`, NUL-terminated) --- */
/* {"exists":bool,"reserve_a":"..","reserve_b":"..","lp_supply":"..","fees":N} */
int32_t ldex_amm_pool_info(const char *config_path, const char *storage_path,
                           const uint8_t *amm_program_id,
                           const uint8_t *token_a_def,
                           const uint8_t *token_b_def, ldex_u128 fees,
                           uint8_t *out, size_t cap);
/* {"balance":"..","definition":"<hex>"} */
int32_t ldex_amm_token_balance(const char *config_path,
                               const char *storage_path,
                               const uint8_t *account_id, uint8_t *out,
                               size_t cap);

/* On-chain price history (design.md §5.11 layer ②). Pure read of the
 * price_indexer daemon's persisted CSV — no chain call, non-blocking.
 * Writes JSON [{"b":block,"t":unix_ms,"p":price_b_per_a}, ...]
 * (oldest->newest, <= max_points). Empty [] until the daemon has run. */
int32_t ldex_amm_price_history(const uint8_t *amm_program_id,
                               const uint8_t *token_a_def,
                               const uint8_t *token_b_def, ldex_u128 fees,
                               uint32_t max_points, uint8_t *out,
                               size_t cap);

/* Pool analytics (RFP Usability #3) — aggregate-only, no individual
 * positions. JSON {"tvlA","tvlB","volA","volB","feeRevA","feeRevB",
 * "samples","feeBps"}: TVL = latest on-chain reserves (exact); vol/fee
 * = reserve-delta approximation over the on-chain-sourced feed. */
int32_t ldex_amm_volume_estimate(const uint8_t *amm_program_id,
                                 const uint8_t *token_a_def,
                                 const uint8_t *token_b_def, ldex_u128 fees,
                                 uint8_t *out, size_t cap);

/* ON-CHAIN price history (design.md §5.11③) — reads the pool account's
 * on-chain observation ring directly; gapless by construction (every
 * swap/liquidity tx pushed an observation). JSON
 * [{"t":unix_ms,"p":price_b_per_a}, ...]. This is the source of truth;
 * the off-chain price_indexer is now only >ring-window archival. */
int32_t ldex_amm_onchain_price_history(const char *config_path,
                                       const char *storage_path,
                                       const uint8_t *amm_program_id,
                                       const uint8_t *token_a_def,
                                       const uint8_t *token_b_def,
                                       ldex_u128 fees, uint8_t *out,
                                       size_t cap);

/* --- signed-submit ops (open wallet, build, sign, submit) --- */
int32_t ldex_amm_new_pool(const char *config_path, const char *storage_path,
                          const uint8_t *amm_program_id,
                          const uint8_t *user_holding_a,
                          const uint8_t *user_holding_b,
                          const uint8_t *user_holding_lp, ldex_u128 amount_a,
                          ldex_u128 amount_b, ldex_u128 fees,
                          uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_swap_exact_in(const char *config_path,
                               const char *storage_path,
                               const uint8_t *amm_program_id,
                               const uint8_t *user_holding_a,
                               const uint8_t *user_holding_b,
                               const uint8_t *token_definition_in,
                               ldex_u128 swap_amount_in,
                               ldex_u128 min_amount_out, ldex_u128 fees,
                               uint64_t deadline, uint8_t *out_tx_hash);

/* RFP Func #8 — swap with the user side using Associated Token Accounts.
 * The owner authorises the spend (signer); the ATA program internally
 * PDA-authorises the sender ATA. The ATA addresses are deterministic from
 * (owner, definition); the FFI derives them via LDEX_ATA_PROGRAM_ID. */
int32_t ldex_amm_swap_exact_in_ata(const char *config_path,
                                   const char *storage_path,
                                   const uint8_t *amm_program_id,
                                   const uint8_t *owner,
                                   const uint8_t *token_def_a,
                                   const uint8_t *token_def_b,
                                   const uint8_t *token_definition_in,
                                   ldex_u128 swap_amount_in,
                                   ldex_u128 min_amount_out,
                                   ldex_u128 fees, uint64_t deadline,
                                   uint8_t *out_tx_hash);

/* RFP Func #8 — exact-output swap with the user side using ATAs. */
int32_t ldex_amm_swap_exact_out_ata(const char *config_path,
                                    const char *storage_path,
                                    const uint8_t *amm_program_id,
                                    const uint8_t *owner,
                                    const uint8_t *token_def_a,
                                    const uint8_t *token_def_b,
                                    const uint8_t *token_definition_in,
                                    ldex_u128 exact_amount_out,
                                    ldex_u128 max_amount_in,
                                    ldex_u128 fees, uint64_t deadline,
                                    uint8_t *out_tx_hash);

/* RFP Func #8 — add liquidity with the user side using ATAs. */
int32_t ldex_amm_add_liquidity_ata(const char *config_path,
                                   const char *storage_path,
                                   const uint8_t *amm_program_id,
                                   const uint8_t *owner,
                                   const uint8_t *token_def_a,
                                   const uint8_t *token_def_b,
                                   ldex_u128 min_amount_liquidity,
                                   ldex_u128 max_amount_to_add_token_a,
                                   ldex_u128 max_amount_to_add_token_b,
                                   ldex_u128 fees, uint64_t deadline,
                                   uint8_t *out_tx_hash);

/* RFP Func #8 — remove liquidity with the user side using ATAs.
 * Owner signs (provides outer-tx nonce); the AMM's RemoveLiquidity does
 * not require user-holding authorisation (vault transfers are PDA-auth,
 * LP burn is lp_def-PDA-auth) so no new on-chain instruction is needed. */
int32_t ldex_amm_remove_liquidity_ata(const char *config_path,
                                      const char *storage_path,
                                      const uint8_t *amm_program_id,
                                      const uint8_t *owner,
                                      const uint8_t *token_def_a,
                                      const uint8_t *token_def_b,
                                      ldex_u128 remove_liquidity_amount,
                                      ldex_u128 min_amount_to_remove_token_a,
                                      ldex_u128 min_amount_to_remove_token_b,
                                      ldex_u128 fees, uint64_t deadline,
                                      uint8_t *out_tx_hash);

/* Private (PrivateOwned) swap — design.md §5.10 "Private" mode. One
 * privacy-preserving tx over the deployed AMM; user holdings are
 * deshielded inside the proof circuit and re-shielded — no public
 * account ever appears on-chain. Same args as ldex_amm_swap_exact_in.
 * Proving runs in-process (risc0): set RISC0_DEV_MODE=1 for the dev
 * loop; real proofs need LOGOS_BLOCKCHAIN_CIRCUITS (~270 s). */
/* user_holding_a/_b are the user's PRIVATE (PrivateOwned) holding account
 * ids; token_def_a/_b are the pool's two token-definition ids (known from
 * bootstrap LDEX_DEF_A/B) — definitions are passed explicitly because
 * private holdings have no public state to read them from. */
int32_t ldex_amm_private_swap_exact_in(const char *config_path,
                                       const char *storage_path,
                                       const uint8_t *amm_program_id,
                                       const uint8_t *user_holding_a,
                                       const uint8_t *user_holding_b,
                                       const uint8_t *token_def_a,
                                       const uint8_t *token_def_b,
                                       const uint8_t *token_definition_in,
                                       ldex_u128 swap_amount_in,
                                       ldex_u128 min_amount_out,
                                       ldex_u128 fees, uint64_t deadline,
                                       uint8_t *out_tx_hash);

/* Private-Disposable swap — RFP-literal account-A model (design.md
 * §5.10 mode 2). Top program = deployed account-A router; AMM + token
 * are chained-call deps. User's private input holding deshielded into a
 * fresh single-use public account A, A swaps publicly, A's output
 * re-shielded to the user's private output holding — one proof. a_holding
 * _a/_b are two fresh public accounts the caller creates per op (never
 * reused). Proving in-process: RISC0_DEV_MODE=1 dev / ~270 s real. */
int32_t ldex_amm_disposable_swap_exact_in(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_program_id, const uint8_t *router_program_id,
    const uint8_t *user_holding_a, const uint8_t *user_holding_b,
    const uint8_t *a_holding_a, const uint8_t *a_holding_b,
    const uint8_t *token_def_a, const uint8_t *token_def_b,
    const uint8_t *token_definition_in, ldex_u128 swap_amount_in,
    ldex_u128 min_amount_out, ldex_u128 fees, uint64_t deadline,
    uint8_t *out_tx_hash);

/* Private add/remove liquidity (RFP Func #2: LP via deshield→interact→
 * re-shield from a private account). Same proven mechanism as
 * ldex_amm_private_swap_exact_in: holdings deshielded in-circuit, LP
 * minted/burned in the public pool, outputs re-shielded — no public
 * address on-chain. user_holding_a/b/lp are the user's PRIVATE holdings;
 * token_def_a/b are the pool's definition ids. Real proof (RISC0_DEV_
 * MODE=1 dev / minutes real). */
/* Initialize a fresh public account as a token holding for token_def
 * (public token::InitializeAccount; no proof). Used by Private-Disposable
 * to make the router's fresh account-A holdings valid before the AMM
 * validates them. */
int32_t ldex_amm_init_token_holding(const char *config_path,
                                    const char *storage_path,
                                    const uint8_t *token_def,
                                    const uint8_t *holding,
                                    uint8_t *out_tx_hash);

int32_t ldex_amm_private_add_liquidity(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_program_id, const uint8_t *user_holding_a,
    const uint8_t *user_holding_b, const uint8_t *user_holding_lp,
    const uint8_t *token_def_a, const uint8_t *token_def_b,
    ldex_u128 min_amount_liquidity, ldex_u128 max_amount_to_add_token_a,
    ldex_u128 max_amount_to_add_token_b, ldex_u128 fees,
    uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_private_remove_liquidity(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_program_id, const uint8_t *user_holding_a,
    const uint8_t *user_holding_b, const uint8_t *user_holding_lp,
    const uint8_t *token_def_a, const uint8_t *token_def_b,
    ldex_u128 remove_liquidity_amount,
    ldex_u128 min_amount_to_remove_token_a,
    ldex_u128 min_amount_to_remove_token_b, ldex_u128 fees,
    uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_swap_exact_out(const char *config_path,
                                const char *storage_path,
                                const uint8_t *amm_program_id,
                                const uint8_t *user_holding_a,
                                const uint8_t *user_holding_b,
                                const uint8_t *token_definition_in,
                                ldex_u128 exact_amount_out,
                                ldex_u128 max_amount_in, ldex_u128 fees,
                                uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_add_liquidity(const char *config_path,
                               const char *storage_path,
                               const uint8_t *amm_program_id,
                               const uint8_t *user_holding_a,
                               const uint8_t *user_holding_b,
                               const uint8_t *user_holding_lp,
                               ldex_u128 min_amount_liquidity,
                               ldex_u128 max_amount_to_add_token_a,
                               ldex_u128 max_amount_to_add_token_b,
                               ldex_u128 fees, uint64_t deadline,
                               uint8_t *out_tx_hash);

int32_t ldex_amm_remove_liquidity(const char *config_path,
                                  const char *storage_path,
                                  const uint8_t *amm_program_id,
                                  const uint8_t *user_holding_a,
                                  const uint8_t *user_holding_b,
                                  const uint8_t *user_holding_lp,
                                  ldex_u128 remove_liquidity_amount,
                                  ldex_u128 min_amount_to_remove_token_a,
                                  ldex_u128 min_amount_to_remove_token_b,
                                  ldex_u128 fees, uint64_t deadline,
                                  uint8_t *out_tx_hash);

/* WLEZ (wrapped native LEZ) — pure derivations + Initialize / Wrap /
 * Unwrap submit ops. Public txs (no privacy proof). See
 * `docs/wlez-design.md`. */

/* Pure: returns the WLEZ token definition account id derived from the
 * WLEZ program. No chain call. */
int32_t ldex_wlez_definition_id(const uint8_t *wlez_program_id,
                                uint8_t *out);
/* Pure: returns the WLEZ native-vault account id. No chain call. */
int32_t ldex_wlez_vault_id(const uint8_t *wlez_program_id, uint8_t *out);

/* One-shot setup: claims the vault PDA + creates the WLEZ token
 * definition. Idempotent — re-running this on a deployed-and-init'd
 * WLEZ is a no-op tx. `reference_token_def` is any existing
 * token-program-owned definition (e.g. a TOKENA def); WLEZ reads its
 * `program_owner` to find the token program. `payer_holding` is any
 * keypair-derived account in the wallet (used as the tx's signer for
 * fee payment; not read by the Initialize logic). */
int32_t ldex_wlez_initialize(const char *config_path,
                             const char *storage_path,
                             const uint8_t *wlez_program_id,
                             const uint8_t *reference_token_def,
                             const uint8_t *payer_holding,
                             uint8_t *out_tx_hash);

/* Lock `amount` native LEZ from `user_native_account` into the WLEZ
 * vault and mint `amount` WLEZ into `user_wlez_holding`. The user's
 * native account is the tx signer. `user_wlez_holding` must already
 * be initialized as a Fungible holding of the WLEZ definition (init
 * via `ldex_amm_init_token_holding` against the WLEZ definition id
 * beforehand if it isn't). */
int32_t ldex_wlez_wrap(const char *config_path, const char *storage_path,
                       const uint8_t *wlez_program_id,
                       const uint8_t *user_native_account,
                       const uint8_t *user_wlez_holding, ldex_u128 amount,
                       uint8_t *out_tx_hash);

/* Burn `amount` WLEZ from `user_wlez_holding` and release `amount`
 * native LEZ back to `user_native_account`. The WLEZ holding is the
 * signer. */
int32_t ldex_wlez_unwrap(const char *config_path,
                         const char *storage_path,
                         const uint8_t *wlez_program_id,
                         const uint8_t *user_wlez_holding,
                         const uint8_t *user_native_account,
                         ldex_u128 amount, uint8_t *out_tx_hash);

/* amm_v2 combined private-swap program — testnet-compatible mode-2
 * disposable swap. Replaces the (router + amm + 4× token::Transfer)
 * recursive tree with (amm_v2 + 4× token::Transfer). Saves 1 chained
 * call (~12-15M outer-STARK cycles, ~5-10% wall-clock reduction
 * minimum; measured ~64% in practice on first-run pools). Receipts
 * verify under upstream PRIVACY_PRESERVING_CIRCUIT_ID (no nssa
 * change). amm_v2 pools are amm_v2-owned (separate PDA space from the
 * canonical AMM) and have no on-chain TWAP oracle (drift-free).
 *
 * Setup: `ldex_amm_v2_new_pool` (public tx, no proof) creates an
 * amm_v2 pool. Then `ldex_amm_v2_disposable_swap` (privacy-preserving
 * tx, single STARK) runs a mode-2 swap against it. */

int32_t ldex_amm_v2_new_pool(const char *config_path,
                             const char *storage_path,
                             const uint8_t *amm_v2_program_id,
                             const uint8_t *user_holding_a,
                             const uint8_t *user_holding_b,
                             const uint8_t *user_holding_lp,
                             ldex_u128 amount_a, ldex_u128 amount_b,
                             ldex_u128 fees, uint64_t deadline,
                             uint8_t *out_tx_hash);

int32_t ldex_amm_v2_disposable_swap(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *user_holding_a, const uint8_t *user_holding_b,
    const uint8_t *a_holding_a, const uint8_t *a_holding_b,
    const uint8_t *token_def_a, const uint8_t *token_def_b,
    const uint8_t *token_definition_in, ldex_u128 swap_amount_in,
    ldex_u128 min_amount_out, ldex_u128 fees, uint64_t deadline,
    uint8_t *out_tx_hash);

/* amm_v2 add-liquidity / remove-liquidity (public txs, no proof). */
int32_t ldex_amm_v2_add_liquidity(const char *config_path,
                                  const char *storage_path,
                                  const uint8_t *amm_v2_program_id,
                                  const uint8_t *user_holding_a,
                                  const uint8_t *user_holding_b,
                                  const uint8_t *user_holding_lp,
                                  ldex_u128 min_amount_liquidity,
                                  ldex_u128 max_amount_to_add_token_a,
                                  ldex_u128 max_amount_to_add_token_b,
                                  ldex_u128 fees, uint64_t deadline,
                                  uint8_t *out_tx_hash);

int32_t ldex_amm_v2_remove_liquidity(const char *config_path,
                                     const char *storage_path,
                                     const uint8_t *amm_v2_program_id,
                                     const uint8_t *user_holding_a,
                                     const uint8_t *user_holding_b,
                                     const uint8_t *user_holding_lp,
                                     ldex_u128 remove_liquidity_amount,
                                     ldex_u128 min_amount_to_remove_token_a,
                                     ldex_u128 min_amount_to_remove_token_b,
                                     ldex_u128 fees, uint64_t deadline,
                                     uint8_t *out_tx_hash);

/* amm_v2 mode-0 public swap (no proof). */
int32_t ldex_amm_v2_swap_exact_in(const char *config_path,
                                  const char *storage_path,
                                  const uint8_t *amm_v2_program_id,
                                  const uint8_t *user_holding_a,
                                  const uint8_t *user_holding_b,
                                  const uint8_t *token_definition_in,
                                  ldex_u128 swap_amount_in,
                                  ldex_u128 min_amount_out, ldex_u128 fees,
                                  uint64_t deadline, uint8_t *out_tx_hash);

/* amm_v2 mode-1 PRIVATE PrivateOwned swap (privacy tx). */
int32_t ldex_amm_v2_private_swap_exact_in(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *user_holding_a, const uint8_t *user_holding_b,
    const uint8_t *token_def_a, const uint8_t *token_def_b,
    const uint8_t *token_definition_in, ldex_u128 swap_amount_in,
    ldex_u128 min_amount_out, ldex_u128 fees, uint64_t deadline,
    uint8_t *out_tx_hash);

/* amm_v2 mode-2 disposable native-in (LEZ → token, combined into amm_v2). */
int32_t ldex_amm_v2_disposable_swap_native_in(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id, const uint8_t *wlez_program_id,
    const uint8_t *user_native, const uint8_t *wlez_vault,
    const uint8_t *wlez_definition, const uint8_t *a_wlez_holding,
    const uint8_t *a_holding_out, const uint8_t *token_def_out,
    const uint8_t *user_holding_out,
    ldex_u128 swap_amount_in, ldex_u128 min_amount_out,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

/* amm_v2 mode-2 disposable native-out (token → LEZ, combined into amm_v2). */
int32_t ldex_amm_v2_disposable_swap_native_out(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id, const uint8_t *wlez_program_id,
    const uint8_t *user_holding_in, const uint8_t *a_holding_in,
    const uint8_t *a_wlez_holding, const uint8_t *wlez_definition,
    const uint8_t *wlez_vault, const uint8_t *user_native,
    const uint8_t *token_def_in, ldex_u128 swap_amount_in,
    ldex_u128 min_amount_out, ldex_u128 fees, uint64_t deadline,
    uint8_t *out_tx_hash);

/* amm_v2 ATA-side mode-0 ops (RFP Func #8). The ATA program id comes
 * from LDEX_ATA_PROGRAM_ID in process env; the FFI derives both
 * trader ATAs deterministically from (owner, token_def) via the ATA
 * program's PDA scheme. Owner signs; the chained ata::Transfer
 * PDA-authorises the spend from sender ATA. */
int32_t ldex_amm_v2_swap_exact_in_ata(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *owner, const uint8_t *token_def_a,
    const uint8_t *token_def_b, const uint8_t *token_definition_in,
    ldex_u128 swap_amount_in, ldex_u128 min_amount_out,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_v2_swap_exact_out_ata(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *owner, const uint8_t *token_def_a,
    const uint8_t *token_def_b, const uint8_t *token_definition_in,
    ldex_u128 exact_amount_out, ldex_u128 max_amount_in,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

int32_t ldex_amm_v2_add_liquidity_ata(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *owner, const uint8_t *token_def_a,
    const uint8_t *token_def_b,
    ldex_u128 min_amount_liquidity,
    ldex_u128 max_amount_to_add_token_a,
    ldex_u128 max_amount_to_add_token_b,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

/* RFP-004 Func #8 (LP side) — pool create whose initial user LP is
 * minted into `ATA(owner, lp_def)`. Token deposits drain from the
 * user's keypair `user_holding_a/b` via canonical `token::Transfer`
 * (the new vaults start default and only the token program's PDA-
 * claim path lawfully initialises them). The user's LP ATA is
 * initialised in-tx by a chained `ata::Create` after the LP
 * definition is created. */
int32_t ldex_amm_v2_new_pool_ata(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *owner,
    const uint8_t *user_holding_a, const uint8_t *user_holding_b,
    ldex_u128 amount_a, ldex_u128 amount_b,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

/* RFP-004 Func #8 — remove-liquidity with user-side ATAs. Burns LP from
 * `ata(owner, lp_def)` and returns underlying into `(ata_a, ata_b)`. */
int32_t ldex_amm_v2_remove_liquidity_ata(
    const char *config_path, const char *storage_path,
    const uint8_t *amm_v2_program_id,
    const uint8_t *owner, const uint8_t *token_def_a,
    const uint8_t *token_def_b,
    ldex_u128 remove_liquidity_amount,
    ldex_u128 min_amount_to_remove_token_a,
    ldex_u128 min_amount_to_remove_token_b,
    ldex_u128 fees, uint64_t deadline, uint8_t *out_tx_hash);

#ifdef __cplusplus
}
#endif

#endif /* LDEX_AMM_FFI_H */
