#ifndef LDEX_CORE_INTERFACE_H
#define LDEX_CORE_INTERFACE_H

#include <QObject>
#include <QString>
#include "interface.h"

/**
 * @brief Public API of the LDEX core module.
 *
 * Methods declared Q_INVOKABLE here are callable from the QML UI App via
 * logos.callModule("ldex_core", "<method>", [args]).
 *
 * Walking-skeleton surface only. Real surface (quote/swap/addLiquidity/...,
 * the deshield->swap->reshield prover path via wallet-ffi) lands in later
 * tasks; this proves the QML <-> native-module IPC seam first.
 */
class LdexCoreInterface : public PluginInterface
{
public:
    virtual ~LdexCoreInterface() = default;

    /** Liveness probe: echoes back, proving the UI->module call path. */
    Q_INVOKABLE virtual QString ping(const QString& msg) = 0;

    /** Module status string. */
    Q_INVOKABLE virtual QString getStatus() = 0;

    /**
     * Offline wallet-ffi linkage probe: drives the real LEZ wallet (Rust)
     * through the C ABI - create_new -> create_account_public -> destroy -
     * with no sequencer. Returns the new public account id (hex) or an error.
     */
    Q_INVOKABLE virtual QString walletProbe() = 0;

    /**
     * Real chain read: opens a wallet pointed at the local standalone
     * sequencer (127.0.0.1:3040) and returns its current block height.
     * Proves the full UI -> native -> wallet-ffi -> chain path.
     */
    Q_INVOKABLE virtual QString chainHeight() = 0;

    /**
     * Fee-tier-aware AMM pool PDA for (token_a, token_b, fee_bps), via the
     * ldex_amm_ffi shim. Pure (no wallet/sequencer) - validates the AMM
     * shim links into this module and that fee-tier PDAs differ per tier.
     * All ids are 64-hex (32 bytes). Returns the pool id hex or an error.
     */
    Q_INVOKABLE virtual QString ammPoolId(const QString& ammHex,
                                          const QString& tokenAHex,
                                          const QString& tokenBHex,
                                          int feeBps) = 0;

    /** One-click dev setup: returns the contents of scripts/bootstrap.env
     *  (KEY=VALUE lines) so the UI can auto-fill - the user pastes nothing.
     *  Native file read (the core module is not sandboxed; QML is). */
    Q_INVOKABLE virtual QString devBootstrap() = 0;

    /** All wallet holdings: JSON array [{address,definition,balance,name}]
     *  (name = TOKENA/TOKENB for known defs, else short def). */
    Q_INVOKABLE virtual QString walletTokens() = 0;
    /** Wallet accounts: JSON array [{address,public}] (base58 addresses). */
    Q_INVOKABLE virtual QString accounts() = 0;
    /** Pool state for an arbitrary pair+tier (per-pool view).
     *  JSON {"exists":bool,"reserve_a":"..","reserve_b":"..",
     *  "lp_supply":"..","fees":N}. ids accept Public/<b58>|b58|hex. */
    Q_INVOKABLE virtual QString poolInfoFor(const QString& defAHex,
                                            const QString& defBHex,
                                            int feeBps) = 0;
    /** On-chain price history (design.md §5.11). JSON array
     *  [{"b":block,"t":unix_ms,"p":priceBperA}, ...] from the persisted
     *  price_indexer feed. Empty [] until the indexer has sampled. */
    Q_INVOKABLE virtual QString priceHistory(const QString& defAHex,
                                             const QString& defBHex,
                                             int feeBps) = 0;
    /** JSON array of pools for the dev token pair across fee tiers:
     *  [{"fee":N,"exists":bool,"reserve_a":"..","reserve_b":"..",
     *    "lp_supply":".."}, ...]. */
    Q_INVOKABLE virtual QString pools() = 0;
    /** Token-agnostic counterparts of the env-pair methods - take
     *  explicit token-definition / holding hex ids so the mini-app can
     *  trade against any pool that exists on chain, not just the
     *  bootstrap TOKENA/TOKENB pair. All hex args are 64 hex chars
     *  (32 bytes) or "Public/<b58>". `direction`: 0 = defA→defB,
     *  1 = defB→defA. Returns the same JSON shape as the env-pair
     *  variants. */
    Q_INVOKABLE virtual QString quoteFor(const QString& defAHex,
                                         const QString& defBHex,
                                         int direction,
                                         const QString& amountIn,
                                         int feeBps) = 0;
    /** Token-agnostic public swap, ATA-only (RFP Func #8). `config` packs
     *  the call identity as "<defA>|<defB>|<defIn>" (the pay side's def);
     *  the FFI derives `ATA(owner, def_*)` from the env-bound owner.
     *  Compressed to ≤5 args because the SDK QtProviderObject dispatch
     *  caps callModule arity at 5 (cpp-sdk qt_provider_object.cpp). */
    Q_INVOKABLE virtual QString swapExactInAtaFor(const QString& config,
                                                  const QString& amountIn,
                                                  const QString& minOut,
                                                  int feeBps) = 0;
    /** Token-agnostic pool create. Args are the user's KEYPAIR token
     *  HOLDINGS (the deposits drain via canonical `token::Transfer`);
     *  the user's initial LP is minted into `ATA(owner, lp_def)`. */
    Q_INVOKABLE virtual QString createPoolFor(const QString& holdingAHex,
                                              const QString& holdingBHex,
                                              const QString& amountA,
                                              const QString& amountB,
                                              int feeBps) = 0;
    /** Token-agnostic private swap. Same modes as `privateSwap` (0=Public,
     *  1=Private/PrivateOwned, 2=Private-Disposable router, 3=Fast),
     *  but caller passes the pool's def ids AND the user's PrivateOwned
     *  source/destination holdings for those two tokens (the wallet's
     *  LDEX_PRIV_<L> for each token, shielded by bootstrap). Direction:
     *  0 = pay defA→receive defB, 1 = pay defB→receive defA. `config`
     *  packs all of "<mode>|<direction>|<defA>|<defB>|<privA>|<privB>"
     *  into one string to fit under the SDK's 5-arg callModule cap. */
    Q_INVOKABLE virtual QString privateSwapFor(const QString& config,
                                               const QString& amountIn,
                                               const QString& minOut,
                                               int feeBps) = 0;
    /** Non-blocking variant of `privateSwapFor`: spawns a background thread
     *  that runs the proof, returns `"job=<N>"` within milliseconds. Required
     *  because the Logos SDK bridge has a hardcoded 20-second QtRO timeout
     *  (`Timeout::Timeout(int=20000)` in logos_mode.h) that cannot be
     *  overridden from QML; STARK proof generation legitimately takes
     *  minutes. Poll `jobStatus(N)` until it returns something other than
     *  `"pending"`. */
    Q_INVOKABLE virtual QString privateSwapForStart(const QString& config,
                                                    const QString& amountIn,
                                                    const QString& minOut,
                                                    int feeBps) = 0;
    /** Returns the current state of a job started via *Start. While the
     *  background thread is running this returns `"pending"`; when done
     *  it returns the same payload `privateSwapFor` would have returned
     *  synchronously (`"tx=0x…"` or a friendly error string). */
    Q_INVOKABLE virtual QString jobStatus(int jobId) = 0;

