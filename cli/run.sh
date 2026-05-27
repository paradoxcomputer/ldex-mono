#!/usr/bin/env bash
# Build (if needed) and run the LDEX CLI with the right LD_LIBRARY_PATH.
#
# Usage:
#   bash cli/run.sh <subcommand> [args...]
#   bash cli/run.sh status
#   bash cli/run.sh balance --all
#   bash cli/run.sh swap A B 100 --mode public
#
# This is a dev-loop convenience wrapper that rebuilds on every call.
# For long-term use, install once with `bash cli/install.sh`.
set -uo pipefail

# Discover repo root from the script's own location, so the wrapper
# works no matter where the user clones the repo.
SELF="${BASH_SOURCE[0]:-$0}"
HERE="$(cd "$(dirname "$SELF")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"

export PATH="$HOME/.cargo/bin:$PATH"
export LD_LIBRARY_PATH="$REPO/mini-app/core/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

cd "$REPO/cli" || exit 2
cargo build --release --quiet 2>&1 | tail -3

exec ./target/release/ldex "$@"
