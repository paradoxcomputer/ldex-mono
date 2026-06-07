# `ldex` - command-line client for the LDEX privacy DEX

A single binary that exposes every mini-app feature behind the same FFI
the QML uses. Bypass the GUI when it misbehaves; script flows that the UI
doesn't (yet) support.

## Build & run

```bash
bash cli/run.sh <subcommand> [args...]
```

The wrapper sets `LD_LIBRARY_PATH` to the vendored FFI libs at
`mini-app/core/lib/` and builds the binary on first run (incremental
afterwards). The binary itself is at `cli/target/release/ldex`.

## Setup

Requires a populated `scripts/bootstrap.env` from `scripts/bootstrap.sh`.
The CLI reads it automatically (override with `--env-file PATH`).

## Tokens

Refer to tokens by their bootstrap letter (`A`, `B`, …, `J`) or by `LEZ`
(the wrapped-native bridge). Raw `Public/<b58>` and 64-hex ids also work
for any token argument.

## Commands

### Read-only

| Command | What it shows |
|---|---|
| `ldex status` | sequencer + chain height + wallet sync + native LEZ |
| `ldex sync` | force wallet sync to chain head |
| `ldex balance` | A/B/LEZ balances split into HOLD / ATA / PRIV / TOTAL |
| `ldex balance --all` | every token from `LDEX_TOKENS` |
| `ldex accounts` | every wallet-owned account (pub + priv) |
| `ldex pools` | every existing `(pair, fee_tier)` pool with reserves + LP |
| `ldex pool A B [-f 5]` | one pool's reserves, LP supply, cum_volume, cum_fees |
| `ldex quote A B 100 [-f 5]` | constant-product output for the trade, plus impact + fee |
| `ldex env` | dump the resolved bootstrap environment |

### Native LEZ ↔ WLEZ

| Command | Effect |
|---|---|
| `ldex wrap 1000` | native LEZ (USER_OWNER) → WLEZ (HOLD_W) |
| `ldex unwrap 100` | WLEZ (HOLD_W) → native LEZ |

### Shielding (real STARK, ~3-5 min each on CPU)

| Command | Effect |
|---|---|
| `ldex shield A 50` | HOLD_A → PRIV_A |
| `ldex deshield A 50` | PRIV_A → HOLD_A |

### Swaps

```bash
# Public mode-0 ATA swap (~15 s):
ldex swap A B 100

# Private mode-1 PrivateOwned swap (~10-15 min STARK on CPU):
ldex swap A B 100 --mode private

# Disposable mode-2 fresh-A swap (~15-25 min STARK):
ldex swap A B 100 --mode disposable

# All modes accept --fee and --slip:
ldex swap A B 100 -f 5 -s 0.5    # 0.05% fee tier, 0.5% slippage
```

### Top up between runs

```bash
# Default: 1000 native LEZ + 10000 TOKENA / TOKENB into each ATA.
ldex fund

# Custom amounts + wider token set:
ldex fund --lez 500 --token 5000 --tokens "A B C D"

# LEZ only:
ldex fund --skip-tokens

# Tokens only:
ldex fund --skip-lez --tokens "A B C"
```

`fund` waits for on-chain inclusion (~15 s/op) and prints the pre→post
delta for each ATA, so you can spot a tx that submitted but didn't land.
Native LEZ comes from the dev sequencer's preconfigured funder account
(`2RHZ…`); tokens come from your `HOLD_<L>` (already minted by bootstrap).

### Liquidity

```bash
ldex pool-create A B -f 5 --amount-a 100000 --amount-b 100000
ldex liq add A B 5000 5000 -f 5            # public add (mode-0 ATA)
ldex liq remove A B 1000 -f 5              # public remove
ldex liq add A B 5000 5000 -f 5 -m private # private add (STARK)
```

## End-to-end smoke (no UI)

```bash
source scripts/bootstrap.env  # not required - CLI loads it - but useful for shell completions

bash cli/run.sh status
bash cli/run.sh balance
bash cli/run.sh pools

bash cli/run.sh quote A B 100
bash cli/run.sh swap A B 100              # public, ~15 s
bash cli/run.sh balance                   # confirm ATA_A −100, ATA_B +98

bash cli/run.sh wrap 500
bash cli/run.sh unwrap 100
bash cli/run.sh balance
```

## Why a CLI

Three reasons:

1. **The UI has had bugs.** The CLI exercises the same FFI surface, so
   it's the floor for "does the chain side work?" - useful while the
   mini-app is being polished.
2. **Scripting.** Pipe `ldex` calls into shell loops for stress tests,
   reproducible bench runs, or operator dashboards.
3. **Debugging.** A failing swap in the GUI is opaque; `ldex swap`
   prints the exact rc / tx hash / elapsed time, no console digging
   required.

The CLI links the **same** `libldex_amm_ffi.so` the mini-app loads, so
there is no behavioural drift between the two surfaces.

## Notes

- **Auto-sync.** Commands that read private state (`status`, `balance`)
  sync the wallet to chain head first. Pass `--no-sync` to skip.
- **Latency disclosure.** Private/Disposable swaps and shield/deshield
  generate real RISC-Zero STARKs. On a Ryzen 7 PRO 7840U (no GPU),
  expect 3-5 min for shield/deshield, 10-15 min for mode-1, 15-25 min
  for mode-2 / native-batched. Faster on a GPU prover; orthogonal work.
- **Pool ordering.** Pools are keyed by `(defA, defB)` in a canonical
  order. The CLI probes both orderings so `pool A B` and `pool B A`
  resolve to the same pool, but values are always shown in the pool's
  on-chain ordering.
