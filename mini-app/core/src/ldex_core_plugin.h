#ifndef LDEX_CORE_PLUGIN_H
#define LDEX_CORE_PLUGIN_H

#include <QObject>
#include <QString>
#include <QHash>
#include "ldex_core_interface.h"
#include "logos_api.h"
#include "logos_sdk.h"

/**
 * @brief LDEX core module plugin (walking skeleton).
 */
class LdexCorePlugin : public QObject, public LdexCoreInterface
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID LdexCoreInterface_iid FILE "metadata.json")
    Q_INTERFACES(LdexCoreInterface PluginInterface)

public:
    explicit LdexCorePlugin(QObject* parent = nullptr);
    ~LdexCorePlugin() override;

    // PluginInterface
    QString name() const override { return "ldex_core"; }
    QString version() const override { return "0.1.0"; }

    // LdexCoreInterface
    Q_INVOKABLE QString ping(const QString& msg) override;
    Q_INVOKABLE QString getStatus() override;
    Q_INVOKABLE QString walletProbe() override;
    Q_INVOKABLE QString chainHeight() override;
    Q_INVOKABLE QString ammPoolId(const QString& ammHex,
                                  const QString& tokenAHex,
                                  const QString& tokenBHex,
                                  int feeBps) override;
    Q_INVOKABLE QString devBootstrap() override;
    Q_INVOKABLE QString walletTokens() override;
    Q_INVOKABLE QString accounts() override;
    Q_INVOKABLE QString poolInfoFor(const QString& defAHex,
                                    const QString& defBHex,
                                    int feeBps) override;
    Q_INVOKABLE QString priceHistory(const QString& defAHex,
                                     const QString& defBHex,
                                     int feeBps) override;
    Q_INVOKABLE QString pools() override;
    Q_INVOKABLE QString analytics() override;
    Q_INVOKABLE QString walletCreate(const QString& homeDir,
                                     const QString& password,
                                     const QString& sequencerUrl) override;
    Q_INVOKABLE QString walletImport(const QString& homeDir,
                                     const QString& mnemonic,
                                     const QString& password,
                                     const QString& sequencerUrl) override;
    Q_INVOKABLE QString quoteFor(const QString& defAHex, const QString& defBHex,
                                 int direction, const QString& amountIn,
                                 int feeBps) override;
    Q_INVOKABLE QString swapExactInAtaFor(const QString& config,
                                          const QString& amountIn,
                                          const QString& minOut, int feeBps) override;
    Q_INVOKABLE QString createPoolFor(const QString& holdingAHex,
                                      const QString& holdingBHex,
                                      const QString& amountA, const QString& amountB,
                                      int feeBps) override;
    Q_INVOKABLE QString privateSwapFor(const QString& config,
                                       const QString& amountIn,
                                       const QString& minOut,
                                       int feeBps) override;
    Q_INVOKABLE QString privateSwapForStart(const QString& config,
                                            const QString& amountIn,
                                            const QString& minOut,
                                            int feeBps) override;
    Q_INVOKABLE QString privateSwapNativeFor(const QString& config,
                                             const QString& amountIn,
                                             const QString& minOut,
                                             int feeBps) override;
    Q_INVOKABLE QString privateSwapNativeForStart(const QString& config,
                                                  const QString& amountIn,
                                                  const QString& minOut,
                                                  int feeBps) override;
    Q_INVOKABLE QString jobStatus(int jobId) override;
    Q_INVOKABLE QString nativeBalance() override;
    Q_INVOKABLE QString wrapNative(const QString& amount) override;
    Q_INVOKABLE QString unwrapNative(const QString& amount) override;
    Q_INVOKABLE QString consolidateWlezToHoldW(const QString& amount) override;
    Q_INVOKABLE QString shieldToken(const QString& letter,
                                    const QString& amount) override;
    Q_INVOKABLE QString deshieldToken(const QString& letter,
                                      const QString& amount) override;
    // *Start variants spawn the underlying op on a worker thread and
    // return "job=N" immediately so the Logos host's QtRO 20-second
    // bridge timeout never fires. UI polls jobStatus(N). Without these,
    // every shield / deshield / pool-create attempt cascades into
    // "Failed to invoke callRemoteMethod / invalid response" - the
    // STARK proof + chain wait take 3-5 min, the bridge gives up at 20 s.
    Q_INVOKABLE QString shieldTokenStart(const QString& letter,
                                         const QString& amount);
    Q_INVOKABLE QString deshieldTokenStart(const QString& letter,
                                           const QString& amount);
    Q_INVOKABLE QString createPoolForStart(const QString& holdingAHex,
                                           const QString& holdingBHex,
                                           const QString& amountA,
                                           const QString& amountB,
                                           int feeBps);
    Q_INVOKABLE QString wrapNativeStart(const QString& amount);
    Q_INVOKABLE QString unwrapNativeStart(const QString& amount);
    Q_INVOKABLE QString syncPrivateBalances() override;
    Q_INVOKABLE QString privateAddLiquidity(int mode, const QString& minLp,
                                            const QString& maxA,
                                            const QString& maxB,
                                            int feeBps) override;
    Q_INVOKABLE QString privateRemoveLiquidity(int mode,
                                               const QString& lpAmount,
                                               const QString& minA,
                                               const QString& minB,
                                               int feeBps) override;
    Q_INVOKABLE QString addLiquidity(const QString& minLp, const QString& maxA,
                                     const QString& maxB, int feeBps) override;
    Q_INVOKABLE QString removeLiquidity(const QString& lpAmount,
                                        const QString& minA, const QString& minB,
                                        int feeBps) override;

private:
    QHash<QString, QString> m_env;   // cached scripts/bootstrap.env
    bool ensureEnv();                // lazy-load m_env; true if usable

    // LogosAPI initialization (called by the host)
    Q_INVOKABLE void initLogos(LogosAPI* logosAPIInstance);

signals:
    void eventResponse(const QString& eventName, const QVariantList& args);

private:
    LogosModules* logos = nullptr;
    bool m_initialized = false;
};

#endif // LDEX_CORE_PLUGIN_H
