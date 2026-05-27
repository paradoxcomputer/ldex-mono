import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

// LDEX — Uniswap-inspired UI. Light theme, hero swap card, pill nav,
// gradient CTA. Logic unchanged: native ldex_core via logos.callModule.
Item {
    id: root
    width: 920; height: 1000

    // Light/dark theme. `dark` flips the neutral palette; the blue
    // brand accent is shared across both. Bound (not readonly) so the
    // toggle re-themes the whole app live.
    property bool dark: false
    property color bg:     dark ? "#0a0e18" : "#f3f6fc"
    property color card:   dark ? "#141926" : "#ffffff"
    property color panel:  dark ? "#1c2333" : "#f4f7fc"
    property color stroke: dark ? "#2a3245" : "#e3e8f1"
    property color ink:    dark ? "#f1f4fa" : "#0c1220"
    property color sub:    dark ? "#94a0b8" : "#5b6478"
    // Brand: cobalt → sky blue. Same hex in light/dark; the gradient is
    // bright enough to read as a CTA in either palette.
    readonly property color brand:  "#2563eb"
    readonly property color brand2: "#60a5fa"
    // Soft-glow tint behind the busy overlay halo (10% alpha hint).
    readonly property color brandTint: dark ? "#1e3a8a" : "#dbeafe"
    property color okCol:  dark ? "#3fb950" : "#15803d"
    property color errCol: dark ? "#f85149" : "#dc2626"
    // Legacy aliases — kept so any external caller / mid-file
    // reference that still says `root.pink` resolves to the brand.
    readonly property color pink:  brand
    readonly property color pink2: brand2

    property bool loaded:false
    property var env:({}); property var tokens:[]; property var accts:[]
    property var poolRows:[]; property var history:[]
    property var stats:({})  // Usability #3 aggregate analytics
    property string status:"Loading dev setup…"
    // Auto-load on app boot so the user doesn't have to click "Load dev
    // setup" before any token pickers / quotes / swaps work. Deferred via
    // Qt.callLater so the QML scene finishes constructing first (otherwise
    // `loadDev` would try to populate model bindings on partially-built
    // visuals).
    Component.onCompleted: Qt.callLater(function(){
        if (typeof logos !== "undefined" && logos.callModule) loadDev();
    })
    property bool statusOk:true
    property string quoteOut:"—"; property string quoteSub:""

    // --- async action / loading-overlay state ---
    property bool   busy:false
    property string busyTitle:""
    property string busySub:""
    property bool   resultOpen:false
    property bool   resultOk:true
    property string resultMsg:""
    readonly property var proofMsgs:[
        "Deshielding your private balance…",
        "Generating the zero-knowledge proof…",
        "This can take a few minutes — your keys never leave this device.",
        "Re-shielding the output…",
        "Submitting to the sequencer…"]

    function setStatus(s,ok){ root.status=s; root.statusOk=(ok===undefined?true:ok) }
    // Wraps act() with a modal loading layer. The overlay paints before
    // the (currently synchronous) bridge call via Qt.callLater; once the
    // native module's threaded path (privateSwapAsync) is wired the
    // overlay animates throughout. Either way the user gets clear,
    // non-frozen-looking feedback and double-submits are blocked.
    function runAction(m,a,title,sub){
        if(root.busy){ setStatus("Already working — wait for the current op to settle.",false); return }
        if(typeof logos==="undefined"||!logos.callModule){ setStatus("Open inside Basecamp",false); return }
        root.busyTitle=title||"Working…";
        root.busySub=sub||"";
        root.resultOpen=false;
        root.busy=true;
        busyFailsafe.restart();
        // Use the ASYNC variant. The sync `callModule` uses a built-in
        // QtRO timeout of 20 s, which is fatal for private swaps (STARK
        // proof generation legitimately takes minutes — the timeout is
        // what produces `error 1 (InvalidMessage)` on the bridge).
        // 30-minute ceiling matches the busyFailsafe upper bound.
        var finished = false;
        function onResult(r){
            if (finished) return;
            finished = true;
            var ok=false;
            try{
                try{ var p=JSON.parse(r); if(typeof p==="string") r=p }catch(e){}
                ok=!(String(r).indexOf("failed")>=0||String(r).indexOf("Couldn't")>=0
                       ||String(r).indexOf("ERR")>=0||String(r).indexOf("rc=")>=0
                       ||String(r).indexOf("error")>=0);
            }catch(e){
                r="JS error in runAction callback: "+String(e);
                ok=false;
            }
            var h=root.history.slice(); h.unshift({time:Qt.formatDateTime(new Date(),"hh:mm:ss"),
                action:m, result:String(r), ok:ok}); root.history=h;
            root.busy=false;
            busyFailsafe.stop();
            root.resultOk=ok; root.resultMsg=String(r); root.resultOpen=true;
            if(ok && String(r).indexOf("tx=0x")>=0){
                root.confirmHead = String(r);
                root.confirmLeft = 16;
                setStatus(root.confirmHead+"  —  confirming… 16s", true);
                confirmTimer.restart();
                // Force a private-balance sync — the action may have
                // moved shielded balance, and we want the panel to
                // reflect it without waiting for the 30s debounce.
                maybeSyncPriv(true);
            } else {
                setStatus(r,ok);
            }
            refresh();
        }
        try{
            // Private swap legitimately takes minutes (STARK proof). The
            // SDK bridge has a hardcoded 20 s QtRO timeout that no async
            // call can extend, so we use a submit/poll pattern: the
            // plugin's *Start spawns a background thread and returns
            // `"job=<id>"` instantly, and we poll `jobStatus(id)` every
            // few seconds until done.
            // Every op that takes more than ~20 s (the Logos host's
            // hardcoded QtRO bridge timeout) MUST go through the *Start
            // submit/poll job pump. Otherwise the bridge times out
            // before the plugin replies, the UI gets "Failed to invoke
            // callRemoteMethod" / "invalid response", and *the plugin
            // keeps running the proof anyway* — blocking every other
            // call until the proof completes (cascading the timeout
            // across pool list refreshes, balance reads, etc.).
            //
            // Async ops + their Start dispatcher:
            //   privateSwapFor          → privateSwapForStart        (STARK ~10–15 min)
            //   privateSwapNativeFor    → privateSwapNativeForStart  (STARK ~15–25 min)
            //   shieldToken             → shieldTokenStart           (STARK ~3–5 min)
            //   deshieldToken           → deshieldTokenStart         (STARK ~3–5 min)
            //   createPoolFor           → createPoolForStart         (multi-tx, can exceed 20 s)
            //   wrapNative              → wrapNativeStart            (~15 s — within timeout
            //                                                         but cascades if previous
            //                                                         op blocked the plugin)
            //   unwrapNative            → unwrapNativeStart          (same)
            var startMap = {
                "privateSwapFor":       "privateSwapForStart",
                "privateSwapNativeFor": "privateSwapNativeForStart",
                "shieldToken":          "shieldTokenStart",
                "deshieldToken":        "deshieldTokenStart",
                "createPoolFor":        "createPoolForStart",
                "wrapNative":           "wrapNativeStart",
                "unwrapNative":         "unwrapNativeStart"
            };
            if (startMap[m]) {
                var startMethod = startMap[m];
                var startRaw = logos.callModule("ldex_core", startMethod, a);
                var startStr = String(startRaw);
                try{ var sp=JSON.parse(startStr); if(typeof sp==="string") startStr=sp }catch(e){}
                if (startStr.indexOf("job=") === 0) {
                    root.pendingJobId = parseInt(startStr.slice(4), 10);
                    root.pendingJobOnResult = onResult;
                    root.pendingJobAttempts = 0;
                    jobPollTimer.restart();
                    return;
                }
                // Validation error from *Start — no job spawned.
                onResult(startStr);
                return;
            }
            if (logos.callModuleAsync) {
                logos.callModuleAsync("ldex_core", m, a, onResult, 1800000);
            } else {
                // Older SDK fallback (no async; 20 s ceiling, only safe for
                // public/quick ops — privates will time out under this path).
                Qt.callLater(function(){ onResult(logos.callModule("ldex_core",m,a)) });
            }
        } catch(e) {
            onResult("JS error in runAction dispatch: "+String(e));
        }
    }
    // --- job polling for long-running ops -----------------------------
    // `pendingJobOnResult` is the runAction callback for the current job;
    // we re-enter it once the plugin reports a terminal state. Polling
    // every 5 s gives 720 attempts in the 60-min failsafe window — chosen
    // to cover real-mode mode-1 swaps on slower CPUs (measured 17 min for
    // mode-1 on this dev box; raised the cap so a 1.5x slower machine
    // doesn't false-fail). The plugin's internal busyFailsafe at 60 min
    // matches; jobStatus stays "pending" until the proof finishes.
    property int pendingJobId: 0
    property var pendingJobOnResult: null
    property int pendingJobAttempts: 0
    Timer{
        id: jobPollTimer
        interval: 5000
        repeat: false
        onTriggered: {
            if (!root.pendingJobOnResult || root.pendingJobId === 0) return;
            root.pendingJobAttempts++;
            if (root.pendingJobAttempts > 720) {
                var cb = root.pendingJobOnResult; root.pendingJobOnResult = null;
                cb("Timeout (60 min). Proof may still finish in the background — check seq.log.");
                return;
            }
            var s = logos.callModule("ldex_core","jobStatus",[root.pendingJobId]);
            try{ var p=JSON.parse(s); if(typeof p==="string") s=p }catch(e){}
            if (String(s) === "pending") {
                // Surface elapsed mm:ss in the busy overlay so the user
                // sees progress while a multi-minute proof generates.
                var sec = root.pendingJobAttempts * 5;
                var mm = Math.floor(sec / 60);
                var ss = sec % 60;
                root.busySub = "Proving privately — "
                    + (mm > 0 ? (mm + "m ") : "") + ss + "s elapsed";
                jobPollTimer.restart();
                return;
            }
            // Terminal — fire the callback (which clears busy/history).
            var cb2 = root.pendingJobOnResult; root.pendingJobOnResult = null;
            cb2(s);
        }
    }
    function jget(m,a){ if(typeof logos==="undefined"||!logos.callModule) return null;
        var r=logos.callModule("ldex_core",m,a);
        try{ var p=JSON.parse(r); if(typeof p==="string") p=JSON.parse(p); return p }catch(e){ return null } }
    function act(m,a){
        if(typeof logos==="undefined"||!logos.callModule){ setStatus("Open inside Basecamp",false); return }
        var r=logos.callModule("ldex_core",m,a);
        try{ var p=JSON.parse(r); if(typeof p==="string") r=p }catch(e){}
        var ok=!(String(r).indexOf("failed")>=0||String(r).indexOf("ERR")>=0
                 ||String(r).indexOf("rc=")>=0||String(r).indexOf("error")>=0);
        var h=root.history.slice(); h.unshift({time:Qt.formatDateTime(new Date(),"hh:mm:ss"),
            action:m, result:String(r), ok:ok}); root.history=h;
        if(ok && String(r).indexOf("tx=0x")>=0){ setStatus(r+"  —  waiting for block…",true); confirmTimer.restart() }
        else setStatus(r,ok);
        refresh();
    }
    function loadDev(){
        var e=jget("devBootstrap",[]);
        if(!e||typeof e!=="object"){ setStatus("Couldn't load dev setup — run scripts/bootstrap.sh",false); return }
        root.env=e; root.loaded=!!(e.LDEX_AMM_V2_PROGRAM_ID&&e.LDEX_USER_HOLDING_A);
        setStatus(root.loaded?"Dev setup loaded ✓":"bootstrap.env missing keys",root.loaded);
        if(root.loaded){
            // Seed pair from env (TOKENA/TOKENB are the only tokens with
            // pools on a fresh dev chain).
            root.refreshTokens();
            refresh();
            // Auto-correct selFee on first load: the default is 30 bps
            // but the only seeded pool is at 5 bps, so updateQuote()
            // would call findPool(.., 30) → null → "no pool" until the
            // user manually clicks the 0.05% fee button. Pick the first
            // available tier from the now-populated availFeesList.
            // Without this, the user sees "no pool" on every fresh open
            // until they touch a token selector.
            var avail = root.availFeesList;
            if (avail.length > 0 && avail.indexOf(root.selFee) < 0) {
                root.selFee = avail[0];
            }
            // Brute-force binding refresher: a Timer that fires every
            // 200 ms × 5 ticks bumps pairDirty so any function-call
            // bindings that cached the empty-tokA early-return get
            // re-evaluated against the loaded env. Without this some
            // bindings stay stale until the user clicks something.
            initRefreshTimer.restart();
            quoteTimer.restart();
        }
    }

    // Brute-force binding refresher for the first paint after loadDev.
    // Fires every 200 ms for ~1 s, bumping pairDirty each time. The
    // model: root.availFeesList bindings on the Swap-tab fee-tier buttons
    // and the pairValid()/pairHasPool() bindings on the "no pool" text
    // pick up the change and finally evaluate against the loaded env.
    Timer {
        id: initRefreshTimer
        interval: 200
        repeat: true
        property int ticks: 0
        onTriggered: {
            root.pairDirty++;
            // Re-check selFee against the (potentially-updated)
            // availFeesList; if 30-bps default isn't valid, jump to the
            // first existing tier so the quote line isn't permanently
            // "no pool".
            var avail = root.availFeesList;
            if (avail.length > 0 && avail.indexOf(root.selFee) < 0) {
                root.selFee = avail[0];
                quoteTimer.restart();
            }
            ticks++;
            if (ticks >= 5) { stop(); ticks = 0 }
        }
    }
    // Cached native LEZ balance. The Balances panel reads this via the
    // catalog's LEZ row instead of synchronously calling `nativeBalance`
    // inside `whitelistedBalances()` on every QML re-render — that was
    // firing a wallet-open + network RPC per render, including during
    // animations + binding cascades. Now we read it once per `refresh()`.
    property string nativeBal: "0"
    // Throttle the wallet's private-balance sync. It used to fire inside
    // `walletTokens` (every render) — scanning every block since
    // last_synced. Now we call it explicitly here, debounced by
    // syncSinceMs so back-to-back refreshes don't all trigger a full
    // sync.
    property double lastPrivSyncMs: 0
    function maybeSyncPriv(forceMs){
        var force = forceMs===true;
        var now = Date.now();
        // Sync at most every 30 s unless caller forces (e.g. just after a
        // private op). Private balances are local-cache reads — the only
        // way they change is sync_to_block walking new blocks.
        if (!force && (now - root.lastPrivSyncMs) < 30000) return;
        root.lastPrivSyncMs = now;
        try { logos.callModule("ldex_core","syncPrivateBalances",[]); } catch(e){}
    }
    function refresh(){
        if(!root.loaded) return;
        var t=jget("walletTokens",[]); if(t&&t.length!==undefined) root.tokens=t;
        if (root.env.LDEX_WLEZ_DEF) {
            var nat = jget("nativeBalance",[]);
            root.nativeBal = (nat === null || nat === undefined) ? "0" : String(nat);
        }
        // Background-sync private balances at most every 30s; the
        // post-action refresh below forces a sync.
        maybeSyncPriv(false);
        var ac=jget("accounts",[]); if(ac&&ac.length!==undefined) root.accts=ac;
        var p=jget("pools",[]); if(p&&p.length!==undefined) root.poolRows=p;
        var an=jget("analytics",[]); if(an&&typeof an==="object"&&an.agg) root.stats=an;
        updateQuote();
    }
    function feeVal(){ return root.selFee }
    function dirVal(){ return 0 }   // pay = tokA always
    function balOf(n){ for(var i=0;i<root.tokens.length;i++) if(root.tokens[i].name===n) return root.tokens[i].balance; return "—" }
    // Lookup helpers by def hex. Used by the private-swap dispatch to
    // refuse swaps whose pay-side shielded balance hasn't yet been
    // synced into the wallet's local cache — otherwise the FFI feeds
    // an empty pre-state to amm_v2 and the guest panics with
    // "chained-call pre-state must be an initialized Fungible token holding".
    function privBalanceForDef(defHex){
        if (!defHex || !root.tokens) return "0";
        var dl = String(defHex).toLowerCase();
        for (var i=0;i<root.tokens.length;i++){
            var t = root.tokens[i];
            if (t && String(t.definition).toLowerCase()===dl)
                return String(t.privBalance||"0");
        }
        return "0";
    }
    // Wallet view: only the 10 whitelisted defs, with the catalog's color
    // chip + canonical TOKEN<L> name. Anything else the wallet holds (LP
    // accounts, disposable accounts, ATA accounts, miscellaneous tokens)
    // is hidden from the main list so the panel stays clean.
    function whitelistedBalances(){
        var c = tokenCatalog();
        var out = [];
        for (var k=0;k<c.length;k++){
            var cat = c[k];
            var tot="0", pub="0", priv="0";
            if (cat.isNative) {
                // LEZ row = native LEZ (in user's authenticated_transfer
                // account) + WLEZ holding (under the SPL token program).
                // The user thinks of these together; both flow through
                // the WLEZ wrap/unwrap bridge. Read from the cached
                // `root.nativeBal` — calling `nativeBalance` directly
                // here would fire a network RPC on every QML re-render.
                var nat = root.nativeBal || "0";
                var wlez = "0";
                for (var jn=0;jn<root.tokens.length;jn++){
                    if (root.tokens[jn].definition === cat.def) {
                        wlez = root.tokens[jn].balance || "0"; break;
                    }
                }
                pub = nat;   // unwrapped (native, useful for tx fees)
                priv = wlez; // wrapped (WLEZ, used by the AMM as a pool side)
                // Sum into total (decimal strings within u64 range — JS
                // numbers are fine for the dev amount scale).
                tot = String((parseFloat(nat)||0) + (parseFloat(wlez)||0));
            } else {
                for (var j=0;j<root.tokens.length;j++){
                    var t = root.tokens[j];
                    if (t.definition === cat.def) {
                        tot = t.balance || "0";
                        pub = t.pubBalance || "0";
                        priv = t.privBalance || "0";
                        break;
                    }
                }
            }
            var nz = (tot !== "" && tot !== "0");
            if (!nz && !cat.funded && !cat.shielded) continue;
            out.push({name:cat.name, balance:tot, pubBalance:pub,
                      privBalance:priv, definition:cat.def, color:cat.color,
                      isNative: cat.isNative===true});
        }
        return out;
    }
    function updateQuote(){
        // Resolve amtIn's text via try/catch — if the TextField isn't in
        // scope yet (card not materialized), fall back to the bound
        // mirror `root.amountInText`. Bounded so a ReferenceError never
        // takes the whole function down.
        var amt = "";
        try { amt = amtIn.text } catch(e) { amt = root.amountInText || "" }
        if (!root.loaded || amt.length === 0) {
            root.quoteOut="—"; root.quoteSub=""; return
        }
        if(!root.tokA.def||!root.tokB.def||root.tokA.def===root.tokB.def){
            root.quoteOut="—"; root.quoteSub=""; return
        }
        var p = findPool(root.tokA, root.tokB, root.selFee);
        if (!p){ root.quoteOut="no pool"; root.quoteSub=""; return }
        var d = p.payIsA ? 0 : 1;
        var q = jget("quoteFor",[p.pa, p.pb, d, amt, root.selFee]);
        if(!q||!q.exists){ root.quoteOut="no pool"; root.quoteSub=""; return }
        root.quoteOut=q.out; root.quoteSub="impact "+q.impactPct+"%   ·   fee "+q.feePaid;
    }
    // Mirror of `amtIn.text` at the root scope so updateQuote can read the
    // live amount without depending on the swap-card's id resolution
    // (the TextField is created late in the QML tree).
    property string amountInText: "1000"
    function pairValid(){
        // Touch pairDirty so QML re-evaluates when refreshTokens()
        // bumps it on initial load (tokA/tokB objects are reassigned
        // wholesale; QML's tracker doesn't always invalidate `.def`
        // bindings, so we force the dep via the counter).
        var dep = pairDirty;  // eslint-disable-line no-unused-vars
        return root.loaded && root.tokA.def && root.tokB.def && root.tokA.def!==root.tokB.def;
    }
    // Whether a pool exists at the currently-selected fee tier.
    function pairHasPool(){
        var dep = pairDirty;  // eslint-disable-line no-unused-vars
        return root.pairValidProp && (findPool(root.tokA, root.tokB, root.selFee) !== null);
    }
    // Whether the selected pair matches the env's primary bootstrap pair —
    // private modes (1/2/3) currently only have plumbing for that pair.
    function isEnvPair(){
        var a=root.env.LDEX_DEF_A||"", b=root.env.LDEX_DEF_B||"";
        return (root.tokA.def===a && root.tokB.def===b)
            || (root.tokA.def===b && root.tokB.def===a);
    }
    // RFP Func #7 — user-editable slippage tolerance (%). Default 1.0.
    property real slipPct: 1.0
    // RFP Func #8 — toggle the public-mode source between the keypair
    // Public mode is now ATA-only (RFP Func #8): every trader holds tokens
    // in the deterministic `ATA(owner, definition)`. The previous keypair-
    // holding path is retired and the toggle is gone.

    // --- Token catalog (token-agnostic UI) ---
    // Reads the 10-token universe from `bootstrap.env`: `LDEX_TOKENS`
    // enumerates the letters (default "A B … J"); per-letter
    // `LDEX_DEF_<L>` / `LDEX_HOLD_<L>` / `LDEX_ATA_<L>` are the real
    // on-chain ids the bootstrap wrote. Tokens beyond `LDEX_FUND_LIMIT`
    // (default 8) exist on chain but the user's ATA isn't funded for
    // them — they appear in the picker but the ATA-swap path shows
    // "no funded ATA" until the user funds it from their keypair holding.
    property var customTokens: []
    function tokenColor(letter){
        // Per-token swatch palette — distinct hues for the dev token
        // universe (TOKENA..TOKENJ). Picked to read against both the
        // light and dark backgrounds without re-shading.
        var p = ["#2563eb","#0ea5e9","#10b981","#f59e0b","#f5ac37",
                 "#8b5cf6","#ec4899","#ef4444","#22d3ee","#94a3b8"];
        var idx = letter.charCodeAt(0) - 65;
        return p[idx % p.length];
    }
    function tokenCatalog(){
        var c = [];
        // ── Native LEZ (via WLEZ bridge) ───────────────────────────
        // Surface the gas token as a normal catalog entry. Its `def` is
        // the WLEZ token definition, its `hold` is the user's keypair
        // WLEZ holding from bootstrap. `isNative` is a hint the
        // Balances/Swap surfaces use to read the native-side balance
        // and to offer an explicit Wrap when the holding is short.
        if (root.env.LDEX_WLEZ_DEF) {
            // ATA(USER, WLEZ_DEF) is wired by bootstrap (a portion of
            // the pre-wrapped HOLD_W is token-transferred into the
            // ATA). When present, the native token participates in
            // mode-0 ATA swaps + ATA pool create on equal footing
            // with TOKENA..J.
            var ataW = root.env.LDEX_ATA_W || "";
            c.push({
                sym:"LEZ", name:"LEZ",
                def: root.env.LDEX_WLEZ_DEF,
                hold: root.env.LDEX_HOLD_W || "",
                ata: ataW,
                priv: "",
                funded: ataW.length>0,
                shielded: false,
                color: "#f5ac37",   // amber — matches LEZ branding
                isNative: true
            });
        }
        var letters = (root.env.LDEX_TOKENS || "A B").split(/\s+/);
        var fundLimit = parseInt(root.env.LDEX_FUND_LIMIT || "2", 10);
        for (var i=0;i<letters.length;i++){
            var L = letters[i]; if (!L) continue;
            var def  = root.env["LDEX_DEF_"+L]  || "";
            var hold = root.env["LDEX_HOLD_"+L] || "";
            var ata  = root.env["LDEX_ATA_"+L]  || "";
            var priv = root.env["LDEX_PRIV_"+L] || "";
            // Back-compat aliases for A/B if the env was written before the
            // multi-token bootstrap rewrite.
            if (!def  && L==="A") def  = root.env.LDEX_DEF_A  || "";
            if (!def  && L==="B") def  = root.env.LDEX_DEF_B  || "";
            if (!hold && L==="A") hold = root.env.LDEX_USER_HOLDING_A || "";
            if (!hold && L==="B") hold = root.env.LDEX_USER_HOLDING_B || "";
            if (!ata  && L==="A") ata  = root.env.LDEX_ATA_A  || "";
            if (!ata  && L==="B") ata  = root.env.LDEX_ATA_B  || "";
            c.push({
                sym:"TOKEN"+L, name:"TOKEN"+L,
                def:def, hold:hold, ata:ata, priv:priv,
                funded:(i<fundLimit),
                shielded:(priv.length>0),
                color:tokenColor(L)
            });
        }
        // Custom tokens are merged with no funded holding/ATA — useful for
        // looking up pools by pasting an arbitrary def_id.
        for (var j=0;j<root.customTokens.length;j++) c.push(root.customTokens[j]);
        return c;
    }
    // Find the pool for a (tokX, tokY, fee) tuple regardless of the
    // ordering in which it was created. AMM pool PDAs are not order-
    // canonicalised, so a pool created as (B, A) won't be found by
    // poolInfoFor(A, B). We try BOTH orderings and return whichever
    // exists (or null). `pa`/`pb` are the def ids in the pool's actual
    // a/b orientation — the swap direction needs that.
    function findPool(tokX, tokY, fee){
        if (!tokX.def || !tokY.def || tokX.def===tokY.def) return null;
        var p = jget("poolInfoFor",[tokX.def, tokY.def, fee]);
        if (p && p.exists) return {info:p, pa:tokX.def, pb:tokY.def, payIsA:true};
        p = jget("poolInfoFor",[tokY.def, tokX.def, fee]);
        if (p && p.exists) return {info:p, pa:tokY.def, pb:tokX.def, payIsA:false};
        return null;
    }
    // Currently-selected pair (start with bootstrap pair). pairDirty rebuilds
    // availFees when the user changes a side.
    property int pairDirty: 0
    property var tokA: ({sym:"TOKENA",def:""})
    property var tokB: ({sym:"TOKENB",def:""})

    // Bindable wrappers around the function-call diagnostics. QML's
    // dependency tracker for `model: root.availFeesList` and similar
    // function-call bindings sometimes caches the very first evaluation
    // (when tokA.def is still empty) and never re-evaluates even when
    // pairDirty bumps. These properties bind to expressions that
    // EXPLICITLY list every dep, guaranteeing re-evaluation on any
    // change. Repeater models and `visible:` conditions read these
    // properties instead of calling the functions directly.
    property var availFeesList: {
        // Force tracking of pairDirty, loaded, tokA, tokB.
        var d = pairDirty;
        if (!root.loaded || !root.tokA.def || !root.tokB.def) return [];
        var out = []; var tiers = [1, 5, 30, 100];
        for (var i = 0; i < tiers.length; i++) {
            if (root.findPool(root.tokA, root.tokB, tiers[i])) out.push(tiers[i]);
        }
        return out;
    }
    property bool pairValidProp: {
        var d = pairDirty;
        return root.loaded && root.tokA.def && root.tokB.def && root.tokA.def !== root.tokB.def;
    }
    property bool pairHasPoolProp: {
        var d = pairDirty;
        return pairValidProp && (root.findPool(root.tokA, root.tokB, root.selFee) !== null);
    }
    function tokenBySym(s){ var c=tokenCatalog();
        for (var i=0;i<c.length;i++) if (c[i].sym===s) return c[i];
        return {sym:s,name:s,def:"",color:"#888"}; }
    function refreshTokens(){
        // Sync tokA/tokB to the catalog (resolves the dynamic env-derived defs).
        root.tokA = tokenBySym(root.tokA.sym);
        root.tokB = tokenBySym(root.tokB.sym);
        root.pairDirty++;
    }
    // Available fee tiers for the current pair — recomputed when pairDirty
    // changes (i.e., either side picker fired). Only fees whose pool
    // exists on chain (in EITHER ordering) are surfaced in the swap card.
    function availFees(){
        // Read pairDirty FIRST so QML's binding tracker treats it as a
        // dependency even when the early-return below skips the lookup.
        // Without this, the very first evaluation happens when tokA.def is
        // still empty (early-return) and the binding is never tagged on
        // pairDirty, so subsequent bumps from refreshTokens() don't fire
        // a re-eval. Result: app opens, Swap shows "no pool" until the
        // user manually re-selects either side.
        var dep = pairDirty;  // eslint-disable-line no-unused-vars
        var ld = root.loaded;
        if (!ld || !root.tokA.def || !root.tokB.def) return [];
        var out=[]; var tiers=[1,5,30,100];
        for (var i=0;i<tiers.length;i++){
            if (findPool(root.tokA, root.tokB, tiers[i])) out.push(tiers[i]);
        }
        return out;
    }
    // Selected fee tier (must be in availFees() OR the only choice if empty).
    property int selFee: 30

    // Filter+sort for the Pools view. Source = the env-pair × 4 fee-tier
    // rows (poolRows from pools()) decorated with vol/tvl from analytics().
    // Search query matches the env's def_a/def_b or the literal tokena/
    // tokenb symbols; sort modes: 0=Volume desc, 1=Fee asc, 2=TVL desc.
    function sortedPoolList(query, sortMode){
        // Only existing pools — non-existent fee tiers are noise; users
        // create new pools via the dedicated "+ New pool" screen.
        var list = (root.poolRows||[]).filter(function(p){ return p.exists===true });
        var stats = (root.stats && root.stats.pools) || [];
        for (var i=0;i<list.length;i++){
            var p=list[i]; var vA=0,vB=0,tvlA=0,tvlB=0;
            for (var k=0;k<stats.length;k++){
                if (stats[k].fee===p.fee && stats[k].exists){
                    vA=stats[k].volA; vB=stats[k].volB;
                    tvlA=stats[k].tvlA; tvlB=stats[k].tvlB; break;
                }
            }
            p.volA=vA; p.volB=vB; p.tvlA=tvlA; p.tvlB=tvlB;
            p.volSum=(parseFloat(vA)||0)+(parseFloat(vB)||0);
            p.tvlSum=(parseFloat(tvlA)||0)+(parseFloat(tvlB)||0);
        }
        var q = (query||"").trim().toLowerCase();
        if (q.length>0){
            list = list.filter(function(p){
                var sa=(p.symA||"").toLowerCase();
                var sb=(p.symB||"").toLowerCase();
                var pa=(p.pa||"").toLowerCase();
                var pb=(p.pb||"").toLowerCase();
                return sa.indexOf(q)>=0 || sb.indexOf(q)>=0
                    || pa.indexOf(q)>=0 || pb.indexOf(q)>=0
                    || (sa+"/"+sb).indexOf(q)>=0;
            });
        }
        if (sortMode===0)      list.sort(function(a,b){ return (b.volSum||0)-(a.volSum||0); });
        else if (sortMode===1) list.sort(function(a,b){ return (a.fee||0)-(b.fee||0); });
        else                   list.sort(function(a,b){ return (b.tvlSum||0)-(a.tvlSum||0); });
        return list;
    }
    function minRecv(){
        var o=parseFloat(root.quoteOut); if(isNaN(o)) return "1";
        var t=Math.max(0, Math.min(50, root.slipPct))/100;  // clamp 0–50%
        return String(Math.max(1, Math.floor(o*(1-t))))
    }
    // RFP Usability #8 — effective price = out/in, oriented by swap direction.
    function effPrice(){
        // Same boot-time guard as updateQuote.
        if (typeof amtIn === "undefined") return "—";
        var i=parseFloat(amtIn.text), o=parseFloat(root.quoteOut);
        if(isNaN(i)||i<=0||isNaN(o)||o<=0) return "—";
        var p=o/i; var unit=root.dirVal()===0?"B/A":"A/B";
        return p.toFixed(p<1?6:4)+" "+unit;
    }
    function defA(){ return root.env.LDEX_DEF_A||"" }
    function defB(){ return root.env.LDEX_DEF_B||"" }

    Timer{ id:quoteTimer; interval:600; repeat:false; onTriggered:root.updateQuote() }
    // Confirm window — 1 s ticks, 16 s total (one block). Shows the
    // remaining seconds in the toast so the user can see progress.
    // On the final tick we refresh balances + clear the toast.
    property string confirmHead: ""    // "Pool created. tx=0x…"
    property int confirmLeft: 0
    Timer{ id:confirmTimer; interval:1000; repeat:true; running:false
        onRunningChanged: if(running){ root.confirmLeft = 16 }
        onTriggered:{
            root.confirmLeft -= 1;
            if (root.confirmLeft <= 0){
                running = false;
                root.refresh();
                root.setStatus(root.confirmHead+"  ✓ Confirmed.", true);
            } else {
                root.setStatus(root.confirmHead+"  —  confirming… "
                               +root.confirmLeft+"s", true);
            }
        }
    }
    // Failsafe — if a bridge call never returns / errors out invisibly,
    // unstick the busy state after 60 minutes (real-proof upper bound).
    Timer{ id:busyFailsafe; interval:3600000; repeat:false; running:false
        onTriggered:{ if(root.busy){ root.busy=false;
            root.setStatus("Bridge timeout — op may still be proving in the background. Refresh to check.",false) } } }

    Rectangle{ anchors.fill:parent; gradient:Gradient{
        GradientStop{ position:0.0; color:root.dark ? "#0e1424" : "#eaf1fb" }
        GradientStop{ position:1.0; color:root.bg } } }

    // Token-picker plumbing: pickerSide ∈ {0,1} = which side (A/B) the open
    // popup is editing.
    property int pickerSide: 0
    function setPick(side, tok){
        // Avoid setting both sides to the same token (forces a real pair).
        if (side===0){
            if (tok.def && tok.def===root.tokB.def) root.tokB=root.tokA;
            root.tokA=tok;
        } else {
            if (tok.def && tok.def===root.tokA.def) root.tokA=root.tokB;
            root.tokB=tok;
        }
        root.pairDirty++;
        // If the selected fee tier is no longer available, fall back to the
        // first available one (or 30 bps if none).
        var avail=root.availFeesList;
        if (avail.length>0 && avail.indexOf(root.selFee)<0) root.selFee=avail[0];
        quoteTimer.restart();
    }
    Popup {
        id: tokenPicker
        modal: true; focus: true
        x: (root.width  - width)/2; y: (root.height - height)/2
        width: 360; height: 460
        background: Rectangle { color: root.card; border.color: root.stroke; radius: 18 }
        ColumnLayout {
            anchors.fill: parent; anchors.margins: 18; spacing: 8
            Text { text: "Select token ("+(root.pickerSide===0?"pay":"receive")+")"
                color: root.ink; font.pixelSize: 16; font.weight: Font.Bold }
            // Custom def_id paste field.
            RowLayout { Layout.fillWidth: true; spacing: 6
                TextField { id: customDef; Layout.fillWidth: true
                    placeholderText: "paste Public/… definition id"
                    font.pixelSize: 11
                    background: Rectangle { radius: 10; color: root.panel; border.color: root.stroke } }
                Rectangle { implicitHeight: 30; implicitWidth: 64; radius: 10; color: root.brand
                    MouseArea { anchors.fill: parent
                        onClicked: {
                            var d = customDef.text.trim();
                            if (d.length === 0) return;
                            var sym = "Custom-"+d.slice(d.length-4);
                            var t = { sym: sym, name: "Custom token", def: d, color: "#888" };
                            // De-dup against catalog
                            var c = root.tokenCatalog(); var seen = false;
                            for (var i=0;i<c.length;i++) if (c[i].def===d){ t=c[i]; seen=true; break; }
                            if (!seen) root.customTokens = root.customTokens.concat([t]);
                            root.setPick(root.pickerSide, t);
                            customDef.text = "";
                            tokenPicker.close();
                        } }
                    Text { anchors.centerIn: parent; text: "Add"
                        color: "white"; font.pixelSize: 12; font.weight: Font.Bold } } }
            // Whitelist + customs.
            ScrollView { Layout.fillWidth: true; Layout.fillHeight: true
                contentWidth: availableWidth
                ColumnLayout {
                    width: parent.width; spacing: 4
                    Repeater {
                        model: root.tokenCatalog()
                        delegate: Rectangle {
                            Layout.fillWidth: true; Layout.preferredHeight: 44
                            radius: 12; color: root.panel; border.color: root.stroke
                            MouseArea { anchors.fill: parent
                                onClicked: { root.setPick(root.pickerSide, modelData); tokenPicker.close() } }
                            RowLayout { anchors.fill: parent; anchors.margins: 10; spacing: 10
                                Rectangle { width: 22; height: 22; radius: 11; color: modelData.color || "#888" }
                                ColumnLayout { spacing: 0
                                    Text { text: modelData.sym; color: root.ink
                                        font.pixelSize: 13; font.weight: Font.DemiBold }
                                    Text { text: modelData.name; color: root.sub; font.pixelSize: 10 } }
                                Item { Layout.fillWidth: true }
                                Text { visible: modelData.def && modelData.def.length>0
                                    text: modelData.def.length>20 ? (modelData.def.substring(0,12)+"…"+modelData.def.substring(modelData.def.length-6)) : modelData.def
                                    color: root.sub; font.pixelSize: 10 }
                                Text { visible: !modelData.def || modelData.def.length===0
                                    text: "no def — bootstrap to enable"
                                    color: root.errCol; font.pixelSize: 10; font.italic: true } } } } } }
            Text { Layout.fillWidth: true; color: root.sub; font.pixelSize: 10; wrapMode: Text.WordWrap
                text: "Bootstrap pair (TOKENA/TOKENB) is the only pair with on-chain liquidity in dev. Custom defs enable pool lookup; create a pool in the Pools tab first." }
        }
    }

    StackView{ id:nav; anchors.fill:parent; initialItem:mainPage }

    // ===== modal loading / result overlay (topmost) =====
    Item {
        id: overlay
        anchors.fill:parent
        visible: opacity > 0.01
        opacity: (root.busy || root.resultOpen) ? 1 : 0
        Behavior on opacity { NumberAnimation { duration:220; easing.type:Easing.OutCubic } }

        // dim, click-absorbing backdrop (modal)
        Rectangle {
            anchors.fill:parent; color:"#0d111c"; opacity:0.55
            MouseArea { anchors.fill:parent; hoverEnabled:true
                onClicked:{ if(root.resultOpen && !root.busy) root.resultOpen=false } } }

        // staged-message cycler (animates on the threaded path)
        property int msgIdx:0
        Timer { interval:2600; repeat:true; running:root.busy
            onRunningChanged: if(running) overlay.msgIdx=0
            onTriggered: overlay.msgIdx=(overlay.msgIdx+1)%root.proofMsgs.length }

        // center card
        Rectangle {
            id:card
            anchors.centerIn:parent
            width:380
            height:cardCol.implicitHeight+56
            radius:26; color:root.card
            border.color:root.stroke
            scale: overlay.opacity
            Behavior on scale { NumberAnimation { duration:240; easing.type:Easing.OutBack } }

            // soft halo
            Rectangle { anchors.centerIn:parent; width:parent.width+26; height:parent.height+26
                radius:34; color:"transparent"; border.color:root.brand; border.width:1; opacity:0.12; z:-1 }

            ColumnLayout {
                id:cardCol
                anchors.centerIn:parent
                width:parent.width-56
                spacing:18

                // ---- LOADING ----
                Item {
                    visible:root.busy
                    Layout.alignment:Qt.AlignHCenter
                    Layout.preferredWidth:96; Layout.preferredHeight:96
                    // pulsing glow
                    Rectangle { anchors.centerIn:parent; width:84; height:84; radius:42
                        color:root.brand; opacity:0.10
                        SequentialAnimation on scale { running:root.busy; loops:Animation.Infinite
                            NumberAnimation { from:0.7; to:1.15; duration:1100; easing.type:Easing.InOutQuad }
                            NumberAnimation { from:1.15; to:0.7; duration:1100; easing.type:Easing.InOutQuad } } }
                    // faint track ring
                    Rectangle { anchors.centerIn:parent; width:72; height:72; radius:36
                        color:"transparent"; border.color:root.stroke; border.width:6 }
                    // orbiting comet
                    Item { anchors.centerIn:parent; width:72; height:72
                        NumberAnimation on rotation { running:root.busy; loops:Animation.Infinite
                            from:0; to:360; duration:1050 }
                        Rectangle { width:14; height:14; radius:7
                            anchors.horizontalCenter:parent.horizontalCenter; y:-1
                            gradient:Gradient{
                                GradientStop{ position:0.0; color:root.brand }
                                GradientStop{ position:1.0; color:root.brand2 } } } }
                }
                Text { visible:root.busy; Layout.alignment:Qt.AlignHCenter
                    text:root.busyTitle; color:root.ink
                    font.pixelSize:19; font.weight:Font.Bold }
                Text { visible:root.busy && root.busySub.length>0
                    Layout.fillWidth:true; horizontalAlignment:Text.AlignHCenter
                    wrapMode:Text.WordWrap; text:root.busySub
                    color:root.sub; font.pixelSize:13 }
                Text { visible:root.busy
                    Layout.fillWidth:true; horizontalAlignment:Text.AlignHCenter
                    wrapMode:Text.WordWrap; text:root.proofMsgs[overlay.msgIdx]
                    color:root.brand; font.pixelSize:12
                    Behavior on opacity { NumberAnimation { duration:200 } } }
                // indeterminate shimmer bar
                Rectangle { visible:root.busy; Layout.fillWidth:true
                    Layout.preferredHeight:5; radius:3; color:root.panel; clip:true
                    Rectangle { width:parent.width*0.4; height:parent.height; radius:3
                        gradient:Gradient{ orientation:Gradient.Horizontal
                            GradientStop{ position:0.0; color:root.brand }
                            GradientStop{ position:1.0; color:root.brand2 } }
                        NumberAnimation on x { running:root.busy; loops:Animation.Infinite
                            from:-card.width*0.4; to:card.width; duration:1300 } } }
                Text { visible:root.busy; Layout.alignment:Qt.AlignHCenter
                    text:"Keep this open — proving happens on your device."
                    color:root.sub; font.pixelSize:10 }

                // ---- RESULT ----
                Rectangle { visible:root.resultOpen && !root.busy
                    Layout.alignment:Qt.AlignHCenter
                    width:64; height:64; radius:32
                    color: root.resultOk ? "#e7f6ec" : "#fdeaea"
                    scale: (root.resultOpen && !root.busy) ? 1 : 0
                    Behavior on scale { NumberAnimation { duration:300; easing.type:Easing.OutBack } }
                    Text { anchors.centerIn:parent
                        text: root.resultOk ? "✓" : "✕"
                        color: root.resultOk ? root.okCol : root.errCol
                        font.pixelSize:32; font.weight:Font.Bold } }
                Text { visible:root.resultOpen && !root.busy
                    Layout.alignment:Qt.AlignHCenter
                    text: root.resultOk ? "Success" : "Couldn’t complete"
                    color:root.ink; font.pixelSize:19; font.weight:Font.Bold }
                TextEdit { visible:root.resultOpen && !root.busy
                    Layout.fillWidth:true; horizontalAlignment:TextEdit.AlignHCenter
                    wrapMode:TextEdit.WrapAnywhere; text:root.resultMsg
                    readOnly:true; selectByMouse:true
                    color:root.sub; font.pixelSize:12 }
                Rectangle { visible:root.resultOpen && !root.busy
                    Layout.alignment:Qt.AlignHCenter
                    width:140; height:42; radius:14
                    gradient:Gradient{
                        GradientStop{ position:0.0; color:root.brand }
                        GradientStop{ position:1.0; color:root.brand2 } }
                    MouseArea { anchors.fill:parent; onClicked:root.resultOpen=false }
                    Text { anchors.centerIn:parent; text:"Done"
                        color:"white"; font.pixelSize:14; font.weight:Font.Bold } }
            }
        }

        // auto-dismiss a successful result
        Timer { interval:6000; running:root.resultOpen && !root.busy && root.resultOk
            onTriggered:root.resultOpen=false }
    }

    // soft "pill" button factory look is inlined per-button for clarity.

    Component {
        id: mainPage
        Item {
            ColumnLayout {
                anchors.fill:parent; spacing:0

                // top bar
                RowLayout {
                    Layout.fillWidth:true; Layout.margins:22
                    Rectangle{ width:34; height:34; radius:17; color:root.brand }
                    Text{ text:"  LDEX"; color:root.ink; font.pixelSize:22; font.weight:Font.Bold }
                    Item{ Layout.fillWidth:true }
                    // pill tab nav
                    Rectangle{ radius:22; color:root.card; border.color:root.stroke
                        implicitHeight:44; implicitWidth:tabRow.implicitWidth+10
                        RowLayout{ id:tabRow; anchors.centerIn:parent; spacing:2
                            Repeater{ model:["Swap","Pools","Account"]
                                delegate:Rectangle{
                                    radius:18; implicitHeight:36
                                    implicitWidth:tlbl.implicitWidth+30
                                    color: tabs.currentIndex===index ? root.brand : "transparent"
                                    MouseArea{ anchors.fill:parent; onClicked:tabs.currentIndex=index }
                                    Text{ id:tlbl; anchors.centerIn:parent; text:modelData
                                        font.pixelSize:14; font.weight:Font.DemiBold
                                        color: tabs.currentIndex===index ? "white" : root.sub } } } } }
                    Item{ Layout.preferredWidth:14 }
                    // light/dark toggle
                    Rectangle{
                        implicitWidth:40; implicitHeight:40; radius:20
                        color:root.card; border.color:root.stroke
                        MouseArea{ anchors.fill:parent; onClicked:root.dark=!root.dark }
                        Text{ anchors.centerIn:parent
                            text: root.dark ? "☀" : "☾"
                            color:root.ink; font.pixelSize:18 }
                    }
                    Item{ Layout.preferredWidth:14 }
                    Rectangle{
                        radius:20; implicitHeight:40; implicitWidth:clbl.implicitWidth+34
                        color:root.loaded ? root.card : root.brand
                        border.color: root.loaded ? root.stroke : "transparent"
                        MouseArea{ anchors.fill:parent; onClicked: root.loaded?root.refresh():root.loadDev() }
                        Text{ id:clbl; anchors.centerIn:parent
                            text: root.loaded ? "Refresh" : "Load dev setup"
                            font.pixelSize:14; font.weight:Font.DemiBold
                            color: root.loaded ? root.ink : "white" } }
                }

                // hidden index holder
                Item{ id:tabs; property int currentIndex:0 }

                StackLayout {
                    id: tabStack
                    Layout.fillWidth:true; Layout.fillHeight:true
                    currentIndex: tabs.currentIndex

                    // ===== SWAP (hero) =====
                    // Wrapped in a ScrollView so the swap card stays
                    // reachable when the window is small (the Logos host
                    // can open the mini-app in a narrow frame; without
                    // scrolling the bottom of the card disappears).
                    ScrollView {
                        // contentItem is a Flickable; tell it how much
                        // content there is so the vertical scrollbar
                        // appears when the swap card is taller than the
                        // visible viewport. `anchors.fill:parent` on
                        // ScrollView itself + an inner ColumnLayout sized
                        // explicitly via implicitHeight makes the bar
                        // appear AsNeeded on small windows.
                        clip: true
                        ScrollBar.vertical.policy: ScrollBar.AsNeeded
                        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                        contentWidth: availableWidth
                        contentHeight: swapCol.implicitHeight + 36
                        ColumnLayout {
                            id: swapCol
                            // Centred horizontally within the visible
                            // viewport, anchored to the top of the
                            // ScrollView's contentItem so it pushes
                            // content size below the fold.
                            anchors.top: parent.top
                            anchors.topMargin: 18
                            anchors.horizontalCenter: parent.horizontalCenter
                            width: Math.min(480, parent.width - 24)
                            spacing: 14

                            Rectangle {
                                Layout.fillWidth:true; radius:24; color:root.card
                                border.color:root.stroke
                                Layout.preferredHeight:swc.implicitHeight+36
                                ColumnLayout{
                                    id:swc; anchors.fill:parent; anchors.margins:18; spacing:8
                                    // Title row + slippage (RFP Func #7, now top-right).
                                    RowLayout{ Layout.fillWidth:true; spacing:6
                                        Text{ text:"Swap"; color:root.ink; font.pixelSize:18; font.weight:Font.Bold }
                                        Item{ Layout.fillWidth:true }
                                        Text{ text:"Slippage"; color:root.sub; font.pixelSize:11 }
                                        TextField{ id:slipFld; Layout.preferredWidth:54
                                            text: root.slipPct.toFixed(2)
                                            horizontalAlignment:TextInput.AlignRight
                                            inputMethodHints:Qt.ImhFormattedNumbersOnly
                                            font.pixelSize:11
                                            background:Rectangle{ radius:10; color:root.panel; border.color:root.stroke }
                                            onEditingFinished:{ var v=parseFloat(text);
                                                if(!isNaN(v)&&v>=0&&v<=50){ root.slipPct=v } else { text=root.slipPct.toFixed(2) } } }
                                        Text{ text:"%"; color:root.sub; font.pixelSize:11 }
                                        Repeater{ model:[0.1, 0.5, 1.0, 3.0]
                                            delegate:Rectangle{ implicitHeight:22
                                                implicitWidth:plbl.implicitWidth+12; radius:11
                                                color: Math.abs(root.slipPct-modelData)<0.001 ? root.brand : root.panel
                                                border.color:root.stroke
                                                MouseArea{ anchors.fill:parent
                                                    onClicked:{ root.slipPct=modelData; slipFld.text=modelData.toFixed(2) } }
                                                Text{ id:plbl; anchors.centerIn:parent
                                                    text:modelData.toFixed(modelData<1?1:0)+"%"
                                                    color: Math.abs(root.slipPct-modelData)<0.001 ? "white" : root.sub
                                                    font.pixelSize:10 } } } }

                                    // pay panel
                                    Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:88
                                        radius:20; color:root.panel
                                        ColumnLayout{ anchors.fill:parent; anchors.margins:14; spacing:4
                                            Text{ text:"You pay"; color:root.sub; font.pixelSize:12 }
                                            RowLayout{ Layout.fillWidth:true
                                                TextField{ id:amtIn; text:"1000"; Layout.fillWidth:true
                                                    color:root.ink; font.pixelSize:28; font.weight:Font.DemiBold
                                                    selectByMouse:true
                                                    background:Rectangle{ color:"transparent" }
                                                    onTextChanged:{ root.amountInText = text; quoteTimer.restart() }
                                                    Component.onCompleted: { root.amountInText = text; quoteTimer.restart() } }
                                                // Token-A picker chip
                                                Rectangle{ radius:18; height:38; width:tpA.implicitWidth+44
                                                    color:root.card; border.color:root.stroke
                                                    MouseArea{ anchors.fill:parent
                                                        onClicked:{ pickerSide=0; tokenPicker.open() } }
                                                    RowLayout{ anchors.centerIn:parent; spacing:6
                                                        Rectangle{ width:18;height:18;radius:9
                                                            color: root.tokA.color || root.brand }
                                                        Text{ id:tpA; text:root.tokA.sym||"select"
                                                            color:root.ink; font.pixelSize:15; font.weight:Font.DemiBold }
                                                        Text{ text:"▾"; color:root.sub; font.pixelSize:10 } } } }
                                            Text{ text: root.loaded ? ("Balance "+root.balOf(root.tokA.sym)) : ""
                                                color:root.sub; font.pixelSize:11 } } }

                                    // swap direction circle (swaps tokA <-> tokB)
                                    Rectangle{ Layout.alignment:Qt.AlignHCenter
                                        width:38; height:38; radius:19; color:root.card
                                        border.color:root.stroke
                                        MouseArea{ anchors.fill:parent
                                            onClicked:{ var t=root.tokA; root.tokA=root.tokB; root.tokB=t;
                                                root.pairDirty++; quoteTimer.restart() } }
                                        Text{ anchors.centerIn:parent; text:"↓"; color:root.brand
                                            font.pixelSize:18; font.weight:Font.Bold } }

                                    // receive panel
                                    Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:88
                                        radius:20; color:root.panel
                                        ColumnLayout{ anchors.fill:parent; anchors.margins:14; spacing:4
                                            Text{ text:"You receive (est.)"; color:root.sub; font.pixelSize:12 }
                                            RowLayout{ Layout.fillWidth:true
                                                Text{ text:root.quoteOut; Layout.fillWidth:true
                                                    color:root.ink; font.pixelSize:28; font.weight:Font.DemiBold }
                                                // Token-B picker chip
                                                Rectangle{ radius:18; height:38; width:tpB.implicitWidth+44
                                                    color:root.card; border.color:root.stroke
                                                    MouseArea{ anchors.fill:parent
                                                        onClicked:{ pickerSide=1; tokenPicker.open() } }
                                                    RowLayout{ anchors.centerIn:parent; spacing:6
                                                        Rectangle{ width:18;height:18;radius:9
                                                            color: root.tokB.color || root.brand2 }
                                                        Text{ id:tpB; text:root.tokB.sym||"select"
                                                            color:root.ink; font.pixelSize:15; font.weight:Font.DemiBold }
                                                        Text{ text:"▾"; color:root.sub; font.pixelSize:10 } } } }
                                            Text{ text:root.quoteSub; color:root.sub; font.pixelSize:11 } } }

                                    // Fee tier — discovered AFTER the pair is set, and only shows
                                    // tiers whose pool exists on chain.
                                    RowLayout{ Layout.fillWidth:true; spacing:6
                                        Text{ text:"Fee tier"; color:root.sub; font.pixelSize:12 }
                                        Item{ Layout.fillWidth:true }
                                        Text{ visible:!root.pairValidProp; color:root.errCol; font.pixelSize:11
                                            text: !root.loaded
                                                ? "Loading dev setup…"
                                                : (!root.tokA.def || !root.tokB.def
                                                    ? "Select a token for both sides."
                                                    : (root.tokA.def===root.tokB.def
                                                        ? "Pick two different tokens."
                                                        : "No pool for this pair — create one in the Pools tab.")) }
                                        Repeater{ visible:root.pairValidProp; model:root.availFeesList
                                            delegate:Rectangle{ implicitHeight:30
                                                implicitWidth:flbl.implicitWidth+22; radius:14
                                                color: root.selFee===modelData ? root.brand : root.panel
                                                border.color: root.selFee===modelData ? root.brand : root.stroke
                                                MouseArea{ anchors.fill:parent
                                                    onClicked:{ root.selFee=modelData; quoteTimer.restart() } }
                                                Text{ id:flbl; anchors.centerIn:parent
                                                    text:(modelData/100).toFixed(2)+"%"
                                                    font.pixelSize:12
                                                    font.weight: root.selFee===modelData?Font.Bold:Font.Normal
                                                    color: root.selFee===modelData ? "white" : root.ink } } }
                                        Text{ visible:root.pairValidProp&&root.availFeesList.length===0
                                            color:root.errCol; font.pixelSize:11
                                            text:"No pool at any fee tier. Create one in the Pools tab." } }

                                    // privacy mode (per-trade, design.md §5.10).
                                    // Mode 2 (Private-Disposable, RFP-literal account-A) is
                                    // the default: atomic deshield→swap→reshield in one proof
                                    // (router shape, ~3 min on CPU with the monolithic guest).
                                    // Mode 1 (full PrivateOwned) hides the public address
                                    // entirely; same proof cost as mode-2 on idle CPU.
                                    // Mode 0 (Public) skips the proof and settles in one block.
                                    // (Former mode-3 "Fast non-atomic" was removed once mode-2
                                    // monolithic dropped to the same cycle count as mode-1.)
                                    Item{ id:privSel; property int mode:2 }
                                    Text{ text:"Privacy"; color:root.sub; font.pixelSize:12 }
                                    RowLayout{ Layout.fillWidth:true; spacing:6
                                        Repeater{ model:["Public","Private","Disposable"]
                                            Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:34
                                                radius:14
                                                color: privSel.mode===index ? root.brand : root.panel
                                                border.color: privSel.mode===index ? root.brand : root.stroke
                                                MouseArea{ anchors.fill:parent; onClicked:privSel.mode=index }
                                                Text{ anchors.centerIn:parent; text:modelData
                                                    font.pixelSize:12
                                                    font.weight: privSel.mode===index?Font.Bold:Font.Normal
                                                    color: privSel.mode===index ? "white" : root.ink } } } }
                                    Rectangle{ Layout.fillWidth:true; radius:12
                                        color:root.panel; border.color:root.stroke
                                        Layout.preferredHeight:discl.implicitHeight+20
                                        Text{ id:discl; anchors.fill:parent; anchors.margins:10
                                            wrapMode:Text.WordWrap; font.pixelSize:11; color:root.sub
                                            text: privSel.mode===0
                                                ? "Public — fully transparent: your account address, trade size, direction and pool are all visible on-chain."
                                                : privSel.mode===1
                                                ? "Private (strongest) — no public address ever appears on-chain. On-chain: trade size, direction, pool. Private: who traded, the source of funds, the destination, and any link between your trades. Atomic; single STARK proof on CPU."
                                                : "Private-Disposable (RFP-literal) — a fresh single-use public address is created per trade and never reused, with net-zero flow through it. On-chain: that ephemeral address + trade size, direction, pool. Private: your identity and links across trades. Atomic; same proof cost as Private." } }

                                    // Pre-confirmation summary (RFP Func #7):
                                    // estimated fee + min received; for private
                                    // modes the atomic / no-partial-deshield
                                    // guarantee (LEZ privacy txs carry no
                                    // separate gas leg — design §5.2/§9).
                                    Rectangle{ Layout.fillWidth:true; radius:12
                                        color:root.panel; border.color:root.stroke
                                        Layout.preferredHeight:prec.implicitHeight+18
                                        ColumnLayout{ id:prec; anchors.fill:parent
                                            anchors.margins:9; spacing:2
                                            // RFP Usability #8 — effective price line.
                                            Text{ Layout.fillWidth:true; color:root.sub
                                                font.pixelSize:10
                                                text:"Effective price  "+root.effPrice()
                                                    +"   ·   Slippage  "+root.slipPct.toFixed(2)+"%" }
                                            Text{ Layout.fillWidth:true; color:root.ink
                                                font.pixelSize:11
                                                text:"You receive ≥ "+root.minRecv()
                                                    +"   ·   "+(root.quoteSub.length?root.quoteSub:"fee —") }
                                            Text{ visible:privSel.mode===1 || privSel.mode===2
                                                Layout.fillWidth:true
                                                wrapMode:Text.WordWrap; color:root.sub
                                                font.pixelSize:10
                                                text:"Atomic: the deshield, swap and re-shield are one proof — it "
                                                    +"either all settles or nothing does (no partial deshield, "
                                                    +"funds can't be stranded). The shielded balance must cover "
                                                    +"the amount; the tx is rejected before submit otherwise. "
                                                    +"Privacy txs carry no separate gas fee on LEZ." }
                                            // Source: always ATA in Public mode (RFP Func #8); the
                                            // keypair-holding path has been retired so every trader
                                            // holds tokens in their deterministic `ATA(owner, def)`.
                                            Text{ visible:privSel.mode===0
                                                Layout.fillWidth:true; wrapMode:Text.WordWrap
                                                text:"Public swaps move tokens through your ATAs (RFP Func 8) — "
                                                    +"one deterministic holding per (owner, token)."
                                                color:root.sub; font.pixelSize:10 } } }

                                    // Pair valid but no pool at any fee tier? Surface a hint
                                    // pointing at the dedicated Create-Pool screen instead of
                                    // burying the create flow under the Swap button.
                                    Text{ visible: root.pairValidProp && root.availFeesList.length===0
                                        Layout.fillWidth:true; wrapMode:Text.WordWrap
                                        text:"No pool for "+(root.tokA.sym||"")+"/"+(root.tokB.sym||"")
                                            +". Tap the button below to open the Create-Pool screen with the pair pre-filled."
                                        color:root.sub; font.pixelSize:11 }

                                    Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:54
                                        radius:18
                                        property bool needCreate: root.pairValidProp && root.availFeesList.length===0
                                        gradient:Gradient{
                                            GradientStop{ position:0.0; color:root.brand }
                                            GradientStop{ position:1.0; color:root.brand2 } }
                                        opacity: (root.loaded && root.pairValidProp && !root.busy
                                                  && (needCreate || root.pairHasPoolProp)) ? 1 : 0.5
                                        MouseArea{ anchors.fill:parent
                                            enabled: root.loaded && root.pairValidProp && !root.busy
                                                  && (parent.needCreate || root.pairHasPoolProp)
                                            onClicked:{
                                                if (parent.needCreate){
                                                    // Pool create lives on its own screen now — push it
                                                    // pre-filled with the current pair + fee.
                                                    nav.push(createPoolView, {
                                                        initFee: root.selFee,
                                                        initSymA: root.tokA.sym,
                                                        initSymB: root.tokB.sym
                                                    });
                                                    return
                                                }
                                                // Swap path.
                                                var p = root.findPool(root.tokA, root.tokB, root.selFee);
                                                if (!p) return;
                                                var d = p.payIsA ? 0 : 1;        // direction in pool's a/b
                                                var defIn = p.payIsA ? p.pa : p.pb;
                                                if (privSel.mode===0) {
                                                    // Public swap — ATA-only (RFP Func #8). Trader's two
                                                    // ATAs derive from (owner, def) inside the FFI; both
                                                    // must already be funded for the input side.
                                                    if (!root.tokA.funded || !root.tokB.funded){
                                                        root.setStatus("Public swap needs both ATAs funded; "
                                                            +"these tokens aren't pre-funded by bootstrap.",false);
                                                        return
                                                    }
                                                    // Pack ids into one QString — the SDK QtProviderObject
                                                    // dispatch caps callModule arity at 5.
                                                    root.runAction("swapExactInAtaFor",
                                                        [p.pa+"|"+p.pb+"|"+defIn,
                                                         amtIn.text, root.minRecv(), root.selFee],
                                                        "ATA swap","Submitting via ATAs…");
                                                } else {
                                                    // Private modes — any pair where BOTH sides have
                                                    // shielded balances (bootstrap creates LDEX_PRIV_<L>
                                                    // for the first FUND_LIMIT tokens). Dispatch via
                                                    // privateSwapFor with the pool's actual (pa, pb)
                                                    // ordering + the per-side PrivateOwned holdings.
                                                    //
                                                    // Native-LEZ batched path: when ONE side is the
                                                    // native LEZ catalog entry (`isNative`) AND the
                                                    // mode is Disposable (only mode with a batched
                                                    // router variant), dispatch to privateSwapNativeFor
                                                    // which chains WLEZ::Wrap/Unwrap into the privacy
                                                    // proof. Saves one block wait + tx round-trip and
                                                    // gives wrap+swap atomicity. See
                                                    // docs/batched-native-swap.md.
                                                    var payNative = root.tokA.isNative === true;
                                                    var recvNative = root.tokB.isNative === true;
                                                    if ((payNative || recvNative)
                                                        && privSel.mode === 2) {
                                                        // The non-native side must have a PrivateOwned
                                                        // holding for the user to receive into (NativeIn)
                                                        // or spend from (NativeOut).
                                                        var tokSide = payNative ? root.tokB : root.tokA;
                                                        if (!tokSide.priv) {
                                                            root.setStatus(
                                                                "Native-batched swap needs a shielded holding for "
                                                                +tokSide.sym+". Use Public mode, or shield first.",false);
                                                            return
                                                        }
                                                        // Balance guard. NativeIn pays from the user's native LEZ
                                                        // gas account; NativeOut pays from the token's PRIV holding.
                                                        // Either way, we refuse before firing if the pay side can't
                                                        // cover `amtIn` — otherwise an empty/unsynced pre-state hits
                                                        // amm_v2 and the guest panics ("chained-call pre-state must be
                                                        // an initialized Fungible token holding"). Catches both an
                                                        // empty PRIV account AND a wallet that just hasn't synced yet
                                                        // (click Refresh in that case).
                                                        var needN = parseFloat(amtIn.text) || 0;
                                                        if (payNative) {
                                                            var natN = parseFloat(root.nativeBal) || 0;
                                                            if (natN < needN) {
                                                                root.setStatus(
                                                                    "Insufficient native LEZ — you have "+natN
                                                                    +", need "+needN+".",false);
                                                                return
                                                            }
                                                        } else {
                                                            var privN = parseFloat(root.privBalanceForDef(tokSide.def)) || 0;
                                                            if (privN < needN) {
                                                                root.setStatus(
                                                                    "Insufficient shielded "+tokSide.sym+" — you have "+privN
                                                                    +", need "+needN
                                                                    +". Shield more via the Account tab, or Refresh if you "
                                                                    +"believe the balance is stale.",false);
                                                                return
                                                            }
                                                        }
                                                        var nDir = payNative ? 0 : 1; // 0=NativeIn, 1=NativeOut
                                                        // 3-field pipe-delimited config: direction|token_def|priv_holding
                                                        var ncfg = nDir+"|"+tokSide.def+"|"+tokSide.priv;
                                                        root.runAction("privateSwapNativeFor",
                                                            [ncfg, amtIn.text, root.minRecv(), root.selFee],
                                                            payNative
                                                                ? "Batched native-in private swap (LEZ→"+tokSide.sym+")"
                                                                : "Batched native-out private swap ("+tokSide.sym+"→LEZ)",
                                                            "Proving privately — this can take a few minutes.")
                                                        return
                                                    }
                                                    if (!root.tokA.shielded || !root.tokB.shielded){
                                                        root.setStatus(
                                                            "Private modes need shielded balances on both sides. "
                                                            +"This pair has only "+root.tokenCatalog()
                                                                .filter(function(t){return t.shielded}).length
                                                            +" tokens with LDEX_PRIV_<L> set (bootstrap funds "
                                                            +"the first FUND_LIMIT tokens). Use Public mode, or "
                                                            +"re-bootstrap to shield more.",false);
                                                        return
                                                    }
                                                    // Pay-side shielded balance must cover `amtIn`. Without this,
                                                    // an unsynced or empty PRIV account passes empty bytes as a
                                                    // pre-state to amm_v2 and the guest panics with
                                                    // "chained-call pre-state must be an initialized Fungible token
                                                    // holding". Catches both genuinely-zero PRIV and a wallet that
                                                    // simply hasn't synced yet (suggest Refresh in the message).
                                                    var needP = parseFloat(amtIn.text) || 0;
                                                    var payPriv = parseFloat(root.privBalanceForDef(root.tokA.def)) || 0;
                                                    if (payPriv < needP){
                                                        root.setStatus(
                                                            "Insufficient shielded "+root.tokA.sym+" — you have "
                                                            +payPriv+", need "+needP
                                                            +". Shield more via the Account tab, or Refresh if you "
                                                            +"believe the balance is stale.",false);
                                                        return
                                                    }
                                                    // Pass the priv accounts in the pool's a/b order so the
                                                    // plugin's wire direction matches the AMM's tokenA convention.
                                                    var pPa = p.payIsA ? root.tokA.priv : root.tokB.priv;
                                                    var pPb = p.payIsA ? root.tokB.priv : root.tokA.priv;
                                                    // 6-field pipe-delimited config — SDK caps callModule arity at 5.
                                                    var cfg = privSel.mode+"|"+d+"|"+p.pa+"|"+p.pb+"|"+pPa+"|"+pPb;
                                                    root.runAction("privateSwapFor",
                                                        [cfg, amtIn.text, root.minRecv(), root.selFee],
                                                        privSel.mode===1?"Private swap":"Private-Disposable swap",
                                                        "Proving privately — this can take a few minutes.")
                                                }
                                            } }
                                        Text{ anchors.centerIn:parent
                                            text: parent.needCreate
                                                ? "Create pool"
                                                : (privSel.mode===0?"Swap"
                                                   :(privSel.mode===1?"Private swap":"Private-Disposable swap"))
                                            color:"white"; font.pixelSize:17; font.weight:Font.Bold } }
                                }
                            }
                            Text{ Layout.fillWidth:true; horizontalAlignment:Text.AlignHCenter
                                wrapMode:Text.WordWrap; color:root.sub; font.pixelSize:11
                                text:"Dev tokens TOKENA/TOKENB. Trades settle in ~15s blocks." }
                        }
                    }

                    // ===== POOLS =====
                    ScrollView{ contentWidth:availableWidth
                        ColumnLayout{ width:parent.width; spacing:10
                            RowLayout{ Layout.fillWidth:true; Layout.margins:22
                                Text{ text:"Pools"; color:root.ink; font.pixelSize:22; font.weight:Font.Bold }
                                Item{ Layout.fillWidth:true }
                                Rectangle{ implicitHeight:38; radius:14
                                    implicitWidth:newPoolLbl.implicitWidth+28
                                    gradient:Gradient{
                                        GradientStop{ position:0.0; color:root.brand }
                                        GradientStop{ position:1.0; color:root.brand2 } }
                                    MouseArea{ anchors.fill:parent
                                        enabled:root.loaded
                                        onClicked:nav.push(createPoolView,{
                                            initFee: root.selFee,
                                            initSymA: root.tokA.sym||"TOKENA",
                                            initSymB: root.tokB.sym||"TOKENB"
                                        }) }
                                    Text{ id:newPoolLbl; anchors.centerIn:parent
                                        text:"+ New pool"; color:"white"
                                        font.pixelSize:13; font.weight:Font.DemiBold } } }
                            // Search bar — filter by token symbol or definition id.
                            RowLayout{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22; spacing:6
                                TextField{ id:poolSearch; Layout.fillWidth:true
                                    placeholderText:"Search by token symbol or definition id…"
                                    font.pixelSize:12
                                    background:Rectangle{ radius:12; color:root.panel; border.color:root.stroke } }
                                Text{ text:"Sort"; color:root.sub; font.pixelSize:11 }
                                ComboBox{ id:poolSort; Layout.preferredWidth:140
                                    model:["Volume (desc)","Fee (asc)","TVL (desc)"]
                                    font.pixelSize:11
                                    background:Rectangle{ radius:10; color:root.panel; border.color:root.stroke } } }
                            Repeater{ model:root.sortedPoolList(poolSearch.text, poolSort.currentIndex)
                                delegate:Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                    Layout.preferredHeight:64; radius:18; color:root.card; border.color:root.stroke
                                    MouseArea{ anchors.fill:parent; enabled:modelData.exists===true
                                        onClicked:nav.push(poolView,{
                                            pfee: modelData.fee,
                                            pDefA: modelData.pa,
                                            pDefB: modelData.pb,
                                            pSymA: modelData.symA,
                                            pSymB: modelData.symB
                                        }) }
                                    RowLayout{ anchors.fill:parent; anchors.margins:16; spacing:12
                                        Rectangle{ width:24;height:24;radius:12;color:root.brand }
                                        Rectangle{ width:24;height:24;radius:12;color:root.brand2
                                            Layout.leftMargin:-14 }
                                        Text{ text:(modelData.symA||"")+" / "+(modelData.symB||""); color:root.ink
                                            font.pixelSize:15; font.weight:Font.DemiBold }
                                        Rectangle{ radius:10; color:root.panel; implicitHeight:22
                                            implicitWidth:fb.implicitWidth+16
                                            Text{ id:fb; anchors.centerIn:parent; text:(modelData.fee/100).toFixed(2)+"%"
                                                color:root.sub; font.pixelSize:11 } }
                                        Item{ Layout.fillWidth:true }
                                        Text{ visible:modelData.exists===true; color:root.sub; font.pixelSize:11
                                            text:"vol "+(parseFloat(modelData.volSum)||0).toFixed(0) }
                                        Text{ visible:modelData.exists===true; color:root.sub; font.pixelSize:13
                                            text:"price "+((parseFloat(modelData.reserve_b)||0)/(parseFloat(modelData.reserve_a)||1)).toFixed(4) }
                                        Text{ visible:modelData.exists===true; text:"open ›"; color:root.brand
                                            font.pixelSize:13; font.weight:Font.DemiBold }
                                        Button{ visible:modelData.exists!==true; text:"Create"; enabled:root.loaded
                                            implicitHeight:32; padding:6; leftPadding:14; rightPadding:14
                                            background: Rectangle{ radius:11
                                                color: parent.enabled
                                                    ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                                    : root.panel
                                                border.color: parent.enabled ? "transparent" : root.stroke }
                                            contentItem: Text{ text:parent.text
                                                color: parent.enabled ? "white" : root.sub
                                                font.pixelSize:12; font.weight:Font.DemiBold
                                                horizontalAlignment:Text.AlignHCenter
                                                verticalAlignment:Text.AlignVCenter }
                                            onClicked:nav.push(createPoolView,{
                                                initFee: modelData.fee,
                                                initSymA: root.tokA.sym||"TOKENA",
                                                initSymB: root.tokB.sym||"TOKENB"
                                            }) } } } }
                            Text{ visible:root.sortedPoolList(poolSearch.text, poolSort.currentIndex).length===0
                                Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                color:root.sub; font.pixelSize:12; horizontalAlignment:Text.AlignHCenter
                                text:"No pools exist yet. Use '+ New pool' to create one." }
                            Item{ Layout.preferredHeight:8 }
                        }
                    }

                    // ===== ACCOUNT =====
                    // Analytics moved out of a global tab into each pool's
                    // detail view (Pools → row → "open"). See poolView
                    // Component for per-pool TVL / volume / fee revenue —
                    // EXACT on-chain values read from each pool's
                    // PoolDefinition.
                    ScrollView{ contentWidth:availableWidth
                        ColumnLayout{ width:parent.width; spacing:12
                            Text{ text:"Account"; color:root.ink; font.pixelSize:22; font.weight:Font.Bold; Layout.margins:22 }
                            Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                radius:18; color:root.card; border.color:root.stroke
                                Layout.preferredHeight:bcol.implicitHeight+28
                                ColumnLayout{ id:bcol; anchors.fill:parent; anchors.margins:16; spacing:8
                                    RowLayout{ Layout.fillWidth:true
                                        Text{ text:"Balances"; color:root.ink; font.pixelSize:15; font.weight:Font.Bold }
                                        Item{ Layout.fillWidth:true }
                                        Text{ text:"public"; color:root.sub; font.pixelSize:10
                                              Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                        Text{ text:"shielded"; color:root.sub; font.pixelSize:10
                                              Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                        Text{ text:"total"; color:root.sub; font.pixelSize:10
                                              Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                    }
                                    Repeater{ model:root.whitelistedBalances()
                                        delegate:RowLayout{ Layout.fillWidth:true; spacing:6
                                            Rectangle{ width:20;height:20;radius:10; color: modelData.color }
                                            Text{ text:modelData.name; color:root.ink; font.pixelSize:14
                                                  Layout.preferredWidth:90 }
                                            // Selectable def_id — first 12 chars of the canonical id.
                                            TextEdit{ text:modelData.definition.substring(0, 18)
                                                color:root.sub; font.pixelSize:10
                                                readOnly:true; selectByMouse:true
                                                Layout.fillWidth:true }
                                            Text{ text:modelData.pubBalance; color:root.ink; font.pixelSize:13
                                                  Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                            Text{ text:modelData.privBalance; color:root.brand; font.pixelSize:13
                                                  Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                            Text{ text:modelData.balance; color:root.ink; font.pixelSize:14
                                                  font.weight:Font.DemiBold
                                                  Layout.preferredWidth:80; horizontalAlignment:Text.AlignRight }
                                        }
                                    } } }
                            // ── Native LEZ wrap/unwrap ─────────────────
                            // Visible only when the bootstrap wired WLEZ
                            // (`LDEX_WLEZ_DEF` present in env). LEZ shows in
                            // the catalog as a single token but is internally
                            // split between native (the user's gas account)
                            // and wrapped (WLEZ token holding) — this card
                            // moves balance between the two so the user can
                            // trade LEZ in the AMM.
                            Rectangle{ visible: !!root.env.LDEX_WLEZ_DEF
                                Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                radius:18; color:root.card; border.color:root.stroke
                                Layout.preferredHeight:wcol.implicitHeight+28
                                ColumnLayout{ id:wcol; anchors.fill:parent; anchors.margins:16; spacing:8
                                    Text{ text:"Native LEZ ↔ wrapped"; color:root.ink
                                          font.pixelSize:15; font.weight:Font.Bold }
                                    Text{ text:"Wrap → WLEZ in your ATA (immediately tradeable). Unwrap drains the keypair holding (HOLD_W); to unwrap WLEZ currently in your ATA, move it to HOLD_W first. 1:1, no fees."
                                          color:root.sub; font.pixelSize:11; wrapMode:Text.WordWrap
                                          Layout.fillWidth:true }
                                    RowLayout{ Layout.fillWidth:true; spacing:8
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Wrap amount"; color:root.sub; font.pixelSize:10 }
                                            TextField{ id:wrapAmt; text:"1000"; Layout.preferredWidth:96
                                                background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                                        Button{ text:"Wrap → ATA"; enabled: root.loaded && !root.busy
                                            implicitHeight:34; padding:6; leftPadding:16; rightPadding:16
                                            background: Rectangle{ radius:12
                                                color: parent.enabled
                                                    ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                                    : root.panel
                                                border.color: parent.enabled ? "transparent" : root.stroke }
                                            contentItem: Text{ text:parent.text
                                                color: parent.enabled ? "white" : root.sub
                                                font.pixelSize:12; font.weight:Font.DemiBold
                                                horizontalAlignment:Text.AlignHCenter
                                                verticalAlignment:Text.AlignVCenter }
                                            onClicked: root.runAction("wrapNative", [wrapAmt.text],
                                                "Wrap LEZ", "Locking native LEZ into the WLEZ vault, minting into ATA…") }
                                        Item{ Layout.fillWidth:true }
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Unwrap amount"; color:root.sub; font.pixelSize:10 }
                                            TextField{ id:unwrapAmt; text:"1000"; Layout.preferredWidth:96
                                                background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                                        Button{ text:"Unwrap HOLD_W"; enabled: root.loaded && !root.busy
                                            implicitHeight:34; padding:6; leftPadding:16; rightPadding:16
                                            background: Rectangle{ radius:12
                                                color: parent.enabled
                                                    ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                                    : root.panel
                                                border.color: parent.enabled ? "transparent" : root.stroke }
                                            contentItem: Text{ text:parent.text
                                                color: parent.enabled ? "white" : root.sub
                                                font.pixelSize:12; font.weight:Font.DemiBold
                                                horizontalAlignment:Text.AlignHCenter
                                                verticalAlignment:Text.AlignVCenter }
                                            onClicked: root.runAction("unwrapNative", [unwrapAmt.text],
                                                "Unwrap WLEZ", "Burning WLEZ from HOLD_W and releasing native LEZ…") } }
                                    // Second row — "move ATA → HOLD_W" so users can
                                    // unwrap WLEZ that accumulated in the ATA via
                                    // swaps. WLEZ::Unwrap requires the holding to be
                                    // owner-signed; ATAs are PDA-owned, so a hop
                                    // through HOLD_W is mandatory.
                                    RowLayout{ Layout.fillWidth:true; spacing:8
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Move ATA→HOLD_W amount"; color:root.sub; font.pixelSize:10 }
                                            TextField{ id:moveAmt; text:"1000"; Layout.preferredWidth:140
                                                background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                                        Button{ text:"Move WLEZ ATA→HOLD_W"; enabled: root.loaded && !root.busy
                                            implicitHeight:34; padding:6; leftPadding:16; rightPadding:16
                                            background: Rectangle{ radius:12
                                                color: parent.enabled
                                                    ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                                    : root.panel
                                                border.color: parent.enabled ? "transparent" : root.stroke }
                                            contentItem: Text{ text:parent.text
                                                color: parent.enabled ? "white" : root.sub
                                                font.pixelSize:12; font.weight:Font.DemiBold
                                                horizontalAlignment:Text.AlignHCenter
                                                verticalAlignment:Text.AlignVCenter }
                                            onClicked: root.runAction("consolidateWlezToHoldW", [moveAmt.text],
                                                "Move WLEZ", "Transferring WLEZ from your ATA into the keypair holding so it can be unwrapped…") }
                                        Item{ Layout.fillWidth:true } } } }
                            // ── Token privacy (manual shield / deshield) ────
                            // Lets the user move a token between its public
                            // ATA (ATA_<L>) and its PrivateOwned account
                            // (PRIV_<L>) without going through a swap. Useful
                            // to top up the shielded side before a mode-1/2
                            // swap, or to pull funds back out to ATA. Direct
                            // wallet-FFI transfer; no AMM, no STARK.
                            Rectangle{ id:privCard
                                Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                radius:18; color:root.card; border.color:root.stroke
                                Layout.preferredHeight:pcol.implicitHeight+28
                                // Letters in the env that have a PRIV_<L> wired (shielded-capable).
                                property var privTokenLetters: {
                                    var out = [];
                                    var letters = ((root.env.LDEX_TOKENS||"A B").split(/\s+/));
                                    for (var i=0;i<letters.length;i++){
                                        var L=letters[i];
                                        if (root.env["LDEX_PRIV_"+L] && root.env["LDEX_HOLD_"+L]) out.push(L);
                                    }
                                    return out;
                                }
                                ColumnLayout{ id:pcol; anchors.fill:parent; anchors.margins:16; spacing:8
                                    Text{ text:"Shield / Deshield tokens"; color:root.ink
                                          font.pixelSize:15; font.weight:Font.Bold }
                                    Text{ text:"Shield moves tokens from your keypair holding (HOLD_<L>) into a PrivateOwned account visible only to you. Deshield reverses it, into HOLD_<L>. One STARK-proven transfer per click — tens of seconds under real proofs. (ATAs are PDA-owned, so the token program won't accept them as a signed sender; HOLD has the keypair, ATA doesn't. Your displayed pub balance sums HOLD+ATA, so the total moves as expected.)"
                                          color:root.sub; font.pixelSize:11; wrapMode:Text.WordWrap
                                          Layout.fillWidth:true }
                                    RowLayout{ Layout.fillWidth:true; spacing:8
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Token"; color:root.sub; font.pixelSize:10 }
                                            ComboBox{ id:privTok; Layout.preferredWidth:110
                                                model: privCard.privTokenLetters.map(function(L){ return "TOKEN"+L })
                                                font.pixelSize:12
                                                background:Rectangle{ radius:10; color:root.panel; border.color:root.stroke } } }
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Direction"; color:root.sub; font.pixelSize:10 }
                                            ComboBox{ id:privDir; Layout.preferredWidth:160
                                                model:["Shield (ATA → Priv)","Deshield (Priv → ATA)"]
                                                font.pixelSize:12
                                                background:Rectangle{ radius:10; color:root.panel; border.color:root.stroke } } }
                                        ColumnLayout{ spacing:2
                                            Text{ text:"Amount"; color:root.sub; font.pixelSize:10 }
                                            TextField{ id:privAmt; text:"1000"; Layout.preferredWidth:96
                                                background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                                        Button{
                                            text: privDir.currentIndex===0 ? "Shield" : "Deshield"
                                            enabled: root.loaded && !root.busy
                                                && privCard.privTokenLetters.length>0
                                            implicitHeight:34; padding:6; leftPadding:16; rightPadding:16
                                            background: Rectangle{ radius:12
                                                color: parent.enabled
                                                    ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                                    : root.panel
                                                border.color: parent.enabled ? "transparent" : root.stroke }
                                            contentItem: Text{ text:parent.text
                                                color: parent.enabled ? "white" : root.sub
                                                font.pixelSize:12; font.weight:Font.DemiBold
                                                horizontalAlignment:Text.AlignHCenter
                                                verticalAlignment:Text.AlignVCenter }
                                            onClicked:{
                                                var letters = privCard.privTokenLetters;
                                                if (letters.length===0) return;
                                                var L = letters[Math.max(0,privTok.currentIndex)];
                                                var m = privDir.currentIndex===0 ? "shieldToken" : "deshieldToken";
                                                var title = privDir.currentIndex===0 ? "Shield" : "Deshield";
                                                var sub = privDir.currentIndex===0
                                                    ? "Moving "+privAmt.text+" TOKEN"+L+" from ATA into your private holding…"
                                                    : "Moving "+privAmt.text+" TOKEN"+L+" from your private holding back to ATA…";
                                                root.runAction(m, [L, privAmt.text], title, sub);
                                            } }
                                        Item{ Layout.fillWidth:true } } } }
                            Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                radius:18; color:root.card; border.color:root.stroke
                                Layout.preferredHeight:acol.implicitHeight+28
                                ColumnLayout{ id:acol; anchors.fill:parent; anchors.margins:16; spacing:6
                                    Text{ text:"Addresses ("+root.accts.length+")"; color:root.ink; font.pixelSize:15; font.weight:Font.Bold }
                                    Repeater{ model:root.accts
                                        delegate:TextEdit{ Layout.fillWidth:true; wrapMode:TextEdit.WrapAnywhere
                                            color:root.sub; font.pixelSize:11
                                            readOnly:true; selectByMouse:true
                                            text:(modelData["public"]?"pub  ":"priv ")+modelData.address } } } }
                            Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                                Layout.bottomMargin:16; radius:18; color:root.card; border.color:root.stroke
                                Layout.preferredHeight:hcol.implicitHeight+28
                                ColumnLayout{ id:hcol; anchors.fill:parent; anchors.margins:16; spacing:6
                                    Text{ text:"Activity"; color:root.ink; font.pixelSize:15; font.weight:Font.Bold }
                                    Text{ visible:root.history.length===0; color:root.sub; font.pixelSize:12; text:"No actions yet." }
                                    Repeater{ model:root.history
                                        delegate:TextEdit{ Layout.fillWidth:true; wrapMode:TextEdit.WrapAnywhere; font.pixelSize:11
                                            readOnly:true; selectByMouse:true
                                            color:modelData.ok?root.okCol:root.errCol
                                            text:modelData.time+"  "+modelData.action+"  —  "+modelData.result } } } }
                        }
                    }
                }

                // status toast
                Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:44
                    color:root.card; border.color:root.stroke
                    Text{ anchors.fill:parent; anchors.margins:12; wrapMode:Text.WrapAnywhere
                        font.pixelSize:12; color:root.statusOk?root.okCol:root.errCol; text:root.status } }
            }
        }
    }

    // ===== per-pool detail =====
    Component {
        id: poolView
        Item {
            id: poolRoot
            property int pfee:30
            // Pair the view was opened for — defaults to env A/B if not
            // overridden by the caller (kept for backwards-compat with the
            // old single-pair list).
            property string pDefA: root.defA()
            property string pDefB: root.defB()
            property string pSymA: "TOKENA"
            property string pSymB: "TOKENB"
            // Whether the bootstrap LP holding (LDEX_USER_HOLDING_LP) is
            // for this exact pool — only the env A/B pair has one on
            // dev. For other pools we hide the Add/Remove form until we
            // wire per-pool LP-holding tracking (RFP Func #2 follow-up).
            readonly property bool isEnvPair: (pDefA===root.defA() && pDefB===root.defB())
                                            || (pDefA===root.defB() && pDefB===root.defA())
            property var pinfo:({})
            property var prices:[]
            property bool seeded:false
            function seedHistory(){
                var h=root.jget("priceHistory",[pDefA,pDefB,pfee]);
                if(h&&h.length!==undefined){ var a=[];
                    for(var i=0;i<h.length;i++){ var v=parseFloat(h[i].p); if(!isNaN(v)) a.push(v) }
                    poolRoot.prices=a; poolRoot.seeded=true; chart.requestPaint() } }
            function loadPool(){
                if(!poolRoot.seeded) seedHistory();
                var p=root.jget("poolInfoFor",[pDefA,pDefB,pfee]);
                if(p){ pinfo=p;
                    if(p.exists){ var pr=(parseFloat(p.reserve_b)||0)/(parseFloat(p.reserve_a)||1);
                        var a=prices.slice(); a.push(pr); if(a.length>4000) a.shift(); prices=a; chart.requestPaint() } } }
            Component.onCompleted:{ seedHistory(); loadPool() }
            Timer{ interval:15000; repeat:true; running:true; onTriggered:poolRoot.loadPool() }

            ColumnLayout{
                anchors.fill:parent; spacing:14
                RowLayout{ Layout.fillWidth:true; Layout.margins:22
                    Rectangle{ radius:18; implicitHeight:36; implicitWidth:bk.implicitWidth+28
                        color:root.card; border.color:root.stroke
                        MouseArea{ anchors.fill:parent; onClicked:nav.pop() }
                        Text{ id:bk; anchors.centerIn:parent; text:"‹ Back"; color:root.ink; font.pixelSize:14 } }
                    Item{ Layout.fillWidth:true }
                    Text{ text:pSymA+" / "+pSymB+" · "+(pfee/100).toFixed(2)+"%"
                        color:root.ink; font.pixelSize:18; font.weight:Font.Bold } }

                RowLayout{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22; spacing:12
                    Repeater{ model:[
                        {k:"Reserve "+pSymA, v: pinfo.reserve_a||"—"},
                        {k:"Reserve "+pSymB, v: pinfo.reserve_b||"—"},
                        {k:"Price "+pSymB+"/"+pSymA, v: pinfo.exists?((parseFloat(pinfo.reserve_b)||0)/(parseFloat(pinfo.reserve_a)||1)).toFixed(4):"—"},
                        {k:"LP supply", v: pinfo.lp_supply||"—"} ]
                        delegate:Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:72
                            radius:16; color:root.card; border.color:root.stroke
                            ColumnLayout{ anchors.centerIn:parent; spacing:3
                                Text{ text:modelData.k; color:root.sub; font.pixelSize:11; Layout.alignment:Qt.AlignHCenter }
                                Text{ text:modelData.v; color:root.ink; font.pixelSize:16; font.weight:Font.Bold; Layout.alignment:Qt.AlignHCenter } } } } }

                // Per-pool analytics — EXACT on-chain values from this
                // pool's PoolDefinition (reserves + cumulative volume +
                // LP-fee accumulators maintained by amm_v2's swap_logic).
                // Aggregate-per-pool only; no individual LP/trader positions.
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:anaCol.implicitHeight+28
                    ColumnLayout{ id:anaCol; anchors.fill:parent; anchors.margins:14; spacing:8
                        RowLayout{ Layout.fillWidth:true
                            Text{ text:"Analytics"; color:root.ink
                                  font.pixelSize:15; font.weight:Font.Bold }
                            Item{ Layout.fillWidth:true }
                            Text{ text:pSymA+"/"+pSymB+" · "+(pfee/100).toFixed(2)+"%"
                                  color:root.sub; font.pixelSize:11 } }
                        GridLayout{ Layout.fillWidth:true; columns:2; columnSpacing:10; rowSpacing:10
                            Repeater{ model:[
                                {k:"TVL",
                                 v: pinfo.exists
                                    ? (parseFloat(pinfo.reserve_a)||0).toFixed(0)+" "+pSymA+" + "+(parseFloat(pinfo.reserve_b)||0).toFixed(0)+" "+pSymB
                                    : "—"},
                                {k:"LP supply",
                                 v: pinfo.lp_supply||"—"},
                                {k:"Cumulative volume",
                                 v: pinfo.exists
                                    ? (parseFloat(pinfo.cum_volume_a)||0).toFixed(0)+" "+pSymA+" / "+(parseFloat(pinfo.cum_volume_b)||0).toFixed(0)+" "+pSymB
                                    : "—"},
                                {k:"LP fee revenue",
                                 v: pinfo.exists
                                    ? (parseFloat(pinfo.cum_fees_a)||0).toFixed(4)+" "+pSymA+" / "+(parseFloat(pinfo.cum_fees_b)||0).toFixed(4)+" "+pSymB
                                    : "—"}]
                                delegate:Rectangle{ Layout.fillWidth:true; Layout.preferredHeight:68
                                    radius:14; color:root.panel; border.color:root.stroke
                                    ColumnLayout{ anchors.fill:parent; anchors.margins:12; spacing:3
                                        Text{ text:modelData.k; color:root.sub; font.pixelSize:11 }
                                        Text{ text:modelData.v; color:root.ink; font.pixelSize:14
                                              font.weight:Font.DemiBold; Layout.fillWidth:true
                                              elide:Text.ElideRight } } } } }
                        Text{ Layout.fillWidth:true; wrapMode:Text.WordWrap
                              color:root.sub; font.pixelSize:10
                              text:"EXACT on-chain values read from this pool's PoolDefinition. "+
                                   "Aggregate only — no individual LP or trader positions." } } }

                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    Layout.preferredHeight:220; radius:18; color:root.card; border.color:root.stroke
                    ColumnLayout{ anchors.fill:parent; anchors.margins:14; spacing:4
                        Text{ text:"Price — on-chain history (B per A)"; color:root.sub; font.pixelSize:12 }
                        Canvas{ id:chart; Layout.fillWidth:true; Layout.fillHeight:true
                            onPaint:{ var ctx=getContext("2d"); ctx.reset();
                                ctx.fillStyle=root.card; ctx.fillRect(0,0,width,height);
                                var d=poolRoot.prices; if(!d||d.length<2){ ctx.fillStyle="#7d7d8d";
                                    ctx.fillText("collecting price samples…",10,20); return }
                                var mn=Math.min.apply(null,d), mx=Math.max.apply(null,d); if(mx===mn) mx=mn+1;
                                ctx.strokeStyle=root.brand; ctx.lineWidth=2; ctx.beginPath();
                                for(var i=0;i<d.length;i++){ var x=i/(d.length-1)*(width-16)+8;
                                    var y=height-8-((d[i]-mn)/(mx-mn))*(height-16);
                                    if(i===0) ctx.moveTo(x,y); else ctx.lineTo(x,y) }
                                ctx.stroke(); ctx.fillStyle="#7d7d8d";
                                ctx.fillText(mx.toFixed(4),6,12); ctx.fillText(mn.toFixed(4),6,height-4) } } } }

                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    Layout.bottomMargin:22; radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:lq.implicitHeight+28
                    ColumnLayout{ id:lq; anchors.fill:parent; anchors.margins:16; spacing:8
                        RowLayout{ Layout.fillWidth:true; spacing:8
                            Text{ text:"Liquidity ("+pSymA+" / "+pSymB+")"
                                color:root.ink; font.pixelSize:15; font.weight:Font.Bold }
                            Item{ Layout.fillWidth:true }
                            Item{ id:lpPriv; property bool on:false }
                            Rectangle{ visible:poolRoot.isEnvPair
                                implicitHeight:28; radius:14
                                implicitWidth:lpPl.implicitWidth+22
                                color: lpPriv.on ? root.brand : root.panel
                                border.color: lpPriv.on ? root.brand : root.stroke
                                MouseArea{ anchors.fill:parent; onClicked:lpPriv.on=!lpPriv.on }
                                Text{ id:lpPl; anchors.centerIn:parent
                                    text: lpPriv.on ? "Private LP ✓" : "Private LP"
                                    font.pixelSize:11
                                    color: lpPriv.on ? "white" : root.sub } } }
                        // For the env A/B pool we have a bootstrap LP holding;
                        // wire Add/Remove through it. For other pools, surface a
                        // hint until per-pool LP-holding tracking is wired
                        // (RFP Func #2 follow-up).
                        RowLayout{ visible:poolRoot.isEnvPair; spacing:8; Layout.fillWidth:true
                            ColumnLayout{ spacing:2
                                Text{ text:"Add max "+pSymA; color:root.sub; font.pixelSize:10 }
                                TextField{ id:pMaxA; text:"10000"; Layout.preferredWidth:96
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                            ColumnLayout{ spacing:2
                                Text{ text:"Add max "+pSymB; color:root.sub; font.pixelSize:10 }
                                TextField{ id:pMaxB; text:"20000"; Layout.preferredWidth:96
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                            ColumnLayout{ spacing:2
                                Text{ text:"Min LP"; color:root.sub; font.pixelSize:10 }
                                TextField{ id:pMinLp; text:"1"; Layout.preferredWidth:64
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                            Button{ text:"Add"; enabled:root.loaded && !root.busy
                                implicitHeight:34; padding:6; leftPadding:18; rightPadding:18
                                background: Rectangle{ radius:12
                                    color: parent.enabled
                                        ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                        : root.panel
                                    border.color: parent.enabled ? "transparent" : root.stroke }
                                contentItem: Text{ text:parent.text
                                    color: parent.enabled ? "white" : root.sub
                                    font.pixelSize:13; font.weight:Font.DemiBold
                                    horizontalAlignment:Text.AlignHCenter
                                    verticalAlignment:Text.AlignVCenter }
                                onClicked:root.runAction("privateAddLiquidity",
                                    [lpPriv.on?1:0,pMinLp.text,pMaxA.text,pMaxB.text,pfee],
                                    lpPriv.on?"Adding liquidity (private)":"Adding liquidity",
                                    lpPriv.on?"Proving privately — this can take a few minutes.":"Submitting…") }
                            Item{ Layout.fillWidth:true }
                            ColumnLayout{ spacing:2
                                Text{ text:"Burn LP"; color:root.sub; font.pixelSize:10 }
                                TextField{ id:pLp; text:"10"; Layout.preferredWidth:64
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                            Button{ text:"Remove"; enabled:root.loaded && !root.busy
                                implicitHeight:34; padding:6; leftPadding:18; rightPadding:18
                                background: Rectangle{ radius:12
                                    color: parent.enabled
                                        ? (parent.pressed ? Qt.darker(root.brand,1.15) : root.brand)
                                        : root.panel
                                    border.color: parent.enabled ? "transparent" : root.stroke }
                                contentItem: Text{ text:parent.text
                                    color: parent.enabled ? "white" : root.sub
                                    font.pixelSize:13; font.weight:Font.DemiBold
                                    horizontalAlignment:Text.AlignHCenter
                                    verticalAlignment:Text.AlignVCenter }
                                onClicked:root.runAction("privateRemoveLiquidity",
                                    [lpPriv.on?1:0,pLp.text,"1","1",pfee],
                                    lpPriv.on?"Removing liquidity (private)":"Removing liquidity",
                                    lpPriv.on?"Proving privately — this can take a few minutes.":"Submitting…") } }
                        Text{ visible:!poolRoot.isEnvPair; Layout.fillWidth:true; wrapMode:Text.WordWrap
                            color:root.sub; font.pixelSize:11
                            text:"Add/Remove liquidity for non-bootstrap pools needs a per-pool LP holding — wiring that comes in the next iteration." }
                        Text{ Layout.fillWidth:true; wrapMode:Text.WordWrap; color:root.sub; font.pixelSize:10
                            text:"Chart = persisted on-chain price history (§5.11 price-indexer)." } }
                }
            }
        }
    }

    // ===== Create-Pool screen =====
    // Dedicated screen with two token pickers, seed amounts, fee tier,
    // and a live price preview computed from the entered amounts (the
    // pool opens at exactly amountB / amountA — there's no on-chain
    // price yet because the pool doesn't exist).
    Component {
        id: createPoolView
        Item {
            id: cpRoot
            property int initFee: 30
            property string initSymA: "TOKENA"
            property string initSymB: "TOKENB"
            property string cpSymA: initSymA
            property string cpSymB: initSymB
            property int cpFee: initFee
            // Resolve the selected symbols to catalog entries (with def
            // + hold ids). `root.tokenBySym` falls through to a blank
            // entry if the env isn't loaded yet.
            property var cpA: root.tokenBySym(cpSymA)
            property var cpB: root.tokenBySym(cpSymB)
            // Recompute when env loads or the user picks a new symbol.
            Connections{ target:root
                function onLoadedChanged(){ cpRoot.cpA = root.tokenBySym(cpRoot.cpSymA);
                                            cpRoot.cpB = root.tokenBySym(cpRoot.cpSymB) } }
            function priceBA(){
                var a = parseFloat(cpAmtA.text), b = parseFloat(cpAmtB.text);
                if (isNaN(a)||isNaN(b)||a<=0||b<=0) return "—";
                return (b/a).toFixed(6);
            }
            function priceAB(){
                var a = parseFloat(cpAmtA.text), b = parseFloat(cpAmtB.text);
                if (isNaN(a)||isNaN(b)||a<=0||b<=0) return "—";
                return (a/b).toFixed(6);
            }

            ColumnLayout{
                anchors.fill:parent; spacing:14
                RowLayout{ Layout.fillWidth:true; Layout.margins:22
                    Rectangle{ radius:18; implicitHeight:36; implicitWidth:cpbk.implicitWidth+28
                        color:root.card; border.color:root.stroke
                        MouseArea{ anchors.fill:parent; onClicked:nav.pop() }
                        Text{ id:cpbk; anchors.centerIn:parent; text:"‹ Back"; color:root.ink; font.pixelSize:14 } }
                    Item{ Layout.fillWidth:true }
                    Text{ text:"Create pool"; color:root.ink; font.pixelSize:18; font.weight:Font.Bold } }

                // ── Pair picker ────────────────────────────────────
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:cpPairCol.implicitHeight+28
                    ColumnLayout{ id:cpPairCol; anchors.fill:parent; anchors.margins:16; spacing:8
                        Text{ text:"Pair"; color:root.ink; font.pixelSize:14; font.weight:Font.Bold }
                        Flow{ Layout.fillWidth:true; spacing:6
                            Repeater{ model: root.tokenCatalog()
                                delegate: Rectangle{ implicitHeight:30; radius:14
                                    implicitWidth:cpaL.implicitWidth+30
                                    color: cpRoot.cpSymA===modelData.sym ? root.brand : root.panel
                                    border.color: cpRoot.cpSymA===modelData.sym ? root.brand : root.stroke
                                    MouseArea{ anchors.fill:parent; onClicked:{
                                        if (modelData.sym===cpRoot.cpSymB) cpRoot.cpSymB = cpRoot.cpSymA;
                                        cpRoot.cpSymA = modelData.sym;
                                        cpRoot.cpA = root.tokenBySym(cpRoot.cpSymA);
                                        cpRoot.cpB = root.tokenBySym(cpRoot.cpSymB) } }
                                    RowLayout{ anchors.centerIn:parent; spacing:6
                                        Rectangle{ width:14;height:14;radius:7; color:modelData.color }
                                        Text{ id:cpaL; text:"A: "+modelData.sym
                                            color: cpRoot.cpSymA===modelData.sym ? "white" : root.ink
                                            font.pixelSize:11 } } } } }
                        Flow{ Layout.fillWidth:true; spacing:6
                            Repeater{ model: root.tokenCatalog()
                                delegate: Rectangle{ implicitHeight:30; radius:14
                                    implicitWidth:cpbL.implicitWidth+30
                                    color: cpRoot.cpSymB===modelData.sym ? root.brand : root.panel
                                    border.color: cpRoot.cpSymB===modelData.sym ? root.brand : root.stroke
                                    MouseArea{ anchors.fill:parent; onClicked:{
                                        if (modelData.sym===cpRoot.cpSymA) cpRoot.cpSymA = cpRoot.cpSymB;
                                        cpRoot.cpSymB = modelData.sym;
                                        cpRoot.cpA = root.tokenBySym(cpRoot.cpSymA);
                                        cpRoot.cpB = root.tokenBySym(cpRoot.cpSymB) } }
                                    RowLayout{ anchors.centerIn:parent; spacing:6
                                        Rectangle{ width:14;height:14;radius:7; color:modelData.color }
                                        Text{ id:cpbL; text:"B: "+modelData.sym
                                            color: cpRoot.cpSymB===modelData.sym ? "white" : root.ink
                                            font.pixelSize:11 } } } } } } }

                // ── Seed amounts ──────────────────────────────────
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:cpAmtCol.implicitHeight+28
                    ColumnLayout{ id:cpAmtCol; anchors.fill:parent; anchors.margins:16; spacing:8
                        Text{ text:"Seed amounts"; color:root.ink; font.pixelSize:14; font.weight:Font.Bold }
                        RowLayout{ Layout.fillWidth:true; spacing:10
                            ColumnLayout{ spacing:2; Layout.fillWidth:true
                                Text{ text:"Amount "+cpRoot.cpSymA; color:root.sub; font.pixelSize:11 }
                                TextField{ id:cpAmtA; text:"100000"; Layout.fillWidth:true
                                    font.pixelSize:18; font.weight:Font.DemiBold
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } }
                            ColumnLayout{ spacing:2; Layout.fillWidth:true
                                Text{ text:"Amount "+cpRoot.cpSymB; color:root.sub; font.pixelSize:11 }
                                TextField{ id:cpAmtB; text:"200000"; Layout.fillWidth:true
                                    font.pixelSize:18; font.weight:Font.DemiBold
                                    background:Rectangle{ radius:8; color:root.panel; border.color:root.stroke } } } } } }

                // ── Fee tier ──────────────────────────────────────
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:cpFeeRow.implicitHeight+28
                    ColumnLayout{ id:cpFeeCol; anchors.fill:parent; anchors.margins:16; spacing:8
                        Text{ text:"Fee tier"; color:root.ink; font.pixelSize:14; font.weight:Font.Bold }
                        RowLayout{ id:cpFeeRow; Layout.fillWidth:true; spacing:6
                            Repeater{ model:[1,5,30,100]
                                delegate: Rectangle{ Layout.fillWidth:true; implicitHeight:36
                                    radius:14
                                    color: cpRoot.cpFee===modelData ? root.brand : root.panel
                                    border.color: cpRoot.cpFee===modelData ? root.brand : root.stroke
                                    MouseArea{ anchors.fill:parent; onClicked:cpRoot.cpFee=modelData }
                                    Text{ anchors.centerIn:parent
                                        text:(modelData/100).toFixed(2)+"%"
                                        color: cpRoot.cpFee===modelData ? "white" : root.ink
                                        font.pixelSize:12
                                        font.weight: cpRoot.cpFee===modelData?Font.Bold:Font.Normal } } } } } }

                // ── Initial price preview ─────────────────────────
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    radius:18; color:root.card; border.color:root.stroke
                    Layout.preferredHeight:cpPxCol.implicitHeight+28
                    ColumnLayout{ id:cpPxCol; anchors.fill:parent; anchors.margins:16; spacing:6
                        Text{ text:"Initial price (set by your deposit)"
                              color:root.ink; font.pixelSize:14; font.weight:Font.Bold }
                        RowLayout{ Layout.fillWidth:true; spacing:24
                            ColumnLayout{ spacing:2
                                Text{ text:"1 "+cpRoot.cpSymA+" ="; color:root.sub; font.pixelSize:11 }
                                Text{ text: cpRoot.priceBA()+" "+cpRoot.cpSymB
                                      color:root.ink; font.pixelSize:18; font.weight:Font.DemiBold } }
                            ColumnLayout{ spacing:2
                                Text{ text:"1 "+cpRoot.cpSymB+" ="; color:root.sub; font.pixelSize:11 }
                                Text{ text: cpRoot.priceAB()+" "+cpRoot.cpSymA
                                      color:root.ink; font.pixelSize:18; font.weight:Font.DemiBold } }
                            Item{ Layout.fillWidth:true } }
                        Text{ Layout.fillWidth:true; wrapMode:Text.WordWrap
                              color:root.sub; font.pixelSize:11
                              text:"This ratio becomes the pool's opening price. If it's far from "
                                  +"the market, arbitrageurs will quickly correct it at your expense." } } }

                Item{ Layout.fillHeight:true }

                // ── Submit ─────────────────────────────────────────
                Rectangle{ Layout.fillWidth:true; Layout.leftMargin:22; Layout.rightMargin:22
                    Layout.bottomMargin:22
                    Layout.preferredHeight:54; radius:18
                    // Keypair holdings fund the deposits; the LP lands
                    // in ATA(owner, lp_def). Pool-create needs both
                    // holdings + matching distinct definitions.
                    property bool readyOk: root.loaded && !root.busy
                                          && cpRoot.cpA.def && cpRoot.cpB.def
                                          && cpRoot.cpA.def !== cpRoot.cpB.def
                                          && cpRoot.cpA.hold && cpRoot.cpB.hold
                    gradient:Gradient{
                        GradientStop{ position:0.0; color:root.brand }
                        GradientStop{ position:1.0; color:root.brand2 } }
                    opacity: readyOk ? 1 : 0.5
                    MouseArea{ anchors.fill:parent; enabled:parent.readyOk
                        onClicked:{
                            // Pool create funds vaults from the user's
                            // keypair holdings (token::Transfer with
                            // PDA-claim initialises the brand-new
                            // vaults). User's initial LP lands in
                            // ATA(owner, lp_def) via the in-tx chained
                            // ata::Create + token::Mint.
                            root.runAction("createPoolFor",
                                [cpRoot.cpA.hold, cpRoot.cpB.hold,
                                 cpAmtA.text, cpAmtB.text, cpRoot.cpFee],
                                "Creating pool",
                                "Submitting NewDefinition + initial liquidity…");
                            nav.pop();
                        } }
                    Text{ anchors.centerIn:parent
                          text:"Create pool"; color:"white"
                          font.pixelSize:17; font.weight:Font.Bold } }
            }
        }
    }
}
