use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_error, CoreError, Result};

pub const CONFIG_VERSION: u32 = 1;
pub const CONFIG_RELATIVE_PATH: &str = ".nemesis/config.toml";

#[derive(Clone, Debug)]
pub struct ProjectPaths {
    root: PathBuf,
}

impl ProjectPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn nemesis_dir(&self) -> PathBuf {
        self.root.join(".nemesis")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join(CONFIG_RELATIVE_PATH)
    }

    pub fn integrations_file(&self) -> PathBuf {
        self.nemesis_dir().join("integrations.json")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.nemesis_dir().join("runs")
    }

    pub fn run_dir(&self, run_id: uuid::Uuid) -> PathBuf {
        self.runs_dir().join(run_id.to_string())
    }

    pub fn ensure_runtime_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.runs_dir()).map_err(|source| io_error(self.runs_dir(), source))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NemesisConfig {
    pub version: u32,
    pub project: ProjectConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub reviewer_timeout_seconds: u64,
    pub max_child_depth: u8,
}

impl Default for NemesisConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            project: ProjectConfig {
                name: "nemesis-project".to_owned(),
            },
            runtime: RuntimeConfig {
                reviewer_timeout_seconds: 900,
                max_child_depth: 1,
            },
        }
    }
}

impl NemesisConfig {
    pub fn for_project(root: &Path) -> Self {
        let mut config = Self::default();
        if let Some(name) = root.file_name().and_then(|name| name.to_str()) {
            config.project.name = name.to_owned();
        }
        config
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != CONFIG_VERSION {
            return Err(CoreError::InvalidConfig(format!(
                "unsupported config version {}; expected {}",
                self.version, CONFIG_VERSION
            )));
        }
        if self.project.name.trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "project.name must not be empty".to_owned(),
            ));
        }
        if self.runtime.reviewer_timeout_seconds == 0 {
            return Err(CoreError::InvalidConfig(
                "runtime.reviewer_timeout_seconds must be greater than zero".to_owned(),
            ));
        }
        if self.runtime.max_child_depth == 0 {
            return Err(CoreError::InvalidConfig(
                "runtime.max_child_depth must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn load(paths: &ProjectPaths) -> Result<Self> {
        let path = paths.config_file();
        let content = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
        let config: Self = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, paths: &ProjectPaths) -> Result<()> {
        self.validate()?;
        fs::create_dir_all(paths.nemesis_dir())
            .map_err(|source| io_error(paths.nemesis_dir(), source))?;
        let content = toml::to_string_pretty(self)?;
        fs::write(paths.config_file(), content)
            .map_err(|source| io_error(paths.config_file(), source))
    }
}

pub fn discover_project_root(start: &Path) -> PathBuf {
    let mut current = start.to_path_buf();
    loop {
        let markers = [
            current.join(".nemesis"),
            current.join(".git"),
            current.join("openspec"),
            current.join(".specify"),
            current.join("Cargo.toml"),
        ];
        if markers.iter().any(|marker| marker.exists()) {
            return current;
        }
        if !current.pop() {
            return start.to_path_buf();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_and_validates() {
        let config = NemesisConfig::default();
        config.validate().expect("default configuration is valid");
        let serialized = toml::to_string(&config).expect("serialize config");
        let decoded: NemesisConfig = toml::from_str(&serialized).expect("deserialize config");
        assert_eq!(config, decoded);
    }

    #[test]
    fn root_discovery_prefers_nearest_marker() {
        let root = PathBuf::from("/tmp/nemesis-root");
        let paths = ProjectPaths::new(&root);
        assert_eq!(paths.config_file(), root.join(CONFIG_RELATIVE_PATH));
    }
}
