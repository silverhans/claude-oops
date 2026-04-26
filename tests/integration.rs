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
fn outside_git_repo_errors_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let (_, stderr, code) = run_oops(dir.path(), &["snap"]);
    assert_ne!(code, 0);
    assert!(
        stderr.to_lowercase().contains("git"),
        "error should mention git: {stderr}"
    );
}
