#include "ldex_core_plugin.h"
#include "logos_api.h"
#include "logos_api_client.h"
#include <QDebug>
#include <QTemporaryDir>
#include <QFile>
#include <QFileInfo>
#include <QDir>
#include <QJsonObject>
#include <QJsonDocument>
#include <QRegularExpression>
#include <QMutex>
#include <QMutexLocker>
#include <QHash>
#include <thread>
#include <functional>
#include <utility>

extern "C" {
#include "wallet_ffi.h"
}
#include "ldex_amm_ffi.h"  // already extern "C"-guarded

LdexCorePlugin::LdexCorePlugin(QObject* parent)
    : QObject(parent)
{
    qDebug() << "LdexCorePlugin: constructor";
}

LdexCorePlugin::~LdexCorePlugin()
{
    qDebug() << "LdexCorePlugin: destructor";
}

void LdexCorePlugin::initLogos(LogosAPI* logosAPIInstance)
{
    if (logos) {
        delete logos;
        logos = nullptr;
    }
    if (logosAPI) {
        delete logosAPI;
        logosAPI = nullptr;
    }
    logosAPI = logosAPIInstance;
    if (logosAPI) {
        logos = new LogosModules(logosAPI);
    }
    m_initialized = (logosAPI != nullptr);
    qDebug() << "LdexCorePlugin: initLogos, initialized=" << m_initialized;
}

QString LdexCorePlugin::ping(const QString& msg)
{
    qDebug() << "LdexCorePlugin: ping" << msg;
    emit eventResponse("pinged", QVariantList() << msg);
    return QString("ldex_core pong: %1").arg(msg);
}

QString LdexCorePlugin::getStatus()
{
    return m_initialized
        ? QStringLiteral("ldex_core: running and initialized")
        : QStringLiteral("ldex_core: loaded, not yet initialized");
}

QString LdexCorePlugin::walletProbe()
{
    QTemporaryDir tmp;
    if (!tmp.isValid())
        return QStringLiteral("walletProbe: could not create temp dir");

    const QString cfgPath = tmp.filePath("wallet_config.json");
    const QString storePath = tmp.filePath("storage");
    QDir().mkpath(storePath);

    // Minimal wallet config (schema mirrors wallet/configs/debug). No
    // sequencer is contacted for create + local keygen.
    const QByteArray cfg =
        "{\n"
        "  \"sequencer_addr\": \"http://127.0.0.1:3040\",\n"
        "  \"seq_poll_timeout\": \"30s\",\n"
        "  \"seq_tx_poll_max_blocks\": 15,\n"
        "  \"seq_poll_max_retries\": 10,\n"
        "  \"seq_block_poll_max_amount\": 100,\n"
        "  \"initial_accounts\": []\n"
        "}\n";
    {
        QFile f(cfgPath);
        if (!f.open(QIODevice::WriteOnly) || f.write(cfg) != cfg.size())
            return QStringLiteral("walletProbe: could not write config");
    }

    const QByteArray cfgC = cfgPath.toUtf8();
    const QByteArray storeC = storePath.toUtf8();

    WalletHandle* h = wallet_ffi_create_new(
        cfgC.constData(), storeC.constData(), "ldex-probe-password");
    if (!h)
        return QStringLiteral("walletProbe: wallet_ffi_create_new returned null");

    FfiBytes32 id;
    const WalletFfiError err = wallet_ffi_create_account_public(h, &id);

    QString out;
    if (err == SUCCESS) {
        const QByteArray hex =
            QByteArray(reinterpret_cast<const char*>(id.data), 32).toHex();
        out = QStringLiteral("ldex_core wallet-ffi OK - new public account 0x%1")
                  .arg(QString::fromLatin1(hex));
    } else {
        out = QStringLiteral("walletProbe: create_account_public failed "
                             "(WalletFfiError=%1)").arg(static_cast<int>(err));
    }

    wallet_ffi_destroy(h);
    return out;
}

QString LdexCorePlugin::chainHeight()
{
    QTemporaryDir tmp;
    if (!tmp.isValid())
        return QStringLiteral("chainHeight: could not create temp dir");

    const QString cfgPath = tmp.filePath("wallet_config.json");
    const QString storePath = tmp.filePath("storage");
    QDir().mkpath(storePath);

    const QByteArray cfg =
        "{\n"
        "  \"sequencer_addr\": \"http://127.0.0.1:3040\",\n"
        "  \"seq_poll_timeout\": \"30s\",\n"
        "  \"seq_tx_poll_max_blocks\": 15,\n"
        "  \"seq_poll_max_retries\": 10,\n"
        "  \"seq_block_poll_max_amount\": 100,\n"
        "  \"initial_accounts\": []\n"
        "}\n";
    {
        QFile f(cfgPath);
        if (!f.open(QIODevice::WriteOnly) || f.write(cfg) != cfg.size())
            return QStringLiteral("chainHeight: could not write config");
    }

    const QByteArray cfgC = cfgPath.toUtf8();
    const QByteArray storeC = storePath.toUtf8();

    WalletHandle* h = wallet_ffi_create_new(
        cfgC.constData(), storeC.constData(), "ldex-probe-password");
    if (!h)
        return QStringLiteral("chainHeight: wallet_ffi_create_new returned null");

    uint64_t height = 0;
    const WalletFfiError err = wallet_ffi_get_current_block_height(h, &height);

    QString out;
    if (err == SUCCESS) {
        out = QStringLiteral("ldex_core chain OK - sequencer block height = %1")
                  .arg(static_cast<qulonglong>(height));
    } else {
        out = QStringLiteral("chainHeight: wallet_ffi_get_current_block_height "
                             "failed (WalletFfiError=%1) - is the local "
                             "sequencer running on 127.0.0.1:3040?")
                  .arg(static_cast<int>(err));
    }

    wallet_ffi_destroy(h);
    return out;
}

QString LdexCorePlugin::ammPoolId(const QString& ammHex,
                                  const QString& tokenAHex,
                                  const QString& tokenBHex,
                                  int feeBps)
{
    auto dec = [](const QString& s, QByteArray& out) -> bool {
        const QByteArray cs = s.toUtf8();
        unsigned char tmp[32];
        if (ldex_amm_parse_account_id(cs.constData(), tmp) != LDEX_AMM_OK)
            return false;
        out = QByteArray(reinterpret_cast<const char*>(tmp), 32);
        return true;
    };
    QByteArray amm, ta, tb;
    if (!dec(ammHex, amm) || !dec(tokenAHex, ta) || !dec(tokenBHex, tb))
        return QStringLiteral("ammPoolId: each id must be 64 hex chars (32 bytes)");

    unsigned char poolOut[32];
    const ldex_u128 fees = static_cast<ldex_u128>(static_cast<unsigned long long>(feeBps));
    const int rc = ldex_amm_pool_id(
        reinterpret_cast<const uint8_t*>(amm.constData()),
        reinterpret_cast<const uint8_t*>(ta.constData()),
        reinterpret_cast<const uint8_t*>(tb.constData()),
        fees, poolOut);
    if (rc != LDEX_AMM_OK)
        return QStringLiteral("ammPoolId: ldex_amm_ffi rc=%1").arg(rc);

    const QString poolHex = QString::fromLatin1(
        QByteArray(reinterpret_cast<const char*>(poolOut), 32).toHex());
    return QStringLiteral("ldex_core AMM pool id (fee %1 bps): 0x%2")
        .arg(feeBps)
        .arg(poolHex);
}

// ---- helpers for the signed AMM ops ----
namespace {
// Accept Public/<b58> | Private/<b58> | <b58> | <64hex> (shim parser).
bool hex32(const QString& s, QByteArray& out) {
    const QByteArray cs = s.toUtf8();
    unsigned char tmp[32];
    if (ldex_amm_parse_account_id(cs.constData(), tmp) != LDEX_AMM_OK)
        return false;
    out = QByteArray(reinterpret_cast<const char*>(tmp), 32);
    return true;
}
bool parseU128(const QString& s, ldex_u128& out) {
    const QByteArray b = s.trimmed().toLatin1();
    if (b.isEmpty()) return false;
    out = 0;
    for (char c : b) {
        if (c < '0' || c > '9') return false;
        out = out * 10 + static_cast<ldex_u128>(c - '0');
    }
    return true;
}
uint64_t parseDeadline(const QString& s) {
    bool ok = false;
    const qulonglong v = s.trimmed().toULongLong(&ok);
    return (!ok || v == 0) ? UINT64_MAX : static_cast<uint64_t>(v);
}
QString hashHex(const unsigned char* h) {
    return QString::fromLatin1(
        QByteArray(reinterpret_cast<const char*>(h), 32).toHex());
}
// RFP Usability #5 - map FFI return codes to user-friendly, actionable
// prose. `op` is a short verb the user recognises ("swap", "create the
// pool", etc.). Stable for the UI; the technical code is appended for
// support diagnostics.
QString rcMessage(const char* op, int rc) {
    QString hint;
    switch (rc) {
    case LDEX_AMM_ERR_NULL:
        hint = QStringLiteral("a required value was missing - please retry");
        break;
    case LDEX_AMM_ERR_WALLET:
        hint = QStringLiteral("couldn't reach your LEZ wallet - check that "
                              "the sequencer is running and the bootstrap "
                              "config is loaded");
        break;
    case LDEX_AMM_ERR_ACCOUNT:
        hint = QStringLiteral("a required account is missing or "
                              "uninitialized (e.g. the pool hasn't been "
                              "created yet, or a token holding wasn't "
                              "initialized) - try creating the pool, or "
                              "wait one block and retry");
        break;
    case LDEX_AMM_ERR_KEY:
        hint = QStringLiteral("your wallet doesn't have the signing key "
                              "for one of the accounts - make sure you're "
                              "the owner");
        break;
    case LDEX_AMM_ERR_SUBMIT:
        hint = QStringLiteral("the transaction was rejected. Common causes: "
                              "insufficient balance, slippage above your "
                              "tolerance, or a stale snapshot - wait a "
                              "block and retry, or lower the amount");
        break;
    case LDEX_AMM_ERR_UTF8:
        hint = QStringLiteral("invalid text in a parameter");
        break;
    default:
        hint = QStringLiteral("unrecognised internal error");
        break;
    }
    return QStringLiteral("Couldn't %1: %2.  (error code %3)")
        .arg(QString::fromLatin1(op), hint).arg(rc);
}
QByteArray walletConfigJson(const QString& sequencerUrl) {
    return QStringLiteral(
        "{\n"
        "  \"sequencer_addr\": \"%1\",\n"
        "  \"seq_poll_timeout\": \"30s\",\n"
        "  \"seq_tx_poll_max_blocks\": 15,\n"
        "  \"seq_poll_max_retries\": 10,\n"
        "  \"seq_block_poll_max_amount\": 100,\n"
        "  \"initial_accounts\": []\n"
        "}\n").arg(sequencerUrl).toUtf8();
}
}  // namespace

