#!/usr/bin/env bash
# Boots the full robot fleet demo stack in Docker (WSL2 or Linux).
#
# Usage:  ./demo/run-demo.sh
#
# What it does:
#   1. Generates PKI certs (skips if already present)
#   2. Builds and starts device-management-service + robot-agent via Docker Compose
#   3. robot-agent enrolls, then streams sine-wave telemetry to the fleet service
#
# Watch logs:  docker compose -f demo/docker/docker-compose.yml logs -f

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

CERTS="demo/pki/certs"

echo "=== Step 1: PKI bootstrap ==="
if [ -f "$CERTS/fleet-ca.pem" ]; then
  echo "Certs already present — skipping (delete $CERTS/ to regenerate)"
else
  if ! command -v cfssl &>/dev/null; then
    echo "ERROR: cfssl not found. Install with:"
    echo "  go install github.com/cloudflare/cfssl/cmd/cfssl@latest"
    echo "  go install github.com/cloudflare/cfssl/cmd/cfssljson@latest"
    exit 1
  fi
  bash demo/pki/bootstrap.sh
fi

echo ""
echo "=== Step 2: Docker Compose up ==="
docker compose -f demo/docker/docker-compose.yml up --build -d

echo ""
echo "=== Demo running ==="
echo "Logs:  docker compose -f demo/docker/docker-compose.yml logs -f"
echo "Stop:  docker compose -f demo/docker/docker-compose.yml down"
echo ""
echo "Waiting 5s then tailing logs (Ctrl-C to stop tailing, stack keeps running)..."
sleep 5
docker compose -f demo/docker/docker-compose.yml logs -f
