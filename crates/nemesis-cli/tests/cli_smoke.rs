use std::fs;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

fn run(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nemesis"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("run nemesis")
}

#[test]
fn foundation_commands_are_real_and_idempotent() {
    let directory = tempdir().expect("temporary directory");

    let version = run(directory.path(), &["version"]);
    assert!(version.status.success(), "version failed: {:?}", version);
    assert!(String::from_utf8_lossy(&version.stdout).contains("nemesis 0.1.0"));

    let init = run(directory.path(), &["init"]);
    assert!(init.status.success(), "init failed: {:?}", init);
    assert!(directory.path().join(".nemesis/config.toml").is_file());

    let repeat = run(directory.path(), &["init"]);
    assert!(repeat.status.success(), "repeat init failed: {:?}", repeat);
    assert!(String::from_utf8_lossy(&repeat.stdout).contains("already existed"));

    for args in [
        vec!["agent", "integration", "list"],
        vec!["sdd", "integration", "list"],
        vec!["doctor"],
    ] {
        let output = run(directory.path(), &args);
        assert!(
            output.status.success(),
            "command {args:?} failed: {:?}",
            output
        );
    }
}

#[cfg(unix)]
#[test]
fn review_and_independent_verification_loop_is_observable() {
    let directory = tempdir().expect("temporary directory");
    fs::create_dir_all(directory.path().join("src")).expect("source directory");
    fs::write(directory.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
    let pi = directory.path().join("pi");
    fs::write(
        &pi,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf "%s\n" "pi-test"
  exit 0
fi
IFS= read -r line
if printf "%s" "$line" | grep -q "finding verifier"; then
  printf '%s\n' '{"type":"agent_start"}'
  printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":"{\"verdict\":\"confirmed\",\"observations\":[\"The source line exists.\"],\"interpretations\":[\"The observation supports the claim.\"],\"conclusion\":\"The finding is confirmed.\",\"supporting_evidence\":[{\"kind\":\"source\",\"path\":\"src/main.rs\",\"locator\":\"line:1\",\"excerpt\":\"fn main() {}\",\"revision\":null}],\"contradicting_evidence\":[],\"uncertainty\":[]}"}}'
  printf '%s\n' '{"type":"agent_settled"}'
else
  printf '%s\n' '{"type":"agent_start"}'
  printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":"{\"summary\":\"one candidate finding\",\"findings\":[{\"id\":\"F-001\",\"claim\":\"The source contains the reported behavior.\",\"category\":\"correctness\",\"severity\":\"high\",\"sources\":[\"src/main.rs\"],\"evidence\":[\"The reviewer observed the source line.\"]}]}"}}'
  printf '%s\n' '{"type":"agent_settled"}'
fi
"#,
    )
    .expect("mock Pi");
    let mut permissions = fs::metadata(&pi).expect("Pi metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&pi, permissions).expect("make Pi executable");

    let init = run(directory.path(), &["init"]);
    assert!(init.status.success(), "init failed: {init:?}");
    let original_path = std::env::var_os("PATH");
    let mut path_entries = vec![pi.parent().expect("mock Pi parent").to_path_buf()];
    if let Some(original) = original_path.as_ref() {
        path_entries.extend(std::env::split_paths(original));
    }
    std::env::set_var(
        "PATH",
        std::env::join_paths(path_entries).expect("join mock PATH"),
    );

    let review = run(
        directory.path(),
        &["--json", "review", "run", "--prompt", "inspect"],
    );
    assert!(review.status.success(), "review failed: {review:?}");
    let review_json: serde_json::Value =
        serde_json::from_slice(&review.stdout).expect("review JSON");
    let review_id = review_json["run_id"]
        .as_str()
        .expect("review run ID")
        .to_owned();

    let wait = run(
        directory.path(),
        &[
            "--json",
            "review",
            "wait",
            &review_id,
            "--timeout-seconds",
            "10",
        ],
    );
    assert!(wait.status.success(), "review wait failed: {wait:?}");
    let wait_json: serde_json::Value = serde_json::from_slice(&wait.stdout).expect("wait JSON");
    assert_eq!(
        wait_json["completed"], true,
        "wait={wait_json:?} review={review:?}"
    );
    let events = run(
        directory.path(),
        &["--json", "review", "events", &review_id, "--tail", "20"],
    );
    assert!(events.status.success(), "review events failed: {events:?}");
    let events_json: serde_json::Value =
        serde_json::from_slice(&events.stdout).expect("events JSON");
    assert!(events_json
        .as_array()
        .is_some_and(|items| !items.is_empty()));

    let result = run(
        directory.path(),
        &["--json", "review", "result", &review_id],
    );
    assert!(result.status.success(), "review result failed: {result:?}");
    let result_json: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("result JSON");
    assert_eq!(result_json["valid"], true);
    assert_eq!(
        result_json["findings"][0]["verification_state"],
        "unverified"
    );

    let verification = run(
        directory.path(),
        &[
            "--json",
            "finding",
            "verify",
            &review_id,
            "F-001",
            "--timeout-seconds",
            "10",
        ],
    );
    assert!(
        verification.status.success(),
        "verification queue failed: {verification:?}"
    );
    let verification_json: serde_json::Value =
        serde_json::from_slice(&verification.stdout).expect("verification JSON");
    let verification_id = verification_json["run_id"]
        .as_str()
        .expect("verification run ID")
        .to_owned();
    let verification_wait = run(
        directory.path(),
        &[
            "--json",
            "review",
            "wait",
            &verification_id,
            "--timeout-seconds",
            "10",
        ],
    );
    assert!(
        verification_wait.status.success(),
        "verification wait failed: {verification_wait:?}"
    );
    let shown = run(
        directory.path(),
        &["--json", "finding", "show", &review_id, "F-001"],
    );
    assert!(shown.status.success(), "finding show failed: {shown:?}");
    let shown_json: serde_json::Value =
        serde_json::from_slice(&shown.stdout).expect("finding JSON");
    assert_eq!(
        shown_json["verifications"][0]["verdict"], "confirmed",
        "shown={shown_json:?} wait={:?} result={:?}",
        verification_wait, result
    );

    fs::write(directory.path().join("src/main.rs"), "fn changed() {}\n")
        .expect("modify source after verification");
    let stale = run(
        directory.path(),
        &["--json", "finding", "show", &review_id, "F-001"],
    );
    let stale_json: serde_json::Value =
        serde_json::from_slice(&stale.stdout).expect("stale finding JSON");
    assert!(stale_json["verifications"][0]["stale_evidence_refs"]
        .as_array()
        .is_some_and(|items| !items.is_empty()));
    assert_eq!(stale_json["verifications"][0]["verdict"], "inconclusive");
    let stale_result = run(
        directory.path(),
        &["--json", "review", "result", &verification_id],
    );
    let stale_result_json: serde_json::Value =
        serde_json::from_slice(&stale_result.stdout).expect("stale result JSON");
    assert_eq!(stale_result_json["record"]["verdict"], "inconclusive");
    assert_eq!(
        stale_result_json["evidence"][0]["validation"]["state"],
        "invalid"
    );

    match original_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
}
