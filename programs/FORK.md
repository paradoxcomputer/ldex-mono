# `programs/` — LDEX on-chain programs (fork provenance)

This directory is a **fork of the Logos LEZ programs workspace**, vendored
into the project tree (no `.git` — the project is intentionally git-free;
build artifacts excluded).

## Upstream

- Repo: `https://github.com/logos-blockchain/lez-programs`
- Vendored at commit **`29b4c0173991b32548b5fcb3d7ec18742dc77070`**
  (2026-05-18, "fix(integration_tests) remove no longer needed program ID").
- Workspace: `amm`, `token`, `ata`, `stablecoin`, `oracle`/`mock_oracle`,
  `tools`, `integration_tests`, `amm-ui` (reference), `artifacts` (IDLs).
  Rust pinned to **1.94.0** (`rust-toolchain.yml`); builds against the
  RISC Zero zkVM (`riscv32im-risc0-zkvm-elf`).

## Our changes vs upstream (RFP-004 Func #6)

Exact diff: `../docs/ldex-amm-fork.patch` (4 files, +118/−17).

Upstream already implemented: per-pool fee tier field, tier validation
(`assert_supported_fee_tier`, tiers {1,5,30,100} bps = 0.01/0.05/0.3/1%),
immutability, and Uniswap-V2 fee→LP accrual in `swap.rs`.

The one RFP gap closed here: **the fee tier was not part of the pool PDA**,
so only one pool per token pair could exist. Changes:

- `amm/core/src/lib.rs` — `compute_pool_pda` / `compute_pool_pda_seed` take
  `fees`; seed = `sha256(token_1 ‖ token_2 ‖ fees_le)` (80 bytes). Each
  `(pair, fee tier)` → distinct pool; vault/LP/lock PDAs derive from the
  pool PDA so they inherit the tier automatically.
- `amm/src/new_definition.rs` — pass `fees` into the pool PDA + the
  production `Claim::Pda` authorization; validate the fee tier *before* the
  PDA assertion (clearer rejection of unsupported tiers).
- `amm/src/tests.rs` — per-tier id rebinding; new
  `test_pool_pda_distinct_per_fee_tier_for_same_pair` (coexistence).
- `integration_tests/tests/amm.rs` — per-tier message in the all-tiers e2e
  test.

## Build / test (from this directory)

```bash
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"
export LOGOS_BLOCKCHAIN_CIRCUITS="$HOME/.logos-blockchain-circuits"

RISC0_DEV_MODE=1 cargo test -p amm_program            # unit (92)
RISC0_DEV_MODE=1 cargo test -p integration_tests --test amm   # e2e (17, zkVM)
RISC0_SKIP_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings
```

Status at vendor time: AMM unit **92/92**, integration **17/17** green.

## Re-syncing from upstream

`docs/ldex-amm-fork.patch` is the portable representation of our delta.
To move to a newer upstream: re-clone `lez-programs` at the new rev,
`git apply` the patch (resolve conflicts in the 4 files), re-vendor here,
and update the commit hash above.
