# Architecture decisions and upstream research

Research was performed against the official repositories and documentation, using GitHub CLI metadata and primary source files. Observed release references at the initialization snapshot: OpenSpec `v2.1.1`, GitHub Spec Kit `v1.4.3`, Pi `v1.6.0`, OpenCode `v1.19.6` in `anomalyco/opencode`, and Codex `rust-v0.171.0`. These are detection/reference points, not hard-coded runtime requirements.

## 1. Boundary decisions

- `nemesis-core` owns only serializable domain models, configuration, and local JSON/JSONL persistence.
- `nemesis-sdd` owns framework detection and read-only artifact normalization.
- `nemesis-agent` owns agent capability contracts and reviewer process lifecycle.
- `nemesis-integrations` owns project-local installation, conflict checks, and managed-file records.
- `nemesis-cli` is orchestration and presentation only.
- No graph database, server, scheduler, plugin marketplace, or replacement SDD workflow is introduced.

A review run's execution state is separate from verification state. A completed Pi process does not make a finding confirmed. The verifier and gate engine will consume evidence later and must produce `confirmed`, `rejected`, or `inconclusive` without treating an LLM opinion as proof.

## 2. OpenSpec

### Verified official mechanisms

- Repository: [Fission-AI/OpenSpec](https://github.com/Fission-AI/OpenSpec)
- Custom schemas are project-local under `openspec/schemas/<name>/schema.yaml`.
- Schema artifacts have `requires` dependencies; the dependency graph determines what is ready, not a mandatory replacement lifecycle.
- Official CLI surfaces include `openspec schema init`, `schema fork`, `schema validate`, and `schema which`.
- `openspec init`/`openspec update` generate tool-specific skills and command/prompt files. The official supported-tool table includes Pi at `.pi/skills/openspec-*/SKILL.md` and `.pi/prompts/opsx-<id>.md`.
- OpenSpec's agent contract documents machine-readable CLI output and stable diagnostics; the adapter can use the CLI later without scraping human prose.

Primary references:

- [Customization](https://github.com/Fission-AI/OpenSpec/blob/main/docs/customization.md)
- [Concepts](https://github.com/Fission-AI/OpenSpec/blob/main/docs/concepts.md)
- [Supported tools](https://github.com/Fission-AI/OpenSpec/blob/main/docs/supported-tools.md)
- [Agent contract](https://github.com/Fission-AI/OpenSpec/blob/main/docs/agent-contract.md)
- [Schema reference](https://github.com/Fission-AI/OpenSpec/blob/main/docs-lab/reference/schemas/schema-yaml.md)

### Selected strategy

OpenSpec does not expose a generic third-party extension registry comparable to Spec Kit's `extension.yml` mechanism. NEMESIS therefore uses the smallest reversible integration: inspect `openspec/config.yaml`, the active schema, dependency declarations, and active change artifacts without modifying them. The Pi extension is installed through Pi's official extension directory and invokes NEMESIS; it does not fork or replace the user's OpenSpec schema.

`nemesis sdd integration install openspec` records this read-only adapter registration. An empty managed-file list is intentional. Custom schemas are never overwritten.

## 3. GitHub Spec Kit

### Verified official mechanisms

- Repository: [github/spec-kit](https://github.com/github/spec-kit)
- Extensions use an `extension.yml` manifest and may provide commands, templates, scripts, hooks, and runtime events.
- The official CLI provides extension discovery and management (`specify extension add`, `list`, `update`, and `remove`), including local development installs.
- Workflows, workflow steps, presets, and versioned bundles are separate official surfaces; bundles compose existing components rather than requiring NEMESIS to replace SDD.
- The official integration management surface is `specify integration install` for agent delivery, while extension management remains `specify extension ...`.

Primary references:

- [Extension overview](https://github.com/github/spec-kit/blob/main/docs/extensions/overview.md)
- [Extension API](https://github.com/github/spec-kit/blob/main/docs/extensions/api.md)
- [Developing extensions](https://github.com/github/spec-kit/blob/main/docs/extensions/developing.md)
- [Bundles](https://github.com/github/spec-kit/blob/main/docs/bundles/overview.md)
- [Spec Kit integrations](https://github.com/github/spec-kit/tree/main/docs/integrations)

### Selected strategy

Spec Kit is detected only in M0/M1. No unverified extension manifest is installed yet. A future adapter should ship a first-party NEMESIS extension through the official `extension.yml` and `specify extension add` path, with explicit version constraints and managed-file ownership. It must not depend on a community quality-gate extension.

## 4. Pi Coding Agent

### Verified official mechanisms

- Repository: [earendil-works/pi](https://github.com/earendil-works/pi)
- Official TypeScript extensions register tools with `ExtensionAPI.registerTool()`, subscribe to lifecycle events, and are auto-discovered from `.pi/extensions/` or `~/.pi/agent/extensions/`.
- The official SDK supports separate `AgentSession` instances and in-memory `SessionManager` values, but an SDK session is not itself filesystem isolation.
- The official RPC mode is a child-process JSONL protocol: `pi --mode rpc --no-session`. Commands are correlated by IDs; events include `agent_start`, `agent_end`, `agent_settled`, `message_end`, tool events, and retries. `abort` is an official command.
- Pi's RPC docs require continuously draining stdout and treating `agent_settled` as the completion boundary; a successful prompt response only means the prompt was accepted.

Primary references:

- [Extensions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
- [SDK](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sdk.md)
- [RPC mode](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)
- [JSON event stream](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/json.md)
- [RPC commands](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc-commands.md)
- [Official subagent example](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/examples/extensions/subagent/index.ts)

### Selected strategy

NEMESIS installs one project-local Pi extension using the official extension API. The extension delegates to the Rust CLI so the parent Pi session can launch, query, and cancel a child run without inheriting its conversation. The Rust runtime uses official RPC as the process boundary because it is language-independent, observable, cancellable, and available in the installed Pi `1.6.0`. Reviewer processes receive `--no-session`, `--no-extensions`, `--no-mcp`, `--no-context-files`, `--no-approve`, and `--tools read`.

## 5. OpenCode

### Verified official mechanisms

- Current official repository: [anomalyco/opencode](https://github.com/anomalyco/opencode) (the older `opencode-ai/opencode` repository is archived).
- The official v2 plugin package is `@opencode/plugin`; plugins use `Plugin.define`, can add/transform tools and agents, and can subscribe to the typed server event stream.
- Project plugins are discovered from `.opencode/plugins/`; package plugins are configured in `opencode.json(c)` and managed by the official `opencode plugin add/list/check/update/remove` commands.
- `@opencode/sdk` embeds OpenCode in-process. Its official client exposes sessions and an async `events.subscribe()` stream; requests can be cancelled with an `AbortSignal`.

Primary references:

- [OpenCode plugin docs](https://opencode.ai/docs/plugins/)
- [Building plugins](https://opencode.ai/docs/build/plugins)
- [Plugin source contract](https://github.com/anomalyco/opencode/blob/dev/packages/plugin/src/promise/plugin.ts)
- [Tool source contract](https://github.com/anomalyco/opencode/blob/dev/packages/plugin/src/promise/tool.ts)
- [SDK README](https://github.com/anomalyco/opencode/blob/dev/packages/sdk/README.md)
- [SDK overview](https://opencode.ai/docs/build/sdk/)

### Selected strategy

No OpenCode files are installed in this initialization. The future adapter should use `.opencode/plugins` or the configured package plugin path, not the archived v1 package or a community plugin. The current API is sufficiently observable for a later native session adapter, but the exact versioned plugin SDK contract should be pinned before implementation.

## 6. OpenAI Codex

### Verified official mechanisms

- Repository: [openai/codex](https://github.com/openai/codex)
- The official app-server protocol is JSON-RPC based and has thread lifecycle methods such as `thread/start`, `thread/resume`, `thread/read`, and `thread/list`, plus turn lifecycle methods and interrupt/cancellation support.
- The checked TypeScript notification envelope contains typed thread, turn, item, command-execution, process, and error notifications. The protocol source is generated/maintained in `codex-rs/app-server-protocol`.
- OpenAI's platform guidance distinguishes `codex exec` for bounded non-interactive jobs, the Codex SDK for programmatic workflows, and app-server for persistent conversations, streamed events, approval handling, and interruption.
- Codex skills are installed as `SKILL.md` directories; OpenSpec's current Codex integration uses `.agents/skills/` and does not generate custom prompt files.

Primary references:

- [Codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [App-server common protocol](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/common.rs)
- [Server notification envelope](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/typescript/ServerNotificationEnvelope.ts)
- [Codex as a platform](https://developers.openai.com/blog/codex-as-a-platform)
- [Codex skills](https://developers.openai.com/plugins/build/skills)

### Selected strategy

No Codex adapter is installed. A future implementation should prefer app-server or the official SDK over parsing CLI text, preserve approval/sandbox semantics, and model thread/turn IDs as parent-child execution metadata. The Agents API multi-agent guide is not treated as proof of an equivalent Codex CLI protocol.

## 7. Lifecycle and evidence policy

NEMESIS uses the fixed lifecycle enum `queued`, `starting`, `running`, `verifying`, `completed`, `failed`, `cancelled`, and `timed_out`. It emits phase-bearing JSONL events instead of invented progress percentages.

A reviewer may produce an unverified finding and evidence. The future verifier must independently validate:

1. whether the finding is valid;
2. whether its evidence supports the claim; and
3. whether a fix resolves the finding.

Only validated evidence may be passed to a versioned gate policy. Missing evidence maps to `inconclusive`, never silently to `pass`.

## 8. Independent finding verification

The first verification loop reuses the existing persisted run store rather than adding a scheduler or service. A verifier is a `ReviewRun` with `kind: verification`, `parent_run_id` set to the originating reviewer run, and a separate Pi `--no-session` process. Its request contains only the finding, candidate references, candidate evidence, project scope, and optional workspace revision; reviewer transcripts and implementer conclusions are not copied.

Verifier output is structurally validated before the run can be completed. Source evidence is collected into the existing `Evidence` model and checked deterministically for workspace scope, SHA-256 content hash, line locator resolution, exact excerpt match, and later staleness. A reported `confirmed` or `rejected` verdict without valid supporting or contradicting evidence is persisted as `inconclusive`, while the original reported verdict and uncertainty remain inspectable. Each verification result is stored in its own run directory and never overwrites the reviewer result.

Timeout and cancellation use the existing child-process lifecycle with a bounded termination grace period. The JSONL event store is exposed through CLI operations for status, events, bounded wait, result retrieval, and cancellation; finding list/show/verify commands retain parent-agent control over semantic workflow decisions.
