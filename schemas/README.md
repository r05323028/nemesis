# NEMESIS model schemas

The Rust structures in `crates/nemesis-core/src/model.rs` are the source of truth for the versioned serialized contracts. Every canonical top-level model carries `schema_version: "nemesis.v1"`.

Persistence formats:

- `run.json`, `request.json`, and `result.json` are one JSON document per file.
- `events.jsonl` is append-only JSONL with a stable `event_id`, `run_id`, and monotonic sequence number within a run.

The tests in `nemesis-core` cover JSON round trips, stable enum wire names, version presence, and JSONL event persistence. A future compatibility release should add generated JSON Schema files rather than hand-maintaining a second source of truth.
