# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build --workspace          # build everything
cargo test --workspace           # run all 176 tests
cargo test -p rustbox-core       # test a single crate
cargo test -p rustbox-daemon -- orchestrator::tests::create_and_get_sandbox  # single test
cargo clippy --workspace         # lint (must be zero warnings)
```

The daemon binary is `rustboxd`, the CLI binary is `sandbox`.

## Architecture

Rustbox is a sandbox runtime that manages lightweight VMs (Firecracker) with a REST API. It's structured as a Rust workspace with 8 crates forming a layered dependency chain:

```
rustbox-cli → rustbox-sdk → rustbox-daemon → rustbox-vm
                                           → rustbox-storage
                                           → rustbox-network
                             ↘ all depend on rustbox-core
rustbox-agent (standalone guest binary, no rustbox-core dependency)
```

**rustbox-core** — Shared types glob-exported from `lib.rs`: `SandboxId`/`SnapshotId`/`CommandId` (UUID v7 wrappers), `SandboxConfig`, `SandboxStatus`, `NetworkPolicy`, `CommandRequest`/`CommandOutput`, `RustboxError`, and the `VmBackend` async trait.

**rustbox-vm** — `VmBackend` trait implementations. `MockBackend` (DashMap-based, no real VMs) is used for all tests on macOS. `FirecrackerBackend` is Linux-only. `AgentClient` speaks length-prefixed JSON over TCP to the guest agent.

**rustbox-agent** — Standalone binary for inside the guest VM. Has its own copy of protocol types (wire-compatible with `rustbox-core::protocol` but no dependency on it). Uses length-prefixed JSON framing (`transport.rs`). `CommandExecutor` manages child processes with streaming stdout/stderr.

**rustbox-storage** — `OverlayConfig` (path generation, mount options), `BaseImageStore` (runtime→ext4 path resolution), `SnapshotStore` (SQLite/WAL via rusqlite with `new_in_memory()` for tests), `archive_overlay`/`restore_overlay` (tar+zstd).

**rustbox-network** — Pure logic modules testable on macOS: `domain_matches()`, `ip_in_any_subnet()`, `NetworkPolicyEvaluator`, `NftablesRuleSet::from_policy()`. Plus `CertificateAuthority` (rcgen) for TLS MITM and `find_credential_headers()` for header injection. Linux-specific modules (netns, TAP devices) are stubbed behind `#[cfg(target_os = "linux")]`.

**rustbox-daemon** — Axum REST API. `Orchestrator` is the core lifecycle coordinator owning `Arc<dyn VmBackend>` + `SnapshotStore`. Routes under `/v1/`: sandboxes CRUD, commands (exec/status/SSE logs/kill), files (upload/download/mkdir), snapshots, settings (timeout/network-policy). `ApiError` maps `RustboxError` variants to HTTP status codes (404/400/409/500). `TimeoutWatchdog` and `SnapshotReaper` run background cleanup loops.

**rustbox-sdk** — `RustboxClient` wrapping reqwest. All methods return typed responses. Integration tests spin up a real Axum server on port 0 with `MockBackend`.

**rustbox-cli** — Clap derive-based CLI. Subcommands: create, list, stop, exec, copy, run, connect, snapshot. Currently prints placeholder output; execution logic will wire to SDK.

## Key Patterns

- **All tests use `MockBackend`** — no KVM required. The daemon tests use `SnapshotStore::new_in_memory()` for SQLite.
- **Wire protocol** — Agent communication uses 4-byte big-endian length prefix + JSON payload, max 16 MiB. Both `rustbox-vm::agent_client` and `rustbox-agent::transport` implement this independently.
- **Route integration tests** use `Router::oneshot()` (tower's `ServiceExt`) — no TCP listener needed.
- **SDK integration tests** spin up a real `axum::serve` on `127.0.0.1:0` and use the SDK client against it.
- **Error mapping**: `RustboxError::SandboxNotFound` → 404, `InvalidConfig` → 400, `SandboxNotRunning` → 409, everything else → 500.
- **`SandboxConfig.timeout`** serializes as `u64` seconds via custom `duration_secs` serde module.
- **`AgentRequest`/`AgentResponse`** use `#[serde(tag = "type", rename_all = "snake_case")]` tagged enum encoding.
