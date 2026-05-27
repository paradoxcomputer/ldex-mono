# Contributing to Psychopomp

Thanks for taking a look. Psychopomp is the TEE-attested outsourced RISC Zero
proving marketplace for the Logos Execution Zone. Most contributions land in
one of three places — see [the README](README.md) for the architecture, and
[BUILD.md](BUILD.md) for the full runbook.

## Repo layout

| Path | What lives there |
|---|---|
| `crates/psychopomp-types` | Wire types (`AttestationDoc`, `WitnessPayload`, `JobRequest`, ...) |
| `crates/psychopomp-attest` | `Attestor`/`Verifier_` traits + Phase-0 `StubAttestor` |
| `crates/psychopomp-client` | Wallet-side SDK (`prove`, `prove_multi`, `ensure_elf_cached`, discovery, reputation) |
| `crates/psychopomp-prover` | Operator daemon: axum HTTP + RISC0 (CUDA via `--features gpu`) |
| `crates/psychopomp-e2e` | End-to-end harness — drives the e2e modes documented in BUILD.md |
| `crates/psychopomp-chain` | LEZ on-chain client: tx builders + state readers + `live-*` examples |
| `crates/psychopomp-hwclass` | Shared `HwClass` enum (Stub / H100CC / MI300SEV / TDX) |
| `guests/{hello,composed,heavy}` | RISC0 guests used by the e2e harness |
| `Phase1-onchain/psychopomp-{registry,escrow}-core` | Pure-Rust state machines |
| `Phase1-onchain/psychopomp-{registry,escrow}-program` | `nssa` AccountWithMetadata bridge |
| `Phase1-onchain/psychopomp-{registry,escrow}/methods` | LEZ guest source + risc0-build wrapper |
| `scripts/` | Build / e2e / bundle / sequencer wrangling |

## Local dev loop

```bash
./scripts/bootstrap-dev.sh                       # one-shot: install + build + test
RISC0_DEV_MODE=1 ./scripts/e2e-localhost.sh      # 1 prover, ~2s
RISC0_DEV_MODE=1 ./scripts/e2e-all.sh            # 3 provers, 11 protocol modes, ~30s
./scripts/e2e-localhost.sh --release             # real STARK, ~10s on a Ryzen-class CPU
```

## Running the tests

The Phase-0 stack and the Phase-1 state-machines are pure Rust — they unit-test
with no toolchain beyond stable + the RISC Zero target. The LEZ guests under
`Phase1-onchain/*/methods/guest/` require `docker buildx` to compile (they go
through `cargo risczero build`). The bootstrap script skips them; CI should
too. To test the part the bootstrap covers manually:

```bash
cargo test --no-fail-fast \
    -p psychopomp-types -p psychopomp-attest -p psychopomp-hwclass \
    -p psychopomp-client -p psychopomp-prover -p psychopomp-e2e \
    -p psychopomp-registry-core -p psychopomp-escrow-core \
    -p psychopomp-registry-program -p psychopomp-escrow-program
```

For the LEZ guests (Phase-1 on-chain — requires docker + a Logos Execution Zone
checkout):

```bash
./scripts/build-lez-guests.sh
```

## Adding a new guest

Mirror `guests/heavy/` + `guests/heavy-methods/`. Two crates:

1. **`guests/<name>/`** — the actual RISC0 guest. Its own workspace
   (`[workspace]` marker), one bin in `src/bin/<name>.rs`. Add to
   `Cargo.toml` workspace `exclude`.
2. **`guests/<name>-methods/`** — a host-side crate whose `build.rs` calls
   `risc0_build::embed_methods()`. Add to workspace `members` and
   `workspace.dependencies` as `<name>-methods = { path = "guests/<name>-methods" }`.

Then expose it through the e2e harness in `crates/psychopomp-e2e/src/main.rs`.

## Style

- `cargo fmt --all`
- `cargo clippy --workspace -- -D warnings` (note: `--all-targets` currently
  trips on a known risc0-build + clippy interaction in the guest-methods
  crates; clippy without `--all-targets` is clean)
- No emojis in code or docs unless the user asked for them.
- Keep comments lean — the existing crates favour terse, motivation-only
  comments over what-the-code-does narration.

## Security

If you find a vulnerability, please **do not** open a public GitHub issue.
See [SECURITY.md](SECURITY.md) for responsible-disclosure instructions.

## Licensing

Dual MIT / Apache-2.0. Submitting a contribution is an implicit declaration
that it is your work and that you license it under those terms (the standard
Rust ecosystem convention).
