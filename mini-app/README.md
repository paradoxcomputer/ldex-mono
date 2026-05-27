# LDEX mini-app (Basecamp)

Privacy-preserving DEX on LEZ — Basecamp mini-app. Two components, as the
verified Basecamp architecture requires:

| Dir | Logos type | Runs | Role |
|-----|-----------|------|------|
| `core/` | `core` (native C++ plugin) | separate non-sandboxed `logos_host` process | DEX/prover backend; will link `wallet-ffi` (risc0 prove) and talk to a LEZ sequencer |
| `ui/`   | `ui_qml` (sandboxed QML) | inside Basecamp/standalone process | UI; calls `core` via `logos.callModule(...)` |

Sandboxed QML cannot run native code, so the prover/sequencer work lives in
`core/`; the UI only renders and dispatches calls.

## Build / run (Nix; no git required — dirs have no `.git`)

```bash
# core module (compiles C++ against the Logos SDK)
cd core && nix build .#default

# UI app — launches in logos-standalone-app (dev runner)
cd ui && nix run .
```

`logos-module-builder` is **experimental** ("do not use" upstream) but is the
only supported build path; expect rough edges.

## Status (walking skeleton — task #9)

- `core/`: minimal `core` module exposing `ping(msg)` and `getStatus()`
  (mirrors `logos-module-builder` `minimal-module`). No `wallet-ffi` yet.
- `ui/`: `ui_qml` app (mirrors `counter_qml`) with buttons that
  `logos.callModule("ldex_core", …)`, guarded so it degrades gracefully.
- Each component builds independently.

## Next (task #10)

1. Wire `ui` → `ldex_core` dependency (metadata `dependencies` + flake input)
   so the QML buttons actually round-trip to the native module in Basecamp.
2. Add `wallet-ffi` to `core/` as an external library (risc0 `prove`),
   connect to a local standalone LEZ sequencer, and perform one real op
   (account balance / public swap) — proving the full UI → native → chain
   seam before layering AMM + the deshield→swap→reshield privacy path.