QString LdexCorePlugin::walletCreate(const QString& homeDir,
                                     const QString& password,
                                     const QString& sequencerUrl)
{
    if (!QDir().mkpath(homeDir))
        return QStringLiteral("walletCreate: cannot create %1").arg(homeDir);
    const QString cfg = homeDir + "/wallet_config.json";
    const QString store = homeDir + "/storage.json";
    {
        QFile f(cfg);
        const QByteArray j = walletConfigJson(sequencerUrl);
        if (!f.open(QIODevice::WriteOnly) || f.write(j) != j.size())
            return QStringLiteral("walletCreate: cannot write %1").arg(cfg);
    }
    const QByteArray cfgC = cfg.toUtf8();
    const QByteArray storeC = store.toUtf8();
    const QByteArray pwC = password.toUtf8();
    WalletHandle* h = wallet_ffi_create_new(
        cfgC.constData(), storeC.constData(), pwC.constData());
    if (!h)
        return QStringLiteral("walletCreate: wallet_ffi_create_new failed");
    wallet_ffi_destroy(h);
    return QStringLiteral("Wallet created. config=%1 storage=%2 "
                          "(note: recovery phrase is printed by the wallet "
                          "engine; mnemonic is not exposed via this FFI)")
        .arg(cfg, store);
}

QString LdexCorePlugin::walletImport(const QString& /*homeDir*/,
                                     const QString& /*mnemonic*/,
                                     const QString& /*password*/,
                                     const QString& /*sequencerUrl*/)
{
    // C (seed-phrase restore) needs a mnemonic-restore FFI not present in
    // the vendored wallet-ffi (only create_new/open/register_*). Tracked in
    // design.md §5.9 / task ④. For now use walletCreate, or point ops at the
    // bootstrap wallet (scripts/bootstrap.env).
    return QStringLiteral(
        "walletImport: seed-phrase restore not yet wired - requires a "
        "wallet_ffi_restore FFI (design.md §5.9). Use 'Create wallet' or "
        "the bootstrap wallet for now.");
}

// Resolve the bootstrap env file. Lookup order:
//   1. $LDEX_BOOTSTRAP_ENV (explicit override)
//   2. $LDEX_REPO/scripts/bootstrap.env (set by run-miniapp.sh)
//   3. ./scripts/bootstrap.env (cwd-relative)
//   4. ../scripts/bootstrap.env, ../../scripts/bootstrap.env (walk up
//      from a plausible cwd like mini-app/ui or cli/)
// No hardcoded user/install paths - public clones must work in any
// directory layout.
static QString envFilePath()
{
    auto check = [](const QString& p) -> bool {
        return !p.isEmpty() && QFileInfo::exists(p);
    };
    QString p = qEnvironmentVariable("LDEX_BOOTSTRAP_ENV");
    if (!p.isEmpty()) return p;  // explicit override wins
    QString repo = qEnvironmentVariable("LDEX_REPO");
    if (!repo.isEmpty()) {
        const QString cand = repo + QStringLiteral("/scripts/bootstrap.env");
        if (check(cand)) return cand;
    }
    for (const auto& rel : {"./scripts/bootstrap.env",
                            "../scripts/bootstrap.env",
                            "../../scripts/bootstrap.env"}) {
        if (check(QString::fromLatin1(rel))) return QString::fromLatin1(rel);
    }
    return QStringLiteral("./scripts/bootstrap.env");  // default for error msg
}

bool LdexCorePlugin::ensureEnv()
{
    if (!m_env.isEmpty())
        return m_env.contains("LDEX_AMM_PROGRAM_ID")
            && m_env.contains("LDEX_USER_HOLDING_A");
    QFile f(envFilePath());
    if (!f.open(QIODevice::ReadOnly | QIODevice::Text))
        return false;
    const QString text = QString::fromUtf8(f.readAll());
    QRegularExpression re(
        QStringLiteral("^(?:export\\s+)?([A-Za-z_][A-Za-z0-9_]*)=\"?(.*?)\"?$"));
    const auto lines = text.split('\n');
    for (const QString& ln : lines) {
        const auto m = re.match(ln.trimmed());
        if (m.hasMatch())
            m_env.insert(m.captured(1), m.captured(2));
    }
    return m_env.contains("LDEX_AMM_PROGRAM_ID")
        && m_env.contains("LDEX_USER_HOLDING_A");
}

// Shared context for the signed ops, decoded from the cached env.
namespace {
struct AmmCtx {
    QByteArray cfg, store;        // utf8 paths
    QByteArray amm;               // canonical AMM program id (used for ATA-based ops)
    QByteArray amm_v2;            // amm_v2 program id (used for all primary swap/pool flows)
    QByteArray a, b, lp;          // 32-byte holding ids
    QByteArray defA, defB;        // token definition ids
    QString err;
};
AmmCtx loadCtx(const QHash<QString, QString>& e) {
    AmmCtx c;
    auto id = [&](const char* k, QByteArray& o) -> bool {
        if (!hex32(e.value(QString::fromLatin1(k)), o)) {
            c.err = QStringLiteral("dev env missing/invalid %1 - click "
                                   "'Load dev setup'").arg(k);
            return false;
        }
        return true;
    };
    c.cfg = e.value("LDEX_WALLET_CONFIG").toUtf8();
    c.store = e.value("LDEX_WALLET_STORAGE").toUtf8();
    if (c.cfg.isEmpty() || c.store.isEmpty())
        c.err = QStringLiteral("dev env missing wallet paths - 'Load dev setup'");
    else if (id("LDEX_AMM_PROGRAM_ID", c.amm) &&
             id("LDEX_AMM_V2_PROGRAM_ID", c.amm_v2) &&
             id("LDEX_USER_HOLDING_A", c.a) &&
             id("LDEX_USER_HOLDING_B", c.b) &&
             id("LDEX_USER_HOLDING_LP", c.lp) &&
             id("LDEX_DEF_A", c.defA) && id("LDEX_DEF_B", c.defB)) { }
    return c;
}
}  // namespace



// Token-agnostic public swap. ATA-only (RFP Func #8): caller passes the
// two pool definition ids (NOT keypair holdings) and the input def; the
// FFI derives ATA(owner, def_a/def_b) from the env-bound owner. `config`
// packs "<defA>|<defB>|<defIn>" - the SDK QtProviderObject dispatch caps
// callModule arity at 5. Kept under the old name for QML compatibility;
// internally now identical to swapExactInAtaFor.

