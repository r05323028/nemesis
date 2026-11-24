# NEMESIS

NEMESIS is an independent, evidence-driven verification layer for existing Spec-Driven Development (SDD) workflows. It adds traceability, adversarial review, independent finding verification, and deterministic gates without becoming another proposal/design/implementation orchestrator.

## What it is — and is not

- **Is:** a Rust CLI and set of small contracts for requirements, evidence, review runs, events, findings, and gate results.
- **Is:** SDD- and agent-agnostic at the core, with adapters at the edges.
- **Is not:** a replacement SDD framework, graph database, distributed scheduler, dashboard, or community extension bundle.
- **Is not yet:** a deterministic quality-gate engine or complete traceability engine. Reviewer findings are persisted as **unverified** until the separate verifier produces a verdict supported by validated evidence.

## Current status

| Area | Status |
| --- | --- |
| Cargo workspace, config, typed errors, logging | Implemented (M0) |
| Versioned serde domain models and JSON/JSONL persistence | Implemented (M0) |
| Agent/SDD/installer contracts | Implemented (M0) |
| Environment and capability detection | Implemented (M0) |
| OpenSpec read-only schema/artifact inspection | Experimental (M1) |
| Pi project extension and RPC reviewer worker | Experimental (M1) |
| Spec Kit, OpenCode, and Codex adapters | Researched; not implemented |
| Independent Pi verifier and evidence integrity checks | Experimental (M2) |
| Deterministic gate engine | Planned |

## Build and test

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## CLI quick start

```bash
cargo run -p nemesis-cli -- --help
cargo run -p nemesis-cli -- version
cargo run -p nemesis-cli -- init
cargo run -p nemesis-cli -- doctor
cargo run -p nemesis-cli -- agent integration list
cargo run -p nemesis-cli -- sdd integration list
```

`nemesis init` only creates `.nemesis/config.toml`, runtime directories, and NEMESIS-owned ignore metadata. It does not rewrite OpenSpec, Spec Kit, or other SDD artifacts.

## Experimental OpenSpec + Pi slice

The first vertical slice uses only verified official surfaces:

1. `nemesis sdd integration install openspec` records a read-only OpenSpec adapter; it does not create or overwrite an OpenSpec schema.
2. `nemesis agent integration install pi` installs `.pi/extensions/nemesis-review.ts`, an NEMESIS-owned Pi Extension using Pi's official `registerTool()` API.
3. The Pi tool calls `nemesis review run`, which queues a persisted run and starts a separate `pi --mode rpc --no-session` process with read-only tools.
4. The worker records Pi JSONL events, lifecycle state, cancellation requests, timeout state, and the raw/structured result under `.nemesis/runs/<run-id>/`.
5. `nemesis finding verify` starts a fresh read-only Pi session with only the finding, candidate references, project scope, and workspace revision; it persists a separate verification record and validates source evidence hashes, locators, and excerpts.

For a local development run, put the built binary on `PATH` or set `NEMESIS_BIN` for the Pi extension. A configured Pi model is required for a real review; standard tests never call a paid model.

```bash
cargo build -p nemesis-cli
export PATH="$PWD/target/debug:$PATH"
nemesis init
nemesis sdd integration install openspec   # only when openspec/ already exists
nemesis agent integration install pi
nemesis review run --prompt "Review the current OpenSpec change for missing requirements and edge cases."
nemesis review wait <review-run-id>
nemesis review events <review-run-id>
nemesis finding list
nemesis finding verify <review-run-id> F-001
nemesis finding show <review-run-id> F-001
```

Reviewers default to read-only execution (`read` only, no session, no extensions, no MCP, and no project approval). A separate Pi session is not treated as filesystem isolation; stronger sandbox/worktree isolation remains future work.

## Architecture

```text
nemesis-cli
    ├── nemesis-integrations ── OpenSpec / Spec Kit detection and safe installers
    ├── nemesis-agent        ── agent capabilities and Pi RPC process runtime
    └── nemesis-core         ── versioned models, config, JSON/JSONL run store
```

The core does not import agent or SDD implementations. Integration files under `integrations/` are NEMESIS-owned extension assets, not upstream community dependencies.

Detailed research and decisions are in [`docs/architecture-decisions.md`](docs/architecture-decisions.md). The unfinished work is listed in [`docs/status.md`](docs/status.md).
