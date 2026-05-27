#!/usr/bin/env bash
# Runs the UI wiring tests under qmltestrunner with offscreen Qt.
# No display required, no chain required (mock `logos` is in-process).
#
# Verifies: every QML button / runAction site routes to the right plugin
# method with the right args. Covers 17 tests including a real button
# click against the rendered Account-tab Shield button.
#
#   bash tests/run-ui-tests.sh
set -uo pipefail

# Discover qmltestrunner. Prefer Qt 6.11 (matches what builds the .lgx).
QMLTESTRUNNER="${QMLTESTRUNNER:-$(find /nix/store -maxdepth 4 -name qmltestrunner -type f 2>/dev/null | sort -r | head -1)}"
if [[ -z "${QMLTESTRUNNER:-}" || ! -x "$QMLTESTRUNNER" ]]; then
  echo "qmltestrunner not found. Set QMLTESTRUNNER=/path/to/qmltestrunner" >&2
  exit 2
fi

# QML modules live next to the test runner in qtdeclarative.
QT_QML_DIR="$(dirname "$(dirname "$QMLTESTRUNNER")")/lib/qt-6/qml"
if [[ ! -d "$QT_QML_DIR/QtQuick" ]]; then
  echo "QtQuick modules not found at $QT_QML_DIR" >&2
  exit 2
fi

cd "$(dirname "$0")"
exec env \
  QT_QPA_PLATFORM=offscreen \
  QML_IMPORT_PATH="$QT_QML_DIR" \
  QML2_IMPORT_PATH="$QT_QML_DIR" \
  "$QMLTESTRUNNER" -input tst_uiwire.qml "$@"
