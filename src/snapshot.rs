//! Core snapshot operations: take, restore, diff, drop.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};

use crate::git::GitRepo;
use crate::storage::{self, SnapshotRecord};

/// What caused a snapshot. Used both for filtering and for `list` output.
/// Kept here as canonical names; hook scripts pass these via `--trigger`.
#[allow(dead_code)]
pub mod trigger {
    pub const MANUAL: &str = "manual";
    pub const SESSION_START: &str = "session-start";
    pub const PRE_EDIT: &str = "pre-edit";
    pub const PRE_BASH: &str = "pre-bash";
    /// Snapshot taken when Claude finishes a turn — content-agnostic
    /// safety net for cases where the dangerous-bash matcher missed
    /// something. Especially useful in agent-loop setups where
    /// Claude operates autonomously without git access.
    pub const POST_TURN: &str = "post-turn";
}

/// Options for taking a snapshot.
pub struct SnapOpts<'a> {
    pub trigger: &'a str,
    pub message: Option<String>,
    /// If true, take the snapshot even if the working tree's tree SHA matches
    /// the most recent snapshot. Manual `snap` invocations set this.
    pub force: bool,
}

/// Outcome of a snapshot attempt.
pub enum SnapOutcome {
    /// A new snapshot was recorded.
    Created(SnapshotRecord),
    /// No snapshot needed (idempotency check). Carries the existing record.
    Skipped(SnapshotRecord),
    /// Repo has no commits yet — nothing to snapshot from.
    NoCommits,
}

/// Take a snapshot of the current working tree.
pub fn snap(repo: &GitRepo, opts: SnapOpts) -> Result<SnapOutcome> {
    if !repo.has_head() {
        return Ok(SnapOutcome::NoCommits);
    }

    let head_sha = repo
        .head_sha()?
        .ok_or_else(|| anyhow!("HEAD missing despite has_head()"))?;
    let head_tree = repo
        .tree_of(&head_sha)
        .context("could not resolve HEAD tree")?;

    let tree_sha = repo
        .capture_tree()
        .context("failed to capture working tree")?;
    let clean = tree_sha == head_tree;

    let sha = if clean {
        // No changes — point the snapshot ref at HEAD itself.
        head_sha.clone()
    } else {
        let msg = opts
            .message
            .as_deref()
            .map(|m| format!("claude-oops snapshot ({}): {}", opts.trigger, m))
            .unwrap_or_else(|| format!("claude-oops snapshot ({})", opts.trigger));
        repo.commit_tree(&tree_sha, &head_sha, &msg)?
    };

    let mut existing = storage::read_all(repo)?;

    // Idempotency: skip if tree matches the most recent snapshot, unless forced.
    if !opts.force {
        if let Some(last) = existing.last() {
            if last.tree_sha == tree_sha {
                return Ok(SnapOutcome::Skipped(last.clone()));
            }
        }
    }

    let id = storage::pick_id(&sha, &existing);

    // Diff stats — only meaningful for stash commits (which have HEAD as parent).
    // For "clean" snapshots that point at HEAD, skip stats.
    let (files_added, files_deleted) = if clean {
        (0, 0)
    } else {
        repo.diff_stats(&sha).unwrap_or((0, 0))
    };

    repo.update_ref(&id, &sha)?;

    let rec = SnapshotRecord {
        id: id.clone(),
        stash_sha: sha,
        tree_sha,
        trigger: opts.trigger.to_string(),
        message: opts.message,
        timestamp: Utc::now().timestamp(),
        files_added,
        files_deleted,
        clean,
    };
    storage::append(repo, &rec)?;
    existing.push(rec.clone());
    Ok(SnapOutcome::Created(rec))
}