    /** Batched native-LEZ private swap. One privacy proof that chains
     *  either WLEZ::Wrap → AMM::Swap → reshield (NativeIn) or deshield
     *  → AMM::Swap → WLEZ::Unwrap (NativeOut), removing one block wait
     *  and one tx round-trip vs. the two-tx wrap-then-swap flow, plus
     *  giving wrap+swap atomicity. Always router-mode (mode 2) - the
     *  routerless `PrivateOwned` path has no batched variant because it
     *  operates from user-owned private holdings directly. `config`
     *  packs `"<direction>|<token_def>|<priv_holding>"` into one string:
     *    direction = 0 → NativeIn  (LEZ → token_def; priv_holding is the
     *                                  user's private holding to receive)
     *    direction = 1 → NativeOut (token_def → LEZ; priv_holding is the
     *                                  user's private holding to spend)
     *  WLEZ definition / vault / program id / user-native account all
     *  come from `m_env`. Returns the same shape as `privateSwapFor`. */
    Q_INVOKABLE virtual QString privateSwapNativeFor(const QString& config,
                                                     const QString& amountIn,
                                                     const QString& minOut,
                                                     int feeBps) = 0;
    /** Non-blocking variant of `privateSwapNativeFor`. Same job-pump as
     *  `privateSwapForStart` - returns `"job=<N>"`, poll `jobStatus`. */
    Q_INVOKABLE virtual QString privateSwapNativeForStart(const QString& config,
                                                          const QString& amountIn,
                                                          const QString& minOut,
                                                          int feeBps) = 0;

