#!/usr/bin/env bash
# Exercise every Phase-0 + Phase-1-off-chain protocol path in DEV mode.
#
# Three prover instances:
#   :8088 (hw=stub)
#   :8089 (hw=h100cc)  <-- TLS dev cert, bearer token "tok-A"
#   :8090 (hw=mi300sev)
#
# Tests run sequentially against the appropriate combination of provers.

set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${RECURSION_SRC_PATH:-}" && -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$PWD/scripts/recursion_zkr.zip"
fi
export RISC0_DEV_MODE=${RISC0_DEV_MODE:-1}

cargo build --release -p psychopomp-prover -p psychopomp-e2e

PROFILE=release
PORT1=${PORT1:-8088}
PORT2=${PORT2:-8089}
PORT3=${PORT3:-8090}
DIR1=$(mktemp -d /tmp/psy-state1.XXXX)
DIR2=$(mktemp -d /tmp/psy-state2.XXXX)
DIR3=$(mktemp -d /tmp/psy-state3.XXXX)
LOG1=$(mktemp /tmp/psy-prover1.XXXX.log)
LOG2=$(mktemp /tmp/psy-prover2.XXXX.log)
LOG3=$(mktemp /tmp/psy-prover3.XXXX.log)
POLICY2=$(mktemp /tmp/psy-policy.XXXX.toml)
REG=$(mktemp /tmp/psy-registry.XXXX.json)

cat >"$POLICY2" <<'EOF'
upload_bearer_tokens = ["tok-A"]
EOF

