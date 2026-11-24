//! Agent runtime contracts and the first process-backed reviewer adapter.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use nemesis_core::config::ProjectPaths;
use nemesis_core::evidence::{content_hash_for_source, validate_evidence};
use nemesis_core::model::{
    CollectionMetadata, Evidence, EvidenceType, EvidenceValidation, ExecutionPhase, Finding,
    LifecycleStatus, ReviewResult, ReviewRun, ReviewerEvent, ReviewerEventType, RunKind, Severity,
    SourceReference, ValidationState, VerificationRecord, VerificationResult, VerificationState,
    MODEL_SCHEMA_VERSION,
};
use nemesis_core::store::RunStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{debug, warn};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("core error: {0}")]
    Core(#[from] nemesis_core::CoreError),
    #[error("agent runtime is unavailable: {0}")]
    Unavailable(String),
    #[error("agent process failed: {0}")]
    Process(String),
    #[error("review output is invalid: {0}")]
    InvalidReview(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub id: String,
    pub display_name: String,
    pub installed: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub supports_independent_sessions: bool,
    pub supports_structured_events: bool,
    pub supports_cancellation: bool,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRequest {
    pub schema_version: String,
    #[serde(default = "default_request_kind")]
    pub kind: RunKind,
    pub run_id: Uuid,
    pub project_root: PathBuf,
    pub prompt: String,
    pub reviewer_profile: String,
    pub agent_runtime: String,
    pub timeout_seconds: u64,
    #[serde(default = "default_termination_grace_seconds")]
    pub termination_grace_seconds: u64,
    #[serde(default)]
    pub source_review_run_id: Option<Uuid>,
    #[serde(default)]
    pub source_finding_id: Option<String>,
    #[serde(default)]
    pub finding: Option<Finding>,
    #[serde(default)]
    pub candidate_evidence: Vec<Evidence>,
    #[serde(default)]
    pub verifier_identity: Option<String>,
    #[serde(default)]
    pub repository_revision: Option<String>,
}

impl ReviewRequest {
    pub fn new(
        run_id: Uuid,
        project_root: impl Into<PathBuf>,
        prompt: impl Into<String>,
        reviewer_profile: impl Into<String>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            schema_version: MODEL_SCHEMA_VERSION.to_owned(),
            kind: RunKind::Review,
            run_id,
            project_root: project_root.into(),
            prompt: prompt.into(),
            reviewer_profile: reviewer_profile.into(),
            agent_runtime: "pi".to_owned(),
            timeout_seconds,
            termination_grace_seconds: default_termination_grace_seconds(),
            source_review_run_id: None,
            source_finding_id: None,
            finding: None,
            candidate_evidence: Vec::new(),
            verifier_identity: None,
            repository_revision: None,
        }
    }

    pub fn for_verification(
        run_id: Uuid,
        project_root: impl Into<PathBuf>,
        source_review_run_id: Uuid,
        finding: Finding,
        candidate_evidence: Vec<Evidence>,
        verifier_identity: impl Into<String>,
        timeout_seconds: u64,
    ) -> Self {
        let verifier_identity = verifier_identity.into();
        Self {
            schema_version: MODEL_SCHEMA_VERSION.to_owned(),
            kind: RunKind::Verification,
            run_id,
            project_root: project_root.into(),
            prompt: String::new(),
            reviewer_profile: verifier_identity.clone(),
            agent_runtime: "pi".to_owned(),
            timeout_seconds,
            termination_grace_seconds: default_termination_grace_seconds(),
            source_review_run_id: Some(source_review_run_id),
            source_finding_id: Some(finding.id.clone()),
            finding: Some(finding),
            candidate_evidence,
            verifier_identity: Some(verifier_identity),
            repository_revision: None,
        }
    }
}

fn default_termination_grace_seconds() -> u64 {
    2
}

fn default_request_kind() -> RunKind {
    RunKind::Review
}

#[derive(Clone, Debug, Default)]
pub struct PiRuntime;

impl PiRuntime {
    pub fn id(&self) -> &'static str {
        "pi"
    }

    pub fn detect(&self) -> AgentCapabilities {
        let executable = find_executable("pi");
        let version = executable
            .as_ref()
            .and_then(|path| command_version(path).ok());
        AgentCapabilities {
            id: self.id().to_owned(),
            display_name: "Pi Coding Agent".to_owned(),
            installed: executable.is_some(),
            executable: executable.map(|path| path.display().to_string()),
            version,
            supports_independent_sessions: true,
            supports_structured_events: true,
            supports_cancellation: true,
            notes: vec![
                "Uses the official Pi RPC JSONL protocol in a child process.".to_owned(),
                "The reviewer is launched without a session and with read-only tool access."
                    .to_owned(),
            ],
        }
    }

    pub fn executable(&self) -> Result<PathBuf> {
        find_executable("pi").ok_or_else(|| {
            AgentError::Unavailable(
                "the `pi` executable was not found on PATH; install Pi and retry".to_owned(),
            )
        })
    }
}

/// Spawn the detached NEMESIS worker that owns a reviewer run.
///
/// The worker is a normal NEMESIS process rather than a daemon or scheduler. It writes its
/// state to the run store, which lets a later CLI invocation observe or cancel it.
pub fn spawn_worker_process(
    current_executable: &Path,
    project_root: &Path,
    run_id: Uuid,
) -> Result<Child> {
    let mut command = Command::new(current_executable);
    command
        .arg("review")
        .arg("worker")
        .arg(run_id.to_string())
        .arg("--root")
        .arg(project_root)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().map_err(|source| AgentError::Io {
        path: current_executable.to_path_buf(),
        source,
    })
}

/// Execute a persisted Pi review request. This function is called by the hidden worker command.
pub fn run_worker(project_root: &Path, run_id: Uuid) -> Result<()> {
    let paths = ProjectPaths::new(project_root);
    let store = RunStore::new(paths);
    let request: ReviewRequest = store.load_request(run_id)?;
    let runtime = PiRuntime;
    let result = run_pi_review(&store, &runtime, &request);
    if let Err(error) = &result {
        mark_failed(&store, run_id, error.to_string())?;
    }
    result
}

fn run_pi_review(store: &RunStore, runtime: &PiRuntime, request: &ReviewRequest) -> Result<()> {
    let pi = runtime.executable()?;
    run_pi_review_with_executable(store, runtime, request, &pi)
}

