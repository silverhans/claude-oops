//! Shared scaffolding for integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// A throwaway git repo with one initial commit.
pub struct TempRepo {
    pub dir: TempDir,
}

impl Default for TempRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl TempRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        run(p, &["git", "init", "-q", "-b", "main"]);
        run(p, &["git", "config", "user.email", "test@example.com"]);
        run(p, &["git", "config", "user.name", "Test"]);
        // Need at least one commit for HEAD to resolve.
        std::fs::write(p.join("README.md"), "hello\n").unwrap();
        run(p, &["git", "add", "."]);
        run(p, &["git", "commit", "-q", "-m", "initial"]);
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let p = self.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path().join(rel)).unwrap()
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.path().join(rel).exists()
    }
}

fn run(cwd: &Path, args: &[&str]) {
    let status = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {}", args, e));
    assert!(status.success(), "command failed: {:?}", args);
}

/// Path to the compiled binary.
pub fn bin_path() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by cargo for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_claude-oops"))
}

/// Run the binary in `cwd` with `args`. Returns (stdout, stderr, exit-code).
pub fn run_oops(cwd: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin_path())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run claude-oops");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}
