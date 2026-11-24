use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use chrono::Utc;
use clap::{CommandFactory, Parser, Subcommand};
use nemesis_agent::{spawn_worker_process, PiRuntime, ReviewRequest};
use nemesis_core::config::{discover_project_root, NemesisConfig, ProjectPaths};
use nemesis_core::evidence::validate_evidence;
use nemesis_core::model::{
    ExecutionPhase, Finding, LifecycleStatus, ReviewResult, ReviewRun, ReviewerEvent,
    ReviewerEventType, RunKind, VerificationRecord, VerificationResult, MODEL_SCHEMA_VERSION,
};
use nemesis_core::store::RunStore;
use nemesis_integrations::{
    inspect_openspec, install_agent, install_sdd, list_agent_integrations, list_sdd_integrations,
    remove_agent, remove_sdd, InstalledIntegration,
};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const NEMESIS_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Error)]
enum CliError {
    #[error("core error: {0}")]
    Core(#[from] nemesis_core::CoreError),
    #[error("agent error: {0}")]
    Agent(#[from] nemesis_agent::AgentError),
    #[error("SDD error: {0}")]
    Sdd(#[from] nemesis_sdd::SddError),
    #[error("integration error: {0}")]
    Integration(#[from] nemesis_integrations::IntegrationError),
    #[error("invalid command input: {0}")]
    Input(String),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Parser)]
#[command(
    name = "nemesis",
    version,
    about = "Independent evidence-driven verification for SDD workflows"
)]
struct Cli {
    /// Emit machine-readable JSON instead of human-oriented output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize NEMESIS configuration without changing SDD artifacts.
    Init,
    /// Show the NEMESIS version and implementation status.
    Version,
    /// Check the local environment and project integrations.
    Doctor,
    /// Manage coding-agent integrations.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Manage SDD framework integrations.
    Sdd {
        #[command(subcommand)]
        command: SddCommand,
    },
    /// Launch and inspect independent reviewer runs.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    /// List, inspect, and independently verify reviewer findings.
    Finding {
        #[command(subcommand)]
        command: FindingCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SddCommand {
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    /// List detected integrations and project-owned files.
    List,
    /// Install a supported integration safely.
    Install { id: String },
    /// Remove only NEMESIS-managed integration files.
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    /// Queue an independent reviewer and return its stable run ID.
    Run {
        #[arg(long)]
        prompt: String,
        #[arg(long, default_value = "default")]
        reviewer_profile: String,
        #[arg(long, default_value_t = 900)]
        timeout_seconds: u64,
        #[arg(long, default_value = "pi")]
        agent: String,
        #[arg(long)]
        parent_run_id: Option<Uuid>,
    },
    /// Show one run, or all persisted runs when no ID is supplied.
    Status { run_id: Option<Uuid> },
    /// Request cancellation of a non-terminal run.
    Cancel { run_id: Uuid },
    /// Show recent persisted JSONL execution events.
    Events {
        run_id: Uuid,
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    /// Wait for a run to reach a terminal state with a bounded timeout.
    Wait {
        run_id: Uuid,
        #[arg(long, default_value_t = 900)]
        timeout_seconds: u64,
    },
    /// Retrieve the persisted structured result for a run.
    Result { run_id: Uuid },
    /// Internal worker entry point used by the process-backed runtime.
    #[command(hide = true)]
    Worker {
        run_id: Uuid,
        #[arg(long)]
        root: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum FindingCommand {
    /// List findings from completed reviewer results.
    List,
    /// Show one finding scoped to its originating review run.
    Show {
        review_run_id: Uuid,
        finding_id: String,
    },
    /// Launch an independent verifier for one finding.
    Verify {
        review_run_id: Uuid,
        finding_id: String,
        #[arg(long, default_value = "pi-verifier")]
        verifier: String,
        #[arg(long, default_value_t = 900)]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Serialize)]
struct InitReport {
    root: PathBuf,
    config: PathBuf,
    created_config: bool,
    created_runtime_directories: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct VersionReport {
    name: &'static str,
    version: &'static str,
    implemented: &'static [&'static str],
    planned: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct Diagnostic {
    severity: &'static str,
    code: &'static str,
    message: String,
    fix: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    root: PathBuf,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize)]
struct ReviewStatus {
    run: ReviewRun,
    result: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct FindingView {
    review_run_id: Uuid,
    finding: Finding,
    verifications: Vec<VerificationRecord>,
}

#[derive(Debug, Serialize)]
struct WaitReport {
    run: ReviewRun,
    completed: bool,
}

fn main() {
    let cli = Cli::parse();
    init_logging(cli.json);
    if let Err(error) = run(cli) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let root = project_root()?;
    match cli.command {
        None => {
            let mut command = Cli::command();
            command.print_help().map_err(|source| CliError::Io {
                path: PathBuf::from("stdout"),
                source,
            })?;
            println!();
        }
        Some(Command::Init) => emit_init(&root, cli.json)?,
        Some(Command::Version) => emit_version(cli.json),
        Some(Command::Doctor) => emit_doctor(&root, cli.json)?,
        Some(Command::Agent { command }) => match command {
            AgentCommand::Integration { command } => {
                handle_agent_integration(&root, command, cli.json)?
            }
        },
        Some(Command::Sdd { command }) => match command {
            SddCommand::Integration { command } => {
                handle_sdd_integration(&root, command, cli.json)?
            }
        },
        Some(Command::Review { command }) => handle_review(&root, command, cli.json)?,
        Some(Command::Finding { command }) => handle_finding(&root, command, cli.json)?,
    }
    Ok(())
}

fn init_logging(json_output: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(!json_output)
        .try_init();
}

fn project_root() -> Result<PathBuf> {
    let current = std::env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("current directory"),
        source,
    })?;
    let root = discover_project_root(&current);
    fs::canonicalize(&root).map_err(|source| CliError::Io { path: root, source })
}

fn emit_init(root: &Path, json_output: bool) -> Result<()> {
    let paths = ProjectPaths::new(root);
    let created_config = if paths.config_file().is_file() {
        NemesisConfig::load(&paths)?;
        false
    } else {
        NemesisConfig::for_project(root).save(&paths)?;
        true
    };
    let runtime_dir = paths.runs_dir();
    let created_runtime_directories = !runtime_dir.is_dir();
    paths.ensure_runtime_dirs()?;
    let local_gitignore = paths.nemesis_dir().join(".gitignore");
    if !local_gitignore.exists() {
        fs::write(&local_gitignore, "runs/\nintegrations.json\n").map_err(|source| {
            CliError::Io {
                path: local_gitignore.clone(),
                source,
            }
        })?;
    }
    let report = InitReport {
        root: root.to_path_buf(),
        config: paths.config_file(),
        created_config,
        created_runtime_directories,
        message: if created_config {
            "NEMESIS configuration initialized; existing SDD artifacts were not modified."
                .to_owned()
        } else {
            "NEMESIS configuration already existed and was validated; no files were overwritten."
                .to_owned()
        },
    };
    if json_output {
        print_json(&report)?;
    } else {
        println!("{}", report.message);
        println!("root: {}", report.root.display());
        println!("config: {}", report.config.display());
    }
    Ok(())
}

fn emit_version(json_output: bool) {
    let report = VersionReport {
        name: "nemesis",
        version: NEMESIS_VERSION,
        implemented: &[
            "configuration",
            "domain models",
            "integration detection",
            "Pi RPC reviewer runtime",
            "independent finding verification",
            "evidence integrity validation",
            "review event and result observability",
        ],
        planned: &[
            "deterministic gate engine",
            "Spec Kit adapter",
            "OpenCode adapter",
            "Codex adapter",
        ],
    };
    if json_output {
        let _ = print_json(&report);
    } else {
        println!("{} {}", report.name, report.version);
        println!("M0 foundation and experimental OpenSpec + Pi slice");
    }
}

fn emit_doctor(root: &Path, json_output: bool) -> Result<()> {
    let paths = ProjectPaths::new(root);
    let mut diagnostics = Vec::new();
    if paths.config_file().is_file() {
        match NemesisConfig::load(&paths) {
            Ok(_) => diagnostics.push(Diagnostic {
                severity: "ok",
                code: "config_valid",
                message: "NEMESIS configuration is valid.".to_owned(),
                fix: None,
            }),
            Err(error) => diagnostics.push(Diagnostic {
                severity: "error",
                code: "config_invalid",
                message: error.to_string(),
                fix: Some(
                    "repair or remove .nemesis/config.toml, then run `nemesis init`".to_owned(),
                ),
            }),
        }
        if paths.nemesis_dir().is_dir() {
            let probe = paths
                .nemesis_dir()
                .join(format!(".doctor-write-test-{}", std::process::id()));
            match fs::write(&probe, b"write test") {
                Ok(()) => {
                    let _ = fs::remove_file(&probe);
                    diagnostics.push(Diagnostic {
                        severity: "ok",
                        code: "runtime_writable",
                        message: ".nemesis is writable.".to_owned(),
                        fix: None,
                    });
                }
                Err(error) => diagnostics.push(Diagnostic {
                    severity: "error",
                    code: "runtime_not_writable",
                    message: format!("cannot write NEMESIS runtime state: {error}"),
                    fix: Some("check ownership and permissions for .nemesis/".to_owned()),
                }),
            }
        }
    } else {
        diagnostics.push(Diagnostic {
            severity: "warning",
            code: "not_initialized",
            message: "NEMESIS is not initialized in this project.".to_owned(),
            fix: Some("run `nemesis init`".to_owned()),
        });
    }

    for (command, label) in [("git", "Git"), ("cargo", "Cargo")] {
        if command_available(command) {
            diagnostics.push(Diagnostic {
                severity: "ok",
                code: "tool_available",
                message: format!("{label} is available on PATH."),
                fix: None,
            });
        } else {
            diagnostics.push(Diagnostic {
                severity: "warning",
                code: "tool_missing",
                message: format!("{label} is not available on PATH."),
                fix: Some(format!("install {label} or adjust PATH")),
            });
        }
    }

    let agents = list_agent_integrations(root)?;
    for agent in &agents {
        diagnostics.push(Diagnostic {
            severity: if agent.installed { "ok" } else { "info" },
            code: "agent_detection",
            message: format!(
                "{}: {}",
                agent.display_name,
                if agent.installed {
                    "detected"
                } else {
                    "not detected"
                }
            ),
            fix: (!agent.installed)
                .then(|| format!("install {} if this integration is required", agent.id)),
        });
    }
    let sdd = list_sdd_integrations(root)?;
    for framework in &sdd {
        diagnostics.push(Diagnostic {
            severity: if framework.detected { "ok" } else { "info" },
            code: "sdd_detection",
            message: format!(
                "{}: {}",
                framework.display_name,
                if framework.detected {
                    "detected"
                } else {
                    "not detected"
                }
            ),
            fix: None,
        });
    }

    let report = DoctorReport {
        root: root.to_path_buf(),
        diagnostics,
    };
    if json_output {
        print_json(&report)?;
    } else {
        println!("NEMESIS doctor: {}", root.display());
        for diagnostic in &report.diagnostics {
            let suffix = diagnostic
                .fix
                .as_deref()
                .map(|fix| format!(" Fix: {fix}"))
                .unwrap_or_default();
            println!(
                "[{}] {} — {}{}",
                diagnostic.severity, diagnostic.code, diagnostic.message, suffix
            );
        }
    }
    Ok(())
}

fn handle_agent_integration(
    root: &Path,
    command: IntegrationCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        IntegrationCommand::List => {
            let statuses = list_agent_integrations(root)?;
            if json_output {
                print_json(&statuses)?;
            } else {
                println!("Agent integrations:");
                for status in statuses {
                    let state = if status.installed {
                        "detected"
                    } else {
                        "not detected"
                    };
                    let version = status
                        .version
                        .unwrap_or_else(|| "unknown version".to_owned());
                    let files = if status.project_files.is_empty() {
                        String::new()
                    } else {
                        format!("; managed: {}", status.project_files.join(", "))
                    };
                    println!("- {} ({state}, {version}){files}", status.id);
                }
            }
        }
        IntegrationCommand::Install { id } => {
            let record = install_agent(root, &id)?;
            emit_installation(record, json_output);
        }
        IntegrationCommand::Remove { id } => {
            remove_agent(root, &id)?;
            emit_message(
                &format!("Removed NEMESIS-managed {id} integration files."),
                json_output,
            )?;
        }
    }
    Ok(())
}

fn handle_sdd_integration(
    root: &Path,
    command: IntegrationCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        IntegrationCommand::List => {
            let statuses = list_sdd_integrations(root)?;
            if json_output {
                print_json(&statuses)?;
            } else {
                println!("SDD integrations:");
                for status in statuses {
                    let state = if status.detected {
                        "detected"
                    } else {
                        "not detected"
                    };
                    let version = status.version.unwrap_or_else(|| "CLI not found".to_owned());
                    println!("- {} ({state}, {version})", status.id);
                    for detail in status.details {
                        println!("  {detail}");
                    }
                }
            }
        }
        IntegrationCommand::Install { id } => {
            let record = install_sdd(root, &id)?;
            emit_installation(record, json_output);
        }
        IntegrationCommand::Remove { id } => {
            remove_sdd(root, &id)?;
            emit_message(
                &format!("Removed NEMESIS-managed {id} adapter registration."),
                json_output,
            )?;
        }
    }
    Ok(())
}

fn handle_finding(root: &Path, command: FindingCommand, json_output: bool) -> Result<()> {
    match command {
        FindingCommand::List => {
            let views = finding_views(root)?;
            if json_output {
                print_json(&views)?;
            } else if views.is_empty() {
                println!("no persisted findings");
            } else {
                for view in views {
                    println!(
                        "{}:{} [{}] {}",
                        view.review_run_id,
                        view.finding.id,
                        lifecycle_verification_label(&view.verifications),
                        view.finding.claim
                    );
                }
            }
        }
        FindingCommand::Show {
            review_run_id,
            finding_id,
        } => {
            let view = finding_view(root, review_run_id, &finding_id)?;
            if json_output {
                print_json(&view)?;
            } else {
                println!("review run: {}", view.review_run_id);
                println!("finding: {}", view.finding.id);
                println!("claim: {}", view.finding.claim);
                println!("category: {}", view.finding.category);
                println!("verifications: {}", view.verifications.len());
                for verification in view.verifications {
                    println!(
                        "  {}: {:?}",
                        verification.verification_run_id, verification.verdict
                    );
                }
            }
        }
        FindingCommand::Verify {
            review_run_id,
            finding_id,
            verifier,
            timeout_seconds,
        } => queue_verification(
            root,
            review_run_id,
            &finding_id,
            &verifier,
            timeout_seconds,
            json_output,
        )?,
    }
    Ok(())
}

fn finding_views(root: &Path) -> Result<Vec<FindingView>> {
    let paths = ProjectPaths::new(root);
    let store = RunStore::new(paths.clone());
    let mut views = Vec::new();
    for run in list_runs(&paths)? {
        if run.kind != RunKind::Review || run.final_result_ref.is_none() {
            continue;
        }
        let Ok(result) = store.load_result::<ReviewResult>(run.run_id) else {
            continue;
        };
        for finding in result.findings {
            views.push(FindingView {
                review_run_id: run.run_id,
                verifications: verification_records(root, run.run_id, &finding.id)?,
                finding,
            });
        }
    }
    Ok(views)
}

fn finding_view(root: &Path, review_run_id: Uuid, finding_id: &str) -> Result<FindingView> {
    finding_views(root)?
        .into_iter()
        .find(|view| view.review_run_id == review_run_id && view.finding.id == finding_id)
        .ok_or_else(|| {
            CliError::Input(format!(
                "finding {finding_id} was not found in review run {review_run_id}"
            ))
        })
}

fn revalidate_verification(root: &Path, mut result: VerificationResult) -> VerificationResult {
    result.record.stale_evidence_refs.clear();
    for evidence in &mut result.evidence {
        let _ = validate_evidence(root, evidence, "verification-consumer");
        if evidence.validation.state != nemesis_core::model::ValidationState::Valid {
            result.record.stale_evidence_refs.push(evidence.id);
        }
    }
    if !result.record.stale_evidence_refs.is_empty()
        && matches!(
            result.record.verdict,
            nemesis_core::model::VerificationState::Confirmed
                | nemesis_core::model::VerificationState::Rejected
        )
    {
        result.record.verdict = nemesis_core::model::VerificationState::Inconclusive;
        let message =
            "Verification evidence is stale or invalid and must be revalidated before relying on the verdict."
                .to_owned();
        if !result.record.uncertainty.contains(&message) {
            result.record.uncertainty.push(message);
        }
    }
    result
}

fn verification_records(
    root: &Path,
    source_review_run_id: Uuid,
    finding_id: &str,
) -> Result<Vec<VerificationRecord>> {
    let paths = ProjectPaths::new(root);
    let store = RunStore::new(paths.clone());
    let mut records = Vec::new();
    for run in list_runs(&paths)? {
        if run.kind != RunKind::Verification || run.parent_run_id != Some(source_review_run_id) {
            continue;
        }
        let Ok(result) = store.load_result::<VerificationResult>(run.run_id) else {
            continue;
        };
        if !result.valid {
            continue;
        }
        let result = revalidate_verification(root, result);
        if result.record.finding_id == finding_id {
            records.push(result.record);
        }
    }
    records.sort_by_key(|record| record.verified_at);
    Ok(records)
}

fn lifecycle_verification_label(records: &[VerificationRecord]) -> &'static str {
    records
        .last()
        .map(|record| match record.verdict {
            nemesis_core::model::VerificationState::Confirmed => "confirmed",
            nemesis_core::model::VerificationState::Rejected => "rejected",
            nemesis_core::model::VerificationState::Inconclusive => "inconclusive",
            nemesis_core::model::VerificationState::Unverified => "unverified",
        })
        .unwrap_or("unverified")
}

fn queue_verification(
    root: &Path,
    source_review_run_id: Uuid,
    finding_id: &str,
    verifier_identity: &str,
    timeout_seconds: u64,
    json_output: bool,
) -> Result<()> {
    if timeout_seconds == 0 {
        return Err(CliError::Input(
            "--timeout-seconds must be greater than zero".to_owned(),
        ));
    }
    let paths = ProjectPaths::new(root);
    let config = NemesisConfig::load(&paths)?;
    let store = RunStore::new(paths.clone());
    let source_run = store.load_run(source_review_run_id)?;
    if source_run.kind != RunKind::Review {
        return Err(CliError::Input(
            "finding verification requires an originating reviewer run".to_owned(),
        ));
    }
    let result: ReviewResult = store
        .load_result(source_review_run_id)
        .map_err(|_| CliError::Input("the reviewer result is unavailable or invalid".to_owned()))?;
    if !result.valid {
        return Err(CliError::Input(
            "the reviewer result is invalid and cannot be verified".to_owned(),
        ));
    }
    let finding = result
        .findings
        .iter()
        .find(|finding| finding.id == finding_id)
        .cloned()
        .ok_or_else(|| {
            CliError::Input(format!(
                "finding {finding_id} was not found in review run {source_review_run_id}"
            ))
        })?;
    let candidate_evidence = result
        .evidence
        .iter()
        .filter(|evidence| finding.evidence_refs.contains(&evidence.id))
        .cloned()
        .collect::<Vec<_>>();
    let runtime = PiRuntime;
    if !runtime.detect().installed {
        return Err(CliError::Agent(nemesis_agent::AgentError::Unavailable(
            "Pi is not installed or not on PATH".to_owned(),
        )));
    }

    let verification_run_id = Uuid::new_v4();
    let run = ReviewRun {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        kind: RunKind::Verification,
        run_id: verification_run_id,
        parent_run_id: Some(source_review_run_id),
        agent_runtime: "pi".to_owned(),
        reviewer_profile: verifier_identity.to_owned(),
        lifecycle_status: LifecycleStatus::Queued,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        final_result_ref: None,
        error: None,
    };
    store.ensure(verification_run_id)?;
    store.save_run(&run)?;
    let mut request = ReviewRequest::for_verification(
        verification_run_id,
        root,
        source_review_run_id,
        finding,
        candidate_evidence,
        verifier_identity,
        timeout_seconds.min(config.runtime.reviewer_timeout_seconds),
    );
    request.termination_grace_seconds = config.runtime.termination_grace_seconds;
    request.repository_revision = git_revision(root);
    store.save_request(verification_run_id, &request)?;
    store.append_event(&ReviewerEvent {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        event_id: Uuid::new_v4(),
        run_id: verification_run_id,
        sequence: 1,
        timestamp: Utc::now(),
        event_type: ReviewerEventType::RunQueued,
        phase: ExecutionPhase::Queued,
        payload: json!({
            "agent_runtime": "pi",
            "kind": "verification",
            "source_review_run_id": source_review_run_id,
            "finding_id": finding_id,
        }),
    })?;
    let executable = std::env::current_exe().map_err(|source| CliError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    if let Err(error) = spawn_worker_process(&executable, root, verification_run_id) {
        let mut failed = run;
        failed.lifecycle_status = LifecycleStatus::Failed;
        failed.completed_at = Some(Utc::now());
        failed.error = Some(error.to_string());
        store.save_run(&failed)?;
        return Err(error.into());
    }
    if json_output {
        print_json(&run)?;
    } else {
        println!("queued verification run {verification_run_id}");
        println!("status: nemesis review status {verification_run_id}");
    }
    Ok(())
}

fn git_revision(root: &Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .args(["-C", root.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!revision.is_empty()).then_some(revision)
}

fn handle_review(root: &Path, command: ReviewCommand, json_output: bool) -> Result<()> {
    match command {
        ReviewCommand::Run {
            prompt,
            reviewer_profile,
            timeout_seconds,
            agent,
            parent_run_id,
        } => queue_review(
            root,
            &prompt,
            &reviewer_profile,
            timeout_seconds,
            &agent,
            parent_run_id,
            json_output,
        ),
        ReviewCommand::Status { run_id } => review_status(root, run_id, json_output),
        ReviewCommand::Cancel { run_id } => cancel_review(root, run_id, json_output),
        ReviewCommand::Events { run_id, tail } => review_events(root, run_id, tail, json_output),
        ReviewCommand::Wait {
            run_id,
            timeout_seconds,
        } => review_wait(root, run_id, timeout_seconds, json_output),
        ReviewCommand::Result { run_id } => review_result(root, run_id, json_output),
        ReviewCommand::Worker { run_id, root } => {
            nemesis_agent::run_worker(&root, run_id)?;
            Ok(())
        }
    }
}

fn queue_review(
    root: &Path,
    prompt: &str,
    reviewer_profile: &str,
    timeout_seconds: u64,
    agent: &str,
    parent_run_id: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    if prompt.trim().is_empty() {
        return Err(CliError::Input("--prompt must not be empty".to_owned()));
    }
    if timeout_seconds == 0 {
        return Err(CliError::Input(
            "--timeout-seconds must be greater than zero".to_owned(),
        ));
    }
    if agent != "pi" {
        return Err(CliError::Input(format!(
            "agent runtime `{agent}` is not implemented; use `--agent pi`"
        )));
    }
    let paths = ProjectPaths::new(root);
    let config = NemesisConfig::load(&paths)?;
    let runtime = PiRuntime;
    let capabilities = runtime.detect();
    if !capabilities.installed {
        return Err(CliError::Agent(nemesis_agent::AgentError::Unavailable(
            "Pi is not installed or not on PATH".to_owned(),
        )));
    }
    let run_id = Uuid::new_v4();
    let run = ReviewRun {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        kind: RunKind::Review,
        run_id,
        parent_run_id,
        agent_runtime: agent.to_owned(),
        reviewer_profile: reviewer_profile.to_owned(),
        lifecycle_status: LifecycleStatus::Queued,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        final_result_ref: None,
        error: None,
    };
    let store = RunStore::new(paths);
    store.ensure(run_id)?;
    store.save_run(&run)?;
    let mut request = ReviewRequest::new(
        run_id,
        root,
        augmented_prompt(root, prompt)?,
        reviewer_profile,
        timeout_seconds.min(config.runtime.reviewer_timeout_seconds),
    );
    request.termination_grace_seconds = config.runtime.termination_grace_seconds;
    store.save_request(run_id, &request)?;
    store.append_event(&ReviewerEvent {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        event_id: Uuid::new_v4(),
        run_id,
        sequence: 1,
        timestamp: Utc::now(),
        event_type: ReviewerEventType::RunQueued,
        phase: ExecutionPhase::Queued,
        payload: json!({"agent_runtime": agent, "reviewer_profile": reviewer_profile, "kind": "review"}),
    })?;
    let executable = std::env::current_exe().map_err(|source| CliError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    if let Err(error) = spawn_worker_process(&executable, root, run_id) {
        let mut failed = run;
        failed.lifecycle_status = LifecycleStatus::Failed;
        failed.completed_at = Some(Utc::now());
        failed.error = Some(error.to_string());
        store.save_run(&failed)?;
        return Err(error.into());
    }
    if json_output {
        print_json(&run)?;
    } else {
        println!("queued review run {run_id}");
        println!("status: nemesis review status {run_id}");
    }
    Ok(())
}

fn augmented_prompt(root: &Path, prompt: &str) -> Result<String> {
    let Some(inspection) = inspect_openspec(root)? else {
        return Ok(prompt.to_owned());
    };
    let schema = inspection
        .active_schema
        .as_ref()
        .map(|schema| schema.name.as_str())
        .unwrap_or("unknown");
    let changes = inspection
        .changes
        .iter()
        .map(|change| change.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "{prompt}\n\nNEMESIS read-only OpenSpec context: active schema={schema}; active changes=[{changes}]. Inspect the actual artifacts before making claims."
    ))
}

fn refreshed_result_value(root: &Path, run: &ReviewRun) -> Result<serde_json::Value> {
    let store = RunStore::new(ProjectPaths::new(root));
    match run.kind {
        RunKind::Verification => {
            let result: VerificationResult = store.load_result(run.run_id)?;
            let value = serde_json::to_value(revalidate_verification(root, result))
                .map_err(nemesis_core::CoreError::from)?;
            Ok(value)
        }
        RunKind::Review => Ok(store.load_result(run.run_id)?),
    }
}

fn review_status(root: &Path, run_id: Option<Uuid>, json_output: bool) -> Result<()> {
    let paths = ProjectPaths::new(root);
    let store = RunStore::new(paths.clone());
    let runs = if let Some(run_id) = run_id {
        vec![store.load_run(run_id)?]
    } else {
        list_runs(&paths)?
    };
    let statuses = runs
        .into_iter()
        .map(|run| {
            let result = run
                .final_result_ref
                .and_then(|_| refreshed_result_value(root, &run).ok());
            ReviewStatus { run, result }
        })
        .collect::<Vec<_>>();
    if json_output {
        print_json(&statuses)?;
    } else {
        if statuses.is_empty() {
            println!("no persisted review runs");
        }
        for status in statuses {
            println!(
                "{} {} ({})",
                status.run.run_id,
                status.run.agent_runtime,
                status.run.lifecycle_status.to_string()
            );
            if let Some(error) = status.run.error {
                println!("  error: {error}");
            }
            if status.result.is_some() {
                println!("  result: available");
            }
        }
    }
    Ok(())
}

fn review_events(root: &Path, run_id: Uuid, tail: usize, json_output: bool) -> Result<()> {
    let store = RunStore::new(ProjectPaths::new(root));
    let events = store.load_events(run_id)?;
    let start = events.len().saturating_sub(tail);
    let selected = &events[start..];
    if json_output {
        print_json(&selected)?;
    } else {
        for event in selected {
            println!(
                "{} {} {:?} {:?} {}",
                event.sequence,
                event.timestamp,
                event.event_type,
                event.phase,
                serde_json::to_string(&event.payload).unwrap_or_else(|_| "{}".to_owned())
            );
        }
    }
    Ok(())
}

fn review_result(root: &Path, run_id: Uuid, json_output: bool) -> Result<()> {
    let store = RunStore::new(ProjectPaths::new(root));
    if !store.result_path(run_id).is_file() {
        return Err(CliError::Input(format!(
            "no persisted result is available for run {run_id}"
        )));
    }
    let run = store.load_run(run_id)?;
    let result = refreshed_result_value(root, &run)?;
    if json_output {
        print_json(&result)?;
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(nemesis_core::CoreError::from)?
        );
    }
    Ok(())
}

fn review_wait(root: &Path, run_id: Uuid, timeout_seconds: u64, json_output: bool) -> Result<()> {
    if timeout_seconds == 0 {
        return Err(CliError::Input(
            "--timeout-seconds must be greater than zero".to_owned(),
        ));
    }
    let store = RunStore::new(ProjectPaths::new(root));
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        let run = store.load_run(run_id)?;
        if run.lifecycle_status.is_terminal() {
            let report = WaitReport {
                run,
                completed: true,
            };
            if json_output {
                print_json(&report)?;
            } else {
                println!(
                    "{} {}",
                    report.run.run_id,
                    lifecycle_name(&report.run.lifecycle_status)
                );
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            let report = WaitReport {
                run,
                completed: false,
            };
            if json_output {
                print_json(&report)?;
            } else {
                println!(
                    "{} still running after {} seconds",
                    report.run.run_id, timeout_seconds
                );
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn cancel_review(root: &Path, run_id: Uuid, json_output: bool) -> Result<()> {
    let paths = ProjectPaths::new(root);
    let store = RunStore::new(paths);
    let run = store.load_run(run_id)?;
    if !run.lifecycle_status.is_terminal() {
        store.request_cancel(run_id)?;
    }
    if json_output {
        print_json(&json!({"run_id": run_id, "requested": !run.lifecycle_status.is_terminal()}))?;
    } else if run.lifecycle_status.is_terminal() {
        println!(
            "run {run_id} is already {}",
            lifecycle_name(&run.lifecycle_status)
        );
    } else {
        println!("cancellation requested for {run_id}");
    }
    Ok(())
}

fn list_runs(paths: &ProjectPaths) -> Result<Vec<ReviewRun>> {
    if !paths.runs_dir().is_dir() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    let entries = fs::read_dir(paths.runs_dir()).map_err(|source| CliError::Io {
        path: paths.runs_dir(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CliError::Io {
            path: paths.runs_dir(),
            source,
        })?;
        let Some(run_id) = entry
            .file_name()
            .to_str()
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            continue;
        };
        if let Ok(run) = RunStore::new(paths.clone()).load_run(run_id) {
            runs.push(run);
        }
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    Ok(runs)
}

fn lifecycle_name(status: &LifecycleStatus) -> &'static str {
    match status {
        LifecycleStatus::Queued => "queued",
        LifecycleStatus::Starting => "starting",
        LifecycleStatus::Running => "running",
        LifecycleStatus::Verifying => "verifying",
        LifecycleStatus::Completed => "completed",
        LifecycleStatus::Failed => "failed",
        LifecycleStatus::Cancelled => "cancelled",
        LifecycleStatus::TimedOut => "timed out",
    }
}

fn command_available(command: &str) -> bool {
    ProcessCommand::new(command)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn emit_installation(record: InstalledIntegration, json_output: bool) {
    if json_output {
        let _ = print_json(&record);
    } else {
        println!("installed {}", record.id);
        println!("strategy: {}", record.strategy);
        if record.managed_files.is_empty() {
            println!("managed files: none; adapter registration is read-only");
        } else {
            println!("managed files: {}", record.managed_files.join(", "));
        }
    }
}

fn emit_message(message: &str, json_output: bool) -> Result<()> {
    if json_output {
        print_json(&json!({"message": message}))?;
    } else {
        println!("{message}");
    }
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let output = serde_json::to_string_pretty(value).map_err(nemesis_core::CoreError::from)?;
    println!("{output}");
    Ok(())
}

trait LifecycleStatusDisplay {
    fn to_string(&self) -> String;
}

impl LifecycleStatusDisplay for LifecycleStatus {
    fn to_string(&self) -> String {
        lifecycle_name(self).to_owned()
    }
}