fn run_pi_review_with_executable(
    store: &RunStore,
    runtime: &PiRuntime,
    request: &ReviewRequest,
    pi: &Path,
) -> Result<()> {
    let mut run = store.load_run(request.run_id)?;
    if run.lifecycle_status.is_terminal() {
        return Ok(());
    }

    let mut sequence = next_sequence(store, request.run_id);
    set_status(store, &mut run, LifecycleStatus::Starting, None, None)?;
    append_event(
        store,
        request.run_id,
        &mut sequence,
        ReviewerEventType::RunStarted,
        ExecutionPhase::Starting,
        json!({"agent_runtime": runtime.id()}),
    )?;

    let started_at = Utc::now();
    run.started_at = Some(started_at);
    run.lifecycle_status = LifecycleStatus::Starting;
    store.save_run(&run)?;

    let child = Command::new(pi)
        .args([
            OsString::from("--mode"),
            OsString::from("rpc"),
            OsString::from("--no-session"),
            OsString::from("--no-approve"),
            OsString::from("--no-extensions"),
            OsString::from("--no-skills"),
            OsString::from("--no-prompt-templates"),
            OsString::from("--no-mcp"),
            OsString::from("--no-context-files"),
            OsString::from("--tools"),
            OsString::from("read"),
            // Keep reviewer latency bounded; the lifecycle timeout remains authoritative.
            OsString::from("--thinking"),
            OsString::from("minimal"),
        ])
        .current_dir(&request.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AgentError::Io {
            path: pi.to_path_buf(),
            source,
        })?;
    let mut child = ManagedChild::new(child);

    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Process("Pi stdout was not available".to_owned()))?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Process("Pi stderr was not available".to_owned()))?;
    let stdout_rx = spawn_line_reader(stdout);
    let stderr_rx = spawn_text_reader(stderr);
    let stdin = child
        .child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Process("Pi stdin was not available".to_owned()))?;

    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds.max(1));
    let prompt = match request.kind {
        RunKind::Review => reviewer_prompt(&request.prompt),
        RunKind::Verification => verifier_prompt(request),
    };
    let startup = match write_rpc_until_startup(
        store,
        request.run_id,
        stdin,
        json!({
            "id": "nemesis-run",
            "type": "prompt",
            "message": prompt,
        }),
        deadline,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = terminate_child(
                &mut child.child,
                pi,
                Duration::from_secs(request.termination_grace_seconds.max(1)),
            );
            return Err(error);
        }
    };
    let mut stdin = match startup {
        StartupWriteOutcome::Ready(stdin) => stdin,
        StartupWriteOutcome::Cancelled => {
            finalize_controlled_run(
                store,
                &mut run,
                &mut sequence,
                &mut child.child,
                pi,
                request,
                ControlSignal {
                    timed_out: false,
                    reason: "startup_cancelled",
                },
            )?;
            return Ok(());
        }
        StartupWriteOutcome::TimedOut => {
            finalize_controlled_run(
                store,
                &mut run,
                &mut sequence,
                &mut child.child,
                pi,
                request,
                ControlSignal {
                    timed_out: true,
                    reason: "startup_timeout",
                },
            )?;
            return Ok(());
        }
    };
    set_status(store, &mut run, LifecycleStatus::Running, None, None)?;

    let mut final_output = String::new();
    let mut settled = false;
    let mut cancel_requested = false;
    let mut timed_out = false;
    let mut termination_deadline = None;

    loop {
        if !cancel_requested && store.is_cancel_requested(request.run_id) {
            cancel_requested = true;
            termination_deadline = Some(
                Instant::now() + Duration::from_secs(request.termination_grace_seconds.max(1)),
            );
            match write_abort_bounded(stdin, Duration::from_millis(100)) {
                Ok(returned_stdin) => {
                    stdin = returned_stdin;
                    debug!(run_id = %request.run_id, "sent Pi abort request");
                }
                Err(error) => {
                    finalize_controlled_run(
                        store,
                        &mut run,
                        &mut sequence,
                        &mut child.child,
                        pi,
                        request,
                        ControlSignal {
                            timed_out: false,
                            reason: &format!("abort_write_failed: {error}"),
                        },
                    )?;
                    return Ok(());
                }
            }
        }
        if !timed_out && Instant::now() >= deadline {
            timed_out = true;
            termination_deadline = Some(
                Instant::now() + Duration::from_secs(request.termination_grace_seconds.max(1)),
            );
            match write_abort_bounded(stdin, Duration::from_millis(100)) {
                Ok(returned_stdin) => {
                    stdin = returned_stdin;
                    warn!(run_id = %request.run_id, "Pi reviewer timed out; sent abort request");
                }
                Err(error) => {
                    finalize_controlled_run(
                        store,
                        &mut run,
                        &mut sequence,
                        &mut child.child,
                        pi,
                        request,
                        ControlSignal {
                            timed_out: true,
                            reason: &format!("abort_write_failed: {error}"),
                        },
                    )?;
                    return Ok(());
                }
            }
        }
        if (cancel_requested || timed_out)
            && termination_deadline.is_some_and(|until| Instant::now() >= until)
        {
            let _ = terminate_child(
                &mut child.child,
                pi,
                Duration::from_secs(request.termination_grace_seconds.max(1)),
            );
            break;
        }

        match stdout_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(line)) => {
                let value: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(error) => {
                        append_event(
                            store,
                            request.run_id,
                            &mut sequence,
                            ReviewerEventType::AgentEvent,
                            ExecutionPhase::Reviewing,
                            json!({"raw": line, "parse_error": error.to_string()}),
                        )?;
                        continue;
                    }
                };
                append_event(
                    store,
                    request.run_id,
                    &mut sequence,
                    ReviewerEventType::AgentEvent,
                    event_phase(&value),
                    value.clone(),
                )?;
                if value.get("type").and_then(Value::as_str) == Some("message_end")
                    && value
                        .get("message")
                        .and_then(|message| message.get("role"))
                        .and_then(Value::as_str)
                        == Some("assistant")
                {
                    final_output = message_text(value.get("message"));
                }
                match value.get("type").and_then(Value::as_str) {
                    Some("agent_start") | Some("turn_start") => {
                        set_status(store, &mut run, LifecycleStatus::Running, None, None)?;
                    }
                    Some("agent_settled") => {
                        settled = true;
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Err(error)) => {
                return Err(AgentError::Process(error));
            }
            Err(RecvTimeoutError::Timeout) => {
                if child
                    .try_wait()
                    .map_err(|source| AgentError::Io {
                        path: pi.to_path_buf(),
                        source,
                    })?
                    .is_some()
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if !settled && !cancel_requested && !timed_out {
        // A closed event stream without agent_settled is abnormal. Do not wait forever on it.
        if child
            .try_wait()
            .map_err(|source| AgentError::Io {
                path: pi.to_path_buf(),
                source,
            })?
            .is_none()
        {
            let _ = terminate_child(
                &mut child.child,
                pi,
                Duration::from_secs(request.termination_grace_seconds.max(1)),
            );
        }
    }

    if cancel_requested {
        let cleanup = terminate_child(
            &mut child.child,
            pi,
            Duration::from_secs(request.termination_grace_seconds.max(1)),
        );
        if let Err(error) = cleanup {
            append_event(
                store,
                request.run_id,
                &mut sequence,
                ReviewerEventType::AgentEvent,
                ExecutionPhase::Finalizing,
                json!({"cleanup_error": error.to_string()}),
            )?;
        }
        finalize_cancelled(store, &mut run, &mut sequence)?;
        return Ok(());
    }
    if timed_out {
        let cleanup = terminate_child(
            &mut child.child,
            pi,
            Duration::from_secs(request.termination_grace_seconds.max(1)),
        );
        if let Err(error) = cleanup {
            append_event(
                store,
                request.run_id,
                &mut sequence,
                ReviewerEventType::AgentEvent,
                ExecutionPhase::Finalizing,
                json!({"cleanup_error": error.to_string()}),
            )?;
        }
        finalize_timed_out(store, &mut run, &mut sequence, request.timeout_seconds)?;
        return Ok(());
    }

    drop(stdin);
    let exit_status = wait_for_child(
        &mut child.child,
        pi,
        Duration::from_secs(request.termination_grace_seconds.max(1)),
    )?;
    child.disarm();
    if let Ok(Ok(stderr)) = stderr_rx.recv_timeout(Duration::from_millis(200)) {
        if !stderr.trim().is_empty() {
            append_event(
                store,
                request.run_id,
                &mut sequence,
                ReviewerEventType::AgentEvent,
                ExecutionPhase::Finalizing,
                json!({"stderr": stderr}),
            )?;
        }
    }
    append_event(
        store,
        request.run_id,
        &mut sequence,
        ReviewerEventType::ProcessExited,
        ExecutionPhase::Finalizing,
        json!({"success": exit_status.success(), "code": exit_status.code()}),
    )?;

    if !settled || !exit_status.success() {
        let detail = format!(
            "Pi exited before a settled review result (status: {})",
            exit_status
        );
        mark_failed_with_sequence(store, &mut run, &mut sequence, detail)?;
        return Ok(());
    }

    match request.kind {
        RunKind::Review => {
            let result = parse_review_result(request.run_id, final_output);
            store.save_result(request.run_id, &result)?;
            if !result.valid {
                run.final_result_ref = Some(result.result_id);
                store.save_run(&run)?;
                let detail = result
                    .parse_error
                    .clone()
                    .unwrap_or_else(|| "review result failed validation".to_owned());
                mark_failed_with_sequence(store, &mut run, &mut sequence, detail)?;
                return Ok(());
            }
            finalize_completed(
                store,
                &mut run,
                &mut sequence,
                result.result_id,
                result.structured,
                result.valid,
            )?;
        }
        RunKind::Verification => {
            let result = parse_verification_result(request, final_output);
            store.save_result(request.run_id, &result)?;
            if !result.valid {
                run.final_result_ref = Some(result.result_id);
                store.save_run(&run)?;
                let detail = result
                    .parse_error
                    .clone()
                    .unwrap_or_else(|| "verification result failed validation".to_owned());
                mark_failed_with_sequence(store, &mut run, &mut sequence, detail)?;
                return Ok(());
            }
            finalize_completed(
                store,
                &mut run,
                &mut sequence,
                result.result_id,
                result.structured,
                result.valid,
            )?;
        }
    }
    Ok(())
}

fn finalize_completed(
    store: &RunStore,
    run: &mut ReviewRun,
    sequence: &mut u64,
    result_id: Uuid,
    structured: bool,
    valid: bool,
) -> Result<()> {
    run.lifecycle_status = LifecycleStatus::Completed;
    run.completed_at = Some(Utc::now());
    run.final_result_ref = Some(result_id);
    store.save_run(run)?;
    append_event(
        store,
        run.run_id,
        sequence,
        ReviewerEventType::RunCompleted,
        ExecutionPhase::Finalizing,
        json!({"result_id": result_id, "structured": structured, "valid": valid}),
    )
}

fn reviewer_prompt(request: &str) -> String {
    format!(
        "You are an independent NEMESIS reviewer. Work read-only: do not modify files, run write-capable commands, or trust another agent's assessment. Inspect the current project and challenge the request below. Return exactly one JSON object, without Markdown fences, using this shape: {{\"summary\":\"...\",\"findings\":[{{\"id\":\"F-001\",\"claim\":\"...\",\"category\":\"requirements|architecture|correctness|compliance|edge_case|testing\",\"severity\":\"informational|low|medium|high|critical\",\"sources\":[\"path or locator\"],\"evidence\":[\"specific observable evidence\"]}}]}}. An empty findings array is valid. A finding is a claim to be independently verified later; do not call it confirmed.\n\nReview request:\n{request}"
    )
}

fn verifier_prompt(request: &ReviewRequest) -> String {
    let finding = request
        .finding
        .as_ref()
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned()))
        .unwrap_or_else(|| "{}".to_owned());
    let candidate_evidence =
        serde_json::to_string(&request.candidate_evidence).unwrap_or_else(|_| "[]".to_owned());
    format!(
        "You are an independent NEMESIS finding verifier. Start a fresh read-only investigation; the reviewer claim is only a hypothesis. Do not use reviewer reasoning or implementer conclusions. Inspect primary project sources, seek both supporting and contradicting evidence, and do not modify files or run write-capable commands. Return exactly one JSON object without Markdown fences. Required shape: {{\"verdict\":\"confirmed|rejected|inconclusive\",\"observations\":[\"what you directly observed\"],\"interpretations\":[\"what the observations may mean\"],\"conclusion\":\"...\",\"supporting_evidence\":[{{\"kind\":\"source|artifact|test|command_output\",\"path\":\"relative path\",\"locator\":\"line:N or lines:N-M\",\"excerpt\":\"exact source excerpt\",\"revision\":null}}],\"contradicting_evidence\":[{{\"kind\":\"source|artifact|test|command_output\",\"path\":\"relative path\",\"locator\":\"line:N or lines:N-M\",\"excerpt\":\"exact source excerpt\",\"revision\":null}}],\"uncertainty\":[\"remaining limitation\"]}}. Every evidence excerpt must be exact and independently inspectable. A confirmed verdict requires valid supporting evidence; a rejected verdict requires valid contradicting evidence. An inability to confirm is inconclusive, not rejected.

Project scope: {project_root}
Originating review run: {source_run}
Finding identifier: {finding_id}
Finding claim: {finding}
Candidate evidence supplied only as leads: {candidate_evidence}",
        project_root = request.project_root.display(),
        source_run = request
            .source_review_run_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        finding_id = request
            .source_finding_id
            .as_deref()
            .unwrap_or("unknown"),
        finding = finding,
        candidate_evidence = candidate_evidence,
    )
}

