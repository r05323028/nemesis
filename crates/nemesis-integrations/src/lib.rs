//! Safe, project-local installation and capability detection for integrations.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use nemesis_agent::{AgentCapabilities, PiRuntime};
use nemesis_core::config::ProjectPaths;
use nemesis_sdd::{OpenSpecIntegration, SddIntegration, SpecKitIntegration};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const PI_EXTENSION: &str = include_str!("../../../integrations/agents/pi/nemesis-review.ts");
const PI_EXTENSION_ID: &str = "nemesis-review-extension@0.1.0";

pub type Result<T> = std::result::Result<T, IntegrationError>;

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("core error: {0}")]
    Core(#[from] nemesis_core::CoreError),
    #[error("agent error: {0}")]
    Agent(#[from] nemesis_agent::AgentError),
    #[error("SDD error: {0}")]
    Sdd(#[from] nemesis_sdd::SddError),
    #[error("integration `{id}` is not supported yet: {reason}")]
    Unsupported { id: String, reason: String },
    #[error("integration conflict at {path}: {reason}")]
    Conflict { path: PathBuf, reason: String },
    #[error("integration `{id}` was not detected: {reason}")]
    NotDetected { id: String, reason: String },
    #[error("NEMESIS must be initialized first; run `nemesis init`")]
    NotInitialized,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationKind {
    Agent,
    Sdd,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledIntegration {
    pub id: String,
    pub kind: IntegrationKind,
    pub version: String,
    pub managed_files: Vec<String>,
    pub strategy: String,
    pub installed_at: chrono::DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentIntegrationStatus {
    pub id: String,
    pub display_name: String,
    pub installed: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub project_files: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SddIntegrationStatus {
    pub id: String,
    pub display_name: String,
    pub detected: bool,
    pub version: Option<String>,
    pub root: Option<PathBuf>,
    pub project_files: Vec<String>,
    pub details: Vec<String>,
}

pub fn list_agent_integrations(project_root: &Path) -> Result<Vec<AgentIntegrationStatus>> {
    let records = load_records(&ProjectPaths::new(project_root))?;
    let pi = PiRuntime;
    let capabilities = pi.detect();
    let mut statuses = vec![status_from_capabilities(
        capabilities,
        record_files(&records, "pi"),
    )];
    for (id, display_name) in [("opencode", "OpenCode"), ("codex", "OpenAI Codex")] {
        let executable = find_executable(id);
        let version = executable.as_ref().and_then(|path| command_version(path));
        statuses.push(AgentIntegrationStatus {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            installed: executable.is_some(),
            executable: executable.map(|path| path.display().to_string()),
            version,
            project_files: record_files(&records, id),
            notes: vec!["Detection only in M0; no upstream files are changed by list.".to_owned()],
        });
    }
    Ok(statuses)
}

pub fn list_sdd_integrations(project_root: &Path) -> Result<Vec<SddIntegrationStatus>> {
    let records = load_records(&ProjectPaths::new(project_root))?;
    let adapters: [&dyn SddIntegration; 2] = [&OpenSpecIntegration, &SpecKitIntegration];
    adapters
        .iter()
        .map(|adapter| {
            let detection = adapter.detect(project_root)?;
            Ok(SddIntegrationStatus {
                id: detection.id.clone(),
                display_name: detection.display_name,
                detected: detection.detected,
                version: detection.version,
                root: detection.root,
                project_files: record_files(&records, &detection.id),
                details: detection.details,
            })
        })
        .collect()
}

pub fn inspect_openspec(project_root: &Path) -> Result<Option<nemesis_sdd::SddInspection>> {
    Ok(OpenSpecIntegration.inspect(project_root)?)
}

pub fn install_agent(project_root: &Path, id: &str) -> Result<InstalledIntegration> {
    ensure_initialized(project_root)?;
    match id {
        "pi" => install_pi(project_root),
        "opencode" | "codex" => Err(IntegrationError::Unsupported {
            id: id.to_owned(),
            reason: "the native adapter is researched but not implemented in M1".to_owned(),
        }),
        other => Err(IntegrationError::Unsupported {
            id: other.to_owned(),
            reason: "unknown agent integration".to_owned(),
        }),
    }
}

pub fn remove_agent(project_root: &Path, id: &str) -> Result<()> {
    ensure_initialized(project_root)?;
    match id {
        "pi" => remove_pi(project_root),
        "opencode" | "codex" => Err(IntegrationError::Unsupported {
            id: id.to_owned(),
            reason: "the native adapter is researched but not implemented in M1".to_owned(),
        }),
        other => Err(IntegrationError::Unsupported {
            id: other.to_owned(),
            reason: "unknown agent integration".to_owned(),
        }),
    }
}

pub fn install_sdd(project_root: &Path, id: &str) -> Result<InstalledIntegration> {
    ensure_initialized(project_root)?;
    match id {
        "openspec" => {
            let detection = OpenSpecIntegration.detect(project_root)?;
            if !detection.detected {
                return Err(IntegrationError::NotDetected {
                    id: id.to_owned(),
                    reason: "create or open an OpenSpec project first".to_owned(),
                });
            }
            let record = InstalledIntegration {
                id: id.to_owned(),
                kind: IntegrationKind::Sdd,
                version: detection
                    .version
                    .unwrap_or_else(|| "detected-project".to_owned()),
                managed_files: Vec::new(),
                strategy: "read-only artifact adapter".to_owned(),
                installed_at: Utc::now(),
            };
            upsert_record(project_root, record.clone())?;
            Ok(record)
        }
        "spec-kit" => Err(IntegrationError::Unsupported {
            id: id.to_owned(),
            reason: "Spec Kit adapter is researched but not implemented in M1".to_owned(),
        }),
        other => Err(IntegrationError::Unsupported {
            id: other.to_owned(),
            reason: "unknown SDD integration".to_owned(),
        }),
    }
}

pub fn remove_sdd(project_root: &Path, id: &str) -> Result<()> {
    ensure_initialized(project_root)?;
    match id {
        "openspec" => remove_record(project_root, id),
        "spec-kit" => Err(IntegrationError::Unsupported {
            id: id.to_owned(),
            reason: "Spec Kit adapter is researched but not implemented in M1".to_owned(),
        }),
        other => Err(IntegrationError::Unsupported {
            id: other.to_owned(),
            reason: "unknown SDD integration".to_owned(),
        }),
    }
}

fn install_pi(project_root: &Path) -> Result<InstalledIntegration> {
    let target = project_root.join(".pi/extensions/nemesis-review.ts");
    if target.exists() {
        let existing = fs::read_to_string(&target).map_err(|source| IntegrationError::Io {
            path: target.clone(),
            source,
        })?;
        if existing != PI_EXTENSION {
            return Err(IntegrationError::Conflict {
                path: target,
                reason:
                    "the existing file is not the exact NEMESIS-managed extension; it was preserved"
                        .to_owned(),
            });
        }
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| IntegrationError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&target, PI_EXTENSION).map_err(|source| IntegrationError::Io {
            path: target.clone(),
            source,
        })?;
    }
    let relative = relative_path(project_root, &target);
    let record = InstalledIntegration {
        id: "pi".to_owned(),
        kind: IntegrationKind::Agent,
        version: PI_EXTENSION_ID.to_owned(),
        managed_files: vec![relative],
        strategy: "official Pi project extension directory".to_owned(),
        installed_at: Utc::now(),
    };
    upsert_record(project_root, record.clone())?;
    Ok(record)
}

fn remove_pi(project_root: &Path) -> Result<()> {
    let target = project_root.join(".pi/extensions/nemesis-review.ts");
    if target.exists() {
        let existing = fs::read_to_string(&target).map_err(|source| IntegrationError::Io {
            path: target.clone(),
            source,
        })?;
        if existing != PI_EXTENSION {
            return Err(IntegrationError::Conflict {
                path: target,
                reason: "the extension was changed after installation; it was preserved".to_owned(),
            });
        }
        fs::remove_file(&target).map_err(|source| IntegrationError::Io {
            path: target,
            source,
        })?;
    }
    remove_record(project_root, "pi")
}

fn ensure_initialized(project_root: &Path) -> Result<()> {
    if !ProjectPaths::new(project_root).config_file().is_file() {
        return Err(IntegrationError::NotInitialized);
    }
    Ok(())
}

fn status_from_capabilities(
    capabilities: AgentCapabilities,
    project_files: Vec<String>,
) -> AgentIntegrationStatus {
    AgentIntegrationStatus {
        id: capabilities.id,
        display_name: capabilities.display_name,
        installed: capabilities.installed,
        executable: capabilities.executable,
        version: capabilities.version,
        project_files,
        notes: capabilities.notes,
    }
}

fn load_records(paths: &ProjectPaths) -> Result<Vec<InstalledIntegration>> {
    if !paths.integrations_file().is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(paths.integrations_file()).map_err(|source| IntegrationError::Io {
            path: paths.integrations_file(),
            source,
        })?;
    Ok(serde_json::from_str(&content)?)
}

fn save_records(paths: &ProjectPaths, records: &[InstalledIntegration]) -> Result<()> {
    fs::create_dir_all(paths.nemesis_dir()).map_err(|source| IntegrationError::Io {
        path: paths.nemesis_dir(),
        source,
    })?;
    let content = serde_json::to_vec_pretty(records)?;
    fs::write(paths.integrations_file(), content).map_err(|source| IntegrationError::Io {
        path: paths.integrations_file(),
        source,
    })
}

fn upsert_record(project_root: &Path, record: InstalledIntegration) -> Result<()> {
    let paths = ProjectPaths::new(project_root);
    let mut records = load_records(&paths)?;
    records.retain(|existing| existing.id != record.id);
    records.push(record);
    records.sort_by(|left, right| left.id.cmp(&right.id));
    save_records(&paths, &records)
}

fn remove_record(project_root: &Path, id: &str) -> Result<()> {
    let paths = ProjectPaths::new(project_root);
    let mut records = load_records(&paths)?;
    records.retain(|record| record.id != id);
    save_records(&paths, &records)
}

fn record_files(records: &[InstalledIntegration], id: &str) -> Vec<String> {
    records
        .iter()
        .find(|record| record.id == id)
        .map(|record| record.managed_files.clone())
        .unwrap_or_default()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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

fn command_version(executable: &Path) -> Option<String> {
    let output = Command::new(executable).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!version.is_empty()).then_some(version)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn initialize(root: &Path) {
        fs::create_dir_all(root.join(".nemesis")).expect("create config directory");
        fs::write(
            root.join(".nemesis/config.toml"),
            "version = 1\n\n[project]\nname = \"test\"\n\n[runtime]\nreviewer_timeout_seconds = 30\nmax_child_depth = 1\n",
        )
        .expect("write config");
    }

    #[test]
    fn pi_install_is_idempotent_and_preserves_conflicts() {
        let directory = tempdir().expect("temporary directory");
        initialize(directory.path());
        let first = install_agent(directory.path(), "pi").expect("install pi");
        let second = install_agent(directory.path(), "pi").expect("repeat install pi");
        assert_eq!(first.managed_files, second.managed_files);
        assert!(directory
            .path()
            .join(".pi/extensions/nemesis-review.ts")
            .is_file());

        fs::write(
            directory.path().join(".pi/extensions/nemesis-review.ts"),
            "user-owned",
        )
        .expect("replace extension");
        let error = install_agent(directory.path(), "pi").expect_err("conflict expected");
        assert!(error.to_string().contains("preserved"));
    }
}
