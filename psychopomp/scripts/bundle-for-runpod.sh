#!/usr/bin/env bash
# Bundle this repo into psychopomp.tar.gz for upload to a RunPod / Vast / Lambda
# pod. Excludes target/, local sequencer + prover state (signing keys!), and
# git metadata. The pod doesn't need any of that to build + run the prover.

set -euo pipefail
cd "$(dirname "$0")/.."

# Refuse to bundle if a sequencer signing key is anywhere it might get swept in.
# The pod operator is untrusted; signing material must not leave the laptop.
for stray in $(find . -maxdepth 4 -type f \
                    \( -name '*signing_key*' -o -name 'storage.json' \) \
                    -not -path './target/*' \
                    -not -path './sequencer-state/*' \
                    -not -path './psychopomp-state/*' 2>/dev/null); do
    echo "[bundle] refusing: candidate-secret outside excluded dirs: $stray" >&2
    exit 1
done

OUT=${OUT:-../psychopomp.tar.gz}
tar -czf "$OUT" \
    --exclude='target' \
    --exclude='*.tar.gz' \
    --exclude='.git' \
    --exclude='./sequencer-state' \
    --exclude='./psychopomp-state' \
    --transform 's,^\.,psychopomp,' \
    .

ls -lh "$OUT"
echo
echo "Upload $OUT to the pod, then on the pod:"
echo "  mkdir -p /opt && cd /opt"
echo "  REPO_TARBALL=\$PWD/psychopomp.tar.gz bash psychopomp/scripts/runpod-bootstrap.sh"
echo "(the bootstrap script untars psychopomp/ next to the tarball, then builds.)"
