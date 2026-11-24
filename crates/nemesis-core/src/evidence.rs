use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::model::{Evidence, EvidenceValidation, ValidationState};

/// Return the SHA-256 of a source file when it is inside the supplied workspace.
pub fn content_hash_for_source(workspace_root: &Path, source_path: &str) -> Option<String> {
    let path = scoped_source_path(workspace_root, source_path).ok()?;
    let content = fs::read(path).ok()?;
    Some(sha256_hex(&content))
}

/// Validate objective evidence properties without deciding whether the claim is semantically true.
///
/// Invalid or stale source evidence is recorded on the evidence object rather than returned as a
/// successful validation. This lets callers preserve the verifier's claim while preventing it from
/// being used as supporting evidence.
pub fn validate_evidence(
    workspace_root: &Path,
    evidence: &mut Evidence,
    validator: impl Into<String>,
) -> Result<()> {
    let mut errors = Vec::new();
    let path = match scoped_source_path(workspace_root, &evidence.source.path) {
        Ok(path) => path,
        Err(error) => {
            errors.push(error);
            evidence.validation = invalid_validation(validator, errors);
            return Ok(());
        }
    };

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            errors.push(format!("source could not be read: {error}"));
            evidence.validation = invalid_validation(validator, errors);
            return Ok(());
        }
    };

    let actual_hash = sha256_hex(&bytes);
    match evidence.content_hash.as_deref() {
        Some(expected) if expected == actual_hash => {}
        Some(expected) => errors.push(format!(
            "content hash mismatch: expected {expected}, observed {actual_hash}"
        )),
        None => errors.push("content_hash is required for source evidence".to_owned()),
    }

    let source_text = String::from_utf8_lossy(&bytes);
    match evidence.source.locator.as_deref() {
        Some(locator) => match excerpt_for_locator(&source_text, locator) {
            Some(excerpt) => match evidence.content.as_deref() {
                Some(quoted)
                    if normalize_newlines(excerpt.trim_end())
                        == normalize_newlines(quoted.trim_end()) => {}
                Some(_) => errors
                    .push("quoted evidence excerpt does not match the source locator".to_owned()),
                None => errors.push("content excerpt is required for source evidence".to_owned()),
            },
            None => errors.push(format!(
                "source locator {locator} does not resolve; use line:N or lines:N-M"
            )),
        },
        None => errors.push("source locator is required for source evidence".to_owned()),
    }

    evidence.validation = if errors.is_empty() {
        EvidenceValidation {
            state: ValidationState::Valid,
            validated_at: Some(Utc::now()),
            validator: Some(validator.into()),
            errors,
        }
    } else {
        invalid_validation(validator, errors)
    };
    Ok(())
}

fn invalid_validation(validator: impl Into<String>, errors: Vec<String>) -> EvidenceValidation {
    EvidenceValidation {
        state: ValidationState::Invalid,
        validated_at: Some(Utc::now()),
        validator: Some(validator.into()),
        errors,
    }
}

fn scoped_source_path(
    workspace_root: &Path,
    source_path: &str,
) -> std::result::Result<PathBuf, String> {
    if source_path.contains("://") {
        return Err(format!(
            "source reference {source_path} is not a workspace file"
        ));
    }

    let root = fs::canonicalize(workspace_root)
        .map_err(|error| format!("workspace root could not be resolved: {error}"))?;
    let candidate = Path::new(source_path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let canonical = fs::canonicalize(&joined).map_err(|error| {
        format!("source reference {source_path} could not be resolved: {error}")
    })?;
    if canonical.strip_prefix(&root).is_err() {
        return Err(format!(
            "source reference {source_path} is outside the verification workspace"
        ));
    }
    Ok(canonical)
}

fn excerpt_for_locator(source: &str, locator: &str) -> Option<String> {
    let (start, end) = parse_line_range(locator)?;
    if start == 0 || end < start {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    if end > lines.len() {
        return None;
    }
    Some(lines[start - 1..end].join("\n"))
}

fn parse_line_range(locator: &str) -> Option<(usize, usize)> {
    let value = locator
        .strip_prefix("line:")
        .or_else(|| locator.strip_prefix("lines:"))
        .or_else(|| locator.strip_prefix("L"))?;
    if let Some((start, end)) = value.split_once('-') {
        Some((
            start.parse().ok()?,
            end.trim_start_matches('L').parse().ok()?,
        ))
    } else {
        let line = value.parse().ok()?;
        Some((line, line))
    }
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::model::{CollectionMetadata, EvidenceType, SourceReference};

    fn evidence(root: &Path, locator: &str, excerpt: &str) -> Evidence {
        Evidence {
            schema_version: crate::model::MODEL_SCHEMA_VERSION.to_owned(),
            id: Uuid::new_v4(),
            evidence_type: EvidenceType::Source,
            source: SourceReference {
                path: "src/lib.rs".to_owned(),
                locator: Some(locator.to_owned()),
                revision: None,
            },
            content: Some(excerpt.to_owned()),
            content_hash: content_hash_for_source(root, "src/lib.rs"),
            git_revision: None,
            collection: CollectionMetadata {
                collector: "test".to_owned(),
                method: "fixture".to_owned(),
                collected_at: Utc::now(),
                command: None,
            },
            validation: EvidenceValidation {
                state: ValidationState::Pending,
                validated_at: None,
                validator: None,
                errors: Vec::new(),
            },
        }
    }

    #[test]
    fn valid_source_evidence_is_accepted() {
        let directory = tempdir().expect("temporary directory");
        fs::create_dir_all(directory.path().join("src")).expect("create source directory");
        fs::write(
            directory.path().join("src/lib.rs"),
            "fn first() {}\nfn second() {}\n",
        )
        .expect("write source");
        let mut value = evidence(directory.path(), "line:2", "fn second() {}");
        validate_evidence(directory.path(), &mut value, "test").expect("validate evidence");
        assert_eq!(value.validation.state, ValidationState::Valid);
    }

    #[test]
    fn nonexistent_source_is_invalid() {
        let directory = tempdir().expect("temporary directory");
        let mut value = evidence(directory.path(), "line:1", "missing");
        value.source.path = "missing.rs".to_owned();
        validate_evidence(directory.path(), &mut value, "test").expect("validate evidence");
        assert_eq!(value.validation.state, ValidationState::Invalid);
        assert!(!value.validation.errors.is_empty());
    }

    #[test]
    fn mismatched_excerpt_and_stale_content_are_invalid() {
        let directory = tempdir().expect("temporary directory");
        fs::write(directory.path().join("src.rs"), "old\n").expect("write source");
        let mut value = evidence(directory.path(), "line:1", "old");
        value.source.path = "src.rs".to_owned();
        fs::write(directory.path().join("src.rs"), "new\n").expect("modify source");
        validate_evidence(directory.path(), &mut value, "test").expect("validate evidence");
        assert_eq!(value.validation.state, ValidationState::Invalid);
        assert!(value
            .validation
            .errors
            .iter()
            .any(|error| error.contains("hash")));
    }
}
