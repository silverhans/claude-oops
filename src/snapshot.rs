//! Core snapshot operations: take, restore, diff, drop.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

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
