//! Snapshot retention policy.
//!
//! Default policy: keep a snapshot if either condition holds —
//! it is among the last 30 by recency, OR it is less than 7 days old.
//! Whichever set is larger wins; we delete only what falls outside *both*.

use anyhow::Result;
use chrono::Utc;

use crate::git::GitRepo;
use crate::storage::{self, SnapshotRecord};

const KEEP_LAST_N: usize = 30;
const KEEP_DAYS: i64 = 7;

/// Outcome of a retention pass.
pub struct CleanReport {
    pub kept: usize,
    pub deleted: Vec<String>,
}

/// Apply the retention policy. Mutates the index and deletes refs.
pub fn clean(repo: &GitRepo) -> Result<CleanReport> {
    let all = storage::read_all(repo)?;
    let now = Utc::now().timestamp();
    let cutoff = now - (KEEP_DAYS * 24 * 60 * 60);

    let total = all.len();
    let last_n_start = total.saturating_sub(KEEP_LAST_N);

    let mut keep: Vec<SnapshotRecord> = Vec::with_capacity(total);
    let mut deleted_ids: Vec<String> = Vec::new();
    for (idx, rec) in all.iter().enumerate() {
        let recent_enough = rec.timestamp >= cutoff;
        let in_last_n = idx >= last_n_start;
        if recent_enough || in_last_n {
            keep.push(rec.clone());
        } else {
            // Best effort — if the ref is already gone, that's fine.
            let _ = repo.delete_ref(&rec.id);
            deleted_ids.push(rec.id.clone());
        }
    }

    if !deleted_ids.is_empty() {
        storage::rewrite(repo, &keep)?;
    }
    Ok(CleanReport {
        kept: keep.len(),
        deleted: deleted_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_rec(id: &str, age_days: i64) -> SnapshotRecord {
        SnapshotRecord {
            id: id.to_string(),
            stash_sha: format!("{}aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", id),
            tree_sha: format!("{}bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", id),
            trigger: "manual".to_string(),
            message: None,
            timestamp: Utc::now().timestamp() - age_days * 24 * 60 * 60,
            files_added: 0,
            files_deleted: 0,
            clean: true,
        }
    }

    /// The decision logic, factored out so we can unit-test it without git.
    fn decide(
        records: &[SnapshotRecord],
        keep_last_n: usize,
        keep_days: i64,
    ) -> (Vec<String>, Vec<String>) {
        let now = Utc::now().timestamp();
        let cutoff = now - (keep_days * 24 * 60 * 60);
        let total = records.len();
        let last_n_start = total.saturating_sub(keep_last_n);
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for (idx, rec) in records.iter().enumerate() {
            if rec.timestamp >= cutoff || idx >= last_n_start {
                kept.push(rec.id.clone());
            } else {
                dropped.push(rec.id.clone());
            }
        }
        (kept, dropped)
    }

    #[test]
    fn keep_recent_even_if_beyond_n() {
        // 50 snaps, all 1 day old — n=30 limit doesn't matter, all are recent.
        let recs: Vec<_> = (0..50).map(|i| make_rec(&format!("r{i}"), 1)).collect();
        let (kept, dropped) = decide(&recs, 30, 7);
        assert_eq!(kept.len(), 50);
        assert!(dropped.is_empty());
    }

    #[test]
    fn keep_last_n_even_if_old() {
        // 50 snaps, all 100 days old. We keep the last 30.
        let recs: Vec<_> = (0..50).map(|i| make_rec(&format!("o{i}"), 100)).collect();
        let (kept, dropped) = decide(&recs, 30, 7);
        assert_eq!(kept.len(), 30);
        assert_eq!(dropped.len(), 20);
        // The dropped should be the oldest (first 20 in append order).
        for i in 0..20 {
            assert!(dropped.contains(&format!("o{i}")));
        }
    }

    #[test]
    fn delete_when_outside_both_windows() {
        // 5 old (100d), then 10 fresh (1d).
        let mut recs: Vec<_> = (0..5).map(|i| make_rec(&format!("old{i}"), 100)).collect();
        recs.extend((0..10).map(|i| make_rec(&format!("new{i}"), 1)));
        // last_n=10: indexes 5..15 are kept. The 5 old are at indexes 0..5,
        // ALL of them get dropped.
        let (kept, dropped) = decide(&recs, 10, 7);
        assert_eq!(kept.len(), 10);
        assert_eq!(dropped.len(), 5);
    }

    #[test]
    fn empty_index_no_op() {
        let (kept, dropped) = decide(&[], 30, 7);
        assert!(kept.is_empty());
        assert!(dropped.is_empty());
    }
}
