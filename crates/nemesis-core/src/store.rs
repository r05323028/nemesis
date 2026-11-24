use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::config::ProjectPaths;
use crate::error::{io_error, CoreError, Result};
use crate::model::{ReviewRun, ReviewerEvent};

#[derive(Clone, Debug)]
pub struct RunStore {
    paths: ProjectPaths,
}

impl RunStore {
    pub fn new(paths: ProjectPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    pub fn ensure(&self, run_id: Uuid) -> Result<()> {
        fs::create_dir_all(self.paths.run_dir(run_id))
            .map_err(|source| io_error(self.paths.run_dir(run_id), source))
    }

    pub fn run_path(&self, run_id: Uuid) -> PathBuf {
        self.paths.run_dir(run_id).join("run.json")
    }

    pub fn request_path(&self, run_id: Uuid) -> PathBuf {
        self.paths.run_dir(run_id).join("request.json")
    }

    pub fn result_path(&self, run_id: Uuid) -> PathBuf {
        self.paths.run_dir(run_id).join("result.json")
    }

    pub fn events_path(&self, run_id: Uuid) -> PathBuf {
        self.paths.run_dir(run_id).join("events.jsonl")
    }

    pub fn cancel_path(&self, run_id: Uuid) -> PathBuf {
        self.paths.run_dir(run_id).join("cancel")
    }

    pub fn save_run(&self, run: &ReviewRun) -> Result<()> {
        self.write_json(&self.run_path(run.run_id), run)
    }

    pub fn load_run(&self, run_id: Uuid) -> Result<ReviewRun> {
        if !self.run_path(run_id).exists() {
            return Err(CoreError::RunNotFound(run_id));
        }
        self.read_json(&self.run_path(run_id))
    }

    pub fn save_request<T: Serialize>(&self, run_id: Uuid, request: &T) -> Result<()> {
        self.ensure(run_id)?;
        self.write_json(&self.request_path(run_id), request)
    }

    pub fn load_request<T: DeserializeOwned>(&self, run_id: Uuid) -> Result<T> {
        self.read_json(&self.request_path(run_id))
    }

    pub fn save_result<T: Serialize>(&self, run_id: Uuid, result: &T) -> Result<()> {
        self.write_json(&self.result_path(run_id), result)
    }

    pub fn load_result<T: DeserializeOwned>(&self, run_id: Uuid) -> Result<T> {
        self.read_json(&self.result_path(run_id))
    }

    pub fn append_event(&self, event: &ReviewerEvent) -> Result<()> {
        self.ensure(event.run_id)?;
        let path = self.events_path(event.run_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        let line = serde_json::to_string(event)?;
        writeln!(file, "{line}").map_err(|source| io_error(path, source))
    }

    pub fn load_events(&self, run_id: Uuid) -> Result<Vec<ReviewerEvent>> {
        let path = self.events_path(run_id);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    pub fn request_cancel(&self, run_id: Uuid) -> Result<()> {
        self.ensure(run_id)?;
        let path = self.cancel_path(run_id);
        File::create(&path).map_err(|source| io_error(path, source))?;
        Ok(())
    }

    pub fn is_cancel_requested(&self, run_id: Uuid) -> bool {
        self.cancel_path(run_id).exists()
    }

    pub fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let content = serde_json::to_vec_pretty(value)?;
        fs::write(&temporary, content).map_err(|source| io_error(&temporary, source))?;
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))
    }

    pub fn read_json<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let content = fs::read_to_string(path).map_err(|source| io_error(path, source))?;
        serde_json::from_str(&content).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::config::ProjectPaths;
    use crate::model::{ExecutionPhase, ReviewerEvent, ReviewerEventType};

    #[test]
    fn events_are_append_only_jsonl() {
        let directory = tempdir().expect("temporary directory");
        let store = RunStore::new(ProjectPaths::new(directory.path()));
        let run_id = Uuid::new_v4();
        let event = ReviewerEvent {
            schema_version: crate::model::MODEL_SCHEMA_VERSION.to_owned(),
            event_id: Uuid::new_v4(),
            run_id,
            sequence: 1,
            timestamp: Utc::now(),
            event_type: ReviewerEventType::RunStarted,
            phase: ExecutionPhase::Starting,
            payload: serde_json::json!({"source": "test"}),
        };
        store.append_event(&event).expect("append event");
        let content = fs::read_to_string(store.events_path(run_id)).expect("read events");
        assert_eq!(content.lines().count(), 1);
        assert!(content.contains("run_started"));
    }
}
