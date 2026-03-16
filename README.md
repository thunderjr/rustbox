# Rustbox

Programmable sandbox runtime. Spin up isolated containers on demand, execute commands with streaming output, manage files, snapshot and restore state, enforce network policies. Built for AI agent tooling, CI runners, interactive dev environments, or anything that needs ephemeral compute with an API.

## Architecture

```
rustbox-cli ──▶ rustbox-sdk ──▶ rustbox-daemon (:8080)
                                  │  │  │
                       rustbox-vm ┘  │  └ rustbox-network
                           │         │
                           │    rustbox-storage (SQLite)
                           │
                    ┌──────▼──────┐
                    │   Docker    │
                    │  Container  │
                    │             │
                    │ rustbox-    │  ◀── length-prefixed JSON over TCP
                    │ agent       │
                    └─────────────┘
```

8 crates, layered dependency chain. Everything shares types from `rustbox-core`.

### Crate Breakdown

| Crate | What it does |
|---|---|
| **rustbox-core** | Shared types: `SandboxId`/`SnapshotId`/`CommandId` (UUID v7), `SandboxConfig`, `SandboxStatus`, `NetworkPolicy`, `CommandRequest`/`CommandOutput`, `RustboxError`, `VmBackend` trait |
| **rustbox-vm** | `VmBackend` implementations: `DockerBackend` (default, macOS + Linux), `FirecrackerBackend` (Linux + KVM), `LocalBackend` (no isolation, dev only), `MockBackend` (tests). Also has `AgentClient` for the TCP wire protocol |
| **rustbox-agent** | Standalone binary that runs inside the container/VM. Receives commands over TCP, executes them, streams stdout/stderr back. Own copy of protocol types (wire-compatible, no `rustbox-core` dependency) |
| **rustbox-storage** | `SnapshotStore` (SQLite/WAL via rusqlite), `OverlayConfig`, `BaseImageStore`, tar+zstd archival |
| **rustbox-network** | Network policy evaluation, nftables rule generation, TLS MITM `CertificateAuthority` (rcgen), credential header injection. Linux-specific modules stubbed behind `#[cfg(target_os = "linux")]` |
| **rustbox-daemon** | Axum REST API under `/v1`. `Orchestrator` owns `Arc<dyn VmBackend>` + `SnapshotStore`. Background tasks: `TimeoutWatchdog`, `SnapshotReaper` |
| **rustbox-sdk** | `RustboxClient` wrapping reqwest. Typed responses for everything |
| **rustbox-cli** | Clap-based CLI binary (`sandbox`). Subcommands: `create`, `list`, `stop`, `exec`, `copy`, `run`, `connect`, `snapshot` |

### Wire Protocol

Agent communication uses **4-byte big-endian length prefix + JSON payload** (max 16 MiB). Both `rustbox-vm::agent_client` and `rustbox-agent::transport` implement this independently.

**Requests** (host -> agent): `Exec`, `Kill`, `WriteFile`, `ReadFile`, `Mkdir`, `Metrics`, `Ping`

**Responses** (agent -> host): `ExecStarted`, `Output` (stdout/stderr streaming), `ExecDone`, `FileContent`, `Ok`, `Error`, `MetricsResult`, `Pong`

### Backend Selection

Set `RUSTBOX_BACKEND` env var:

| Value | Backend | Platform |
|---|---|---|
| *(unset or `docker`)* | `DockerBackend` | macOS + Linux |
| `firecracker` | `FirecrackerBackend` | Linux + KVM only |
| `local` | `LocalBackend` | Any (no isolation) |

Tests always use `MockBackend` — no Docker or KVM needed.

## Setup

### Requirements

- Rust toolchain
- Docker (for running sandboxes)

### Build & Run

```bash
# Build all Docker images (compiles rustbox-agent inside Docker, first build is slow)
make setup

# Start the daemon
make run
```

That's it. `make setup` builds three images (`rustbox-node24`, `rustbox-node22`, `rustbox-python313`) using multi-stage Dockerfiles that compile the agent from source. `make run` starts `rustboxd` on `:8080`.

### Manual Steps

```bash
# Build everything
cargo build --workspace

# Run all 176 tests (MockBackend, no Docker needed)
cargo test --workspace

# Lint
cargo clippy --workspace

# Build a single image
docker build -t rustbox-node24:latest -f images/node24/Dockerfile .

# Start daemon with debug logging
RUST_LOG=debug cargo run --bin rustboxd
```

