#!/usr/bin/env bash
# Verifies the persistent job table: prove one job, kill the prover, restart
# with the same --state-dir, GET /v0/jobs/{id} should return the Done status.

set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${RECURSION_SRC_PATH:-}" && -f scripts/recursion_zkr.zip ]]; then
    export RECURSION_SRC_PATH="$PWD/scripts/recursion_zkr.zip"
fi
export RISC0_DEV_MODE=${RISC0_DEV_MODE:-1}

cargo build --release -p psychopomp-prover -p psychopomp-e2e

PORT=${PORT:-8090}
DIR=$(mktemp -d /tmp/psy-persist.XXXX)

cleanup() { kill ${PID:-} 2>/dev/null || true; rm -rf "$DIR"; }
trap cleanup EXIT

launch() {
    NO_COLOR=1 target/release/psychopomp-prover --bind 127.0.0.1:$PORT --state-dir "$DIR" >/tmp/psy-persist.log 2>&1 &
    PID=$!
    for _ in $(seq 1 120); do
        curl -fsS "http://127.0.0.1:$PORT/v0/health" >/dev/null 2>&1 && break
        sleep 0.25
    done
}

echo "[persist] launch 1"
launch
JOB_OUTPUT=$(target/release/psychopomp-e2e --endpoint "http://127.0.0.1:$PORT" --test hello 2>&1)
echo "$JOB_OUTPUT" | grep PASS

# Grab the most recent job_id from the prover log (ANSI-stripped via NO_COLOR=1)
JOB_ID=$(grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' /tmp/psy-persist.log | tail -1)
if [[ -z "$JOB_ID" ]]; then
    echo "[persist] FAIL — could not find a job_id in prover log" >&2
    cat /tmp/psy-persist.log >&2
    exit 1
fi
echo "[persist] job_id=$JOB_ID"

curl -fsS "http://127.0.0.1:$PORT/v0/jobs/$JOB_ID" | head -c 200; echo

echo "[persist] killing prover"
kill $PID
wait $PID 2>/dev/null || true

echo "[persist] launch 2 (same state-dir)"
launch

STATUS=$(curl -fsS "http://127.0.0.1:$PORT/v0/jobs/$JOB_ID")
echo "[persist] post-restart status: $(echo "$STATUS" | head -c 80)..."
if echo "$STATUS" | grep -q '"state":"done"'; then
    echo "[persist] PASS — Done status survived restart"
else
    echo "[persist] FAIL — expected state=done"
    echo "[persist] full response: $STATUS"
    exit 1
fi
