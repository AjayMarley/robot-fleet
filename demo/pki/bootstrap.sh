#!/usr/bin/env bash
# Generates all PKI certs needed for the demo stack.
# Output: demo/pki/certs/  (gitignored)
# Requires: cfssl, cfssljson  (install: go install github.com/cloudflare/cfssl/cmd/...)

set -euo pipefail
cd "$(dirname "$0")"

CERTS=certs
mkdir -p "$CERTS"

# ── CA config ─────────────────────────────────────────────────────────────────
cat > "$CERTS/ca-config.json" <<'JSON'
{
  "signing": {
    "default": { "expiry": "8760h" },
    "profiles": {
      "server": { "usages": ["signing","key encipherment","server auth"], "expiry": "8760h" },
      "client": { "usages": ["signing","key encipherment","client auth"], "expiry": "8760h" },
      "peer":   { "usages": ["signing","key encipherment","server auth","client auth"], "expiry": "8760h" }
    }
  }
}
JSON

# ── Fleet CA (signs operational device certs + server cert) ───────────────────
cat > "$CERTS/fleet-ca-csr.json" <<'JSON'
{"CN":"Fleet CA","key":{"algo":"ecdsa","size":256},"names":[{"O":"RobotFleet"}]}
JSON
cfssl gencert -initca "$CERTS/fleet-ca-csr.json" | cfssljson -bare "$CERTS/fleet-ca"

# ── Device CA (signs bootstrap device certs) ──────────────────────────────────
cat > "$CERTS/device-ca-csr.json" <<'JSON'
{"CN":"Device CA","key":{"algo":"ecdsa","size":256},"names":[{"O":"RobotFleet"}]}
JSON
cfssl gencert -initca "$CERTS/device-ca-csr.json" | cfssljson -bare "$CERTS/device-ca"

# ── Server cert (device-management-service — SANs cover Docker service names) ─
cat > "$CERTS/server-csr.json" <<'JSON'
{"CN":"device-management-service","hosts":["device-management-service","localhost","127.0.0.1"],"key":{"algo":"ecdsa","size":256}}
JSON
cfssl gencert \
  -ca="$CERTS/fleet-ca.pem" -ca-key="$CERTS/fleet-ca-key.pem" \
  -config="$CERTS/ca-config.json" -profile=peer \
  "$CERTS/server-csr.json" | cfssljson -bare "$CERTS/server"

# ── Device bootstrap cert (robot-agent uses this to enroll) ───────────────────
cat > "$CERTS/device-csr.json" <<'JSON'
{"CN":"robot-001","hosts":[],"key":{"algo":"ecdsa","size":256},"names":[{"O":"RobotFleet"}]}
JSON
cfssl gencert \
  -ca="$CERTS/device-ca.pem" -ca-key="$CERTS/device-ca-key.pem" \
  -config="$CERTS/ca-config.json" -profile=client \
  "$CERTS/device-csr.json" | cfssljson -bare "$CERTS/device"

echo ""
echo "PKI bootstrap complete. Certs written to demo/pki/certs/"
ls -1 "$CERTS/"*.pem