// Token-agnostic ATA-based public swap (F8 generalised). Owner comes
// from m_env; the two def_ids select the pair; the FFI derives both
// ATAs from (owner, def) deterministically. `config` packs
// "<defA>|<defB>|<defIn>".
QString LdexCorePlugin::swapExactInAtaFor(const QString& config,
                                          const QString& amountIn,
                                          const QString& minOut, int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    const QStringList parts = config.split(QChar('|'));
    if (parts.size() != 3)
        return QStringLiteral("ATA swap: config must be "
                              "\"<defA>|<defB>|<defIn>\"");
    QByteArray ownerBytes, da, db, tin;
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("ATA swap: env missing LDEX_USER_OWNER");
    if (!hex32(parts[0], da) || !hex32(parts[1], db) || !hex32(parts[2], tin))
        return QStringLiteral("ATA swap: each id must parse to 32 bytes");
    if (da == db)
        return QStringLiteral("ATA swap: pick two different tokens.");
    const QString ataPid = m_env.value("LDEX_ATA_PROGRAM_ID");
    if (ataPid.isEmpty())
        return QStringLiteral("ATA swap: env missing LDEX_ATA_PROGRAM_ID");
    qputenv("LDEX_ATA_PROGRAM_ID", ataPid.toUtf8());
    ldex_u128 ain, mout;
    if (!parseU128(amountIn, ain) || !parseU128(minOut, mout))
        return QStringLiteral("ATA swap: amounts must be decimal integers");
    unsigned char tx[32];
    const int rc = ldex_amm_v2_swap_exact_in_ata(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(da.constData()),
        reinterpret_cast<const uint8_t*>(db.constData()),
        reinterpret_cast<const uint8_t*>(tin.constData()),
        ain, mout, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("ATA swap submitted. tx=0x%1").arg(hashHex(tx))
        : rcMessage("submit the ATA swap", rc);
}

// Token-agnostic pool creation. Caller passes the user's KEYPAIR
// holdings for the two sides (they fund the deposits via canonical
// `token::Transfer`); the LP is minted into `ATA(owner, lp_def)`,
// which the in-tx chained `ata::Create` initialises after the LP
// definition is created. Owner + ATA program id come from env
// (LDEX_USER_OWNER + LDEX_ATA_PROGRAM_ID).
QString LdexCorePlugin::createPoolFor(const QString& holdingAHex,
                                      const QString& holdingBHex,
                                      const QString& amountA, const QString& amountB,
                                      int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray ha, hb;
    if (!hex32(holdingAHex, ha) || !hex32(holdingBHex, hb))
        return QStringLiteral("createPoolFor: holdings must parse to 32 bytes");
    if (ha == hb)
        return QStringLiteral("createPoolFor: pick two different tokens.");
    ldex_u128 amtA, amtB;
    if (!parseU128(amountA, amtA) || !parseU128(amountB, amtB))
        return QStringLiteral("createPoolFor: amounts must be decimal integers");
    QByteArray ownerBytes;
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("createPoolFor: env missing LDEX_USER_OWNER");
    qputenv("LDEX_ATA_PROGRAM_ID", m_env.value("LDEX_ATA_PROGRAM_ID").toUtf8());

    unsigned char tx[32];
    const int rc = ldex_amm_v2_new_pool_ata(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(ha.constData()),
        reinterpret_cast<const uint8_t*>(hb.constData()),
        amtA, amtB, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Pool created (LP→ATA). tx=0x%1").arg(hashHex(tx))
        : rcMessage("create the pool", rc);
}

// Token-agnostic private swap (modes 1/2/3, ANY pair). Caller supplies
// the pool's def_a/def_b and the user's PrivateOwned holdings for those
// two tokens; the existing per-mode FFI calls are dispatched against
// them. Mode 0 (Public) is handled by the public-side path here too so
// the UI can use a single entry point.
QString LdexCorePlugin::privateSwapFor(const QString& config,
                                       const QString& amountIn,
                                       const QString& minOut, int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    // config = "<mode>|<direction>|<defA>|<defB>|<privA>|<privB>" - 6
    // pipe-delimited fields packed into one QString because the SDK
    // QtProviderObject dispatch caps callModule arity at 5.
    const QStringList parts = config.split(QChar('|'));
    if (parts.size() != 6)
        return QStringLiteral("privateSwapFor: config must be "
                              "\"<mode>|<direction>|<defA>|<defB>|<privA>|<privB>\"");
    bool okM=false, okD=false;
    const int mode = parts[0].toInt(&okM);
    const int direction = parts[1].toInt(&okD);
    if (!okM || !okD)
        return QStringLiteral("privateSwapFor: mode and direction must be ints");
    if (mode == 0)
        return QStringLiteral("privateSwapFor: mode 0 is public - route via "
                              "swapExactInFor / swapExactInAtaFor instead.");
    if (mode != 1 && mode != 2)
        return QStringLiteral("privateSwapFor: unknown mode %1").arg(mode);

    QByteArray dA, dB, pA, pB;
    if (!hex32(parts[2], dA) || !hex32(parts[3], dB)
     || !hex32(parts[4], pA) || !hex32(parts[5], pB))
        return QStringLiteral("privateSwapFor: each id must parse to 32 bytes");
    if (dA == dB)
        return QStringLiteral("privateSwapFor: pick two different tokens.");
    const QByteArray& inDef  = (direction == 0) ? dA : dB;
    const QByteArray& outDef = (direction == 0) ? dB : dA;
    const QByteArray& privSrc = (direction == 0) ? pA : pB;
    const QByteArray& privDst = (direction == 0) ? pB : pA;
    ldex_u128 ain, mout;
    if (!parseU128(amountIn, ain) || !parseU128(minOut, mout))
        return QStringLiteral("privateSwapFor: amounts must be decimal integers");

    if (mode == 2) {
        // Mode 2 - Private-Disposable (RFP-literal account-A model)
        // via **amm_v2**, the combined private-swap program.
        // amm_v2 inlines the router orchestration + AMM math into one
        // chained call; the upstream privacy circuit runs 5 chained
        // calls instead of 6 (saves 1 env::verify). Same on-chain
        // observable shape - net-zero round-trip through fresh A
        // holdings preserves RFP AC#4. Testnet-compatible (no nssa
        // changes; amm_v2 is just a regular deployed LEZ program).
        // LIVE-VERIFIED on dev sequencer at ~18 min wall-clock for
        // a fresh-pool swap (vs ~50 min recursive baseline).
        QByteArray amm_v2;
        if (!hex32(m_env.value(QStringLiteral("LDEX_AMM_V2_PROGRAM_ID")), amm_v2))
            return QStringLiteral(
                "Disposable: env missing LDEX_AMM_V2_PROGRAM_ID - "
                "amm_v2 not deployed/bootstrapped");
        WalletHandle* wh = wallet_ffi_open(c.cfg.constData(), c.store.constData());
        if (!wh) return QStringLiteral("Disposable: could not open wallet");
        FfiBytes32 aA, aB;
        if (wallet_ffi_create_account_public(wh, &aA) != SUCCESS ||
            wallet_ffi_create_account_public(wh, &aB) != SUCCESS) {
            wallet_ffi_destroy(wh);
            return QStringLiteral("Disposable: could not create account A");
        }
        wallet_ffi_save(wh);
        wallet_ffi_destroy(wh);
        unsigned char itx[32];
        ldex_amm_init_token_holding(
            c.cfg.constData(), c.store.constData(),
            reinterpret_cast<const uint8_t*>(dA.constData()), aA.data, itx);
        ldex_amm_init_token_holding(
            c.cfg.constData(), c.store.constData(),
            reinterpret_cast<const uint8_t*>(dB.constData()), aB.data, itx);
        unsigned char tx[32];
        const int rc = ldex_amm_v2_disposable_swap(
            c.cfg.constData(), c.store.constData(),
            reinterpret_cast<const uint8_t*>(amm_v2.constData()),
            reinterpret_cast<const uint8_t*>(pA.constData()),
            reinterpret_cast<const uint8_t*>(pB.constData()),
            aA.data, aB.data,
            reinterpret_cast<const uint8_t*>(dA.constData()),
            reinterpret_cast<const uint8_t*>(dB.constData()),
            reinterpret_cast<const uint8_t*>(inDef.constData()),
            ain, mout, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
        return rc == LDEX_AMM_OK
            ? QStringLiteral("Private-Disposable swap submitted. tx=0x%1").arg(hashHex(tx))
            : rcMessage("submit the amm_v2 disposable swap", rc);
    }
    // Mode 1 - PrivateOwned via amm_v2 (upstream privacy circuit
    // chains amm_v2.SwapExactInputCircuit as the top-level call).
    unsigned char tx[32];
    const int rc = ldex_amm_v2_private_swap_exact_in(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(pA.constData()),
        reinterpret_cast<const uint8_t*>(pB.constData()),
        reinterpret_cast<const uint8_t*>(dA.constData()),
        reinterpret_cast<const uint8_t*>(dB.constData()),
        reinterpret_cast<const uint8_t*>(inDef.constData()),
        ain, mout, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Private swap submitted. tx=0x%1").arg(hashHex(tx))
        : rcMessage("submit the private swap", rc);
}

// ── Background job registry (for >20 s ops) ────────────────────────────────
// The Logos SDK bridge has a hardcoded `Timeout::Timeout(int=20000)` default
// (logos_mode.h) that the QML-facing `callModule` / `callModuleAsync` does
// not forward a caller-supplied override into. STARK proof generation for a
// private swap legitimately takes minutes, so any sync dispatch returns
// `QtRO error 1 (InvalidMessage)` at 20 s. Workaround: the *Start methods
// spawn the work on a detached std::thread, store its result keyed by job
// id, and return `"job=<id>"` instantly. QML polls `jobStatus(id)`.
namespace {
struct JobRegistry {
    QMutex mu;
    QHash<int, QString> results;
    int nextId = 1;
};
JobRegistry& jobs() {
    // function-local static = thread-safe init under C++11
    static JobRegistry r;
    return r;
}
}  // namespace

QString LdexCorePlugin::privateSwapForStart(const QString& config,
                                            const QString& amountIn,
                                            const QString& minOut, int feeBps)
{
    auto& j = jobs();
    int jobId;
    {
        QMutexLocker lk(&j.mu);
        jobId = j.nextId++;
        j.results.insert(jobId, QStringLiteral("pending"));
    }
    // Detach a worker thread. We capture copies of all QStrings so the
    // thread is independent of the caller's lifetime.
    LdexCorePlugin* self = this;
    QString cfg = config, amt = amountIn, mn = minOut;
    int fb = feeBps;
    std::thread([self, jobId, cfg, amt, mn, fb]() {
        QString result;
        try {
            result = self->privateSwapFor(cfg, amt, mn, fb);
        } catch (const std::exception& e) {
            result = QStringLiteral("Exception in privateSwapFor: %1")
                .arg(QString::fromUtf8(e.what()));
        } catch (...) {
            result = QStringLiteral("Unknown exception in privateSwapFor");
        }
        auto& jr = jobs();
        QMutexLocker lk(&jr.mu);
        jr.results.insert(jobId, result);
    }).detach();
    return QStringLiteral("job=%1").arg(jobId);
}

QString LdexCorePlugin::jobStatus(int jobId)
{
    auto& j = jobs();
    QMutexLocker lk(&j.mu);
    return j.results.value(jobId,
        QStringLiteral("unknown job %1").arg(jobId));
}

// ── Generic *Start dispatcher ────────────────────────────────────────
// Reserve a job id, store "pending", spawn a detached std::thread that
// runs `f()` and stores its result under that id. Returns "job=N"
// immediately - caller must NEVER block the QObject's thread.
namespace {
QString spawnJob(std::function<QString()> f, const char* tag) {
    auto& j = jobs();
    int jobId;
    {
        QMutexLocker lk(&j.mu);
        jobId = j.nextId++;
        j.results.insert(jobId, QStringLiteral("pending"));
    }
    const QString tagS = QString::fromUtf8(tag);
    std::thread([f = std::move(f), jobId, tagS]() {
        QString result;
        try {
            result = f();
        } catch (const std::exception& e) {
            result = QStringLiteral("Exception in %1: %2").arg(
                tagS, QString::fromUtf8(e.what()));
        } catch (...) {
            result = QStringLiteral("Unknown exception in %1").arg(tagS);
        }
        auto& jr = jobs();
        QMutexLocker lk(&jr.mu);
        jr.results.insert(jobId, result);
    }).detach();
    return QStringLiteral("job=%1").arg(jobId);
}
}  // namespace

QString LdexCorePlugin::shieldTokenStart(const QString& letter,
                                         const QString& amount) {
    LdexCorePlugin* self = this;
    QString L = letter, A = amount;
    return spawnJob([self, L, A]() { return self->shieldToken(L, A); },
                    "shieldToken");
}

QString LdexCorePlugin::deshieldTokenStart(const QString& letter,
                                           const QString& amount) {
    LdexCorePlugin* self = this;
    QString L = letter, A = amount;
    return spawnJob([self, L, A]() { return self->deshieldToken(L, A); },
                    "deshieldToken");
}

QString LdexCorePlugin::createPoolForStart(const QString& holdingAHex,
                                           const QString& holdingBHex,
                                           const QString& amountA,
                                           const QString& amountB,
                                           int feeBps) {
    LdexCorePlugin* self = this;
    QString hA = holdingAHex, hB = holdingBHex, aA = amountA, aB = amountB;
    return spawnJob(
        [self, hA, hB, aA, aB, feeBps]() {
            return self->createPoolFor(hA, hB, aA, aB, feeBps);
        },
        "createPoolFor");
}

QString LdexCorePlugin::wrapNativeStart(const QString& amount) {
    LdexCorePlugin* self = this;
    QString A = amount;
    return spawnJob([self, A]() { return self->wrapNative(A); }, "wrapNative");
}

QString LdexCorePlugin::unwrapNativeStart(const QString& amount) {
    LdexCorePlugin* self = this;
    QString A = amount;
    return spawnJob([self, A]() { return self->unwrapNative(A); }, "unwrapNative");
}

// ── Batched native-LEZ private swap (one privacy proof) ─────────────
// Two-tx wrap→swap (or swap→unwrap) becomes one privacy-preserving tx
// whose top program is the deployed account-A router, chaining either:
//   NativeIn:  WLEZ::Wrap → AMM::SwapExactInput → reshield to user_priv
//   NativeOut: deshield user_priv → AMM::SwapExactInput → WLEZ::Unwrap
// See docs/batched-native-swap.md. `config` is
// "<direction>|<token_def_hex>|<priv_holding_hex>" where:
//   direction = 0 → NativeIn  (LEZ → token_def; priv_holding is dest)
//   direction = 1 → NativeOut (token_def → LEZ; priv_holding is source)
// Everything else (WLEZ defs, vault, programs, user_native) is read
// from `m_env`.
QString LdexCorePlugin::privateSwapNativeFor(const QString& config,
                                             const QString& amountIn,
                                             const QString& minOut,
                                             int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;

    const QStringList parts = config.split(QChar('|'));
    if (parts.size() != 3)
        return QStringLiteral("privateSwapNativeFor: config must be "
                              "\"<direction>|<token_def>|<priv_holding>\"");
    bool okD = false;
    const int direction = parts[0].toInt(&okD);
    if (!okD || (direction != 0 && direction != 1))
        return QStringLiteral("privateSwapNativeFor: direction must be 0 (NativeIn) or 1 (NativeOut)");
    QByteArray tokDef, privHold;
    if (!hex32(parts[1], tokDef) || !hex32(parts[2], privHold))
        return QStringLiteral("privateSwapNativeFor: token_def and priv_holding must be 32-byte hex");

    QByteArray rtr, wlz, wd, wv;
    if (!hex32(m_env.value(QStringLiteral("LDEX_ROUTER_PROGRAM_ID")), rtr))
        return QStringLiteral("Native swap: env missing LDEX_ROUTER_PROGRAM_ID");
    if (!hex32(m_env.value(QStringLiteral("LDEX_WLEZ_PROGRAM_ID")), wlz))
        return QStringLiteral("Native swap: env missing LDEX_WLEZ_PROGRAM_ID");
    if (!hex32(m_env.value(QStringLiteral("LDEX_WLEZ_DEF")), wd))
        return QStringLiteral("Native swap: env missing LDEX_WLEZ_DEF");
    if (!hex32(m_env.value(QStringLiteral("LDEX_WLEZ_VAULT")), wv))
        return QStringLiteral("Native swap: env missing LDEX_WLEZ_VAULT");
    // User's native account: stored Public/<base58> in env; convert to bytes.
    QByteArray userNat;
    {
        // LDEX_USER_OWNER may already be hex (rare) or "Public/<base58>"
        // (bootstrap default). Drop the prefix and parse base58 via the
        // existing FFI helper for consistency with other call-sites.
        QString s = m_env.value(QStringLiteral("LDEX_USER_OWNER"));
        if (s.isEmpty())
            return QStringLiteral("Native swap: env missing LDEX_USER_OWNER");
        // Reuse the same conversion path the rest of the plugin uses for
        // Public/… ids (see ldex_amm_parse_account_id). For brevity we
        // require the env to have the raw 32-byte hex form OR a
        // "Public/" prefix that the FFI can parse.
        if (s.startsWith(QStringLiteral("Public/"))) {
            uint8_t outId[32];
            QByteArray sb = s.toUtf8();
            const int prc = ldex_amm_parse_account_id(sb.constData(), outId);
            if (prc != LDEX_AMM_OK)
                return QStringLiteral("Native swap: cannot parse LDEX_USER_OWNER (%1)").arg(prc);
            userNat = QByteArray(reinterpret_cast<const char*>(outId), 32);
        } else if (!hex32(s, userNat)) {
            return QStringLiteral("Native swap: LDEX_USER_OWNER must be hex32 or Public/<base58>");
        }
    }

    ldex_u128 ain, mout;
    if (!parseU128(amountIn, ain) || !parseU128(minOut, mout))
        return QStringLiteral("Native swap: amounts must be decimal integers");

    // Spin up two fresh public accounts (one per holding) - the
    // existing `disposable_swap_exact_in` flow uses the same shape; a
    // holding is itself a public account whose data is a TokenHolding.
    // For NativeIn:  aWlez = A's WLEZ holding (mint target);
    //                aOut  = A's output-token holding (AMM credits).
    // For NativeOut: aIn   = A's input-token holding (deshield target);
    //                aWlez = A's WLEZ holding (AMM credits before unwrap).
    WalletHandle* wh = wallet_ffi_open(c.cfg.constData(), c.store.constData());
    if (!wh) return QStringLiteral("Native swap: could not open wallet");
    FfiBytes32 aWlez, aTok;
    if (wallet_ffi_create_account_public(wh, &aWlez) != SUCCESS ||
        wallet_ffi_create_account_public(wh, &aTok)  != SUCCESS) {
        wallet_ffi_destroy(wh);
        return QStringLiteral("Native swap: could not create disposable accounts");
    }
    wallet_ffi_save(wh);
    wallet_ffi_destroy(wh);

    unsigned char ix[32];
    const int rwi = ldex_amm_init_token_holding(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(wd.constData()),
        aWlez.data, ix);
    if (rwi != LDEX_AMM_OK) return rcMessage("init A's WLEZ holding", rwi);
    const int rti = ldex_amm_init_token_holding(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(tokDef.constData()),
        aTok.data, ix);
    if (rti != LDEX_AMM_OK) return rcMessage("init A's token holding", rti);

    unsigned char tx[32];
    if (direction == 0) {
        // NativeIn: LEZ → token_def. priv_holding = destination.
        // Routed through amm_v2 (combined: WLEZ::Wrap + AMM math
        // inline + 2× token::Transfer vault movements + reshield).
        const int rc = ldex_amm_v2_disposable_swap_native_in(
            c.cfg.constData(), c.store.constData(),
            reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
            reinterpret_cast<const uint8_t*>(wlz.constData()),
            reinterpret_cast<const uint8_t*>(userNat.constData()),
            reinterpret_cast<const uint8_t*>(wv.constData()),
            reinterpret_cast<const uint8_t*>(wd.constData()),
            aWlez.data, aTok.data,
            reinterpret_cast<const uint8_t*>(tokDef.constData()),
            reinterpret_cast<const uint8_t*>(privHold.constData()),
            ain, mout, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
        return rc == LDEX_AMM_OK
            ? QStringLiteral("Batched native-in swap submitted. tx=0x%1").arg(hashHex(tx))
            : rcMessage("submit the batched native-in swap", rc);
    } else {
        // NativeOut: token_def → LEZ. priv_holding = source.
        // Routed through amm_v2 (combined: deshield + AMM math inline
        // + 2× token::Transfer vault movements + WLEZ::Unwrap).
        const int rc = ldex_amm_v2_disposable_swap_native_out(
            c.cfg.constData(), c.store.constData(),
            reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
            reinterpret_cast<const uint8_t*>(wlz.constData()),
            reinterpret_cast<const uint8_t*>(privHold.constData()),
            aTok.data, aWlez.data,
            reinterpret_cast<const uint8_t*>(wd.constData()),
            reinterpret_cast<const uint8_t*>(wv.constData()),
            reinterpret_cast<const uint8_t*>(userNat.constData()),
            reinterpret_cast<const uint8_t*>(tokDef.constData()),
            ain, mout, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
        return rc == LDEX_AMM_OK
            ? QStringLiteral("Batched native-out swap submitted. tx=0x%1").arg(hashHex(tx))
            : rcMessage("submit the batched native-out swap", rc);
    }
}

QString LdexCorePlugin::privateSwapNativeForStart(const QString& config,
                                                  const QString& amountIn,
                                                  const QString& minOut,
                                                  int feeBps)
{
    auto& j = jobs();
    int jobId;
    {
        QMutexLocker lk(&j.mu);
        jobId = j.nextId++;
        j.results.insert(jobId, QStringLiteral("pending"));
    }
    LdexCorePlugin* self = this;
    QString cfg = config, amt = amountIn, mn = minOut;
    int fb = feeBps;
    std::thread([self, jobId, cfg, amt, mn, fb]() {
        QString result;
        try {
            result = self->privateSwapNativeFor(cfg, amt, mn, fb);
        } catch (const std::exception& e) {
            result = QStringLiteral("Exception in privateSwapNativeFor: %1")
                .arg(QString::fromUtf8(e.what()));
        } catch (...) {
            result = QStringLiteral("Unknown exception in privateSwapNativeFor");
        }
        auto& jr = jobs();
        QMutexLocker lk(&jr.mu);
        jr.results.insert(jobId, result);
    }).detach();
    return QStringLiteral("job=%1").arg(jobId);
}

// ── Native LEZ via WLEZ (auto-wrap bridge) ──────────────────────────
// The mini-app surfaces "LEZ" as a normal catalog entry; under the hood
// it's a WLEZ token holding. These three methods give the UI the
// primitives it needs: read native balance, wrap, unwrap. Swap-flavoured
// chaining (wrap-then-swap, or swap-then-unwrap) happens in QML by
// composing wrapNative/unwrapNative with the existing swapExactInFor.

QString LdexCorePlugin::nativeBalance()
{
    if (!ensureEnv()) return QStringLiteral("0");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("0");
    QByteArray ownerBytes;
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("0");
    WalletHandle* h = wallet_ffi_open(c.cfg.constData(), c.store.constData());
    if (!h) return QStringLiteral("0");
    FfiBytes32 fid;
    std::memcpy(fid.data, ownerBytes.constData(), 32);
    uint8_t out_le[16] = {0};
    const auto rc = wallet_ffi_get_balance(h, &fid, /*is_public=*/true, &out_le);
    wallet_ffi_destroy(h);
    if (rc != SUCCESS) return QStringLiteral("0");
    // Low 8 bytes - dev amounts fit comfortably in u64.
    qulonglong v = 0;
    for (int i = 7; i >= 0; --i) v = (v << 8) | out_le[i];
    return QString::number(v);
}

QString LdexCorePlugin::wrapNative(const QString& amount)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray pidBytes, ownerBytes, target;
    if (!hex32(m_env.value("LDEX_WLEZ_PROGRAM_ID"), pidBytes))
        return QStringLiteral("wrapNative: env missing LDEX_WLEZ_PROGRAM_ID");
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("wrapNative: env missing LDEX_USER_OWNER");
    // Prefer ATA(USER, WLEZ_DEF) as the wrap destination so the newly
    // minted WLEZ is immediately spendable by mode-0 ATA swaps and
    // ATA-side pool ops - without an extra token::Transfer. Falls back
    // to the keypair HOLD_W if the bootstrap didn't wire the ATA (e.g.
    // pre-WLEZ-ATA bootstraps).
    const QString ataW = m_env.value("LDEX_ATA_W");
    if (!ataW.isEmpty() && hex32(ataW, target)) {
        // ATA path - wrap lands directly in ATA(USER, WLEZ_DEF).
    } else if (!hex32(m_env.value("LDEX_HOLD_W"), target)) {
        return QStringLiteral("wrapNative: env missing LDEX_ATA_W and LDEX_HOLD_W");
    }
    ldex_u128 amt;
    if (!parseU128(amount, amt))
        return QStringLiteral("wrapNative: amount must be a decimal integer");
    unsigned char tx[32];
    const int rc = ldex_wlez_wrap(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(pidBytes.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(target.constData()),
        amt, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Wrap submitted (→%1). tx=0x%2")
              .arg(ataW.isEmpty() ? QStringLiteral("HOLD_W") : QStringLiteral("ATA"))
              .arg(hashHex(tx))
        : rcMessage("wrap native LEZ", rc);
}

// Scan blocks since the wallet's last_synced and update local private
// balances. Called by the UI on a throttled timer + after each action,
// instead of inside `walletTokens` (which fires every render). Returns
// "synced to block <id>" on success or a friendly error string.
QString LdexCorePlugin::syncPrivateBalances()
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first.");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    WalletHandle* h = wallet_ffi_open(c.cfg.constData(), c.store.constData());
    if (!h) return QStringLiteral("syncPrivateBalances: could not open wallet");
    uint64_t headBlock = 0;
    if (wallet_ffi_get_current_block_height(h, &headBlock) != SUCCESS) {
        wallet_ffi_destroy(h);
        return QStringLiteral("syncPrivateBalances: could not read chain head");
    }
    if (headBlock == 0) {
        wallet_ffi_destroy(h);
        return QStringLiteral("synced to block 0 (chain has no blocks yet)");
    }
    const auto rc = wallet_ffi_sync_to_block(h, headBlock);
    wallet_ffi_destroy(h);
    return rc == SUCCESS
        ? QStringLiteral("synced to block %1").arg(headBlock)
        : QStringLiteral("syncPrivateBalances: rc=%1 at block %2")
              .arg(static_cast<int>(rc)).arg(headBlock);
}

QString LdexCorePlugin::unwrapNative(const QString& amount)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray pidBytes, ownerBytes, holdW;
    if (!hex32(m_env.value("LDEX_WLEZ_PROGRAM_ID"), pidBytes))
        return QStringLiteral("unwrapNative: env missing LDEX_WLEZ_PROGRAM_ID");
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("unwrapNative: env missing LDEX_USER_OWNER");
    // Unwrap drains the keypair WLEZ holding - WLEZ::Unwrap asserts
    // `user_holding.is_authorized`, which requires a signing key the
    // wallet actually holds. ATA_W is PDA-owned by the ATA program;
    // the wallet can't sign for it directly. If the user has WLEZ in
    // ATA_W they want to unwrap, they must token::Transfer it into
    // HOLD_W first (UI surfaces this via a separate "move to HOLD_W"
    // helper, planned).
    if (!hex32(m_env.value("LDEX_HOLD_W"), holdW))
        return QStringLiteral("unwrapNative: env missing LDEX_HOLD_W");
    ldex_u128 amt;
    if (!parseU128(amount, amt))
        return QStringLiteral("unwrapNative: amount must be a decimal integer");
    unsigned char tx[32];
    const int rc = ldex_wlez_unwrap(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(pidBytes.constData()),
        reinterpret_cast<const uint8_t*>(holdW.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        amt, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Unwrap submitted (from HOLD_W). tx=0x%1").arg(hashHex(tx))
        : rcMessage("unwrap native LEZ", rc);
}

// Move WLEZ from ATA(USER, WLEZ_DEF) into HOLD_W via ata::Transfer.
// Needed because WLEZ::Unwrap asserts `user_holding.is_authorized`,
// which only a keypair-signed holding can satisfy. ATAs are PDA-owned
// by the ATA program; the wallet has no signing key for them. So the
// unwrap-from-ATA workflow is two-step: this helper, then unwrapNative.
QString LdexCorePlugin::consolidateWlezToHoldW(const QString& amount)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray ataPidBytes, ownerBytes, ataW, holdW;
    if (!hex32(m_env.value("LDEX_ATA_PROGRAM_ID"), ataPidBytes))
        return QStringLiteral("consolidateWlezToHoldW: env missing LDEX_ATA_PROGRAM_ID");
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("consolidateWlezToHoldW: env missing LDEX_USER_OWNER");
    if (!hex32(m_env.value("LDEX_ATA_W"), ataW))
        return QStringLiteral("consolidateWlezToHoldW: env missing LDEX_ATA_W");
    if (!hex32(m_env.value("LDEX_HOLD_W"), holdW))
        return QStringLiteral("consolidateWlezToHoldW: env missing LDEX_HOLD_W");
    ldex_u128 amt;
    if (!parseU128(amount, amt))
        return QStringLiteral("consolidateWlezToHoldW: amount must be a decimal integer");
    unsigned char tx[32];
    const int rc = ldex_ata_transfer(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(ataPidBytes.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(ataW.constData()),
        reinterpret_cast<const uint8_t*>(holdW.constData()),
        amt, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Moved %1 WLEZ → HOLD_W. tx=0x%2").arg(amount).arg(hashHex(tx))
        : rcMessage("move WLEZ from ATA to HOLD_W", rc);
}

// Manual shield: ATA(USER, DEF_<L>)  →  PRIV_<L>.
// Routes through `ldex_token_shield` (LDEX-side FFI that wraps
// `wallet.send_privacy_preserving_tx` with a token-program Transfer
// instruction). Single privacy-preserving tx - generates a STARK
// in-wallet, then the sequencer verifies + lands it. We deliberately
// do NOT use `wallet_ffi_transfer_shielded_owned`: that one targets
// the native LEZ `authenticated_transfer_program` and checks
// `account.balance` (the native field, always 0 on token holdings) -
// so it returns InsufficientFunds (rc=9) on every token shield attempt.
QString LdexCorePlugin::shieldToken(const QString& letter, const QString& amount)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray from, to;
    // Source MUST be HOLD_<L> (keypair-owned), not ATA_<L> (PDA-owned).
    // The token guest's transfer asserts `sender.is_authorized`, which
    // requires a signature the wallet can only produce for keypair
    // accounts. The bootstrap shields from HOLD too, for the same reason.
    if (!hex32(m_env.value("LDEX_HOLD_" + letter), from))
        return QStringLiteral("shieldToken: env missing LDEX_HOLD_%1").arg(letter);
    if (!hex32(m_env.value("LDEX_PRIV_" + letter), to))
        return QStringLiteral("shieldToken: env missing LDEX_PRIV_%1").arg(letter);
    ldex_u128 amt;
    if (!parseU128(amount, amt))
        return QStringLiteral("shieldToken: amount must be a decimal integer");
    unsigned char tx[32];
    const int rc = ldex_token_shield(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(from.constData()),
        reinterpret_cast<const uint8_t*>(to.constData()),
        amt, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Shielded %1 TOKEN%2. tx=0x%3")
              .arg(amount, letter, hashHex(tx))
        : rcMessage(QStringLiteral("shield TOKEN%1").arg(letter).toUtf8().constData(), rc);
}

// Manual deshield: PRIV_<L>  →  ATA(USER, DEF_<L>). Same primitive
// as `shieldToken`, reversed direction.
QString LdexCorePlugin::deshieldToken(const QString& letter, const QString& amount)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray from, to;
    // Destination is HOLD_<L> (keypair) - recipient is just a credit
    // (no sender_authorization required for the recipient), and HOLD is
    // the bootstrap-initialised TokenHolding for this letter. The
    // wallet's pub-balance display sums HOLD+ATA, so the user sees the
    // total pub balance increase by the deshielded amount.
    if (!hex32(m_env.value("LDEX_PRIV_" + letter), from))
        return QStringLiteral("deshieldToken: env missing LDEX_PRIV_%1").arg(letter);
    if (!hex32(m_env.value("LDEX_HOLD_" + letter), to))
        return QStringLiteral("deshieldToken: env missing LDEX_HOLD_%1").arg(letter);
    ldex_u128 amt;
    if (!parseU128(amount, amt))
        return QStringLiteral("deshieldToken: amount must be a decimal integer");
    unsigned char tx[32];
    const int rc = ldex_token_deshield(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(from.constData()),
        reinterpret_cast<const uint8_t*>(to.constData()),
        amt, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Deshielded %1 TOKEN%2. tx=0x%3")
              .arg(amount, letter, hashHex(tx))
        : rcMessage(QStringLiteral("deshield TOKEN%1").arg(letter).toUtf8().constData(), rc);
}

// Token-agnostic quote - takes explicit def_a/def_b instead of using
// env's c.defA/c.defB. Constant-product math mirrors `quote()` exactly.
QString LdexCorePlugin::quoteFor(const QString& defAHex, const QString& defBHex,
                                 int direction, const QString& amountIn,
                                 int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("{\"exists\":false}");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("{\"exists\":false}");
    QByteArray da, db;
    if (!hex32(defAHex, da) || !hex32(defBHex, db))
        return QStringLiteral("{\"exists\":false}");
    // compute_pool_pda_seed in amm_core panics if defA == defB. Catch it
    // here as an empty quote - a Rust panic across FFI takes down the
    // whole module process and poisons every subsequent call.
    if (da == db)
        return QStringLiteral("{\"exists\":false}");
    unsigned char buf[512];
    const int rc = ldex_amm_pool_info(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(da.constData()),
        reinterpret_cast<const uint8_t*>(db.constData()),
        static_cast<ldex_u128>(feeBps), buf, sizeof(buf));
    if (rc != LDEX_AMM_OK)
        return QStringLiteral("{\"exists\":false}");
    const QJsonObject o =
        QJsonDocument::fromJson(QByteArray(reinterpret_cast<char*>(buf))).object();
    if (!o.value("exists").toBool())
        return QStringLiteral("{\"exists\":false}");
    const double ra = o.value("reserve_a").toString().toDouble();
    const double rb = o.value("reserve_b").toString().toDouble();
    const double ain = amountIn.toDouble();
    const double fee = feeBps / 10000.0;
    const double effIn = ain * (1.0 - fee);
    const double rin = (direction == 0) ? ra : rb;
    const double rout = (direction == 0) ? rb : ra;
    if (rin <= 0 || rout <= 0 || ain <= 0 || (rin + effIn) <= 0)
        return QStringLiteral("{\"exists\":true,\"out\":\"0\",\"feePaid\":\"0\",\"impactPct\":\"0\"}");
    const double out = rout * effIn / (rin + effIn);
    const double spot = rout / rin;
    const double exec = out / ain;
    const double impact = spot > 0 ? (1.0 - exec / spot) * 100.0 : 0.0;
    const double feePaid = ain * fee;
    auto f = [](double v) { return QString::number(v, 'f', 4); };
    return QStringLiteral(
        "{\"exists\":true,\"out\":\"%1\",\"feePaid\":\"%2\",\"impactPct\":\"%3\"}")
        .arg(f(out), f(feePaid), f(impact));
}

// RFP Func #8 - public swap where the user side is the user's
// deterministic ATA per (owner, definition). Chains `ata::Transfer` for
// the input leg (owner-authorised - ATA program internally PDA-authorises
// the sender ATA via its seed) + the existing vault-PDA-authorised
// `token::Transfer` for the output leg into the recipient ATA. The FFI
// derives both ATAs internally from owner + the two token defs via
// `LDEX_ATA_PROGRAM_ID` - we mirror that env into the process here so the
// FFI sees it.


QString LdexCorePlugin::addLiquidity(const QString& minLp, const QString& maxA,
                                     const QString& maxB, int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    ldex_u128 mlp, mxa, mxb;
    if (!parseU128(minLp, mlp) || !parseU128(maxA, mxa) || !parseU128(maxB, mxb))
        return QStringLiteral("addLiquidity: amounts must be decimal integers");
    QByteArray ownerBytes, ataPidBytes;
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("addLiquidity: env missing LDEX_USER_OWNER");
    if (!hex32(m_env.value("LDEX_ATA_PROGRAM_ID"), ataPidBytes))
        return QStringLiteral("addLiquidity: env missing LDEX_ATA_PROGRAM_ID");
    qputenv("LDEX_ATA_PROGRAM_ID", m_env.value("LDEX_ATA_PROGRAM_ID").toUtf8());

    // Ensure LP-ATA exists (idempotent; first add for a pool needs it).
    unsigned char poolPda[32], lpDef[32], txAtaLp[32];
    if (ldex_amm_pool_id(reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
                         reinterpret_cast<const uint8_t*>(c.a.constData()),
                         reinterpret_cast<const uint8_t*>(c.b.constData()),
                         static_cast<ldex_u128>(feeBps), poolPda) != LDEX_AMM_OK
        || ldex_amm_lp_definition_id(reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
                                     poolPda, lpDef) != LDEX_AMM_OK)
        return QStringLiteral("addLiquidity: LP-def derivation failed");
    const int rcAta = ldex_ata_create(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(ataPidBytes.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        lpDef, txAtaLp);
    if (rcAta != LDEX_AMM_OK)
        return rcMessage("create the LP ATA", rcAta);

    unsigned char tx[32];
    const int rc = ldex_amm_v2_add_liquidity_ata(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(c.a.constData()),
        reinterpret_cast<const uint8_t*>(c.b.constData()),
        mlp, mxa, mxb, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Liquidity added (ATA). tx=0x%1").arg(hashHex(tx))
        : rcMessage("add liquidity", rc);
}

QString LdexCorePlugin::removeLiquidity(const QString& lpAmount,
                                        const QString& minA,
                                        const QString& minB, int feeBps)
{
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    ldex_u128 lpa, mna, mnb;
    if (!parseU128(lpAmount, lpa) || !parseU128(minA, mna) || !parseU128(minB, mnb))
        return QStringLiteral("removeLiquidity: amounts must be decimal integers");
    QByteArray ownerBytes;
    if (!hex32(m_env.value("LDEX_USER_OWNER"), ownerBytes))
        return QStringLiteral("removeLiquidity: env missing LDEX_USER_OWNER");
    qputenv("LDEX_ATA_PROGRAM_ID", m_env.value("LDEX_ATA_PROGRAM_ID").toUtf8());
    unsigned char tx[32];
    const int rc = ldex_amm_v2_remove_liquidity_ata(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(ownerBytes.constData()),
        reinterpret_cast<const uint8_t*>(c.a.constData()),
        reinterpret_cast<const uint8_t*>(c.b.constData()),
        lpa, mna, mnb, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Liquidity removed (ATA). tx=0x%1").arg(hashHex(tx))
        : rcMessage("remove liquidity", rc);
}

QString LdexCorePlugin::privateAddLiquidity(int mode, const QString& minLp,
                                            const QString& maxA,
                                            const QString& maxB, int feeBps)
{
    if (mode == 0) return addLiquidity(minLp, maxA, maxB, feeBps);
    if (mode == 2)
        return QStringLiteral("Private-Disposable not supported for "
                              "liquidity; use Private (PrivateOwned).");
    if (mode != 1)
        return QStringLiteral("privateAddLiquidity: unknown mode %1").arg(mode);
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    // Mode-1 LP needs PRIV holdings (PrivateOwned, wallet-owned) on all 3
    // sides - TOKEN_A, TOKEN_B, and LP. Bootstrap creates PRIV_A/B; PRIV_LP
    // isn't pre-created (LP tokens only exist after the first add). Lazy-
    // create PRIV_LP on first call + remember it in env so subsequent
    // remove-liquidity calls can decrypt the commitment.
    QByteArray privA, privB, privLp;
    if (!hex32(m_env.value("LDEX_PRIV_A"), privA))
        return QStringLiteral("privateAddLiquidity: env missing LDEX_PRIV_A - re-bootstrap to seed private holdings.");
    if (!hex32(m_env.value("LDEX_PRIV_B"), privB))
        return QStringLiteral("privateAddLiquidity: env missing LDEX_PRIV_B - re-bootstrap to seed private holdings.");
    if (!hex32(m_env.value("LDEX_PRIV_LP"), privLp)) {
        // Auto-create a fresh PrivateOwned account for the LP holding.
        WalletHandle* wh = wallet_ffi_open(c.cfg.constData(), c.store.constData());
        if (!wh) return QStringLiteral("privateAddLiquidity: could not open wallet to create PRIV_LP");
        FfiBytes32 plp;
        if (wallet_ffi_create_account_private(wh, &plp) != SUCCESS) {
            wallet_ffi_destroy(wh);
            return QStringLiteral("privateAddLiquidity: could not create PRIV_LP");
        }
        wallet_ffi_save(wh);
        wallet_ffi_destroy(wh);
        privLp = QByteArray(reinterpret_cast<const char*>(plp.data), 32);
        // Cache for the rest of this session + future remove-liquidity calls.
        m_env.insert(QStringLiteral("LDEX_PRIV_LP"),
                     QStringLiteral("Private/") + QString::fromLatin1(privLp.toHex()));
    }
    ldex_u128 mlp, mxa, mxb;
    if (!parseU128(minLp, mlp) || !parseU128(maxA, mxa) || !parseU128(maxB, mxb))
        return QStringLiteral("privateAddLiquidity: amounts must be decimal integers");
    unsigned char tx[32];
    // Routes through amm_v2 (the legacy v1 amm path returned
    // InvalidPrivacyPreservingProof). Verified live tx 1cbadef0…2521496
    // at 24 min on this CPU.
    const int rc = ldex_amm_v2_private_add_liquidity(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(privA.constData()),
        reinterpret_cast<const uint8_t*>(privB.constData()),
        reinterpret_cast<const uint8_t*>(privLp.constData()),
        reinterpret_cast<const uint8_t*>(c.defA.constData()),
        reinterpret_cast<const uint8_t*>(c.defB.constData()),
        mlp, mxa, mxb, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Private liquidity added. tx=0x%1").arg(hashHex(tx))
        : rcMessage("add liquidity privately", rc);
}

QString LdexCorePlugin::privateRemoveLiquidity(int mode,
                                               const QString& lpAmount,
                                               const QString& minA,
                                               const QString& minB, int feeBps)
{
    if (mode == 0) return removeLiquidity(lpAmount, minA, minB, feeBps);
    if (mode == 2)
        return QStringLiteral("Private-Disposable not supported for "
                              "liquidity; use Private (PrivateOwned).");
    if (mode != 1)
        return QStringLiteral("privateRemoveLiquidity: unknown mode %1").arg(mode);
    if (!ensureEnv())
        return QStringLiteral("Load dev setup first (bootstrap.env not loaded).");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return c.err;
    QByteArray privA, privB, privLp;
    if (!hex32(m_env.value("LDEX_PRIV_A"), privA))
        return QStringLiteral("privateRemoveLiquidity: env missing LDEX_PRIV_A");
    if (!hex32(m_env.value("LDEX_PRIV_B"), privB))
        return QStringLiteral("privateRemoveLiquidity: env missing LDEX_PRIV_B");
    if (!hex32(m_env.value("LDEX_PRIV_LP"), privLp))
        return QStringLiteral("privateRemoveLiquidity: env missing LDEX_PRIV_LP - add private liquidity first to seed it.");
    ldex_u128 lpa, mna, mnb;
    if (!parseU128(lpAmount, lpa) || !parseU128(minA, mna) || !parseU128(minB, mnb))
        return QStringLiteral("privateRemoveLiquidity: amounts must be decimal integers");
    unsigned char tx[32];
    const int rc = ldex_amm_v2_private_remove_liquidity(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(privA.constData()),
        reinterpret_cast<const uint8_t*>(privB.constData()),
        reinterpret_cast<const uint8_t*>(privLp.constData()),
        reinterpret_cast<const uint8_t*>(c.defA.constData()),
        reinterpret_cast<const uint8_t*>(c.defB.constData()),
        lpa, mna, mnb, static_cast<ldex_u128>(feeBps), UINT64_MAX, tx);
    return rc == LDEX_AMM_OK
        ? QStringLiteral("Private liquidity removed. tx=0x%1").arg(hashHex(tx))
        : rcMessage("remove liquidity privately", rc);
}

QString LdexCorePlugin::devBootstrap()
{
    // Force a fresh read so clicking "Load dev setup" picks up a
    // re-generated bootstrap.env without restarting the app. (m_env is
    // cached for the steady-state ops; this is the one place where the
    // user intentionally asks for it to be reloaded.)
    m_env.clear();
    if (!ensureEnv())
        return QStringLiteral("ERR: %1 not found / no keys - run "
                              "scripts/bootstrap.sh (sequencer up).")
            .arg(envFilePath());
    QJsonObject obj;
    for (auto it = m_env.constBegin(); it != m_env.constEnd(); ++it)
        obj.insert(it.key(), it.value());
    return QString::fromUtf8(
        QJsonDocument(obj).toJson(QJsonDocument::Compact));
}


// Token pairs that exist in the env, as (symA, symB, defHexA, defHexB).
// Includes every LDEX_DEF_<L> for L in LDEX_TOKENS, plus LEZ (LDEX_WLEZ_DEF)
// when present. Pairs are unordered: (A,B) is emitted once, not (B,A) too.
struct PairEntry { QString symA, symB; QByteArray defA, defB; };
static QList<PairEntry> enumeratePairs(const QHash<QString, QString>& e)
{
    QList<QPair<QString, QByteArray>> toks;   // (sym, defBytes)
    const QStringList letters = e.value("LDEX_TOKENS", "A B")
                                    .split(QChar(' '), Qt::SkipEmptyParts);
    for (const QString& L : letters) {
        QByteArray db;
        if (hex32(e.value("LDEX_DEF_" + L), db) && db.size() == 32)
            toks.append({"TOKEN" + L, db});
    }
    QByteArray wd;
    if (hex32(e.value("LDEX_WLEZ_DEF"), wd) && wd.size() == 32)
        toks.append({"LEZ", wd});
    QList<PairEntry> pairs;
    for (int i = 0; i < toks.size(); ++i)
        for (int j = i + 1; j < toks.size(); ++j)
            pairs.append({toks[i].first, toks[j].first,
                          toks[i].second, toks[j].second});
    return pairs;
}

QString LdexCorePlugin::pools()
{
    if (!ensureEnv())
        return QStringLiteral("[]");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty())
        return QStringLiteral("[]");
    const int tiers[4] = {1, 5, 30, 100};
    const QList<PairEntry> pairs = enumeratePairs(m_env);
    QString arr = QStringLiteral("[");
    bool first = true;
    for (const PairEntry& p : pairs) {
        const QString paHex = QString::fromLatin1(p.defA.toHex());
        const QString pbHex = QString::fromLatin1(p.defB.toHex());
        for (int i = 0; i < 4; ++i) {
            unsigned char buf[512];
            const int rc = ldex_amm_pool_info(
                c.cfg.constData(), c.store.constData(),
                reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
                reinterpret_cast<const uint8_t*>(p.defA.constData()),
                reinterpret_cast<const uint8_t*>(p.defB.constData()),
                static_cast<ldex_u128>(tiers[i]), buf, sizeof(buf));
            QString pj = rc == LDEX_AMM_OK
                ? QString::fromUtf8(reinterpret_cast<char*>(buf))
                : QStringLiteral("{\"exists\":false}");
            // splice {fee, pa, pb, symA, symB} into the returned object
            // ( pj starts with '{' )
            const QString obj = QStringLiteral(
                "{\"fee\":%1,\"pa\":\"%2\",\"pb\":\"%3\","
                "\"symA\":\"%4\",\"symB\":\"%5\",%6")
                .arg(tiers[i]).arg(paHex, pbHex, p.symA, p.symB, pj.mid(1));
            if (!first) arr += QStringLiteral(",");
            first = false;
            arr += obj;
        }
    }
    return arr + QStringLiteral("]");
}

QString LdexCorePlugin::analytics()
{
    if (!ensureEnv()) return QStringLiteral("{\"pools\":[],\"agg\":{}}");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("{\"pools\":[],\"agg\":{}}");
    const int tiers[4] = {1, 5, 30, 100};
    const QList<PairEntry> pairs = enumeratePairs(m_env);
    double aTvlA = 0, aTvlB = 0, aVolA = 0, aVolB = 0, aFrA = 0, aFrB = 0;
    int active = 0;
    QString rows = QStringLiteral("[");
    bool first = true;
    for (const PairEntry& p : pairs) {
        for (int i = 0; i < 4; ++i) {
            unsigned char pbuf[512];
            // Single on-chain read: TVL + EXACT cumulative volume + EXACT
            // cumulative LP fee revenue (RFP Usability #3) - both
            // maintained on-chain by amm_v2's swap_logic path.
            const int prc = ldex_amm_pool_info(
                c.cfg.constData(), c.store.constData(),
                reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
                reinterpret_cast<const uint8_t*>(p.defA.constData()),
                reinterpret_cast<const uint8_t*>(p.defB.constData()),
                static_cast<ldex_u128>(tiers[i]), pbuf, sizeof(pbuf));
            const QJsonObject po = prc == LDEX_AMM_OK
                ? QJsonDocument::fromJson(QByteArray(reinterpret_cast<char*>(pbuf))).object()
                : QJsonObject();
            const bool exists = po.value("exists").toBool();
            const double tvlA = po.value("reserve_a").toString().toDouble();
            const double tvlB = po.value("reserve_b").toString().toDouble();
            const double volA = po.value("cum_volume_a").toString().toDouble();
            const double volB = po.value("cum_volume_b").toString().toDouble();
            const double frA  = po.value("cum_fees_a").toString().toDouble();
            const double frB  = po.value("cum_fees_b").toString().toDouble();
            if (exists) { ++active; aTvlA += tvlA; aTvlB += tvlB;
                aVolA += volA; aVolB += volB; aFrA += frA; aFrB += frB; }
            if (!first) rows += QStringLiteral(",");
            first = false;
            rows += QStringLiteral(
                "{\"fee\":%1,\"symA\":\"%2\",\"symB\":\"%3\","
                "\"exists\":%4,\"tvlA\":%5,\"tvlB\":%6,"
                "\"volA\":%7,\"volB\":%8,\"feeRevA\":%9,\"feeRevB\":%10}")
                .arg(tiers[i]).arg(p.symA, p.symB)
                .arg(exists ? "true" : "false")
                .arg(tvlA, 0, 'f', 4).arg(tvlB, 0, 'f', 4)
                .arg(volA, 0, 'f', 4).arg(volB, 0, 'f', 4)
                .arg(frA, 0, 'f', 4).arg(frB, 0, 'f', 4);
        }
    }
    const QString agg = QStringLiteral("{\"tvlA\":%1,\"tvlB\":%2,"
        "\"volA\":%3,\"volB\":%4,\"feeRevA\":%5,\"feeRevB\":%6,"
        "\"activePools\":%7}")
        .arg(aTvlA, 0, 'f', 4).arg(aTvlB, 0, 'f', 4)
        .arg(aVolA, 0, 'f', 4).arg(aVolB, 0, 'f', 4)
        .arg(aFrA, 0, 'f', 4).arg(aFrB, 0, 'f', 4).arg(active);
    return QStringLiteral("{\"pools\":%1],\"agg\":%2}").arg(rows).arg(agg);
}


QString LdexCorePlugin::walletTokens()
{
    if (!ensureEnv()) return QStringLiteral("[]");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("[]");
    const QStringList letters = m_env.value("LDEX_TOKENS", "A B")
                                    .split(QChar(' '), Qt::SkipEmptyParts);
    WalletHandle* h = wallet_ffi_open(c.cfg.constData(), c.store.constData());
    if (!h) return QStringLiteral("[]");

    // NOTE: private (shielded) balance sync used to live here but it scans
    // every block from `last_synced` → `head` and that fires on EVERY
    // walletTokens call - which is every Balances panel re-render. On a
    // long-running chain that's hundreds-thousands of blocks per UI tick.
    // Sync was moved out to a dedicated Q_INVOKABLE that the QML calls on
    // a debounced timer + after every action; balances still update, just
    // without the per-render scan.

    // Sum balances per-letter across every holding the bootstrap recorded:
    //   - LDEX_HOLD_<L>  keypair public holding   (chain query)
    //   - LDEX_ATA_<L>   deterministic ATA        (chain query)
    //   - LDEX_PRIV_<L>  PrivateOwned             (wallet local cache)
    // and (for the public ones) any other public accounts the wallet has
    // that hold the same def - picks up disposable accounts left over from
    // a Fast/Disposable swap. One row per letter so the Balances view
    // mirrors the catalog 1:1.
    // Public token holdings store the token amount in `account.data`
    // (the token program's Fungible payload), NOT in `account.balance`
    // (which is the native LEZ balance of the account, always 0 for a
    // token holding). `wallet_ffi_get_balance` returns the native field,
    // so use the token-aware FFI for the public side. Private accounts
    // live in wallet-local cache and `wallet_ffi_get_balance(false, ...)`
    // returns the right amount for those.
    auto balanceOf = [&](const QString& idStr, bool isPublic) -> qulonglong {
        if (idStr.isEmpty()) return 0;
        QByteArray idBytes;
        if (!hex32(idStr, idBytes) || idBytes.size() != 32) return 0;
        if (isPublic) {
            unsigned char buf[256];
            if (ldex_amm_token_balance(c.cfg.constData(), c.store.constData(),
                    reinterpret_cast<const uint8_t*>(idBytes.constData()),
                    buf, sizeof(buf)) != LDEX_AMM_OK) return 0;
            const QJsonObject o = QJsonDocument::fromJson(
                QByteArray(reinterpret_cast<char*>(buf))).object();
            return o.value("balance").toString().toULongLong();
        }
        // PrivateOwned token holdings - `wallet_ffi_get_balance(is_public=false)`
        // returns the account's NATIVE LEZ balance, not the token amount.
        // Token holdings live in `account.data` (borsh-encoded TokenHolding;
        // for Fungible: [tag=0, def_id(32), balance_u128_le(16)] = 49 bytes).
        // Using get_balance here meant every shielded balance displayed as 0
        // in the UI - the "shielding doesn't work" symptom. Read the
        // private account via get_account_private and decode the data
        // field as TokenHolding::Fungible.
        FfiBytes32 fid; std::memcpy(fid.data, idBytes.constData(), 32);
        FfiAccount acc{};
        if (wallet_ffi_get_account_private(h, &fid, &acc) != SUCCESS) return 0;
        qulonglong v = 0;
        if (acc.data && acc.data_len >= 49 && acc.data[0] == 0 /* Fungible */) {
            // Low 8 bytes of u128 LE balance are at offset 33..40.
            for (int i = 7; i >= 0; --i) v = (v << 8) | acc.data[33 + i];
        }
        wallet_ffi_free_account_data(&acc);
        return v;
    };

    // Track per-letter (defStr, pubBal, privBal). Also collect known-letter
    // def hex so we can sum any *other* public accounts (e.g. disposable
    // account-A holdings left over from a mode-2 disposable swap) into
    // the right letter.
    QHash<QString, QString> hexToLetter;
    QHash<QString, qulonglong> perLetterPub;
    QHash<QString, qulonglong> perLetterPriv;
    QHash<QString, QString> perLetterDef;
    for (const QString& L : letters) {
        const QString defStr = m_env.value("LDEX_DEF_" + L);
        if (defStr.isEmpty()) continue;
        QByteArray defBytes;
        if (!hex32(defStr, defBytes)) continue;
        hexToLetter.insert(QString::fromLatin1(defBytes.toHex()), L);
        perLetterDef.insert(L, defStr);
        qulonglong p = 0;
        p += balanceOf(m_env.value("LDEX_HOLD_" + L), /*pub*/true);
        p += balanceOf(m_env.value("LDEX_ATA_"  + L), /*pub*/true);
        perLetterPub.insert(L, p);
        perLetterPriv.insert(L,
            balanceOf(m_env.value("LDEX_PRIV_" + L), /*pub*/false));
    }

    // Loose public accounts (disposable account-A holdings from a mode-2
    // disposable swap, etc.) - sum into pub balance, skipping the
    // canonical HOLD_/ATA_ ones we already counted.
    FfiAccountList list; list.entries = nullptr; list.count = 0;
    if (wallet_ffi_list_accounts(h, &list) == SUCCESS) {
        for (uintptr_t i = 0; i < list.count; ++i) {
            const FfiAccountListEntry& e = list.entries[i];
            if (!e.is_public) continue;
            unsigned char buf[256];
            if (ldex_amm_token_balance(c.cfg.constData(), c.store.constData(),
                    e.account_id.data, buf, sizeof(buf)) != LDEX_AMM_OK)
                continue;
            const QJsonObject o = QJsonDocument::fromJson(
                QByteArray(reinterpret_cast<char*>(buf))).object();
            const QString defHex = o.value("definition").toString().toLower();
            const QString balStr = o.value("balance").toString();
            if (defHex.isEmpty() || balStr.isEmpty()) continue;
            const QString letter = hexToLetter.value(defHex);
            if (letter.isEmpty()) continue;
            const auto sameAs = [&](const QString& canon) {
                QByteArray cb; if (!hex32(canon, cb) || cb.size() != 32) return false;
                return std::memcmp(cb.constData(), e.account_id.data, 32) == 0;
            };
            if (sameAs(m_env.value("LDEX_HOLD_" + letter)) ||
                sameAs(m_env.value("LDEX_ATA_"  + letter))) continue;
            perLetterPub[letter] += balStr.toULongLong();
        }
        wallet_ffi_free_account_list(&list);
    }
    wallet_ffi_destroy(h);

    QString arr = QStringLiteral("[");
    bool first = true;
    for (const QString& L : letters) {
        const QString defStr = perLetterDef.value(L);
        if (defStr.isEmpty()) continue;
        const qulonglong pb = perLetterPub.value(L, 0);
        const qulonglong pr = perLetterPriv.value(L, 0);
        const qulonglong tot = pb + pr;
        if (!first) arr += QStringLiteral(",");
        first = false;
        // Backwards-compat `balance` field = total (existing QML
        // bindings reading walletTokens[*].balance still work).
        // New fields: pubBalance, privBalance.
        arr += QStringLiteral(
            "{\"address\":\"\",\"definition\":\"%1\","
            "\"balance\":\"%2\",\"pubBalance\":\"%3\","
            "\"privBalance\":\"%4\",\"name\":\"%5\"}")
                .arg(defStr,
                     QString::number(tot),
                     QString::number(pb),
                     QString::number(pr),
                     QStringLiteral("TOKEN") + L);
    }
    return arr + QStringLiteral("]");
}

QString LdexCorePlugin::accounts()
{
    if (!ensureEnv()) return QStringLiteral("[]");
    const QByteArray cfg = m_env.value("LDEX_WALLET_CONFIG").toUtf8();
    const QByteArray st = m_env.value("LDEX_WALLET_STORAGE").toUtf8();
    WalletHandle* h = wallet_ffi_open(cfg.constData(), st.constData());
    if (!h) return QStringLiteral("[]");
    FfiAccountList list; list.entries = nullptr; list.count = 0;
    if (wallet_ffi_list_accounts(h, &list) != SUCCESS) {
        wallet_ffi_destroy(h); return QStringLiteral("[]");
    }
    QString arr = QStringLiteral("[");
    for (uintptr_t i = 0; i < list.count; ++i) {
        const FfiAccountListEntry& e = list.entries[i];
        char* b58 = wallet_ffi_account_id_to_base58(&e.account_id);
        const QString addr = b58 ? QString::fromUtf8(b58) : QString();
        if (b58) wallet_ffi_free_string(b58);
        arr += QStringLiteral("%1{\"address\":\"%2\",\"public\":%3}")
                   .arg(i ? QStringLiteral(",") : QString(), addr,
                        e.is_public ? QStringLiteral("true")
                                    : QStringLiteral("false"));
    }
    wallet_ffi_free_account_list(&list);
    wallet_ffi_destroy(h);
    return arr + QStringLiteral("]");
}

QString LdexCorePlugin::poolInfoFor(const QString& defAHex,
                                    const QString& defBHex, int feeBps)
{
    if (!ensureEnv()) return QStringLiteral("{\"exists\":false}");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("{\"exists\":false}");
    QByteArray da, db;
    if (!hex32(defAHex, da) || !hex32(defBHex, db))
        return QStringLiteral("{\"exists\":false}");
    // Guard against the amm_core panic on defA == defB.
    if (da == db)
        return QStringLiteral("{\"exists\":false}");
    unsigned char buf[512];
    // Pool discovery routes through amm_v2 (pools are amm_v2-owned).
    const int rc = ldex_amm_pool_info(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(da.constData()),
        reinterpret_cast<const uint8_t*>(db.constData()),
        static_cast<ldex_u128>(feeBps), buf, sizeof(buf));
    return rc == LDEX_AMM_OK
        ? QString::fromUtf8(reinterpret_cast<char*>(buf))
        : QStringLiteral("{\"exists\":false}");
}

QString LdexCorePlugin::priceHistory(const QString& defAHex,
                                     const QString& defBHex, int feeBps)
{
    if (!ensureEnv()) return QStringLiteral("[]");
    AmmCtx c = loadCtx(m_env);
    if (!c.err.isEmpty()) return QStringLiteral("[]");
    QByteArray da, db;
    if (!hex32(defAHex, da) || !hex32(defBHex, db))
        return QStringLiteral("[]");
    // Large buffer: ~40 B/point * a few thousand points.
    static thread_local char buf[262144];
    // §5.11③: the ON-CHAIN observation ring is the source of truth
    // (gapless by construction). The off-chain price_indexer
    // (ldex_amm_price_history) remains available for >ring archival.
    // amm_v2 pools intentionally skip the on-chain TWAP oracle (no
    // Clock account in any swap variant, keeps privacy proofs drift-
    // free on slow CPU). Result: an empty ring; the UI Analytics tab
    // still reads `cum_volume_*` and `cum_fees_*` via `poolInfoFor`.
    const int rc = ldex_amm_onchain_price_history(
        c.cfg.constData(), c.store.constData(),
        reinterpret_cast<const uint8_t*>(c.amm_v2.constData()),
        reinterpret_cast<const uint8_t*>(da.constData()),
        reinterpret_cast<const uint8_t*>(db.constData()),
        static_cast<ldex_u128>(feeBps),
        reinterpret_cast<unsigned char*>(buf), sizeof(buf));
    return rc == LDEX_AMM_OK ? QString::fromUtf8(buf) : QStringLiteral("[]");
}
