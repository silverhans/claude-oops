//! Thin wrapper over the `git` subprocess.
//!
//! We shell out instead of linking libgit2 — keeps the binary small and
//! avoids the libgit2 build pain. All operations run in a specified repo
//! root.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Handle to a git working tree we operate on.
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    /// Discover the repo containing `start`. Returns `Err` if not in a repo.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let out = Command::new("git")
            .arg("-C")
            .arg(start.as_ref())
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("failed to invoke `git rev-parse`")?;
        if !out.status.success() {
            return Err(anyhow!(
                "not inside a git repository (run `git init` first)"
            ));
        }
        let root = String::from_utf8(out.stdout)
            .context("git emitted non-utf8 path")?
            .trim()
            .to_string();
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    /// Absolute path to the repo's working tree root.
    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.git` directory (resolved — works for worktrees too).
    pub fn git_dir(&self) -> Result<PathBuf> {
        let out = self
            .git()
            .args(["rev-parse", "--git-dir"])
            .output()
            .context("failed to invoke `git rev-parse --git-dir`")?;
        if !out.status.success() {
            return Err(anyhow!("git rev-parse --git-dir failed"));
        }
        let raw = String::from_utf8(out.stdout)
            .context("non-utf8 .git path")?
            .trim()
            .to_string();
        let p = PathBuf::from(&raw);
        Ok(if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        })
    }

    /// Build a `git -C <root>` command.
    pub fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.root);
        c
    }

    /// Returns true if HEAD resolves (the repo has at least one commit).
    pub fn has_head(&self) -> bool {
        self.git()
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// SHA of HEAD, or `None` if the repo has no commits yet.
    pub fn head_sha(&self) -> Result<Option<String>> {
        if !self.has_head() {
            return Ok(None);
        }
        let out = self
            .git()
            .args(["rev-parse", "HEAD"])
            .output()
            .context("rev-parse HEAD failed")?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8(out.stdout)?.trim().to_string()))
    }

    /// Capture the current working tree (tracked + untracked, respecting
    /// `.gitignore`) as a tree object — without disturbing the user's index.
    ///
    /// We can't use `git stash create` because it ignores untracked files,
    /// and `git stash push -u` mutates the working tree. Instead we build
    /// a private index in `.git/claude-oops/tmp-index`, `git add -A` into it,
    /// and `git write-tree` to materialize the tree.
    pub fn capture_tree(&self) -> Result<String> {
        let tmp_dir = self.git_dir()?.join("claude-oops");
        std::fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("failed to create {}", tmp_dir.display()))?;
        let tmp_index = tmp_dir.join("tmp-index");
        // Stale tmp index from a crashed prior run would just be overwritten
        // by read-tree, but be tidy.
        let _ = std::fs::remove_file(&tmp_index);

        let mut read = self.git();
        read.env("GIT_INDEX_FILE", &tmp_index)
            .args(["read-tree", "HEAD"]);
        let status = read.status().context("read-tree failed to run")?;
        if !status.success() {
            return Err(anyhow!("git read-tree HEAD failed"));
        }

        let mut add = self.git();
        add.env("GIT_INDEX_FILE", &tmp_index).args(["add", "-A"]);
        let status = add.status().context("git add -A failed to run")?;
        if !status.success() {
            return Err(anyhow!("git add -A failed"));
        }

        let mut write = self.git();
        write.env("GIT_INDEX_FILE", &tmp_index).args(["write-tree"]);
        let out = write.output().context("git write-tree failed to run")?;
        let _ = std::fs::remove_file(&tmp_index);
        if !out.status.success() {
            return Err(anyhow!(
                "git write-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    /// Build a commit pointing at `tree`, with `parent` as its sole parent
    /// and `message` as its commit message. Returns the new commit SHA.
    pub fn commit_tree(&self, tree: &str, parent: &str, message: &str) -> Result<String> {
        let out = self
            .git()
            .args(["commit-tree", tree, "-p", parent, "-m", message])
            .output()
            .context("git commit-tree failed to run")?;
        if !out.status.success() {
            return Err(anyhow!(
                "git commit-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    /// Get the tree SHA of a commit.
    pub fn tree_of(&self, commit: &str) -> Result<String> {
        let out = self
            .git()
            .args(["rev-parse", &format!("{}^{{tree}}", commit)])
            .output()
            .context("rev-parse tree failed")?;
        if !out.status.success() {
            return Err(anyhow!(
                "could not resolve tree of {}: {}",
                commit,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    /// Create or update `refs/claude-oops/<id>` to point at `target`.
    pub fn update_ref(&self, id: &str, target: &str) -> Result<()> {
        let refname = format!("refs/claude-oops/{}", id);
        let status = self
            .git()
            .args(["update-ref", &refname, target])
            .status()
            .context("update-ref failed to run")?;
        if !status.success() {
            return Err(anyhow!("git update-ref {} failed", refname));
        }
        Ok(())
    }

    /// Delete `refs/claude-oops/<id>` if it exists.
    pub fn delete_ref(&self, id: &str) -> Result<()> {
        let refname = format!("refs/claude-oops/{}", id);
        let status = self
            .git()
            .args(["update-ref", "-d", &refname])
            .status()
            .context("update-ref -d failed to run")?;
        if !status.success() {
            return Err(anyhow!("git update-ref -d {} failed", refname));
        }
        Ok(())
    }

    /// True if `refs/claude-oops/<id>` exists.
    pub fn ref_exists(&self, id: &str) -> bool {
        let refname = format!("refs/claude-oops/{}", id);
        self.git()
            .args(["rev-parse", "--verify", "--quiet", &refname])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Count of (added, deleted) lines for `commit` against its first parent
    /// (or against the empty tree if it has none). Used for list output.
    pub fn diff_stats(&self, commit: &str) -> Result<(u32, u32)> {
        // `git show --numstat` emits one line per file: "<added>\t<deleted>\t<path>"
        let out = self
            .git()
            .args(["show", "--numstat", "--format=", "--no-renames", commit])
            .output()
            .context("git show --numstat failed")?;
        if !out.status.success() {
            return Ok((0, 0));
        }
        let mut added = 0u32;
        let mut deleted = 0u32;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut parts = line.split('\t');
            let a = parts.next().unwrap_or("0");
            let d = parts.next().unwrap_or("0");
            // Binary files show "-\t-\t..." — count them as 0.
            added = added.saturating_add(a.parse::<u32>().unwrap_or(0));
            deleted = deleted.saturating_add(d.parse::<u32>().unwrap_or(0));
        }
        Ok((added, deleted))
    }
}
