# Development status

## Implemented

- Rust workspace with `nemesis-core`, `nemesis-agent`, `nemesis-sdd`, `nemesis-integrations`, and `nemesis-cli`.
- TOML configuration with validation and safe initialization.
- Versioned serde models for requirements, trace links, evidence, findings, review runs, events, results, and gate results.
- Local JSON/JSONL run persistence with stable run IDs and append-only reviewer events.
- Capability detection for Pi, OpenCode, Codex, OpenSpec, and Spec Kit.
- Safe, idempotent Pi extension installation with conflict preservation.
- Read-only OpenSpec schema and active-change inspection.
- Pi RPC worker with explicit timeout, bounded cancellation cleanup, abnormal-exit reporting, strict structured-output validation, and read-only tool flags.
- Persisted verifier records scoped to originating review runs, validated source evidence hashes/locators/excerpts, and stale-source detection.
- CLI and Pi integration operations for review status/events/wait/result plus finding list/show/verify.
- CLI smoke, mock RPC lifecycle, evidence-integrity, and end-to-end verification-loop tests.

## Experimental

- Pi reviewer and verifier sessions are separate process contexts, but filesystem isolation is not guaranteed by Pi alone.
- Runtime cleanup bounds the direct Pi child; platform-specific process-group/job-tree cleanup for descendants is not implemented.
- Verifier verdicts are model-produced semantic conclusions; NEMESIS only deterministically validates output shape and objective evidence integrity. Missing, invalid, or stale evidence downgrades a reported confirmation/rejection to `inconclusive`.
- A malformed or incomplete reviewer/verifier response is persisted as invalid and the execution run is marked failed rather than completed.
- Verifier evidence is currently source/file-oriented: command-output and test evidence must reference an inspectable workspace file; unsupported runtime-only evidence remains inconclusive.
- OpenSpec installation records a read-only adapter registration and deliberately does not install a custom schema or mutate OpenSpec-managed files.

## Not implemented

- Deterministic policy loading/evaluation and `gate check`/`gate explain`.
- Trace extraction from all upstream artifacts and source/test coverage mapping.
- Spec Kit extension package and lifecycle event adapter.
- Native OpenCode plugin/session adapter.
- Native Codex app-server/SDK adapter.
- Worktree or OS sandbox isolation for reviewers.
- Remote persistence, dashboard, distributed scheduling, and graph database.
