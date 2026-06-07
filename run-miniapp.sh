#!/usr/bin/env bash
# Launch the LDEX mini-app UI in logos-standalone-app (dev runner).
# Loads ui/ (ui_qml) plus its ldex_core dependency, wired via flake input.
#
# This is the fast dev loop. Final packaged testing happens in the real
# Basecamp host (./run-basecamp.sh). No git required (no .git in tree).

set -euo pipefail

# Discover the UI dir from this script's own location so the launcher
# works no matter where the repo is cloned.
SELF="${BASH_SOURCE[0]:-$0}"
REPO="$(cd "$(dirname "$SELF")" && pwd)"
UI_DIR="$REPO/mini-app/ui"
export LDEX_REPO="$REPO"

for p in /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh \
         "$HOME/.nix-profile/etc/profile.d/nix.sh"; do
  [ -e "$p" ] && . "$p" && break
done
export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
export GIT_TERMINAL_PROMPT=0

command -v nix >/dev/null 2>&1 || { echo "error: nix not found" >&2; exit 1; }
[ -d "$UI_DIR" ] || { echo "error: $UI_DIR not found" >&2; exit 1; }

cd "$UI_DIR"

# The ldex_core dependency is a local path: input. When core/ changes, the
# pinned NAR hash in flake.lock goes stale ("NAR hash mismatch"). Re-lock it
# so the dev run always picks up the current core/.
nix flake update ldex_core >/dev/null 2>&1 || rm -f flake.lock

# Mirror logos-basecamp/run-dev.sh display handling.
if [ -n "${WAYLAND_DISPLAY:-}" ] && [ -z "${QT_QPA_PLATFORM:-}" ]; then
  export QT_QPA_PLATFORM=wayland
fi

# Dev-mode proofs: privateSwap on the real STARK prover takes 30-60 min
# per op; with RISC0_DEV_MODE=1 it finishes in seconds. The proofs it
# produces are NOT cryptographically valid - only use this for local
# iteration. To run the real prover, unset RISC0_DEV_MODE before launch:
#   RISC0_DEV_MODE=0 bash run-miniapp.sh   (or unset RISC0_DEV_MODE)
export RISC0_DEV_MODE="${RISC0_DEV_MODE:-1}"
# Surface Rust panic messages + backtrace in the launcher's stderr -
# otherwise `thread caused non-unwinding panic. aborting.` is all we
# see, with no clue where it happened.
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

echo ">> nix run . (logos-standalone-app: ldex_ui + ldex_core)"
echo ">> RISC0_DEV_MODE=$RISC0_DEV_MODE  RUST_BACKTRACE=$RUST_BACKTRACE"
exec nix run . -- "$@"
