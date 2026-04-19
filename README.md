# Robot Fleet Management System

A production-architecture demonstration of a humanoid robot fleet management
system. Each component is an independently testable, independently deployable
module.

## Architecture

```
proto/                      # Protobuf contracts (source of truth)
pki/                        # mTLS helpers, CA bootstrap scripts
device-management-service/  # Robot enrollment, heartbeat, device registry
ota-service/                # Over-the-air update dispatch and tracking
artifact-store/             # Versioned firmware/software binary store (MinIO)
robot-agent/                # Rust gRPC client running on the robot
simulator/                  # Isaac Sim integration + robot controller
demo/                       # Docker Compose demo orchestration
```

## Trust model

Enrollment uses a **two-phase PKI** with a **pre-enrollment registry**:

1. **Factory phase** — each device is registered in the pre-enrollment table
   (`serial → unclaimed`) and receives a device certificate signed by the
   Device CA. The private key is generated on-device and never exported.

2. **First-boot phase** — the robot calls `BootstrapEnroll` over mTLS
   (presenting its device cert). The service:
   - Verifies the cert chain against the Device CA
   - Atomically claims the serial in the pre-enrollment table (replay defence)
   - Signs the robot's CSR with the Fleet CA, issuing an operational cert
   - Returns the Fleet CA chain for the robot to pin

3. **Ongoing operations** — all subsequent calls use the operational cert
   over mTLS against the Fleet CA. Certs are rotated every 30 days via
   `RenewCert`.

This means a stolen device cert cannot be used after the legitimate device
has enrolled (one-time claim), and the private key never leaves the hardware
(no cert-only replay possible if key binding is enforced via TPM).

## Build order

Build and push modules in this order — each is independently testable:

| Step | Module | Key dependency |
|------|--------|---------------|
| 1 | `proto` | none |
| 2 | `pki` | none |
| 3 | `artifact-store` | proto |
| 4a | `device-management-service` | proto, pki |
| 4b | `ota-service` | proto, pki, artifact-store |
| 5 | `robot-agent` (Rust) | proto (via tonic/prost) |
| 6 | `simulator` | robot-agent |

## Running tests

Each module has its own test suite that runs with no external services:

```bash
# pki
cd pki && go test ./...

# device-management-service
cd device-management-service && go test ./...

# ota-service
cd ota-service && go test ./...

# robot-agent (once written)
cd robot-agent && cargo test
```

## Commit conventions

This repository uses [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(pki): add mTLS server and client TLS config builders
test(device-mgmt): add pre-enrollment replay attack tests
fix(enrollment): reject CSR when serial not in pre-enrollment registry
docs(readme): add trust model explanation
```

One branch per module. PRs stay focused — no cross-module changes in a
single PR unless they are a coordinated proto change.
