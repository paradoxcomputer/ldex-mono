#!/usr/bin/env bash
# scripts/run-app.sh — one-shot launcher for the LDEX mini-app.
#
#   ./scripts/run-app.sh
#
# Verifies the LDEX dev sequencer is reachable, sources bootstrap.env
# (regenerating it if missing), then `nix run`s the UI module via the
# logos-standalone-app dev runner.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# 1. Sequencer reachable?
if ! pgrep -f "sequencer_service.*ldex-dev" >/dev/null 2>&1 \
   && ! pgrep -f "sequencer_service.*port 3060" >/dev/null 2>&1; then
  echo "LDEX dev sequencer not running (expected sequencer_service on :3060)." >&2
  echo "  Start it from your LEZ checkout, or re-run scripts/bootstrap.sh." >&2
  exit 1
fi

# 2. bootstrap.env present?
if [[ ! -f scripts/bootstrap.env ]]; then
  echo "scripts/bootstrap.env missing — running scripts/bootstrap.sh first…" >&2
  ./scripts/bootstrap.sh
fi
# shellcheck disable=SC1091
source scripts/bootstrap.env

# 3. Launch the UI (logos-standalone-app dev runner).
cd mini-app/ui
exec nix run .
