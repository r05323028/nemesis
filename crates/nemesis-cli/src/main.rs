use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use chrono::Utc;
use clap::{CommandFactory, Parser, Subcommand};
use nemesis_agent::{spawn_worker_process, PiRuntime, ReviewRequest};
use nemesis_core::config::{discover_project_root, NemesisConfig, ProjectPaths};
use nemesis_core::model::{
    ExecutionPhase, LifecycleStatus, ReviewRun, ReviewerEvent, ReviewerEventType,
    MODEL_SCHEMA_VERSION,
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
    /// Internal worker entry point used by the process-backed runtime.
    #[command(hide = true)]
    Worker {
        run_id: Uuid,
        #[arg(long)]
        root: PathBuf,
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
        ],
        planned: &[
            "independent finding verifier",
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
    let request = ReviewRequest::new(
        run_id,
        root,
        augmented_prompt(root, prompt)?,
        reviewer_profile,
        timeout_seconds.min(config.runtime.reviewer_timeout_seconds),
    );
    store.save_request(run_id, &request)?;
    store.append_event(&ReviewerEvent {
        schema_version: MODEL_SCHEMA_VERSION.to_owned(),
        event_id: Uuid::new_v4(),
        run_id,
        sequence: 1,
        timestamp: Utc::now(),
        event_type: ReviewerEventType::RunQueued,
        phase: ExecutionPhase::Queued,
        payload: json!({"agent_runtime": agent, "reviewer_profile": reviewer_profile}),
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
            let result = run.final_result_ref.and_then(|_| {
                store
                    .read_json::<serde_json::Value>(&store.result_path(run.run_id))
                    .ok()
            });
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