## API Reference

All routes under `/v1`.

### Sandboxes

```
POST   /v1/sandboxes                              Create + start sandbox
GET    /v1/sandboxes                              List all
GET    /v1/sandboxes/:id                          Get details
DELETE /v1/sandboxes/:id                          Stop + remove
GET    /v1/sandboxes/:id/metrics                  Resource stats
PATCH  /v1/sandboxes/:id/timeout                  Update timeout
PATCH  /v1/sandboxes/:id/network-policy           Update network policy
```

### Commands

```
POST   /v1/sandboxes/:id/commands                 Execute a command
GET    /v1/sandboxes/:id/commands/:cid            Get command status
GET    /v1/sandboxes/:id/commands/:cid/logs       Stream output (SSE)
POST   /v1/sandboxes/:id/commands/:cid/kill       Kill running command
```

### Files

```
POST   /v1/sandboxes/:id/files                    Write a file
GET    /v1/sandboxes/:id/files?path=/foo/bar      Read a file
POST   /v1/sandboxes/:id/dirs                     Create directory
```

### Snapshots

```
POST   /v1/snapshots                              Create snapshot
GET    /v1/snapshots                              List all
GET    /v1/snapshots/:id                          Get metadata
DELETE /v1/snapshots/:id                          Delete
```

### Examples

```bash
# Create a sandbox
curl -X POST http://localhost:8080/v1/sandboxes \
  -H 'Content-Type: application/json' \
  -d '{"runtime": "node24"}'

# Execute a command
curl -X POST http://localhost:8080/v1/sandboxes/$ID/commands \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "node", "args": ["-e", "console.log(42)"]}'

# Stream command output (SSE)
curl -N http://localhost:8080/v1/sandboxes/$ID/commands/$CID/logs

# Write a file
curl -X POST http://localhost:8080/v1/sandboxes/$ID/files \
  -H 'Content-Type: application/json' \
  -d '{"path": "/app/index.js", "content": "Y29uc29sZS5sb2coImhlbGxvIik="}'

# Create a snapshot
curl -X POST http://localhost:8080/v1/snapshots \
  -H 'Content-Type: application/json' \
  -d '{"sandbox_id": "'$ID'"}'
```

## CLI

```bash
sandbox create --runtime node24 --timeout 600
sandbox list
sandbox exec $ID -- npm install
sandbox exec $ID --sudo --workdir /app --env NODE_ENV=production -- node server.js
sandbox copy host:/local/file.js $ID:/app/file.js
sandbox run --runtime python313 --rm -- python -c "print('hello')"
sandbox snapshot create $ID
sandbox snapshot list
sandbox snapshot delete $SNAP_ID
sandbox stop $ID
```

## Network Policies

Two modes: `AllowAll` (default) and `DenyAll`.

```json
{
  "network_policy": {
    "mode": "deny_all",
    "allow_domains": ["registry.npmjs.org", "*.github.com"],
    "subnets_allow": ["10.0.0.0/8"],
    "subnets_deny": [],
    "transform_rules": [
      {
        "domain": "api.example.com",
        "headers": { "Authorization": "Bearer ..." }
      }
    ]
  }
}
```

`DenyAll` blocks everything except explicitly allowed domains/subnets. Transform rules inject headers for specific domains (credential brokering via TLS MITM proxy).

## Sandbox Config

```json
{
  "runtime": "node24",
  "cpu_count": "two",
  "timeout": 300,
  "env": { "NODE_ENV": "production" },
  "ports": [3000],
  "network_policy": { "mode": "allow_all" },
  "source": { "type": "git", "url": "https://github.com/user/repo" }
}
```

**Runtimes**: `node24`, `node22`, `python313`

**CPU counts**: `one`, `two`, `four`, `eight`

**Source types**: `git` (with optional auth, depth, revision), `tarball` (URL), `snapshot` (restore from ID)

## Error Mapping

| Error | HTTP Status |
|---|---|
| `SandboxNotFound` | 404 |
| `SnapshotNotFound` | 404 |
| `CommandNotFound` | 404 |
| `InvalidConfig` | 400 |
| `SandboxNotRunning` | 409 |
| Everything else | 500 |

## License

MIT
