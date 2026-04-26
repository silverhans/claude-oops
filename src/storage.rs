//! Snapshot index — append-only JSONL at `.git/claude-oops/index.jsonl`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::git::GitRepo;

/// One row in the index. Stored as one JSON object per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// Short id — first 7 chars of `stash_sha` (extended on collision).
    pub id: String,
    /// Full SHA of the snapshot commit (a stash commit, or HEAD if clean).
    pub stash_sha: String,
    /// Tree SHA — used for the idempotency check.
    pub tree_sha: String,
    /// What caused this snapshot. Free-form but conventionally one of:
    /// `manual`, `session-start`, `pre-edit`, `pre-bash`.
    pub trigger: String,
    /// Optional human-readable message.
    pub message: Option<String>,
    /// Unix epoch seconds.
    pub timestamp: i64,
    /// Lines added in this snapshot's diff vs HEAD.
    pub files_added: u32,
    /// Lines deleted in this snapshot's diff vs HEAD.
    pub files_deleted: u32,
    /// True if the working tree was clean at snapshot time
    /// (snapshot points at HEAD instead of a stash commit).
    pub clean: bool,
}

/// Path to the index file inside the repo's `.git` dir.
pub fn index_path(repo: &GitRepo) -> Result<PathBuf> {
    Ok(repo.git_dir()?.join("claude-oops").join("index.jsonl"))
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

/// Read the entire index. Missing file = empty list. Invalid lines are skipped
/// with a warning to stderr — we'd rather degrade than panic.
pub fn read_all(repo: &GitRepo) -> Result<Vec<SnapshotRecord>> {
    let path = index_path(repo)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SnapshotRecord>(&line) {
            Ok(rec) => out.push(rec),
            Err(e) => eprintln!(
                "claude-oops: skipping malformed index line {}: {}",
                lineno + 1,
                e
            ),
        }
    }
    Ok(out)
}

/// Append a single record.
pub fn append(repo: &GitRepo, rec: &SnapshotRecord) -> Result<()> {
    let path = index_path(repo)?;
    ensure_parent(&path)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {} for append", path.display()))?;
    let line = serde_json::to_string(rec)?;
    writeln!(f, "{}", line)?;
    Ok(())
}

/// Rewrite the index with the given records (used by `drop` and `clean`).
pub fn rewrite(repo: &GitRepo, recs: &[SnapshotRecord]) -> Result<()> {
    let path = index_path(repo)?;
    ensure_parent(&path)?;
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f =
            File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        for rec in recs {
            let line = serde_json::to_string(rec)?;
            writeln!(f, "{}", line)?;
        }
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

/// Pick a unique 7-char (or longer) id from a sha, avoiding collisions in
/// the existing index.
pub fn pick_id(sha: &str, existing: &[SnapshotRecord]) -> String {
    for len in 7..=sha.len() {
        let candidate = &sha[..len];
        if !existing.iter().any(|r| r.id == candidate) {
            return candidate.to_string();
        }
    }
    sha.to_string()
}

/// Find a record by id — accepts unambiguous prefixes too.
pub fn find_by_id<'a>(recs: &'a [SnapshotRecord], needle: &str) -> Result<&'a SnapshotRecord> {
    let exact: Vec<&SnapshotRecord> = recs.iter().filter(|r| r.id == needle).collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    let prefix: Vec<&SnapshotRecord> = recs.iter().filter(|r| r.id.starts_with(needle)).collect();
    match prefix.len() {
        0 => Err(anyhow::anyhow!("no snapshot matches id `{}`", needle)),
        1 => Ok(prefix[0]),
        n => Err(anyhow::anyhow!(
            "ambiguous id `{}` matches {} snapshots",
            needle,
            n
        )),
    }
}