#[derive(Debug, Deserialize)]
struct ParsedVerification {
    verdict: VerificationState,
    observations: Vec<String>,
    interpretations: Vec<String>,
    conclusion: String,
    supporting_evidence: Vec<ParsedEvidence>,
    contradicting_evidence: Vec<ParsedEvidence>,
    uncertainty: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ParsedEvidence {
    kind: String,
    path: String,
    locator: String,
    excerpt: String,
    revision: Option<String>,
}

fn parse_verification_result(request: &ReviewRequest, raw_output: String) -> VerificationResult {
    let result_id = Uuid::new_v4();
    let Some(source_review_run_id) = request.source_review_run_id else {
        return invalid_verification_result(
            request,
            result_id,
            raw_output,
            "verification request omitted the originating review run".to_owned(),
        );
    };
    let Some(finding_id) = request.source_finding_id.clone() else {
        return invalid_verification_result(
            request,
            result_id,
            raw_output,
            "verification request omitted the finding identifier".to_owned(),
        );
    };
    let verifier_identity = request
        .verifier_identity
        .clone()
        .unwrap_or_else(|| "pi-verifier".to_owned());

    let parsed = match parse_structured_verification(&raw_output) {
        Ok(parsed) => parsed,
        Err(error) => {
            return invalid_verification_result(request, result_id, raw_output, error);
        }
    };

    let mut evidence = Vec::new();
    let mut supporting_evidence_refs = Vec::new();
    let mut contradicting_evidence_refs = Vec::new();
    for (items, refs) in [
        (&parsed.supporting_evidence, &mut supporting_evidence_refs),
        (
            &parsed.contradicting_evidence,
            &mut contradicting_evidence_refs,
        ),
    ] {
        for item in items {
            let evidence_id = Uuid::new_v4();
            let evidence_type = match item.kind.as_str() {
                "source" => EvidenceType::Source,
                "artifact" => EvidenceType::Artifact,
                "test" => EvidenceType::Test,
                "command_output" => EvidenceType::CommandOutput,
                _ => unreachable!("parse_structured_verification validates evidence kinds"),
            };
            let mut collected = Evidence {
                schema_version: MODEL_SCHEMA_VERSION.to_owned(),
                id: evidence_id,
                evidence_type,
                source: SourceReference {
                    path: item.path.clone(),
                    locator: Some(item.locator.clone()),
                    revision: item
                        .revision
                        .clone()
                        .or_else(|| request.repository_revision.clone()),
                },
                content: Some(item.excerpt.clone()),
                content_hash: content_hash_for_source(&request.project_root, &item.path),
                git_revision: request.repository_revision.clone(),
                collection: CollectionMetadata {
                    collector: verifier_identity.clone(),
                    method: "verifier-source-read".to_owned(),
                    collected_at: Utc::now(),
                    command: None,
                },
                validation: EvidenceValidation {
                    state: ValidationState::Pending,
                    validated_at: None,
                    validator: None,
                    errors: Vec::new(),
                },
            };
            let _ = validate_evidence(
                &request.project_root,
                &mut collected,
                verifier_identity.clone(),
            );
            refs.push(evidence_id);
            evidence.push(collected);
        }
    }

    let supporting_valid = evidence.iter().any(|item| {
        supporting_evidence_refs.contains(&item.id)
            && item.validation.state == ValidationState::Valid
    });
    let contradicting_valid = evidence.iter().any(|item| {
        contradicting_evidence_refs.contains(&item.id)
            && item.validation.state == ValidationState::Valid
    });
    let reported_verdict = parsed.verdict.clone();
    let mut uncertainty = parsed.uncertainty;
    let verdict = match reported_verdict {
        VerificationState::Confirmed if supporting_valid => VerificationState::Confirmed,
        VerificationState::Rejected if contradicting_valid => VerificationState::Rejected,
        VerificationState::Inconclusive => VerificationState::Inconclusive,
        VerificationState::Confirmed => {
            uncertainty.push(
                "The verifier reported confirmed, but no valid supporting evidence survived deterministic validation."
                    .to_owned(),
            );
            VerificationState::Inconclusive
        }
        VerificationState::Rejected => {
            uncertainty.push(
                "The verifier reported rejected, but no valid contradicting evidence survived deterministic validation."
                    .to_owned(),
            );
            VerificationState::Inconclusive
        }
        VerificationState::Unverified => {
            uncertainty.push("The verifier returned the disallowed unverified verdict.".to_owned());
            VerificationState::Inconclusive
        }
    };
    let record = VerificationRecord {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        verification_id: Uuid::new_v4(),
        verification_run_id: request.run_id,
        source_review_run_id,
        finding_id,
        verifier_identity,
        reported_verdict,
        verdict,
        supporting_evidence_refs,
        contradicting_evidence_refs,
        stale_evidence_refs: Vec::new(),
        observations: parsed.observations,
        interpretations: parsed.interpretations,
        explanation: parsed.conclusion,
        uncertainty,
        verified_at: Utc::now(),
    };
    VerificationResult {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        result_id,
        record,
        evidence,
        raw_output,
        structured: true,
        valid: true,
        parse_error: None,
        completed_at: Utc::now(),
    }
}

fn invalid_verification_result(
    request: &ReviewRequest,
    result_id: Uuid,
    raw_output: String,
    error: String,
) -> VerificationResult {
    let record = VerificationRecord {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        verification_id: Uuid::new_v4(),
        verification_run_id: request.run_id,
        source_review_run_id: request.source_review_run_id.unwrap_or(Uuid::nil()),
        finding_id: request.source_finding_id.clone().unwrap_or_default(),
        verifier_identity: request
            .verifier_identity
            .clone()
            .unwrap_or_else(|| "pi-verifier".to_owned()),
        reported_verdict: VerificationState::Inconclusive,
        verdict: VerificationState::Inconclusive,
        supporting_evidence_refs: Vec::new(),
        contradicting_evidence_refs: Vec::new(),
        stale_evidence_refs: Vec::new(),
        observations: Vec::new(),
        interpretations: Vec::new(),
        explanation: "Verification output was not valid.".to_owned(),
        uncertainty: vec![error.clone()],
        verified_at: Utc::now(),
    };
    VerificationResult {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        result_id,
        record,
        evidence: Vec::new(),
        raw_output,
        structured: false,
        valid: false,
        parse_error: Some(error),
        completed_at: Utc::now(),
    }
}

fn parse_structured_verification(raw: &str) -> std::result::Result<ParsedVerification, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Pi returned no verifier text".to_owned());
    }
    let parsed: ParsedVerification = serde_json::from_str(trimmed)
        .map_err(|error| format!("verifier output was not the required JSON shape: {error}"))?;
    if parsed.conclusion.trim().is_empty() {
        return Err("verifier conclusion must be non-empty".to_owned());
    }
    if parsed.verdict == VerificationState::Unverified {
        return Err("verifier verdict must be confirmed, rejected, or inconclusive".to_owned());
    }
    for value in parsed
        .observations
        .iter()
        .chain(parsed.interpretations.iter())
        .chain(parsed.uncertainty.iter())
    {
        if value.trim().is_empty() {
            return Err("verifier narrative arrays must not contain empty strings".to_owned());
        }
    }
    for evidence in parsed
        .supporting_evidence
        .iter()
        .chain(parsed.contradicting_evidence.iter())
    {
        if evidence.path.trim().is_empty()
            || evidence.locator.trim().is_empty()
            || evidence.excerpt.trim().is_empty()
        {
            return Err("every verifier evidence item needs path, locator, and excerpt".to_owned());
        }
        if !matches!(
            evidence.kind.as_str(),
            "source" | "artifact" | "test" | "command_output"
        ) {
            return Err(format!(
                "unsupported verifier evidence kind: {}",
                evidence.kind
            ));
        }
    }
    Ok(parsed)
}

