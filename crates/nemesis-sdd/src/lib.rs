//! Adapters for discovering and inspecting existing SDD frameworks.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SddError>;

#[derive(Debug, Error)]
pub enum SddError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("YAML error at {path}: {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid SDD data: {0}")]
    InvalidData(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SddDetection {
    pub id: String,
    pub display_name: String,
    pub detected: bool,
    pub root: Option<PathBuf>,
    pub version: Option<String>,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SddInspection {
    pub id: String,
    pub root: PathBuf,
    pub version: Option<String>,
    pub active_schema: Option<SchemaSummary>,
    pub changes: Vec<ChangeSummary>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaSummary {
    pub name: String,
    pub version: Option<String>,
    pub custom: bool,
    pub source: PathBuf,
    pub artifacts: Vec<ArtifactDefinition>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactDefinition {
    pub id: String,
    pub generates: Option<String>,
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    pub name: String,
    pub path: PathBuf,
    pub artifact_files: Vec<PathBuf>,
}

pub trait SddIntegration {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn detect(&self, project_root: &Path) -> Result<SddDetection>;
    fn inspect(&self, project_root: &Path) -> Result<Option<SddInspection>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenSpecIntegration;

impl SddIntegration for OpenSpecIntegration {
    fn id(&self) -> &'static str {
        "openspec"
    }

    fn display_name(&self) -> &'static str {
        "OpenSpec"
    }

    fn detect(&self, project_root: &Path) -> Result<SddDetection> {
        let root = project_root.join("openspec");
        let detected = root.is_dir();
        let version = command_version("openspec");
        let mut details = Vec::new();
        if detected {
            details.push(format!("found {}", root.display()));
            if root.join("config.yaml").is_file() {
                details.push("project config found at openspec/config.yaml".to_owned());
            }
            if root.join("schemas").is_dir() {
                details.push("project custom schemas directory found".to_owned());
            }
        } else {
            details.push("openspec/ directory not found".to_owned());
        }
        if version.is_none() {
            details.push("openspec CLI not found on PATH".to_owned());
        }
        Ok(SddDetection {
            id: self.id().to_owned(),
            display_name: self.display_name().to_owned(),
            detected,
            root: detected.then_some(root),
            version,
            details,
        })
    }

    fn inspect(&self, project_root: &Path) -> Result<Option<SddInspection>> {
        let detection = self.detect(project_root)?;
        let Some(root) = detection.root else {
            return Ok(None);
        };
        let schema_name =
            configured_schema_name(&root)?.unwrap_or_else(|| "spec-driven".to_owned());
        let active_schema = load_schema(&root, &schema_name)?;
        let changes = load_changes(&root)?;
        Ok(Some(SddInspection {
            id: self.id().to_owned(),
            root,
            version: detection.version,
            active_schema,
            changes,
            notes: vec![
                "Inspection is read-only; existing OpenSpec artifacts and schemas are not modified."
                    .to_owned(),
                "Schema dependencies are treated as artifact relationships, not a replacement workflow."
                    .to_owned(),
            ],
        }))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SpecKitIntegration;

impl SddIntegration for SpecKitIntegration {
    fn id(&self) -> &'static str {
        "spec-kit"
    }

    fn display_name(&self) -> &'static str {
        "GitHub Spec Kit"
    }

    fn detect(&self, project_root: &Path) -> Result<SddDetection> {
        let specify_dir = project_root.join(".specify");
        let specs_dir = project_root.join("specs");
        let detected = specify_dir.is_dir() || specs_dir.is_dir();
        let mut details = Vec::new();
        if specify_dir.is_dir() {
            details.push("found .specify/".to_owned());
        }
        if specs_dir.is_dir() {
            details.push("found specs/".to_owned());
        }
        if !detected {
            details.push(".specify/ and specs/ were not found".to_owned());
        }
        let version = command_version("specify");
        if version.is_none() {
            details.push("specify CLI not found on PATH".to_owned());
        }
        Ok(SddDetection {
            id: self.id().to_owned(),
            display_name: self.display_name().to_owned(),
            detected,
            root: detected.then_some(project_root.to_path_buf()),
            version,
            details,
        })
    }

    fn inspect(&self, project_root: &Path) -> Result<Option<SddInspection>> {
        let detection = self.detect(project_root)?;
        if !detection.detected {
            return Ok(None);
        }
        Ok(Some(SddInspection {
            id: self.id().to_owned(),
            root: project_root.to_path_buf(),
            version: detection.version,
            active_schema: None,
            changes: Vec::new(),
            notes: vec![
                "Spec Kit inspection is intentionally limited in M1; no Spec Kit artifacts were modified."
                    .to_owned(),
            ],
        }))
    }
}

fn configured_schema_name(root: &Path) -> Result<Option<String>> {
    let path = root.join("config.yaml");
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|source| SddError::Io {
        path: path.clone(),
        source,
    })?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|source| SddError::Yaml {
            path: path.clone(),
            source,
        })?;
    Ok(value
        .get("schema")
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned))
}

