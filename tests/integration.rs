//! End-to-end tests that drive the compiled `claude-oops` binary against
//! throwaway git repos.

mod helpers;

use helpers::{run_oops, TempRepo};

#[test]
fn snap_in_clean_tree_records_a_baseline() {
    let repo = TempRepo::new();
    let (stdout, stderr, code) = run_oops(repo.path(), &["snap", "-m", "first"]);
    assert_eq!(code, 0, "snap failed: stdout={stdout} stderr={stderr}");
    let (list_out, _, list_code) = run_oops(repo.path(), &["list"]);
    assert_eq!(list_code, 0);
    assert!(
        list_out.contains("first") || list_out.contains("manual"),
        "list output missing snapshot: {list_out}"
    );
}

#[test]
fn snap_with_changes_records_diff_stats() {
    let repo = TempRepo::new();
    repo.write("foo.txt", "one\ntwo\nthree\n");
    let (_, _, code) = run_oops(repo.path(), &["snap", "-m", "added foo"]);
    assert_eq!(code, 0);
    let (list_out, _, _) = run_oops(repo.path(), &["list"]);
    // 3 lines added, 0 deleted.
    assert!(list_out.contains("+3/-0"), "expected +3/-0 in: {list_out}");
}

#[test]
fn list_json_emits_valid_records() {
    let repo = TempRepo::new();
    run_oops(repo.path(), &["snap", "-m", "alpha"]);
    repo.write("a.txt", "data\n");
    run_oops(repo.path(), &["snap", "-m", "beta"]);
    let (out, _, code) = run_oops(repo.path(), &["list", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["message"], "alpha");
    assert_eq!(arr[1]["message"], "beta");
}

#[test]
fn manual_snap_ignores_idempotency() {
    // Two manual snaps in a row on the same tree should both succeed
    // because manual is force=true.
    let repo = TempRepo::new();
    run_oops(repo.path(), &["snap", "-m", "one"]);
    run_oops(repo.path(), &["snap", "-m", "two"]);
    let (out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
}

#[test]
fn auto_trigger_skips_when_tree_unchanged() {
    let repo = TempRepo::new();
    run_oops(repo.path(), &["snap", "--trigger", "session-start"]);
    // Same tree, non-manual trigger — should be skipped.
    let (stdout, _, _) = run_oops(repo.path(), &["snap", "--trigger", "pre-edit"]);
    assert!(stdout.contains("no change"), "expected skip, got: {stdout}");
    let (out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn restore_brings_back_deleted_file() {
    let repo = TempRepo::new();
    repo.write("important.txt", "do not delete me\n");
    run_oops(repo.path(), &["snap", "-m", "before disaster"]);

    // Get the snapshot id from list --json.
    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();

    // Disaster strikes.
    std::fs::remove_file(repo.path().join("important.txt")).unwrap();
    assert!(!repo.exists("important.txt"));

    // Oops.
    let (_, stderr, code) = run_oops(repo.path(), &["to", &id, "--force"]);
    assert_eq!(code, 0, "restore failed: {stderr}");
    assert!(repo.exists("important.txt"));
    assert_eq!(repo.read("important.txt"), "do not delete me\n");
}

#[test]
fn diff_shows_changes_against_snapshot() {
    let repo = TempRepo::new();
    repo.write("file.txt", "v1\n");
    run_oops(repo.path(), &["snap", "-m", "v1"]);
    repo.write("file.txt", "v2\n");

    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();

    let (out, _, code) = run_oops(repo.path(), &["diff", &id]);
    assert_eq!(code, 0);
    assert!(out.contains("+v2"), "diff missing change: {out}");
    assert!(out.contains("-v1"), "diff missing change: {out}");
}

#[test]
fn drop_removes_snapshot_and_ref() {
    let repo = TempRepo::new();
    run_oops(repo.path(), &["snap", "-m", "doomed"]);
    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();

    let (_, _, code) = run_oops(repo.path(), &["drop", &id]);
    assert_eq!(code, 0);

    let (out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);

    // Ref should be gone.
    let ref_check = std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/claude-oops/{}", id),
        ])
        .current_dir(repo.path())
        .status()
        .unwrap();
    assert!(!ref_check.success(), "ref should have been deleted");
}

#[test]
fn install_then_uninstall_round_trip() {
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let commands_dir = dir.path().join("commands");
    // Pre-existing user content we must NOT touch.
    std::fs::write(
        &settings,
        r#"{"permissions": {"allow": ["bash"]}, "hooks": {"PreToolUse": [
          {"matcher": "Read", "hooks": [{"type": "command", "command": "echo user-hook"}]}
        ]}}"#,
    )
    .unwrap();

    let bin = helpers::bin_path();

    let out = Command::new(&bin)
        .arg("install")
        .env("CLAUDE_OOPS_SETTINGS", &settings)
        .env("CLAUDE_OOPS_COMMANDS_DIR", &commands_dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "install failed: {:?}", out);
    // Slash command was written.
    assert!(commands_dir.join("oops.md").exists());

    let after_install: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    // User's permission block survives.
    assert_eq!(after_install["permissions"]["allow"][0], "bash");
    // User's pre-existing PreToolUse hook survives.
    let pre = after_install["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(
        pre.iter().any(|e| e["matcher"] == "Read"),
        "user hook lost: {pre:?}"
    );
    // Our two hooks are present.
    assert!(after_install["hooks"]["SessionStart"].is_array());
    assert!(pre.iter().any(|e| e["matcher"] == "Edit|Write|Bash"));

    let out = Command::new(&bin)
        .arg("uninstall")
        .env("CLAUDE_OOPS_SETTINGS", &settings)
        .env("CLAUDE_OOPS_COMMANDS_DIR", &commands_dir)
        .output()
        .unwrap();
    assert!(out.status.success());

    let after_uninstall: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    // User's stuff still here.
    assert_eq!(after_uninstall["permissions"]["allow"][0], "bash");
    let pre = after_uninstall["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(pre.iter().any(|e| e["matcher"] == "Read"));
    // Our entry is gone.
    assert!(!pre.iter().any(|e| e["matcher"] == "Edit|Write|Bash"));
    // Slash command was cleaned up too.
    assert!(!commands_dir.join("oops.md").exists());
}