fn parse_review_result(run_id: Uuid, raw_output: String) -> ReviewResult {
    let result_id = Uuid::new_v4();
    match parse_structured_output(&raw_output) {
        Ok(parsed) => {
            let mut evidence = Vec::new();
            let mut findings = Vec::new();
            for finding in parsed.findings {
                let mut evidence_refs = Vec::new();
                for claim in finding.evidence {
                    let evidence_id = Uuid::new_v4();
                    evidence_refs.push(evidence_id);
                    evidence.push(Evidence {
                        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
                        id: evidence_id,
                        evidence_type: EvidenceType::ReviewerClaim,
                        source: SourceReference {
                            path: "reviewer://pi".to_owned(),
                            locator: Some(finding.id.clone()),
                            revision: None,
                        },
                        content: Some(claim),
                        content_hash: None,
                        git_revision: None,
                        collection: CollectionMetadata {
                            collector: "pi-rpc".to_owned(),
                            method: "reviewer-assertion".to_owned(),
                            collected_at: Utc::now(),
                            command: None,
                        },
                        validation: EvidenceValidation {
                            state: ValidationState::Pending,
                            validated_at: None,
                            validator: None,
                            errors: Vec::new(),
                        },
                    });
                }
                findings.push(Finding {
                    schema_version: MODEL_SCHEMA_VERSION.to_owned(),
                    id: finding.id,
                    claim: finding.claim,
                    category: finding.category,
                    severity: finding.severity,
                    source_refs: finding
                        .sources
                        .into_iter()
                        .map(SourceReference::new)
                        .collect(),
                    evidence_refs,
                    verification_state: VerificationState::Unverified,
                });
            }
            ReviewResult {
                schema_version: MODEL_SCHEMA_VERSION.to_owned(),
                result_id,
                run_id,
                summary: Some(parsed.summary),
                findings,
                evidence,
                raw_output,
                structured: true,
                valid: true,
                parse_error: None,
                completed_at: Utc::now(),
            }
        }
        Err(error) => ReviewResult {
            schema_version: MODEL_SCHEMA_VERSION.to_owned(),
            result_id,
            run_id,
            summary: None,
            findings: Vec::new(),
            evidence: Vec::new(),
            raw_output,
            structured: false,
            valid: false,
            parse_error: Some(error),
            completed_at: Utc::now(),
        },
    }
}