/// Resolve a user-supplied path into a repo-root-relative string with `/`
/// separators — without touching the filesystem (so it works for paths
/// that don't exist any more; restore is the whole point).
///
/// We let git compute the cwd-relative-to-repo-root prefix itself
/// (`git rev-parse --show-prefix` from `cwd`); then we tack the user's
/// input onto that and clean it up lexically. This sidesteps the
/// Windows-specific mess where the OS gives us a path with 8.3 short
/// names while git gives us long names — comparing absolute paths
/// directly is unreliable.
///
/// Absolute paths are rejected: pass repo-relative paths instead.
pub fn resolve_path(repo: &GitRepo, cwd: &Path, path: &str) -> Result<String> {
    let _ = repo; // accepted for future use; show_prefix is on GitRepo's namespace
    if PathBuf::from(path).is_absolute() {
        return Err(anyhow!(
            "absolute paths aren't supported — pass a path relative to the repo"
        ));
    }
    let prefix = GitRepo::show_prefix_from(cwd)?;
    // Normalize backslashes the user might pass on Windows.
    let user = path.replace('\\', "/");
    let combined = format!("{}{}", prefix, user);
    let cleaned = clean_slash_path(&combined);
    if cleaned.is_empty() {
        return Ok(String::new());
    }
    if cleaned == ".." || cleaned.starts_with("../") {
        return Err(anyhow!(
            "{} resolves to {}, which is outside the repo",
            path,
            cleaned
        ));
    }
    Ok(cleaned)
}

/// Collapse `.` and `..` components in a `/`-separated path purely lexically.
/// Multiple slashes and leading/trailing slashes are squashed.
fn clean_slash_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => match out.last() {
                Some(prev) if *prev != ".." => {
                    out.pop();
                }
                _ => out.push(".."),
            },
            other => out.push(other),
        }
    }
    out.join("/")
}

/// Restore only the given paths from the snapshot, leaving everything else
/// in the working tree untouched.
///
/// For each path:
/// - If it exists in the snapshot, the working-tree version is overwritten
///   with the snapshot version.
/// - If it exists in the working tree but not in the snapshot, it's deleted
///   (that's what "restore the snapshot's state for this path" means).
/// - Empty intersection → no-op.
///
/// Pathspecs that match multiple files are expanded recursively.
pub fn restore_paths(
    repo: &GitRepo,
    rec: &SnapshotRecord,
    paths: &[String],
) -> Result<RestorePathReport> {
    if paths.is_empty() {
        return Err(anyhow!("restore_paths called with empty paths"));
    }

    // Build a private index from the snapshot tree.
    let tmp_dir = repo.git_dir()?.join("claude-oops");
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create {}", tmp_dir.display()))?;
    let tmp_index = tmp_dir.join("restore-index");
    let _ = std::fs::remove_file(&tmp_index);

    let status = repo
        .git()
        .env("GIT_INDEX_FILE", &tmp_index)
        .args(["read-tree", &rec.tree_sha])
        .status()
        .context("read-tree (restore) failed to run")?;
    if !status.success() {
        return Err(anyhow!("git read-tree {} failed", rec.tree_sha));
    }

    let snap_paths = repo.list_tree_paths(&rec.tree_sha, paths)?;
    let working_paths = repo.list_working_paths(paths)?;

    // Files present in the snapshot under the pathspec → check them out.
    if !snap_paths.is_empty() {
        let mut cmd = repo.git();
        cmd.env("GIT_INDEX_FILE", &tmp_index)
            .args(["checkout-index", "-f", "--"]);
        for p in &snap_paths {
            cmd.arg(p);
        }
        let status = cmd
            .status()
            .context("checkout-index (restore) failed to run")?;
        if !status.success() {
            return Err(anyhow!("git checkout-index failed during restore"));
        }
    }

    // Files in working tree under the pathspec but not in snapshot → remove.
    use std::collections::HashSet;
    let snap_set: HashSet<&String> = snap_paths.iter().collect();
    let mut deleted = Vec::new();
    for w in &working_paths {
        if !snap_set.contains(w) {
            let abs = repo.root().join(w);
            if std::fs::remove_file(&abs).is_ok() {
                deleted.push(w.clone());
            }
        }
    }

    let _ = std::fs::remove_file(&tmp_index);

    if snap_paths.is_empty() && deleted.is_empty() {
        return Err(anyhow!(
            "no matching files in snapshot or working tree for the given paths"
        ));
    }

    Ok(RestorePathReport {
        restored: snap_paths,
        deleted,
    })
}

