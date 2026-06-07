#!/usr/bin/env bash
# Launch the LDEX mini-app via the Logos standalone-app dev runner.
#
#   bash run-miniapp.sh
#
# Proof mode: NOT in RISC0 dev mode (real STARKs). The dev sequencer at
# /tmp/ldex-dev/ was started without RISC0_DEV_MODE in its env, so it
# verifies real STARK proofs. We must match. Mode-1 / mode-2 private
# swaps take 10-15 min wall-clock per swap as a result.
#
# Want fast (fake) proofs for UI iteration? Both sides must agree:
# `export RISC0_DEV_MODE=1` AND restart the sequencer under the same
# flag. Setting it on only one side produces txs the other refuses.
set -euo pipefail

cd "$(dirname "$0")"

echo ">> nix run . (logos-standalone-app: ldex_ui + ldex_core)"
echo ">> RUST_BACKTRACE=1  (real STARK proofs - sequencer is in real mode)"

# Strip RISC0_DEV_MODE from the env handed to `nix run`. `unset` alone
# only modifies this bash; `env -u` guarantees the var is absent in the
# spawned process tree, even if the parent shell or a nix profile re-injected
# it. Mixing dev=1 in the prover with dev=0 in the sequencer (the live
# /tmp/ldex-dev/ runs without RISC0_DEV_MODE) produces fake proofs the
# sequencer refuses to verify.
unset RISC0_DEV_MODE
export RUST_BACKTRACE=1

# Warn loudly if something *still* sets it after the unset (e.g. a wrapper
# script, nix env, or shell startup file). Lets you catch the dev/real
# mismatch before it bites in the chain log.
if [[ -n "${RISC0_DEV_MODE:-}" ]]; then
  echo "WARN: RISC0_DEV_MODE=$RISC0_DEV_MODE leaked back after unset" >&2
fi

# The ui flake declares ldex_core via `path:../core`. Nix in pure-eval
# mode requires the surrounding directory to be a git repo for that
# relative path to resolve; if it isn't, fall back to `--override-input`
# with an absolute path. This lets a fresh `git clone ldex && cd ldex`
# and a `download .zip + extract` user both run with one command.
HERE="$(pwd)"
CORE_ABS="$(cd "$HERE/../core" && pwd)"
# Tell the plugin where to find scripts/bootstrap.env (the
# `envFilePath()` helper checks $LDEX_REPO first). Avoids any
# hardcoded `/home/<someone>/Documents/ldex` default.
LDEX_REPO_ABS="$(cd "$HERE/../.." && pwd)"
export LDEX_REPO="$LDEX_REPO_ABS"
if git -C "$HERE" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  exec env -u RISC0_DEV_MODE LDEX_REPO="$LDEX_REPO_ABS" nix run .
else
  echo ">> (not a git repo - using --override-input fallback for ldex_core)"
  exec env -u RISC0_DEV_MODE LDEX_REPO="$LDEX_REPO_ABS" nix run --override-input ldex_core "path:$CORE_ABS" .
fi