#[derive(Debug, Deserialize)]
struct ParsedReview {
    summary: String,
    findings: Vec<ParsedFinding>,
}

#[derive(Debug, Deserialize)]
struct ParsedFinding {
    id: String,
    claim: String,
    category: String,
    severity: Severity,
    sources: Vec<String>,
    evidence: Vec<String>,
}

fn parse_structured_output(raw: &str) -> std::result::Result<ParsedReview, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Pi returned no assistant text".to_owned());
    }
    let candidate = if let Some(stripped) = trimmed.strip_prefix("```json") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else if let Some(stripped) = trimmed.strip_prefix("```") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else {
        trimmed
    };
    let parsed: ParsedReview = serde_json::from_str(candidate)
        .map_err(|error| format!("assistant output was not the required JSON shape: {error}"))?;
    if parsed.summary.trim().is_empty() {
        return Err("summary must be a non-empty string".to_owned());
    }
    let mut ids = HashSet::new();
    for finding in &parsed.findings {
        if finding.id.trim().is_empty() || finding.claim.trim().is_empty() {
            return Err("each finding must contain a non-empty id and claim".to_owned());
        }
        if finding.category.trim().is_empty() {
            return Err(format!(
                "finding {} must contain a non-empty category",
                finding.id
            ));
        }
        if !matches!(
            finding.category.as_str(),
            "requirements"
                | "architecture"
                | "correctness"
                | "compliance"
                | "edge_case"
                | "testing"
        ) {
            return Err(format!(
                "unsupported finding category: {}",
                finding.category
            ));
        }
        if !ids.insert(finding.id.trim().to_owned()) {
            return Err(format!("duplicate finding identifier: {}", finding.id));
        }
    }
    Ok(parsed)
}

fn message_text(message: Option<&Value>) -> String {
    let Some(content) = message.and_then(|message| message.get("content")) else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        block.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn event_phase(value: &Value) -> ExecutionPhase {
    match value.get("type").and_then(Value::as_str) {
        Some("agent_start") | Some("turn_start") | Some("message_start") => {
            ExecutionPhase::Reviewing
        }
        Some("agent_settled") | Some("agent_end") | Some("turn_end") => ExecutionPhase::Finalizing,
        _ => ExecutionPhase::Reviewing,
    }
}

fn spawn_line_reader<R: Read + Send + 'static>(
    reader: R,
) -> Receiver<std::result::Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let value = line.map_err(|error| error.to_string());
            if sender.send(value).is_err() {
                break;
            }
        }
    });
    receiver
}

fn spawn_text_reader<R: Read + Send + 'static>(
    mut reader: R,
) -> Receiver<std::result::Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = String::new();
        let result = reader
            .read_to_string(&mut output)
            .map(|_| output)
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

fn write_rpc(stdin: &mut impl Write, value: &Value) -> Result<()> {
    let line = serde_json::to_string(value)?;
    writeln!(stdin, "{line}").map_err(|source| AgentError::Io {
        path: PathBuf::from("pi-rpc-stdin"),
        source,
    })?;
    stdin.flush().map_err(|source| AgentError::Io {
        path: PathBuf::from("pi-rpc-stdin"),
        source,
    })?;
    Ok(())
}

enum StartupWriteOutcome {
    Ready(ChildStdin),
    Cancelled,
    TimedOut,
}