fn load_schema(root: &Path, schema_name: &str) -> Result<Option<SchemaSummary>> {
    let custom_path = root.join("schemas").join(schema_name).join("schema.yaml");
    if custom_path.is_file() {
        let schema = parse_schema_file(&custom_path, true)?;
        return Ok(Some(schema));
    }
    if schema_name == "spec-driven" {
        return Ok(Some(SchemaSummary {
            name: "spec-driven".to_owned(),
            version: None,
            custom: false,
            source: PathBuf::from("OpenSpec built-in schema: spec-driven"),
            artifacts: vec![
                ArtifactDefinition {
                    id: "proposal".to_owned(),
                    generates: Some("proposal.md".to_owned()),
                    requires: Vec::new(),
                },
                ArtifactDefinition {
                    id: "specs".to_owned(),
                    generates: Some("specs/**/*.md".to_owned()),
                    requires: vec!["proposal".to_owned()],
                },
                ArtifactDefinition {
                    id: "design".to_owned(),
                    generates: Some("design.md".to_owned()),
                    requires: vec!["proposal".to_owned()],
                },
                ArtifactDefinition {
                    id: "tasks".to_owned(),
                    generates: Some("tasks.md".to_owned()),
                    requires: vec!["specs".to_owned(), "design".to_owned()],
                },
            ],
        }));
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct SchemaFile {
    name: Option<String>,
    version: Option<serde_yaml::Value>,
    artifacts: Option<Vec<SchemaArtifact>>,
}

#[derive(Debug, Deserialize)]
struct SchemaArtifact {
    id: String,
    generates: Option<String>,
    requires: Option<Vec<String>>,
}

fn parse_schema_file(path: &Path, custom: bool) -> Result<SchemaSummary> {
    let content = std::fs::read_to_string(path).map_err(|source| SddError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file: SchemaFile = serde_yaml::from_str(&content).map_err(|source| SddError::Yaml {
        path: path.to_path_buf(),
        source,
    })?;
    let name = file.name.unwrap_or_else(|| {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_owned()
    });
    let version = file.version.map(|value| match value {
        serde_yaml::Value::String(value) => value,
        other => format!("{other:?}"),
    });
    let artifacts = file
        .artifacts
        .unwrap_or_default()
        .into_iter()
        .map(|artifact| ArtifactDefinition {
            id: artifact.id,
            generates: artifact.generates,
            requires: artifact.requires.unwrap_or_default(),
        })
        .collect();
    Ok(SchemaSummary {
        name,
        version,
        custom,
        source: path.to_path_buf(),
        artifacts,
    })
}

fn load_changes(root: &Path) -> Result<Vec<ChangeSummary>> {
    let changes_root = root.join("changes");
    if !changes_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut changes = Vec::new();
    let entries = std::fs::read_dir(&changes_root).map_err(|source| SddError::Io {
        path: changes_root.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SddError::Io {
            path: changes_root.clone(),
            source,
        })?;
        let path = entry.path();
        if !path.is_dir() || path.file_name().and_then(|name| name.to_str()) == Some("archive") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                SddError::InvalidData(format!("invalid change path {}", path.display()))
            })?
            .to_owned();
        let mut artifact_files = Vec::new();
        collect_files(&path, &mut artifact_files)?;
        changes.push(ChangeSummary {
            name,
            path,
            artifact_files,
        });
    }
    changes.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(changes)
}

fn collect_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(root).map_err(|source| SddError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SddError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, output)?;
        } else if path.is_file() {
            output.push(path);
        }
    }
    output.sort();
    Ok(())
}

fn command_version(command: &str) -> Option<String> {
    let output = Command::new(command).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!stdout.is_empty()).then_some(stdout)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn openspec_inspection_reads_custom_schema_without_writing() {
        let directory = tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("openspec/schemas/review-first"))
            .expect("create schema directory");
        fs::create_dir_all(root.join("openspec/changes/example/specs/auth"))
            .expect("create change directory");
        fs::write(root.join("openspec/config.yaml"), "schema: review-first\n")
            .expect("write config");
        fs::write(
            root.join("openspec/schemas/review-first/schema.yaml"),
            "name: review-first\nversion: 1\nartifacts:\n  - id: proposal\n    generates: proposal.md\n    requires: []\n  - id: review\n    generates: review.md\n    requires: [proposal]\n",
        )
        .expect("write schema");
        fs::write(
            root.join("openspec/changes/example/proposal.md"),
            "# Proposal\n",
        )
        .expect("write proposal");

        let inspection = OpenSpecIntegration
            .inspect(root)
            .expect("inspect OpenSpec")
            .expect("OpenSpec detected");
        let schema = inspection.active_schema.expect("schema");
        assert!(schema.custom);
        assert_eq!(schema.artifacts[1].requires, vec!["proposal"]);
        assert_eq!(inspection.changes.len(), 1);
        assert!(root
            .join("openspec/schemas/review-first/schema.yaml")
            .is_file());
    }

    #[test]
    fn missing_framework_is_not_detected() {
        let directory = tempdir().expect("temporary directory");
        let detection = OpenSpecIntegration
            .detect(directory.path())
            .expect("detect");
        assert!(!detection.detected);
    }
}
