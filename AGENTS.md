# AGENTS.md

## Project Identity

NEMESIS is an enhancement framework for existing Spec-Driven Development (SDD) tools.

It extends established SDD workflows with:

- Requirement traceability
- Adversarial review
- Independent review
- Finding verification
- Evidence-based quality gates

**NEMESIS is not a replacement for an SDD framework.**

Its purpose is to strengthen existing workflows, not recreate their specification management, change lifecycle, or implementation processes.

## Core Principles

### 1. Extend, Don't Replace

- Reuse the native capabilities of Spec Kit, OpenSpec, and other supported SDD frameworks.
- Prefer official extension mechanisms, schemas, templates, and commands.
- Preserve the host framework's existing workflow and artifacts.
- Do not introduce a competing specification format or lifecycle.
- Avoid dependencies on unofficial community extensions.

### 2. Agent-Driven, Not Script-Driven

- Delegate semantic reasoning, review, and verification to agents.
- Use deterministic code for orchestration, parsing, validation, state management, and evidence integrity.
- Do not hardcode subjective review decisions into deterministic scripts.
- Prefer native subagent capabilities when available.
- Use CLI-based child agents as a fallback when native subagents are unavailable.
- Ensure child-agent execution is observable, including progress, status, completion, and failure.

### 3. Evidence Over Assertions

- Every verification result must be supported by inspectable evidence.
- Distinguish observations, interpretations, and conclusions.
- Never treat an agent's assertion as sufficient proof.
- Findings must be independently verifiable.
- Quality gates must evaluate evidence, not merely reported success.
- Missing or inconclusive evidence must not be treated as success.

### 4. Independent and Adversarial Review

- Reviewers should actively seek contradictions, omissions, unsupported claims, and implementation defects.
- Independent review must not simply repeat the implementer's conclusions.
- Findings should be verified before being accepted or rejected.
- Do not conflate finding discovery, finding verification, and remediation.
- Preserve traceability between requirements, implementation, tests, findings, and evidence.

### 5. Minimal, Composable Architecture

- Prefer small, explicit interfaces over speculative abstractions.
- Avoid unnecessary compatibility layers.
- Keep framework-specific behavior isolated from core logic.
- Keep agent-runtime-specific behavior isolated from SDD integrations.
- Implement only what is required by current, demonstrated use cases.
- Prefer composition over deep inheritance or excessive configuration.

## Architecture Boundaries

NEMESIS should maintain clear conceptual boundaries:

**Core**
- Shared domain models and contracts
- Traceability and evidence semantics
- Finding lifecycle and quality-gate evaluation
- Framework-independent orchestration

**SDD Integrations**
- Native integration with supported SDD frameworks
- Mapping existing artifacts and workflows into NEMESIS concepts
- Framework-specific extension installation and configuration

**Agent Runtime**
- Native subagent execution when supported
- CLI child-agent fallback
- Execution lifecycle, progress monitoring, and error reporting
- Runtime-specific capability detection

These are conceptual responsibilities, not mandatory directory names or crate boundaries.

Do not introduce new architectural layers without a concrete need.

## Implementation Guidelines

- Follow existing repository conventions before introducing new ones.
- Inspect relevant code, tests, and documentation before making changes.
- Prefer focused changes over broad refactoring.
- Keep public interfaces small and explicit.
- Avoid adding dependencies without clear justification.
- Preserve backward compatibility unless a breaking change is intentional and documented.
- Do not silently change existing SDD framework artifacts or behavior.

### Rust

- Follow idiomatic Rust practices.
- Prefer explicit types and error handling.
- Avoid unnecessary cloning, allocation, and dynamic dispatch.
- Do not use `unwrap()` or `expect()` in production paths without a justified invariant.
- Use `cargo fmt` and `cargo clippy` for applicable changes.
- Keep build artifacts and generated files out of version control.

## Verification Requirements

Before reporting a task as complete:

1. Verify the implementation against the requested behavior.
2. Run relevant automated checks when feasible.
3. Inspect failures and distinguish regressions from pre-existing issues.
4. Provide concrete evidence for important claims.
5. Report unverified assumptions and remaining limitations.

Never claim a test passed unless it was actually executed and its result observed.

If verification cannot be performed, explicitly report it as unverified.

## Change Discipline

- Make the smallest coherent change that solves the problem.
- Do not refactor unrelated code.
- Do not create speculative abstractions for future integrations.
- Update documentation when observable behavior or contracts change.
- Add or update tests for meaningful behavior changes.
- Preserve existing user work and avoid destructive operations without authorization.

## Documentation

- Keep this file focused on durable, repository-wide instructions.
- Keep README.md focused on project introduction and user onboarding.
- Place detailed architecture, integration contracts, and workflows under `docs/`.
- Prefer linking to existing documentation over duplicating it.
- Treat implementation and tests as the source of truth for current behavior.

## Agent Completion Report

When completing implementation work, summarize:

- **Changes:** What was implemented and why.
- **Verification:** Checks performed and their actual results.
- **Evidence:** Relevant test output, artifacts, or references.
- **Limitations:** Anything incomplete, uncertain, or not verified.

Be concise. Do not claim completion beyond the available evidence.
