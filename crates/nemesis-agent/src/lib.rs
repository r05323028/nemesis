//! Agent runtime contracts and the first process-backed reviewer adapter.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use nemesis_core::config::ProjectPaths;
use nemesis_core::model::{
    CollectionMetadata, Evidence, EvidenceType, EvidenceValidation, ExecutionPhase, Finding,
    LifecycleStatus, ReviewResult, ReviewRun, ReviewerEvent, ReviewerEventType, Severity,
    SourceReference, ValidationState, VerificationState, MODEL_SCHEMA_VERSION,
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
    pub run_id: Uuid,
    pub project_root: PathBuf,
    pub prompt: String,
    pub reviewer_profile: String,
    pub agent_runtime: String,
    pub timeout_seconds: u64,
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
            run_id,
            project_root: project_root.into(),
            prompt: prompt.into(),
            reviewer_profile: reviewer_profile.into(),
            agent_runtime: "pi".to_owned(),
            timeout_seconds,
        }
    }
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

    let pi = runtime.executable()?;
    let started_at = Utc::now();
    run.started_at = Some(started_at);
    run.lifecycle_status = LifecycleStatus::Starting;
    store.save_run(&run)?;

    let child = Command::new(&pi)
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
            path: pi.clone(),
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
    let mut stdin = child
        .child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Process("Pi stdin was not available".to_owned()))?;

    let prompt = reviewer_prompt(&request.prompt);
    write_rpc(
        &mut stdin,
        &json!({
            "id": "nemesis-review",
            "type": "prompt",
            "message": prompt,
        }),
    )?;
    set_status(store, &mut run, LifecycleStatus::Running, None, None)?;

    let mut final_output = String::new();
    let mut settled = false;
    let mut cancel_requested = false;
    let mut timed_out = false;
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds.max(1));

    loop {
        if !cancel_requested && store.is_cancel_requested(request.run_id) {
            cancel_requested = true;
            let _ = write_rpc(&mut stdin, &json!({"type": "abort"}));
            debug!(run_id = %request.run_id, "sent Pi abort request");
        }
        if !timed_out && Instant::now() >= deadline {
            timed_out = true;
            let _ = write_rpc(&mut stdin, &json!({"type": "abort"}));
            warn!(run_id = %request.run_id, "Pi reviewer timed out; sent abort request");
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
                        path: pi.clone(),
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
                path: pi.clone(),
                source,
            })?
            .is_none()
        {
            terminate_child(&mut child.child, &pi)?;
        }
    }

    if cancel_requested {
        terminate_child(&mut child.child, &pi)?;
        finalize_cancelled(store, &mut run, &mut sequence)?;
        return Ok(());
    }
    if timed_out {
        terminate_child(&mut child.child, &pi)?;
        finalize_timed_out(store, &mut run, &mut sequence, request.timeout_seconds)?;
        return Ok(());
    }

    drop(stdin);
    let exit_status = wait_for_child(&mut child.child, &pi)?;
    child.disarm();
    let _stderr = stderr_rx.recv_timeout(Duration::from_millis(200)).ok();
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

    let result = parse_review_result(request.run_id, final_output);
    store.save_result(request.run_id, &result)?;
    run.lifecycle_status = LifecycleStatus::Completed;
    run.completed_at = Some(Utc::now());
    run.final_result_ref = Some(result.result_id);
    run.error = result.parse_error.clone();
    store.save_run(&run)?;
    append_event(
        store,
        request.run_id,
        &mut sequence,
        ReviewerEventType::RunCompleted,
        ExecutionPhase::Finalizing,
        json!({
            "result_id": result.result_id,
            "structured": result.structured,
            "parse_error": result.parse_error,
        }),
    )?;
    Ok(())
}

fn reviewer_prompt(request: &str) -> String {
    format!(
        "You are an independent NEMESIS reviewer. Work read-only: do not modify files, run write-capable commands, or trust another agent's assessment. Inspect the current project and challenge the request below. Return exactly one JSON object, without Markdown fences, using this shape: {{\"summary\":\"...\",\"findings\":[{{\"id\":\"F-001\",\"claim\":\"...\",\"category\":\"requirements|architecture|correctness|compliance|edge_case|testing\",\"severity\":\"informational|low|medium|high|critical\",\"sources\":[\"path or locator\"],\"evidence\":[\"specific observable evidence\"]}}]}}. An empty findings array is valid. A finding is a claim to be independently verified later; do not call it confirmed.\n\nReview request:\n{request}"
    )
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
                            collected_at: Utc::now(),
                            command: None,
                        },
                        validation: EvidenceValidation {
                            state: ValidationState::Pending,
                            validated_at: None,
                            validator: None,
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
            parse_error: Some(error),
            completed_at: Utc::now(),
        },
    }
}

#[derive(Debug, Deserialize)]
struct ParsedReview {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<ParsedFinding>,
}

#[derive(Debug, Deserialize)]
struct ParsedFinding {
    id: String,
    claim: String,
    category: String,
    severity: Severity,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
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
    for finding in &parsed.findings {
        if finding.id.trim().is_empty() || finding.claim.trim().is_empty() {
            return Err("each finding must contain a non-empty id and claim".to_owned());
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
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn terminate_child(child: &mut Child, path: &Path) -> Result<()> {
    if child
        .try_wait()
        .map_err(|source| AgentError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .is_none()
    {
        child.kill().map_err(|source| AgentError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let _ = child.wait();
    Ok(())
}

fn wait_for_child(child: &mut Child, path: &Path) -> Result<ExitStatus> {
    child.wait().map_err(|source| AgentError::Io {
        path: path.to_path_buf(),
        source,
    })
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
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|source| AgentError::Io {
            path: executable.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(AgentError::Process(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
        assert!(result.parse_error.is_some());
        assert!(result.findings.is_empty());
    }
}
