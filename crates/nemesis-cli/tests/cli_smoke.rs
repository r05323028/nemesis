use std::process::Command;

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