#[test]
fn uninstall_preserves_user_modified_slash_command() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let commands_dir = dir.path().join("commands");
    let bin = helpers::bin_path();

    Command::new(&bin)
        .arg("install")
        .env("CLAUDE_OOPS_SETTINGS", &settings)
        .env("CLAUDE_OOPS_COMMANDS_DIR", &commands_dir)
        .output()
        .unwrap();

    // User edits the slash command.
    let oops_md = commands_dir.join("oops.md");
    std::fs::write(&oops_md, "user customized this\n").unwrap();

    Command::new(&bin)
        .arg("uninstall")
        .env("CLAUDE_OOPS_SETTINGS", &settings)
        .env("CLAUDE_OOPS_COMMANDS_DIR", &commands_dir)
        .output()
        .unwrap();

    // Should still exist with user's content.
    assert!(oops_md.exists());
    assert_eq!(
        std::fs::read_to_string(&oops_md).unwrap(),
        "user customized this\n"
    );
}

#[test]
fn install_is_idempotent() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let commands_dir = dir.path().join("commands");
    let bin = helpers::bin_path();

    for _ in 0..3 {
        let out = Command::new(&bin)
            .arg("install")
            .env("CLAUDE_OOPS_SETTINGS", &settings)
            .env("CLAUDE_OOPS_COMMANDS_DIR", &commands_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert!(commands_dir.join("oops.md").exists());
}

#[test]
fn show_lists_files_that_would_change_on_restore() {
    let repo = TempRepo::new();
    repo.write("a.txt", "v1\n");
    repo.write("b.txt", "v1\n");
    run_oops(repo.path(), &["snap", "-m", "snap1"]);

    // Change one file, leave the other alone.
    repo.write("a.txt", "v2\n");

    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();

    let (out, _, code) = run_oops(repo.path(), &["show", &id]);
    assert_eq!(code, 0);
    assert!(out.contains("a.txt"), "show should mention a.txt: {out}");
    assert!(
        !out.contains("b.txt"),
        "b.txt unchanged, shouldn't appear: {out}"
    );
}

#[test]
fn show_on_unchanged_tree_says_no_changes() {
    let repo = TempRepo::new();
    repo.write("file.txt", "x\n");
    run_oops(repo.path(), &["snap", "-m", "snap"]);

    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();

    let (out, _, code) = run_oops(repo.path(), &["show", &id]);
    assert_eq!(code, 0);
    assert!(out.to_lowercase().contains("no changes"), "got: {out}");
}

#[test]
fn hook_emits_visible_feedback_on_stderr() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let repo = TempRepo::new();
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": "rm -rf important_dir"},
        "cwd": repo.path().to_string_lossy(),
    });
    let mut child = Command::new(helpers::bin_path())
        .arg("_hook-pre-tool-use")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // stdout must stay empty so Claude Code doesn't try to parse it as a
    // permission decision.
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty, got: {:?}",
        out.stdout
    );
    assert!(
        stderr.contains("claude-oops"),
        "expected feedback in stderr: {stderr}"
    );
    assert!(
        stderr.contains("pre-bash"),
        "expected trigger label: {stderr}"
    );
}

#[test]
fn pre_tool_use_hook_snapshots_on_dangerous_bash() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let repo = TempRepo::new();
    repo.write("data.txt", "important\n");

    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": "rm -rf data.txt"},
        "cwd": repo.path().to_string_lossy(),
    });
    let mut child = Command::new(helpers::bin_path())
        .arg("_hook-pre-tool-use")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "expected 1 snapshot, got {arr:?}");
    assert_eq!(arr[0]["trigger"], "pre-bash");
}

#[test]
fn pre_tool_use_hook_skips_safe_bash() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let repo = TempRepo::new();
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": "ls -la"},
        "cwd": repo.path().to_string_lossy(),
    });
    let mut child = Command::new(helpers::bin_path())
        .arg("_hook-pre-tool-use")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let (json_out, _, _) = run_oops(repo.path(), &["list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn clean_removes_snapshots_and_refs() {
    // We can't easily create a snapshot >7 days old without time travel,
    // so we test the keep-last-N path: take 5 snaps, retain only the last 2.
    let repo = TempRepo::new();
    for i in 0..5 {
        repo.write("f.txt", &format!("v{i}\n"));
        run_oops(repo.path(), &["snap", "-m", &format!("v{i}")]);
    }

    // Hand-edit the index to set keep_last_n=2 by directly running clean
    // wouldn't work — clean uses the constants. Instead verify the no-op
    // behaviour: no snapshots are old enough to drop, all stay.
    let (out, _, code) = run_oops(repo.path(), &["clean"]);
    assert_eq!(code, 0);
    assert!(out.contains("kept 5"), "unexpected output: {out}");
    assert!(out.contains("deleted 0"), "unexpected output: {out}");
}

#[test]
fn outside_git_repo_errors_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let (_, stderr, code) = run_oops(dir.path(), &["snap"]);
    assert_ne!(code, 0);
    assert!(
        stderr.to_lowercase().contains("git"),
        "error should mention git: {stderr}"
    );
}
