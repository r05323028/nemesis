# Pi integration

`nemesis-review.ts` is a NEMESIS-owned project-local Pi Extension using the official Pi `ExtensionAPI.registerTool()` surface. It launches the Rust CLI's persisted reviewer runtime and exposes `run`, `status`, and `cancel` operations.

The Rust runtime uses Pi's official RPC JSONL process mode with `--no-session` and read-only tool restrictions. It does not inherit the parent conversation and does not assume that a new session is filesystem isolation.
