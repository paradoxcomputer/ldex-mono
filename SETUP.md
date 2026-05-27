# LDEX — fresh-clone setup

How to take a clean clone of this repo and reach a working dev chain +
CLI + mini-app. Tested on Ubuntu 22.04 / 24.04 / Nix-on-Linux.

## 0. Prerequisites

- **Rust toolchain** — install via [rustup](https://rustup.rs).
  The repo pins `rustc 1.94` (host can be newer).
- **Nix with flakes** — install [Determinate Nix](https://determinate.systems/nix-installer/)
  (or upstream Nix with `experimental-features = nix-command flakes`).
- **RISC Zero** — `cargo install cargo-binstall && cargo binstall cargo-risczero` then
  `cargo risczero install` (gets the `r0vm` runtime).
- **Docker BuildKit** — required by `cargo risczero build`. `sudo apt install docker-buildx`.
- **`logos-blockchain-circuits`** — RISC Zero circuit blobs. Clone to a
  known dir, then `export LOGOS_BLOCKCHAIN_CIRCUITS=$HOME/.logos-blockchain-circuits`.

## 1. Get the LEZ source tree

The wallet + sequencer + privacy circuit live in the upstream LEZ
repo, not in this repo. Clone the pinned tag `v0.2.0-rc3`:

```bash
mkdir -p ~/ldex-spike && cd ~/ldex-spike
git clone --branch v0.2.0-rc3 https://github.com/logos-co/lez.git
cd lez
cargo build --release   # wallet, sequencer, friends
```

The build emits `target/release/wallet` and (after `--features standalone`)
`target/release/sequencer_service`.

**Non-default location?** Set `LDEX_LEZ_DIR=/path/to/lez` and every LDEX
script + the CLI will find it.

## 2. Clone this repo

```bash
git clone https://gitlab.com/paradoxcomputer/ldex-mono.git ldex
cd ldex
```

Every script in this repo derives the repo root from its own location
(`$0`-based) and uses repo-relative paths. The only external dependency
is the LEZ source tree from step 1.

## 3. Bring up the dev sequencer

```bash
bash run-sequencer.sh    # standalone LEZ sequencer on 127.0.0.1:3040
```

Leave this terminal open. Override the port via `LDEX_SEQUENCER_PORT`
if 3040 is taken; the LEZ binary uses `--port`.

## 4. Bootstrap the chain

In a second terminal:

```bash
bash scripts/bootstrap.sh
```

Deploys the 7 LDEX programs (token, ata, amm, amm_v2, private_swap_router,
wlez), creates the test wallet, mints 10 tokens, shields 8 of them into
private accounts (real STARK proofs, ~5 min × 8 ≈ 40 min on CPU), and
emits `scripts/bootstrap.env` with all the ids the CLI + mini-app need.

Override knobs:
- `LDEX_LEZ_DIR=/path/to/lez` — LEZ clone (default `~/ldex-spike/lez`)
- `LDEX_WALLET_HOME=/path/to/wallet-dir` — wallet state (default `/tmp/ldex-bootstrap/wallet`)
- `LDEX_SEQUENCER_ADDR=http://...` — target sequencer (default `:3040`)

## 5. Install the CLI

```bash
bash cli/install.sh
```

Builds `ldex` and symlinks it onto your PATH (`~/.cargo/bin` if available,
else `~/.local/bin`). The binary has its FFI library path baked in via
RUNPATH, so it works from any cwd without `LD_LIBRARY_PATH`.

Smoke test:

```bash
ldex status
ldex balance
ldex pools
ldex quote A B 100
```

## 6. (Optional) Run the mini-app

The mini-app is the QML/C++ UI shipped as a Logos Basecamp module pair
(`ldex_core` + `ldex_ui`).

```bash
# One-time: clone logos-basecamp for the dev runner.
git clone https://github.com/logos-co/logos-basecamp.git ~/ldex-spike/ref/logos-basecamp
# Or set LDEX_BASECAMP_DIR=/path/to/logos-basecamp

# Launch the standalone-app dev runner:
bash mini-app/ui/run-miniapp.sh
```

First run builds + populates the Nix store; subsequent runs are instant.

## 7. (Optional) Run the test framework

Layer A — plugin/FFI integration tests:

```bash
bash mini-app/tests/layer-a/run.sh           # ~10 s read-only
LAYER_A_MUTATE=1 bash mini-app/tests/layer-a/run.sh   # adds wrap round-trip
```

Layers B and C are in progress.

## Environment variables — full list

| Variable | Default | What |
|---|---|---|
| `LDEX_LEZ_DIR` | `~/ldex-spike/lez` | Path to the upstream LEZ source tree (wallet, sequencer) |
| `LDEX_WALLET_BIN` | `$LDEX_LEZ_DIR/target/release/wallet` | Explicit wallet binary override |
| `LDEX_BASECAMP_DIR` | `~/ldex-spike/ref/logos-basecamp` | logos-basecamp clone (for `run-basecamp.sh`) |
| `LDEX_WALLET_HOME` | `/tmp/ldex-bootstrap/wallet` | Wallet config + storage dir |
| `LDEX_WALLET_PW` | `ldexdev` | Wallet password |
| `LDEX_SEQUENCER_ADDR` | `http://127.0.0.1:3040` | Target sequencer URL |
| `LDEX_SEQUENCER_CONFIG` | `$LDEX_LEZ_DIR/sequencer/service/configs/debug/sequencer_config.json` | Sequencer config path (run-sequencer.sh) |
| `LDEX_ENV_FILE` | `<repo>/scripts/bootstrap.env` | bootstrap.env path (CLI + tests) |
| `LDEX_BOOTSTRAP_OUT` | `<repo>/scripts/bootstrap.env` | Where bootstrap.sh writes its output |
| `LOGOS_BLOCKCHAIN_CIRCUITS` | (required) | Path to RISC Zero circuit blobs |
| `RISC0_DEV_MODE` | unset | Set to 1 for fake-proofs dev loop (sequencer must agree) |

## Troubleshooting

- **`LEZ wallet binary not found`** — set `LDEX_WALLET_BIN` or
  `LDEX_LEZ_DIR`, or clone+build LEZ at `~/ldex-spike/lez`.
- **`NAR hash mismatch` from `nix run`** — `cd mini-app/ui && nix flake update ldex_core`.
- **Bootstrap funding step fails** — see [README troubleshooting] section
  (the sequencer config field names changed in upstream LEZ — modern
  LEZ uses `initial_public_accounts` not `initial_accounts`).
- **Mini-app shows 0 for shielded balances** — fixed in this repo
  (commit-hash TBD); rebuild `mini-app/core` if balances still read 0.
