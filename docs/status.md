# Development status

## Implemented

- Rust workspace with `nemesis-core`, `nemesis-agent`, `nemesis-sdd`, `nemesis-integrations`, and `nemesis-cli`.
- TOML configuration with validation and safe initialization.
- Versioned serde models for requirements, trace links, evidence, findings, review runs, events, results, and gate results.
- Local JSON/JSONL run persistence with stable run IDs and append-only reviewer events.
- Capability detection for Pi, OpenCode, Codex, OpenSpec, and Spec Kit.
- Safe, idempotent Pi extension installation with conflict preservation.
- Read-only OpenSpec schema and active-change inspection.
- Pi RPC worker with explicit timeout, cancellation request, process cleanup, and read-only tool flags.
- CLI smoke and domain/adapter tests.

## Experimental

- Pi reviewer output parsing is best-effort. A malformed or non-JSON reviewer response is persisted as `structured: false` and is not treated as verified.
- OpenSpec installation records a read-only adapter registration and deliberately does not install a custom schema or mutate OpenSpec-managed files.
- Separate Pi process context is independent, but filesystem isolation is not guaranteed by Pi alone.

## Not implemented

- Independent verifier runtime and finding verification commands.
- Deterministic policy loading/evaluation and `gate check`/`gate explain`.
- Trace extraction from all upstream artifacts and source/test coverage mapping.
- Spec Kit extension package and lifecycle event adapter.
- Native OpenCode plugin/session adapter.
- Native Codex app-server/SDK adapter.
- Worktree or OS sandbox isolation for reviewers.
- Remote persistence, dashboard, distributed scheduling, and graph database.