/// What `restore_paths` did.
pub struct RestorePathReport {
    /// Paths that were checked out from the snapshot.
    pub restored: Vec<String>,
    /// Paths that were deleted (present in working tree, absent in snapshot).
    pub deleted: Vec<String>,
}

/// Restore the working tree to the snapshot's tree.
///
/// Strategy: build a private index from the snapshot tree and
/// `checkout-index -a -f` from it. This force-overwrites conflicting
/// local changes — which is exactly what the caller asked for.
/// We deliberately don't use `read-tree -m -u` (merge-aware), because
/// that refuses to clobber uncommitted local changes, and the whole
/// point of restore is to do exactly that. The user already confirmed
/// (or passed `--force`) by the time we get here.
///
/// HEAD is not moved; commit history is untouched. The user's real
/// `.git/index` is also untouched — we operate via a temp index — so
/// after restore, `git status` correctly reflects the diff between
/// HEAD and the new working tree.
///
/// Files in the working tree that aren't in the snapshot (and aren't
/// gitignored) are deleted, so the working tree ends up matching the
/// snapshot exactly.
pub fn restore(repo: &GitRepo, rec: &SnapshotRecord) -> Result<()> {
    let tmp_dir = repo.git_dir()?.join("claude-oops");
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create {}", tmp_dir.display()))?;
    let tmp_index = tmp_dir.join("restore-index");
    let _ = std::fs::remove_file(&tmp_index);

    let status = repo
        .git()
        .env("GIT_INDEX_FILE", &tmp_index)
        .args(["read-tree", &rec.tree_sha])
        .status()
        .context("read-tree (restore) failed to run")?;
    if !status.success() {
        return Err(anyhow!("git read-tree {} failed", rec.tree_sha));
    }

    let status = repo
        .git()
        .env("GIT_INDEX_FILE", &tmp_index)
        .args(["checkout-index", "-a", "-f"])
        .status()
        .context("checkout-index (restore) failed to run")?;
    if !status.success() {
        return Err(anyhow!("git checkout-index failed"));
    }

    // Delete files in the working tree that aren't in the snapshot.
    use std::collections::HashSet;
    let snap_set: HashSet<String> = repo
        .list_tree_paths(&rec.tree_sha, &[])?
        .into_iter()
        .collect();
    let working = repo.list_working_paths(&[])?;
    for w in working {
        if !snap_set.contains(&w) {
            let abs = repo.root().join(&w);
            let _ = std::fs::remove_file(&abs);
        }
    }

    let _ = std::fs::remove_file(&tmp_index);
    Ok(())
}

/// Show diff between the snapshot and the current working tree.
///
/// We capture the working tree to a tree object first so the diff includes
/// untracked files (`git diff <tree>` only considers tracked content).
pub fn diff(repo: &GitRepo, rec: &SnapshotRecord) -> Result<()> {
    let current_tree = repo.capture_tree()?;
    let status = repo
        .git()
        .args(["diff", &rec.tree_sha, &current_tree])
        .status()
        .context("git diff failed to run")?;
    if !status.success() {
        return Err(anyhow!("git diff failed"));
    }
    Ok(())
}

/// Per-file summary of what `to <id>` would change.
///
/// Returns `(letter, path)` pairs where the letter follows
/// `git diff --name-status` conventions (`A`, `M`, `D`, `R`, …). The
/// comparison is from the *current working tree* to the snapshot — i.e.
/// these are the files that would change back to the snapshot's version
/// if you ran `claude-oops to <id>`.
pub fn show_files(repo: &GitRepo, rec: &SnapshotRecord) -> Result<Vec<(char, String)>> {
    let current_tree = repo.capture_tree()?;
    repo.name_status(&current_tree, &rec.tree_sha)
}

/// Delete the snapshot ref + remove from the index.
pub fn drop(repo: &GitRepo, id: &str) -> Result<SnapshotRecord> {
    let mut all = storage::read_all(repo)?;
    let rec = storage::find_by_id(&all, id)?.clone();
    if repo.ref_exists(&rec.id) {
        repo.delete_ref(&rec.id)?;
    }
    all.retain(|r| r.id != rec.id);
    storage::rewrite(repo, &all)?;
    Ok(rec)
}
