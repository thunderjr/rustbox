# Rustbox

A sandbox runtime that manages lightweight Firecracker microVMs through a REST API. Run untrusted code in isolated virtual machines with network policy enforcement, snapshot support, and streaming command execution.

## Architecture

```
┌───────────┐     ┌───────────┐
│ rustbox-cli│────▶│ rustbox-sdk│
└───────────┘     └─────┬─────┘
                        │ HTTP
                  ┌─────▼─────┐
                  │  rustbox-  │
                  │  daemon    │──── REST API (:8080)
                  └──┬──┬──┬──┘
                     │  │  │
          ┌──────────┘  │  └──────────┐
          ▼             ▼             ▼
    ┌──────────┐  ┌──────────┐  ┌──────────┐
    │rustbox-vm│  │ rustbox- │  │ rustbox- │
    │          │  │ storage  │  │ network  │
    └────┬─────┘  └──────────┘  └──────────┘
         │ TCP (length-prefixed JSON)
    ┌────▼─────┐
    │ rustbox- │  ← runs inside guest VM
    │ agent    │
    └──────────┘
```

All crates share types from **rustbox-core** (IDs, configs, traits, errors).

## Crates

| Crate | Purpose |
|-------|---------|
| `rustbox-core` | Shared types: IDs, configs, `VmBackend` trait, errors |
| `rustbox-vm` | VM backends — `FirecrackerBackend` (Linux) and `MockBackend` (testing) |
| `rustbox-agent` | Guest agent binary — executes commands inside the VM |
| `rustbox-storage` | OverlayFS config, base images, SQLite snapshot store, tar+zstd archival |
| `rustbox-network` | Network policy evaluation, nftables rule generation, TLS MITM proxy |
| `rustbox-daemon` | Axum REST API, orchestrator, timeout watchdog, snapshot reaper |
| `rustbox-sdk` | Rust client library wrapping the daemon API |
| `rustbox-cli` | CLI tool (`sandbox create`, `exec`, `copy`, `snapshot`, etc.) |

## Quick Start

```bash
# Build everything
cargo build --workspace

# Run all tests (uses MockBackend, no KVM needed)
cargo test --workspace

# Start the daemon (MockBackend for development)
cargo run --bin rustboxd

# In another terminal
curl -X POST http://localhost:8080/v1/sandboxes \
  -H 'Content-Type: application/json' \
  -d '{"runtime": "node24"}'
```

## API

All routes are under `/v1`.

### Sandboxes

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/sandboxes` | Create and start a sandbox |
| `GET` | `/sandboxes` | List all sandboxes |
| `GET` | `/sandboxes/:id` | Get sandbox details |
| `DELETE` | `/sandboxes/:id` | Stop and remove a sandbox |
| `GET` | `/sandboxes/:id/metrics` | CPU, memory, network, disk stats |
| `PATCH` | `/sandboxes/:id/timeout` | Update sandbox timeout |
| `PATCH` | `/sandboxes/:id/network-policy` | Update network policy |

### Commands

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/sandboxes/:id/commands` | Execute a command |
| `GET` | `/sandboxes/:id/commands/:cid` | Get command status |
| `GET` | `/sandboxes/:id/commands/:cid/logs` | Stream output (SSE) |
| `POST` | `/sandboxes/:id/commands/:cid/kill` | Kill a running command |

### Files

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/sandboxes/:id/files` | Write a file |
| `GET` | `/sandboxes/:id/files?path=` | Read a file |
| `POST` | `/sandboxes/:id/dirs` | Create a directory |

### Snapshots

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/snapshots` | Create a snapshot |
| `GET` | `/snapshots/:id` | Get snapshot metadata |
| `DELETE` | `/snapshots/:id` | Delete a snapshot |

## Network Policies

Sandboxes support two network modes:

- **AllowAll** (default) — all outbound traffic allowed, with optional deny subnets
- **DenyAll** — all outbound blocked except explicitly allowed domains and subnets

Transform rules can inject headers for specific domains (credential brokering).

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

## CLI

```bash
sandbox create --runtime node24 --timeout 600
sandbox list
sandbox exec <ID> -- npm install
sandbox copy host:/local/file <ID>:/remote/path
sandbox run --runtime python313 --rm -- python script.py
sandbox snapshot create <ID>
sandbox snapshot list
```

## Requirements

- **Development/testing**: macOS or Linux, Rust toolchain. All tests use `MockBackend`.
- **Production**: Linux with KVM (`/dev/kvm`), Firecracker binary installed.
- **Offline-safe**: No telemetry or external calls. SQLite is bundled. All network activity is local (daemon ↔ agent).

## License

MIT