fn write_rpc_until_startup(
    store: &RunStore,
    run_id: Uuid,
    mut stdin: ChildStdin,
    value: Value,
    deadline: Instant,
) -> Result<StartupWriteOutcome> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = write_rpc(&mut stdin, &value);
        let _ = sender.send((result, stdin));
    });
    loop {
        if store.is_cancel_requested(run_id) {
            return Ok(StartupWriteOutcome::Cancelled);
        }
        if Instant::now() >= deadline {
            return Ok(StartupWriteOutcome::TimedOut);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok((Ok(()), stdin)) => {
                if store.is_cancel_requested(run_id) {
                    return Ok(StartupWriteOutcome::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Ok(StartupWriteOutcome::TimedOut);
                }
                return Ok(StartupWriteOutcome::Ready(stdin));
            }
            Ok((Err(error), _)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AgentError::Process(
                    "Pi prompt writer terminated without a result".to_owned(),
                ));
            }
        }
    }
}

fn write_abort_bounded(
    mut stdin: ChildStdin,
    timeout: Duration,
) -> std::result::Result<ChildStdin, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = write_rpc(&mut stdin, &json!({"type": "abort"}));
        let _ = sender.send((result, stdin));
    });
    match receiver.recv_timeout(timeout) {
        Ok((Ok(()), stdin)) => Ok(stdin),
        Ok((Err(error), _)) => Err(error.to_string()),
        Err(RecvTimeoutError::Timeout) => {
            Err("Pi abort write exceeded its bounded grace period".to_owned())
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err("Pi abort writer terminated without a result".to_owned())
        }
    }
}

struct ControlSignal<'a> {
    timed_out: bool,
    reason: &'a str,
}

fn finalize_controlled_run(
    store: &RunStore,
    run: &mut ReviewRun,
    sequence: &mut u64,
    child: &mut Child,
    pi: &Path,
    request: &ReviewRequest,
    signal: ControlSignal<'_>,
) -> Result<()> {
    append_event(
        store,
        run.run_id,
        sequence,
        ReviewerEventType::AgentEvent,
        ExecutionPhase::Finalizing,
        json!({"control": signal.reason}),
    )?;
    if let Err(error) = terminate_child(
        child,
        pi,
        Duration::from_secs(request.termination_grace_seconds.max(1)),
    ) {
        append_event(
            store,
            run.run_id,
            sequence,
            ReviewerEventType::AgentEvent,
            ExecutionPhase::Finalizing,
            json!({"cleanup_error": error.to_string()}),
        )?;
    }
    if signal.timed_out {
        finalize_timed_out(store, run, sequence, request.timeout_seconds)
    } else {
        finalize_cancelled(store, run, sequence)
    }
}

fn set_status(
    store: &RunStore,
    run: &mut ReviewRun,
    status: LifecycleStatus,
    completed_at: Option<chrono::DateTime<Utc>>,
    error: Option<String>,
) -> Result<()> {
    run.lifecycle_status = status;
    if completed_at.is_some() {
        run.completed_at = completed_at;
    }
    if error.is_some() {
        run.error = error;
    }
    store.save_run(run)?;
    Ok(())
}

fn mark_failed(store: &RunStore, run_id: Uuid, error: String) -> Result<()> {
    let mut run = store.load_run(run_id)?;
    let mut sequence = next_sequence(store, run_id);
    mark_failed_with_sequence(store, &mut run, &mut sequence, error)
}

fn mark_failed_with_sequence(
    store: &RunStore,
    run: &mut ReviewRun,
    sequence: &mut u64,
    error: String,
) -> Result<()> {
    run.lifecycle_status = LifecycleStatus::Failed;
    run.completed_at = Some(Utc::now());
    run.error = Some(error.clone());
    store.save_run(run)?;
    append_event(
        store,
        run.run_id,
        sequence,
        ReviewerEventType::RunFailed,
        ExecutionPhase::Finalizing,
        json!({"error": error}),
    )?;
    Ok(())
}

fn finalize_cancelled(store: &RunStore, run: &mut ReviewRun, sequence: &mut u64) -> Result<()> {
    run.lifecycle_status = LifecycleStatus::Cancelled;
    run.completed_at = Some(Utc::now());
    store.save_run(run)?;
    append_event(
        store,
        run.run_id,
        sequence,
        ReviewerEventType::RunCancelled,
        ExecutionPhase::Finalizing,
        json!({"reason": "cancel_requested"}),
    )?;
    Ok(())
}

fn finalize_timed_out(
    store: &RunStore,
    run: &mut ReviewRun,
    sequence: &mut u64,
    timeout_seconds: u64,
) -> Result<()> {
    run.lifecycle_status = LifecycleStatus::TimedOut;
    run.completed_at = Some(Utc::now());
    run.error = Some(format!("review exceeded {timeout_seconds} seconds"));
    store.save_run(run)?;
    append_event(
        store,
        run.run_id,
        sequence,
        ReviewerEventType::RunTimedOut,
        ExecutionPhase::Finalizing,
        json!({"timeout_seconds": timeout_seconds}),
    )?;
    Ok(())
}

fn append_event(
    store: &RunStore,
    run_id: Uuid,
    sequence: &mut u64,
    event_type: ReviewerEventType,
    phase: ExecutionPhase,
    payload: Value,
) -> Result<()> {
    let event = ReviewerEvent {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        event_id: Uuid::new_v4(),
        run_id,
        sequence: *sequence,
        timestamp: Utc::now(),
        event_type,
        phase,
        payload,
    };
    *sequence += 1;
    store.append_event(&event)?;
    Ok(())
}

fn next_sequence(store: &RunStore, run_id: Uuid) -> u64 {
    let path = store.events_path(run_id);
    let Ok(content) = std::fs::read_to_string(path) else {
        return 1;
    };
    content.lines().count() as u64 + 1
}

struct ManagedChild {
    child: Child,
    disarmed: bool,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self {
            child,
            disarmed: false,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Deref for ManagedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let _ = terminate_child(&mut self.child, Path::new("pi"), Duration::from_secs(2));
    }
}

