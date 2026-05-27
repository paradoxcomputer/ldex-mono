#!/usr/bin/env bash
# Launch Logos Basecamp (Nix-built).
#
# Why not the AppImage? The downloaded logos-basecamp AppImage needs
# glibc >= 2.38 / GLIBCXX_3.4.32 (Ubuntu 24.04 era). This machine is
# Ubuntu 22.04 (glibc 2.35), so the AppImage cannot run here. Nix ships
# its own modern glibc/Qt, so the Nix-built binary works fine.
#
# First run builds Basecamp via Nix (slow, mostly binary-cache downloads).
# Subsequent runs are instant.

set -euo pipefail

# logos-basecamp clone location. Override with $LDEX_BASECAMP_DIR.
# Convention documented in SETUP.md is ~/ldex-spike/ref/logos-basecamp.
BC_DIR="${LDEX_BASECAMP_DIR:-$HOME/ldex-spike/ref/logos-basecamp}"

# --- make `nix` available ---
for p in /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh \
         "$HOME/.nix-profile/etc/profile.d/nix.sh"; do
  [ -e "$p" ] && . "$p" && break
done
export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"

if ! command -v nix >/dev/null 2>&1; then
  echo "error: nix not found. Is Nix installed?" >&2
  exit 1
fi

if [ ! -d "$BC_DIR" ]; then
  echo "error: $BC_DIR not found (logos-basecamp clone missing)." >&2
  exit 1
fi

cd "$BC_DIR"

# --- build if we don't have a result yet ---
if [ ! -x "$BC_DIR/result/bin/LogosBasecamp" ]; then
  echo ">> Building Basecamp via Nix (first run; this can take a while)..."
  nix build .#default --print-build-logs
fi

BIN="$BC_DIR/result/bin/LogosBasecamp"
if [ ! -x "$BIN" ]; then
  echo "error: build finished but $BIN is missing." >&2
  echo "Inspect: ls -l $BC_DIR/result/bin" >&2
  exit 1
fi

# --- pick a display backend (mirror logos-basecamp/run-dev.sh) ---
if [ -n "${WAYLAND_DISPLAY:-}" ] && [ -z "${QT_QPA_PLATFORM:-}" ]; then
  export QT_QPA_PLATFORM=wayland
fi

echo ">> Launching $BIN"
exec "$BIN" "$@"