cleanup() {
    kill ${PID1:-} ${PID2:-} ${PID3:-} 2>/dev/null || true
    rm -rf "$DIR1" "$DIR2" "$DIR3"
    for L in "$LOG1" "$LOG2" "$LOG3"; do
        echo "[e2e-all] $L tail:"; tail -10 "$L" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "[e2e-all] launching prover1 :$PORT1 hw=stub (state=$DIR1)"
NO_COLOR=1 target/$PROFILE/psychopomp-prover \
    --bind 127.0.0.1:$PORT1 --state-dir "$DIR1" --hw-class stub \
    >"$LOG1" 2>&1 &
PID1=$!

echo "[e2e-all] launching prover2 :$PORT2 hw=h100cc TLS+auth (state=$DIR2)"
NO_COLOR=1 target/$PROFILE/psychopomp-prover \
    --bind 127.0.0.1:$PORT2 --state-dir "$DIR2" --hw-class h100cc \
    --tls-dev --policy "$POLICY2" \
    >"$LOG2" 2>&1 &
PID2=$!

echo "[e2e-all] launching prover3 :$PORT3 hw=mi300sev (state=$DIR3)"
NO_COLOR=1 target/$PROFILE/psychopomp-prover \
    --bind 127.0.0.1:$PORT3 --state-dir "$DIR3" --hw-class mi300sev \
    >"$LOG3" 2>&1 &
PID3=$!

wait_for_http() {  # url, timeout(s)
    local url=$1
    local extra=${2:-}
    for _ in $(seq 1 120); do
        if curl -fsS $extra "$url" >/dev/null 2>&1; then return 0; fi
        sleep 0.25
    done
    return 1
}
wait_for_http "http://127.0.0.1:$PORT1/v0/health" || { echo "p1 down" >&2; exit 1; }
wait_for_http "https://127.0.0.1:$PORT2/v0/health" "-k" || { echo "p2 down" >&2; exit 1; }
wait_for_http "http://127.0.0.1:$PORT3/v0/health" || { echo "p3 down" >&2; exit 1; }
echo "[e2e-all] all three provers up"

E2E="target/$PROFILE/psychopomp-e2e"

run_test() {
    local label=$1; shift
    echo
    echo "================ test: $label ================"
    "$E2E" "$@"
}

# Existing 4 modes
run_test hello                --endpoint "http://127.0.0.1:$PORT1" --test hello
run_test cached               --endpoint "http://127.0.0.1:$PORT1" --test cached
run_test composed             --endpoint "http://127.0.0.1:$PORT1" --test composed
run_test multi                --endpoint "http://127.0.0.1:$PORT1" --endpoint "http://127.0.0.1:$PORT3" --test multi

# New 5 modes
run_test timelock             --endpoint "http://127.0.0.1:$PORT1" --test timelock --timelock-iters 50000
run_test commit-reveal        --endpoint "http://127.0.0.1:$PORT1" --test commit-reveal

# Diverse needs prover1 (stub) + prover2 (h100) + prover3 (mi300); TLS-dev cert
# is not in our trust store, so for diverse test we use the two plain-HTTP provers.
run_test diverse              --endpoint "http://127.0.0.1:$PORT1" --endpoint "http://127.0.0.1:$PORT3" --test diverse

# Ranked: pre-bias ledger so prover3 wins
run_test ranked               --endpoint "http://127.0.0.1:$PORT1" --endpoint "http://127.0.0.1:$PORT3" --test ranked

# Discovery: write a registry.json with both endpoints + their MRENCLAVE + roots
# pulled live, then load it.
MRE1=$(curl -fsS "http://127.0.0.1:$PORT1/v0/attestation" | python3 -c "import sys,json; print(json.load(sys.stdin)['mrenclave'])")
ROOT1=$(curl -fsS "http://127.0.0.1:$PORT1/v0/attestation/roots" | python3 -c "import sys,json; print(json.load(sys.stdin)['roots'][0]['der_cert'])")
MRE3=$(curl -fsS "http://127.0.0.1:$PORT3/v0/attestation" | python3 -c "import sys,json; print(json.load(sys.stdin)['mrenclave'])")
ROOT3=$(curl -fsS "http://127.0.0.1:$PORT3/v0/attestation/roots" | python3 -c "import sys,json; print(json.load(sys.stdin)['roots'][0]['der_cert'])")
cat >"$REG" <<EOF
{
    "schema_version": 0,
    "operators": [
        {"endpoint": "http://127.0.0.1:$PORT1", "mrenclave": "$MRE1", "attestation_root": "$ROOT1", "hw_class": "Stub", "label": "p1"},
        {"endpoint": "http://127.0.0.1:$PORT3", "mrenclave": "$MRE3", "attestation_root": "$ROOT3", "hw_class": "MI300SEV", "label": "p3"}
    ]
}
EOF
run_test discover             --endpoint "http://127.0.0.1:$PORT1" --test discover --registry "$REG"

# Confirm bearer auth: unauthenticated POST /v0/elf to prover2 must 401, authenticated must 201
echo
echo "================ test: elf-auth ================"
ELF_BYTES=$(printf '%s' 'placeholder ELF bytes - wrong digest, expect 400 not 401')
UNAUTH=$(curl -k -s -o /dev/null -w "%{http_code}" -X POST -H "content-type: application/octet-stream" \
    --data-binary "$ELF_BYTES" "https://127.0.0.1:$PORT2/v0/elf/0000000000000000000000000000000000000000000000000000000000000000")
AUTH=$(curl -k -s -o /dev/null -w "%{http_code}" -X POST -H "content-type: application/octet-stream" \
    -H "Authorization: Bearer tok-A" \
    --data-binary "$ELF_BYTES" "https://127.0.0.1:$PORT2/v0/elf/0000000000000000000000000000000000000000000000000000000000000000")
echo "  no-auth     -> $UNAUTH (want 401)"
echo "  with-bearer -> $AUTH  (want 400; ELF bytes won't compute to claimed image_id)"
if [[ "$UNAUTH" == "401" && "$AUTH" == "400" ]]; then
    echo "PASS  elf upload bearer-auth enforced (401 → 400 after token)"
else
    echo "FAIL  elf-auth gate misbehaving"; exit 1
fi

# Confirm TLS handshake works against prover2 with -k (no client cert validation)
echo
echo "================ test: tls-handshake ================"
TLS=$(curl -k -s -o /dev/null -w "%{http_code}" "https://127.0.0.1:$PORT2/v0/health")
if [[ "$TLS" == "200" ]]; then
    echo "PASS  TLS handshake + health on https://127.0.0.1:$PORT2"
else
    echo "FAIL  TLS handshake (got $TLS)"; exit 1
fi

echo
echo "[e2e-all] all nine protocol modes + tls + auth tests PASS"
