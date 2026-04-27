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

/// Lexically resolve `path` (relative to `cwd`) into a repo-root-relative
/// string, without touching the filesystem (so it works even for paths
/// that don't exist anymore — restore is the whole point).
///
/// Returns `Err` if the path escapes the repo.
pub fn resolve_path(cwd: &Path, repo_root: &Path, path: &str) -> Result<String> {
    let pb = PathBuf::from(path);
    let abs = if pb.is_absolute() { pb } else { cwd.join(&pb) };
    let cleaned = lexical_clean(&abs);
    let rel = cleaned.strip_prefix(repo_root).map_err(|_| {
        anyhow!(
            "{} resolves to {}, which is outside the repo at {}",
            path,
            cleaned.display(),
            repo_root.display()
        )
    })?;
    if rel.as_os_str().is_empty() {
        // The user gave the repo root itself — treat as "everything".
        return Ok(String::new());
    }
    // Always emit forward slashes, regardless of OS — git's pathspecs use
    // `/` even on Windows, and our index/refs are populated by git too.
    Ok(rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

/// Collapse `.` and `..` components purely lexically (no filesystem hits).
fn lexical_clean(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp),
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::ParentDir) | None => out.push(comp),
                Some(Component::Prefix(_)) | Some(Component::RootDir) => {
                    // Can't go above root — silently ignore.
                }
                Some(Component::CurDir) => unreachable!(),
            },
            Component::Normal(_) => out.push(comp),
        }
    }
    out.iter().map(|c| c.as_os_str()).collect()
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
/// Strategy: `git read-tree -m -u <tree>` updates the index and working tree
/// to match the snapshot's tree, then `git checkout-index -a -f` makes the
/// working tree exactly match. HEAD is *not* moved; commit history is
/// untouched. Local file changes that conflict with the snapshot will be
/// overwritten — callers should confirm before invoking this.
pub fn restore(repo: &GitRepo, rec: &SnapshotRecord) -> Result<()> {
    // Use the tree directly so this works whether the snapshot points at a
    // stash commit or at HEAD.
    let tree = &rec.tree_sha;

    let status = repo
        .git()
        .args(["read-tree", "-m", "-u", tree])
        .status()
        .context("git read-tree failed to run")?;
    if !status.success() {
        return Err(anyhow!(
            "git read-tree failed — working tree may have conflicting local changes"
        ));
    }
    let status = repo
        .git()
        .args(["checkout-index", "-a", "-f"])
        .status()
        .context("git checkout-index failed to run")?;
    if !status.success() {
        return Err(anyhow!("git checkout-index failed"));
    }
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
