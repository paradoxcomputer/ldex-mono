#!/usr/bin/env bash
# Layer A - plugin/FFI integration tests.
#
# Usage:
#   bash mini-app/tests/layer-a/run.sh           # read-only tests (~10s)
#   LAYER_A_MUTATE=1 bash mini-app/tests/layer-a/run.sh   # adds wrap round-trip (~30s)
#
# Pre-reqs:
#   - Sequencer reachable at $LDEX_SEQUENCER_ADDR
#   - scripts/bootstrap.env emitted by scripts/bootstrap.sh
set -uo pipefail

# Discover repo root from the script's own location.
SELF="${BASH_SOURCE[0]:-$0}"
HERE="$(cd "$(dirname "$SELF")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
ENV_FILE="${LDEX_ENV_FILE:-$REPO/scripts/bootstrap.env}"

if [ ! -f "$ENV_FILE" ]; then
    echo "FAIL: $ENV_FILE not found - run scripts/bootstrap.sh first" >&2
    exit 2
fi
# shellcheck disable=SC1090
source "$ENV_FILE"

export PATH="$HOME/.cargo/bin:$PATH"
export LIBRARY_PATH="${LIBRARY_PATH:-}:$REPO/mini-app/core/lib"
export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:-}:$REPO/mini-app/core/lib"

cd "$REPO/mini-app/tests/layer-a" || exit 2

# Compile against the vendored FFI library.
cargo build --release 2>&1 | tail -5
if [ ! -x ./target/release/layer-a ]; then
    echo "FAIL: layer-a binary did not build" >&2
    exit 2
fi

exec ./target/release/layer-a