    // ── Native LEZ via WLEZ (wrapped-native bridge) ────────────────
    /** Current native-LEZ balance of `LDEX_USER_OWNER` as a decimal
     *  string. Always returns a string (returns `"0"` if env isn't
     *  loaded). The UI surfaces this as the "LEZ" balance in the
     *  catalog - under the hood it's the user's native account
     *  balance, separate from any WLEZ token holdings. */
    Q_INVOKABLE virtual QString nativeBalance() = 0;
    /** Lock `amount` native LEZ in the WLEZ vault and mint `amount`
     *  WLEZ into `LDEX_HOLD_W` (the bootstrap-created keypair holding
     *  for the WLEZ token definition). Returns "Wrap submitted.
     *  tx=0x<hash>" or a friendly rcMessage. */
    Q_INVOKABLE virtual QString wrapNative(const QString& amount) = 0;
    /** Burn `amount` WLEZ from `LDEX_HOLD_W` and release `amount`
     *  native LEZ back to `LDEX_USER_OWNER`. */
    Q_INVOKABLE virtual QString unwrapNative(const QString& amount) = 0;
    /** Move `amount` WLEZ from `ATA(USER, WLEZ_DEF)` into the keypair
     *  `HOLD_W`. Needed because `WLEZ::Unwrap` requires
     *  `user_holding.is_authorized` - only satisfiable by a keypair
     *  account the wallet can sign for (ATAs are PDA-owned). */
    Q_INVOKABLE virtual QString consolidateWlezToHoldW(const QString& amount) = 0;
    /** Manual shield: move `amount` of TOKEN<letter> from the user's
     *  ATA(USER, DEF_<letter>) into the wallet-owned PrivateOwned account
     *  PRIV_<letter>. Backed by `wallet_ffi_transfer_shielded_owned`.
     *  Returns "Shielded ... tx=0x<hash>" or a friendly error. */
    Q_INVOKABLE virtual QString shieldToken(const QString& letter,
                                            const QString& amount) = 0;
    /** Manual deshield: move `amount` of TOKEN<letter> from PRIV_<letter>
     *  back to ATA(USER, DEF_<letter>). Backed by
     *  `wallet_ffi_transfer_deshielded`. */
    Q_INVOKABLE virtual QString deshieldToken(const QString& letter,
                                              const QString& amount) = 0;
    /** Scan blocks since the wallet's last_synced and update local
     *  cached private balances. Returns the new head block id on
     *  success or an error string. Used to be inlined in
     *  `walletTokens()` but that fired on every render; the UI now
     *  calls this on a throttled timer + after each action. */
    Q_INVOKABLE virtual QString syncPrivateBalances() = 0;

    /** RFP Usability #3 - pool analytics, AGGREGATE ONLY (no individual
     *  positions). JSON {"pools":[{"fee":N,"exists":bool,"tvlA","tvlB",
     *  "volA","volB","feeRevA","feeRevB","samples"}, ...],
     *  "agg":{"tvlA","tvlB","volA","volB","feeRevA","feeRevB",
     *  "activePools"}}. TVL = exact on-chain reserves; vol/fee = approx
     *  reserve-delta over the on-chain-sourced feed. Now consumed only
     *  by the Pools list sort/enrichment - the per-pool detail view
     *  reads each pool's exact stats from `poolInfoFor` directly. */
    Q_INVOKABLE virtual QString analytics() = 0;

    // --- Wallet onboarding (design.md §5.9: A create / C import) ---
    /** A: create a new LEZ wallet (fresh mnemonic) at homeDir, sequencer
     *  at sequencerUrl. Persistent. Returns status/error. */
    Q_INVOKABLE virtual QString walletCreate(const QString& homeDir,
                                             const QString& password,
                                             const QString& sequencerUrl) = 0;
    /** C: import an existing LEZ wallet from a seed phrase. */
    Q_INVOKABLE virtual QString walletImport(const QString& homeDir,
                                             const QString& mnemonic,
                                             const QString& password,
                                             const QString& sequencerUrl) = 0;

    // --- Signed AMM ops ---
    // Wallet/AMM/holding ids come from the cached dev env (devBootstrap or
    // lazy-loaded from scripts/bootstrap.env). The token-agnostic
    // variants (`*For`) are what the UI dispatches against any pair -
    // the fixed env-pair-only entry points (`createPool` / `swapExactIn`
    // / `swapExactInAta` / `privateSwap` / `quote`) have been retired;
    // use `createPoolFor` / `swapExactInAtaFor` / `privateSwapFor` /
    // `quoteFor` instead.
    /** Private liquidity (RFP Func #2). mode: 0 = Public (delegates to
     *  addLiquidity/removeLiquidity), 1 = Private (PrivateOwned -
     *  holdings deshielded in-circuit, LP minted/burned in the public
     *  pool, outputs re-shielded; LP position public, owner untraceable).
     *  Same proven mechanism as privateSwap mode 1. */
    Q_INVOKABLE virtual QString privateAddLiquidity(int mode,
                                                    const QString& minLp,
                                                    const QString& maxA,
                                                    const QString& maxB,
                                                    int feeBps) = 0;
    Q_INVOKABLE virtual QString privateRemoveLiquidity(int mode,
                                                       const QString& lpAmount,
                                                       const QString& minA,
                                                       const QString& minB,
                                                       int feeBps) = 0;
    Q_INVOKABLE virtual QString addLiquidity(const QString& minLp,
                                             const QString& maxA,
                                             const QString& maxB,
                                             int feeBps) = 0;
    Q_INVOKABLE virtual QString removeLiquidity(const QString& lpAmount,
                                                const QString& minA,
                                                const QString& minB,
                                                int feeBps) = 0;
};

#define LdexCoreInterface_iid "org.logos.LdexCoreInterface"
Q_DECLARE_INTERFACE(LdexCoreInterface, LdexCoreInterface_iid)

#endif // LDEX_CORE_INTERFACE_H
