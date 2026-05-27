#!/usr/bin/env bash
# Self-run MULTI-NODE Logos devnet (Path A+). PREREQUISITE: host
# docker-network port-publishing must work (see scripts/bootstrap notes /
# memory testnet-deployment). Test:
#   docker network create t; docker run -d --name t -p 18091:80 --network t nginx:alpine
#   curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18091/   # must be 200
# If 000, apply the privileged host fix first (restart docker daemon so it
# reinstalls iptables for all nets, and/or fix the host firewall's docker
# FORWARD/NAT integration), then re-test.
set -euo pipefail
LB="$HOME/ldex-spike/logos-blockchain"
N="${LDEX_DEVNET_NODES:-4}"   # cfgsync.yaml n_hosts is 4
cd "$LB"
docker tag ghcr.io/logos-blockchain/logos-blockchain@sha256:c5243681b353278cabb562a176f0a5cfbefc2056f18cebc47fe0e3720c29fb12 logos-blockchain:latest
DOCKER_COMPOSE_LIBP2P_REPLICAS="$N" docker compose up -d --no-build
echo "multi-node devnet up ($N nodes). Bootstrap node host ports: 3000, 18080."
echo "Then point LEZ L2 sequencer/indexer bedrock url at the bootstrap node"
echo "and re-run: LDEX_SEQUENCER_ADDR=http://127.0.0.1:3050 LDEX_WALLET_HOME=/tmp/ldex-testnet/wallet \\"
echo "  LDEX_BOOTSTRAP_OUT=.../scripts/bootstrap.testnet.env bash scripts/bootstrap.sh"
