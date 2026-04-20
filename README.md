# Robot Fleet Management System

A production-architecture demonstration of a humanoid robot fleet management
system. Each component is independently testable and independently deployable.

## Repository layout

```
proto/                      # Protobuf contracts (source of truth for all services)
pki/                        # mTLS helpers (rcgen CSR signing, cert verification)
device-management-service/  # Enrollment, heartbeat, device registry, provisioning
ota-service/                # Over-the-air update dispatch and tracking
artifact-store/             # Versioned firmware binary store (MinIO-backed)
robot-agent/                # Rust gRPC client running on each robot
simulator/                  # Isaac Sim integration + robot controller
demo/                       # Docker Compose demo orchestration + PKI bootstrap
```

## Device lifecycle — three phases

```
Phase 0  Manufacturing (offline, one-time per batch)
  ├─ Operator seeds FACTORY_MANIFEST with serial + one-time token + model
  └─ Token represents a secret injected at manufacturing time
     (hardware equivalent: burned into fuse memory or secure enclave)

Phase 1  Factory provisioning (first boot, no cert yet)
  ├─ Robot generates keypair  ← simulates TPM keygen; private key never leaves device
  ├─ Robot sends CSR + serial + token  →  ProvisioningService (port 8445, TLS-only)
  ├─ Service verifies serial in manifest, validates token, signs CSR with Device CA
  ├─ Service seeds pre_enrollment table  →  device is now ready for Phase 2
  └─ Robot stores device cert on disk

Phase 2  Bootstrap enrollment (first operational boot)
  ├─ Robot connects to bootstrap endpoint (port 8444, mTLS — Device CA trust)
  ├─ Service verifies cert chain against Device CA
  ├─ Service atomically claims serial in pre_enrollment  ← replay defence
  ├─ Service signs new CSR with Fleet CA → operational cert
  └─ Robot stores operational cert + pins Fleet CA chain

Phase 3  Normal operation
  ├─ All connections use operational cert (port 8443, mTLS — Fleet CA trust)
  ├─ StreamHeartbeat  — liveness, CPU/memory, directives
  ├─ StreamTelemetry  — high-frequency joint/sensor data
  └─ WatchUpdates     — OTA commands; cert rotated every 30 days via RenewCert
```

## Why three phases?

| Concern | Phase |
|---|---|
| Hardware identity (serial, model) | Phase 0 — offline, admin-controlled |
| Device cert issuance | Phase 1 — provisioning service, Device CA |
| Fleet admission | Phase 2 — bootstrap endpoint, pre-enrollment claim |
| Ongoing operations | Phase 3 — Fleet CA, short-lived certs |

The Device CA key only ever signs device certs at provisioning time — it never
touches operational infrastructure. The Fleet CA only signs certs for devices
that have passed the admission check. A compromised device cert cannot enroll
twice (atomic claim); a compromised operational cert cannot be renewed for an
inactive device.

In production, Phase 1 token auth would be replaced by TPM hardware attestation
(EK certificate from chip manufacturer, or a TPM quote proving key is
hardware-bound). The token is the software stand-in for that proof.

## Port layout

| Port | Auth | Purpose |
|------|------|---------|
| 8443 | mTLS (Fleet CA) | All operational RPCs — heartbeat, telemetry, OTA, device queries |
| 8444 | mTLS (Device CA) | Bootstrap enrollment only |
| 8445 | TLS only (no client cert) | Factory provisioning — token auth, no device cert yet |

## Running the demo

```bash
# 1. Generate PKI infrastructure (Fleet CA, Device CA, server cert)
rm -rf demo/pki/certs && bash demo/pki/bootstrap.sh

# 2. Start the fleet management service
sudo docker compose -f demo/docker/docker-compose.yml down -v
sudo docker compose -f demo/docker/docker-compose.yml up --build -d device-management-service

# 3. Add robots live — each one runs all three phases automatically
sudo docker compose -f demo/docker/docker-compose.yml up -d robot-agent-1
sudo docker compose -f demo/docker/docker-compose.yml up -d robot-agent-2
sudo docker compose -f demo/docker/docker-compose.yml up -d robot-agent-3

# Watch what happens in the DMS
sudo docker compose -f demo/docker/docker-compose.yml logs -f device-management-service
```

**Expected DMS log sequence per robot:**
```
INFO  [Phase 0] factory manifest seeded — manufacturing token registered  serial=robot-001
INFO  [Phase 1 complete] device cert issued (Device CA signed) — pre-enrollment seeded  serial=robot-001  port=8445
INFO  [Phase 2 complete] bootstrap enrollment — operational cert issued (Fleet CA signed)  serial=robot-001  device_id=<uuid>  port=8444
INFO  [Phase 3] heartbeat stream connected — robot is live  device_id=<uuid>  port=8443
INFO  [Phase 3] telemetry stream active  device_id=<uuid>  frames_in_window=50  fps=10  port=8443
```

## Running tests

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Each crate has a self-contained test suite that runs with no external services.
Integration tests (real SQLite, no mocks) are in the same suite — `rusqlite`
uses a bundled SQLite so there are no system dependencies.

## Commit conventions

[Conventional Commits](https://www.conventionalcommits.org/):

```
feat(provisioning): add ProvisioningService with factory manifest token auth
feat(device-mgmt): add factory_manifest table and claim_manifest_entry
fix(enrollment): reject CSR when serial not in pre-enrollment registry
test(pki): add CA hierarchy and cert verification tests
```

One branch per module. PRs stay focused — no cross-module changes in a single
PR unless they are a coordinated proto change.