fn terminate_child(child: &mut Child, path: &Path, grace: Duration) -> Result<()> {
    if child
        .try_wait()
        .map_err(|source| AgentError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .is_none()
    {
        if let Err(error) = child.kill() {
            if error.kind() != io::ErrorKind::InvalidInput {
                return Err(AgentError::Io {
                    path: path.to_path_buf(),
                    source: error,
                });
            }
        }
    }
    let deadline = Instant::now() + grace;
    loop {
        if child
            .try_wait()
            .map_err(|source| AgentError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AgentError::Process(format!(
                "child process did not exit within {} seconds after termination",
                grace.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_child(child: &mut Child, path: &Path, grace: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        if let Some(status) = child.try_wait().map_err(|source| AgentError::Io {
            path: path.to_path_buf(),
            source,
        })? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            terminate_child(child, path, grace)?;
            return child
                .try_wait()
                .map_err(|source| AgentError::Io {
                    path: path.to_path_buf(),
                    source,
                })?
                .ok_or_else(|| {
                    AgentError::Process(
                        "child process remained running after bounded cleanup".to_owned(),
                    )
                });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = directory.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn command_version(executable: &Path) -> Result<String> {
    let mut child = Command::new(executable)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AgentError::Io {
            path: executable.to_path_buf(),
            source,
        })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child
            .try_wait()
            .map_err(|source| AgentError::Io {
                path: executable.to_path_buf(),
                source,
            })?
            .is_some()
        {
            let output = child.wait_with_output().map_err(|source| AgentError::Io {
                path: executable.to_path_buf(),
                source,
            })?;
            if !output.status.success() {
                return Err(AgentError::Process(
                    String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                ));
            }
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        if Instant::now() >= deadline {
            let cleanup = terminate_child(&mut child, executable, Duration::from_millis(250));
            return match cleanup {
                Ok(()) => Err(AgentError::Process(
                    "Pi version probe exceeded 2 seconds".to_owned(),
                )),
                Err(error) => Err(AgentError::Process(format!(
                    "Pi version probe exceeded 2 seconds; cleanup failed: {error}"
                ))),
            };
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_prompt_requires_unverified_structured_findings() {
        let prompt = reviewer_prompt("Check authentication");
        assert!(prompt.contains("without Markdown fences"));
        assert!(prompt.contains("do not call it confirmed"));
    }

    #[test]
    fn structured_review_output_is_parsed_without_claiming_verification() {
        let output = r#"{"summary":"One issue","findings":[{"id":"F-001","claim":"The timeout is unbounded","category":"edge_case","severity":"high","sources":["src/main.rs:4"],"evidence":["No timeout configuration is present"]}]}"#;
        let result = parse_review_result(Uuid::new_v4(), output.to_owned());
        assert!(result.structured);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].verification_state,
            VerificationState::Unverified
        );
        assert_eq!(result.evidence.len(), 1);
    }

    #[test]
    fn malformed_review_output_is_inconclusive_for_parsing() {
        let result = parse_review_result(Uuid::new_v4(), "not json".to_owned());
        assert!(!result.structured);
        assert!(!result.valid);
        assert!(result.parse_error.is_some());
        assert!(result.findings.is_empty());
    }

    #[test]
    fn empty_object_and_duplicate_finding_ids_are_invalid() {
        let empty = parse_review_result(Uuid::new_v4(), "{}".to_owned());
        assert!(!empty.valid);
        assert!(empty.parse_error.is_some());

        let duplicate = parse_review_result(
            Uuid::new_v4(),
            r#"{"summary":"x","findings":[{"id":"F-1","claim":"a","category":"correctness","severity":"low","sources":[],"evidence":[]},{"id":"F-1","claim":"b","category":"correctness","severity":"low","sources":[],"evidence":[]}]}"#.to_owned(),
        );
        assert!(!duplicate.valid);
        assert!(duplicate
            .parse_error
            .as_deref()
            .is_some_and(|error| error.contains("duplicate")));

        let invalid_category = parse_review_result(
            Uuid::new_v4(),
            r#"{"summary":"x","findings":[{"id":"F-1","claim":"a","category":"made_up","severity":"low","sources":[],"evidence":[]}]}"#.to_owned(),
        );
        assert!(!invalid_category.valid);
    }

    #[test]
    fn explicitly_empty_findings_array_is_valid() {
        let result = parse_review_result(
            Uuid::new_v4(),
            r#"{"summary":"No issues found in the inspected scope","findings":[]}"#.to_owned(),
        );
        assert!(result.valid);
        assert!(result.structured);
        assert!(result.findings.is_empty());
    }

    fn verification_request(root: &Path) -> ReviewRequest {
        ReviewRequest::for_verification(
            Uuid::new_v4(),
            root,
            Uuid::new_v4(),
            Finding {
                schema_version: MODEL_SCHEMA_VERSION.to_owned(),
                id: "F-1".to_owned(),
                claim: "The source contains the reported behavior.".to_owned(),
                category: "correctness".to_owned(),
                severity: Severity::High,
                source_refs: vec![SourceReference::new("src/main.rs")],
                evidence_refs: Vec::new(),
                verification_state: VerificationState::Unverified,
            },
            Vec::new(),
            "test-verifier",
            5,
        )
    }

    #[test]
    fn verifier_validates_evidence_and_confirms_only_with_support() {
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir_all(directory.path().join("src")).expect("source directory");
        std::fs::write(directory.path().join("src/main.rs"), "fn main() {}\n")
            .expect("source file");
        let request = verification_request(directory.path());
        let output = r#"{"verdict":"confirmed","observations":["The function is present."],"interpretations":["The observation supports the claim."],"conclusion":"The finding is supported.","supporting_evidence":[{"kind":"source","path":"src/main.rs","locator":"line:1","excerpt":"fn main() {}","revision":null}],"contradicting_evidence":[],"uncertainty":[]}"#;
        let result = parse_verification_result(&request, output.to_owned());
        assert!(result.valid);
        assert_eq!(result.record.verdict, VerificationState::Confirmed);
        assert_eq!(
            result.evidence[0].validation.state,
            ValidationState::Valid,
            "{:?}",
            result.evidence[0].validation.errors
        );
    }

    #[test]
    fn verifier_confirmation_without_evidence_is_inconclusive() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let request = verification_request(directory.path());
        let output = r#"{"verdict":"confirmed","observations":["No primary source was supplied."],"interpretations":["The claim cannot be established."],"conclusion":"The finding appears true.","supporting_evidence":[],"contradicting_evidence":[],"uncertainty":["No evidence was available."]}"#;
        let result = parse_verification_result(&request, output.to_owned());
        assert_eq!(result.record.verdict, VerificationState::Inconclusive);
        assert!(result.evidence.is_empty());
    }

    #[test]
    fn verifier_can_reject_with_valid_contradicting_evidence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir_all(directory.path().join("src")).expect("source directory");
        std::fs::write(directory.path().join("src/main.rs"), "fn main() {}\n")
            .expect("source file");
        let request = verification_request(directory.path());
        let output = r#"{"verdict":"rejected","observations":["The source does not contain the claimed implementation."],"interpretations":["The claim is contradicted by the inspected source."],"conclusion":"The finding is rejected.","supporting_evidence":[],"contradicting_evidence":[{"kind":"source","path":"src/main.rs","locator":"line:1","excerpt":"fn main() {}","revision":null}],"uncertainty":[]}"#;
        let result = parse_verification_result(&request, output.to_owned());
        assert_eq!(result.record.verdict, VerificationState::Rejected);
        assert_eq!(result.evidence[0].validation.state, ValidationState::Valid);
    }

    #[test]
    fn verifier_downgrades_unsupported_confirmation_to_inconclusive() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let request = verification_request(directory.path());
        let output = r#"{"verdict":"confirmed","observations":["The source was unavailable."],"interpretations":["No conclusion can be established."],"conclusion":"The finding may be supported.","supporting_evidence":[{"kind":"source","path":"missing.rs","locator":"line:1","excerpt":"missing","revision":null}],"contradicting_evidence":[],"uncertainty":["The source could not be inspected."]}"#;
        let result = parse_verification_result(&request, output.to_owned());
        assert!(result.valid);
        assert_eq!(result.record.verdict, VerificationState::Inconclusive);
        assert_eq!(
            result.evidence[0].validation.state,
            ValidationState::Invalid
        );
    }

    #[cfg(unix)]
    fn executable_script(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        std::fs::write(&path, body).expect("write mock process");
        let mut permissions = std::fs::metadata(&path)
            .expect("mock metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("make mock executable");
        path
    }

    #[cfg(unix)]
    fn persisted_review(directory: &Path, timeout_seconds: u64) -> (RunStore, ReviewRequest) {
        let store = RunStore::new(ProjectPaths::new(directory));
        let run_id = Uuid::new_v4();
        let run = ReviewRun {
            schema_version: MODEL_SCHEMA_VERSION.to_owned(),
            kind: RunKind::Review,
            run_id,
            parent_run_id: None,
            agent_runtime: "pi".to_owned(),
            reviewer_profile: "test".to_owned(),
            lifecycle_status: LifecycleStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            final_result_ref: None,
            error: None,
        };
        store.ensure(run_id).expect("run directory");
        store.save_run(&run).expect("save run");
        let mut request = ReviewRequest::new(run_id, directory, "test", "test", timeout_seconds);
        request.termination_grace_seconds = 1;
        store.save_request(run_id, &request).expect("save request");
        (store, request)
    }

    #[cfg(unix)]
    #[test]
    fn pi_version_probe_is_bounded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script =
            executable_script(directory.path(), "pi-version-hang", "#!/bin/sh\nsleep 30\n");
        let started = Instant::now();
        let result = command_version(&script);
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mock_rpc_success_persists_valid_completed_result() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(
            directory.path(),
            "pi-success",
            "#!/bin/sh\nread line\nprintf '%s\\n' '{\"type\":\"agent_start\"}'\nprintf '%s\\n' '{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":\"{\\\"summary\\\":\\\"ok\\\",\\\"findings\\\":[]}\"}}'\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\n",
        );
        let (store, request) = persisted_review(directory.path(), 2);
        run_pi_review_with_executable(&store, &PiRuntime, &request, &script).expect("mock review");
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::Completed);
        let result: ReviewResult = store.load_result(request.run_id).expect("load result");
        assert!(result.valid);
    }

    #[cfg(unix)]
    #[test]
    fn mock_rpc_timeout_is_bounded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(directory.path(), "pi-timeout", "#!/bin/sh\nsleep 30\n");
        let (store, request) = persisted_review(directory.path(), 1);
        let started = Instant::now();
        run_pi_review_with_executable(&store, &PiRuntime, &request, &script)
            .expect("timeout is a terminal result");
        assert!(started.elapsed() < Duration::from_secs(4));
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn mock_rpc_invalid_output_is_failed_but_result_remains_inspectable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(
            directory.path(),
            "pi-invalid",
            "#!/bin/sh\nread line\nprintf '%s\\n' '{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":\"{}\"}}'\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\n",
        );
        let (store, request) = persisted_review(directory.path(), 2);
        run_pi_review_with_executable(&store, &PiRuntime, &request, &script)
            .expect("invalid output is a persisted failure");
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::Failed);
        assert!(run.final_result_ref.is_some());
        let result: ReviewResult = store
            .load_result(request.run_id)
            .expect("load invalid result");
        assert!(!result.valid);
    }

    #[cfg(unix)]
    #[test]
    fn mock_rpc_abnormal_exit_is_failed_and_observable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(directory.path(), "pi-exit", "#!/bin/sh\nexit 7\n");
        let (store, request) = persisted_review(directory.path(), 2);
        run_pi_review_with_executable(&store, &PiRuntime, &request, &script)
            .expect("abnormal process is a persisted failure");
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::Failed);
        assert!(store
            .load_events(request.run_id)
            .expect("load events")
            .iter()
            .any(|event| event.event_type == ReviewerEventType::ProcessExited));
    }

    #[cfg(unix)]
    #[test]
    fn blocked_startup_prompt_is_timed_out_not_failed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(
            directory.path(),
            "pi-startup-timeout",
            "#!/bin/sh\nsleep 30\n",
        );
        let (store, mut request) = persisted_review(directory.path(), 1);
        request.prompt = "x".repeat(2 * 1024 * 1024);
        store
            .save_request(request.run_id, &request)
            .expect("save large request");
        let started = Instant::now();
        run_pi_review_with_executable(&store, &PiRuntime, &request, &script)
            .expect("startup timeout is terminal");
        assert!(started.elapsed() < Duration::from_secs(4));
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_during_blocked_startup_is_cancelled() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(
            directory.path(),
            "pi-startup-cancel",
            "#!/bin/sh\nsleep 30\n",
        );
        let (store, mut request) = persisted_review(directory.path(), 10);
        request.prompt = "x".repeat(2 * 1024 * 1024);
        store
            .save_request(request.run_id, &request)
            .expect("save large request");
        let worker_store = store.clone();
        let worker_request = request.clone();
        let worker = std::thread::spawn(move || {
            run_pi_review_with_executable(&worker_store, &PiRuntime, &worker_request, &script)
        });
        std::thread::sleep(Duration::from_millis(150));
        store
            .request_cancel(request.run_id)
            .expect("request cancel");
        worker
            .join()
            .expect("worker thread")
            .expect("cancel result");
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::Cancelled);
    }

    #[cfg(unix)]
    #[test]
    fn mock_rpc_cancellation_without_abort_response_is_bounded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = executable_script(directory.path(), "pi-cancel", "#!/bin/sh\nsleep 30\n");
        let (store, request) = persisted_review(directory.path(), 10);
        let worker_store = store.clone();
        let worker_request = request.clone();
        let worker = std::thread::spawn(move || {
            run_pi_review_with_executable(&worker_store, &PiRuntime, &worker_request, &script)
        });
        std::thread::sleep(Duration::from_millis(150));
        store
            .request_cancel(request.run_id)
            .expect("request cancel");
        worker
            .join()
            .expect("worker thread")
            .expect("cancel result");
        let run = store.load_run(request.run_id).expect("load run");
        assert_eq!(run.lifecycle_status, LifecycleStatus::Cancelled);
    }
}
