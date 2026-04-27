//! Claude Code hook integration.
//!
//! Two responsibilities:
//!
//! 1. `install` / `uninstall` — patch `~/.claude/settings.json` to register
//!    the hooks that drive auto-snapshots.
//! 2. `_hook-pre-tool-use` — the binary entry point Claude Code invokes on
//!    every Edit/Write/Bash. It reads the hook payload from stdin, decides
//!    whether a snapshot is warranted, and triggers one.
//!
//! ### settings.json schema
//!
//! Claude Code expects nested hook entries under
//! `hooks.<EventName>[].hooks[]`:
//!
//! ```json
//! {
//!   "hooks": {
//!     "SessionStart": [
//!       { "matcher": "*", "hooks": [{ "type": "command",
//!         "command": "claude-oops snap --trigger session-start --quiet" }] }
//!     ],
//!     "PreToolUse": [
//!       { "matcher": "Edit|Write|Bash", "hooks": [{ "type": "command",
//!         "command": "claude-oops _hook-pre-tool-use" }] }
//!     ]
//!   }
//! }
//! ```
//!
//! ### stdin payload (PreToolUse)
//!
//! ```json
//! {
//!   "tool_name": "Bash",
//!   "tool_input": { "command": "rm -rf node_modules" }
//! }
//! ```

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use crate::git::GitRepo;
use crate::snapshot::{self, SnapOpts};
use crate::storage;

/// Cooldown for auto-snapshots on Edit/Write — see brief, "smart, not noisy".
const EDIT_WRITE_COOLDOWN: Duration = Duration::from_secs(120);

/// Resolve the path to the user's Claude Code settings file.
/// Override via `CLAUDE_OOPS_SETTINGS` for tests.
pub fn settings_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_OOPS_SETTINGS") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Resolve the directory where Claude Code looks for slash commands.
/// Override via `CLAUDE_OOPS_COMMANDS_DIR` for tests.
pub fn commands_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_OOPS_COMMANDS_DIR") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    Ok(home.join(".claude").join("commands"))
}

/// Markdown body of the `/oops` slash command we ship with `install`.
const OOPS_SLASH_COMMAND: &str = r#"---
description: List claude-oops snapshots and restore one
argument-hint: "[snapshot-id]"
allowed-tools: Bash(claude-oops *)
---

!`claude-oops list`

If the user supplied a snapshot id ($ARGUMENTS):

1. First run `claude-oops show $ARGUMENTS` to see which files would change on restore.
2. Summarize that list for the user (a few examples + total count) so they know
   what's about to be overwritten.
3. Ask the user to confirm the restore. Only after they say yes, run
   `claude-oops to $ARGUMENTS --force` to actually restore.

If no id was supplied:
- Show the list above and ask which snapshot to restore.
- Once they pick one, follow the same `show` → confirm → `to <id> --force` flow.

Always pass `--force` on the actual `to` command — confirmation already happened
in the conversation, and the slash command runs without a TTY (so the binary's
own confirm prompt would just abort).
"#;

/// Load settings.json, defaulting to an empty object if missing.
fn load_settings(path: &std::path::Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("{} is not valid JSON", path.display()))
}

/// Write settings.json atomically (write-then-rename), pretty-printed.
fn save_settings(path: &std::path::Path, v: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let pretty = serde_json::to_string_pretty(v)?;
    std::fs::write(&tmp, format!("{pretty}\n"))
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn ensure_array<'a>(parent: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    let obj = parent.as_object_mut().expect("expected object");
    let entry = obj.entry(key).or_insert_with(|| json!([]));
    if !entry.is_array() {
        *entry = json!([]);
    }
    entry.as_array_mut().expect("just ensured array")
}

/// Hook entries we own. Each is `(event_name, matcher, command)`.
fn our_hooks() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "SessionStart",
            "*",
            "claude-oops snap --trigger session-start --quiet",
        ),
        (
            "PreToolUse",
            "Edit|Write|Bash",
            "claude-oops _hook-pre-tool-use",
        ),
    ]
}

/// Returns true if `entry` is a hook block we own — i.e. its inner
/// `hooks[*].command` starts with `claude-oops`.
fn entry_is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .map(|c| c.trim_start().starts_with("claude-oops"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Path to the `/oops` slash command file.
fn slash_command_path() -> Result<PathBuf> {
    Ok(commands_dir()?.join("oops.md"))
}

/// Write `/oops` slash command. Returns the path written.
fn install_slash_command() -> Result<PathBuf> {
    let path = slash_command_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, OOPS_SLASH_COMMAND)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Remove the `/oops` slash command if we wrote it. We check the contents
/// match what we ship, so we don't clobber a user's customized version.
fn uninstall_slash_command() -> Result<Option<PathBuf>> {
    let path = slash_command_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current == OOPS_SLASH_COMMAND {
        std::fs::remove_file(&path).ok();
        Ok(Some(path))
    } else {
        // User edited it — leave it alone.
        Ok(None)
    }
}

/// Result of `install` — paths we touched, for status output.
pub struct InstallReport {
    pub settings: PathBuf,
    pub slash_command: PathBuf,
}

/// Install everything: settings.json hooks + `/oops` slash command.
/// Idempotent: safe to run twice.
pub fn install() -> Result<InstallReport> {
    let settings = install_settings()?;
    let slash_command = install_slash_command()?;
    Ok(InstallReport {
        settings,
        slash_command,
    })
}

/// Patch settings.json with our hook entries. Idempotent.
fn install_settings() -> Result<PathBuf> {
    let path = settings_path()?;
    let mut settings = load_settings(&path)?;
    if !settings.is_object() {
        return Err(anyhow!(
            "{} top-level is not a JSON object — refusing to overwrite",
            path.display()
        ));
    }

    // Make sure `hooks` exists and is an object.
    {
        let obj = settings.as_object_mut().unwrap();
        let h = obj.entry("hooks").or_insert_with(|| json!({}));
        if !h.is_object() {
            return Err(anyhow!(
                "settings.hooks exists but is not an object — refusing to overwrite"
            ));
        }
    }

    let hooks_obj = settings.get_mut("hooks").unwrap();

    for (event, matcher, command) in our_hooks() {
        let arr = ensure_array(hooks_obj, event);
        // Drop any prior entry of ours so we end up with exactly one.
        arr.retain(|e| !entry_is_ours(e));
        arr.push(json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": command }],
        }));
    }

    save_settings(&path, &settings)?;
    Ok(path)
}

/// What `uninstall` removed.
pub struct UninstallReport {
    pub settings: PathBuf,
    pub removed_slash_command: Option<PathBuf>,
}

/// Remove only the hook entries we previously installed, plus the `/oops`
/// slash command if its contents are unmodified.
pub fn uninstall() -> Result<UninstallReport> {
    let settings = settings_path()?;
    if settings.exists() {
        let mut s = load_settings(&settings)?;
        if let Some(hooks_obj) = s.get_mut("hooks").and_then(|v| v.as_object_mut()) {
            for arr in hooks_obj.values_mut() {
                if let Some(items) = arr.as_array_mut() {
                    items.retain(|e| !entry_is_ours(e));
                }
            }
        }
        save_settings(&settings, &s)?;
    }
    let removed_slash_command = uninstall_slash_command()?;
    Ok(UninstallReport {
        settings,
        removed_slash_command,
    })
}

/// Bash commands matching this regex set get snapshotted before they run.
/// Curated rather than configurable — list grows in response to real footguns.
fn is_dangerous_bash(cmd: &str) -> bool {
    // We're matching loosely: a command contains any of these tokens, ignoring
    // whitespace variations. The cost of a false positive is one extra snapshot
    // (cheap); the cost of a false negative is data loss.
    let needles: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "rm -r ",
        "rm -f ",
        "rm -rf=",
        "mv -f",
        " dd ",
        "git reset --hard",
        "git clean",
        "git checkout --",
        "git checkout .",
        "git restore .",
        "> /dev/sd",
        "mkfs",
        "shred",
    ];
    let normalized = format!(" {} ", cmd);
    if needles.iter().any(|n| normalized.contains(n)) {
        return true;
    }
    // `find ... -delete` regardless of where -delete falls in the chain.
    if normalized.contains(" find ") && normalized.contains(" -delete") {
        return true;
    }
    // `xargs rm` is a common destructive pipeline.
    if normalized.contains("xargs") && normalized.contains(" rm") {
        return true;
    }
    false
}

/// Decide whether a PreToolUse hook event warrants a snapshot.
/// Returns `Some(trigger_label)` if yes.
fn classify_pre_tool_use(payload: &Value) -> Option<&'static str> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Some(snapshot::trigger::PRE_EDIT),
        "Bash" => {
            let cmd = payload
                .get("tool_input")
                .and_then(|t| t.get("command"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if is_dangerous_bash(cmd) {
                Some(snapshot::trigger::PRE_BASH)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Entry point for `claude-oops _hook-pre-tool-use`. Reads JSON from stdin,
/// decides whether to snapshot, and exits 0 in all non-fatal cases (we never
/// want to block Claude Code on a snapshot failure).
pub fn run_pre_tool_use_hook() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok();
    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(_) => return Ok(()), // Bad input → quietly do nothing.
    };
    let Some(trigger) = classify_pre_tool_use(&payload) else {
        return Ok(());
    };

    // The hook runs in the project root that Claude Code is operating on.
    // Prefer the payload's `cwd` field; fall back to our own.
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let repo = match GitRepo::discover(&cwd) {
        Ok(r) => r,
        Err(_) => return Ok(()), // Not in a git repo → do nothing.
    };

    // Edit/Write cooldown: skip if the most recent snapshot was very recent.
    if trigger == snapshot::trigger::PRE_EDIT {
        if let Ok(recs) = storage::read_all(&repo) {
            if let Some(last) = recs.last() {
                let now = chrono::Utc::now().timestamp();
                let age = now.saturating_sub(last.timestamp);
                if age < EDIT_WRITE_COOLDOWN.as_secs() as i64 {
                    return Ok(());
                }
            }
        }
    }

    // Build a short message describing what the hook saw, so the user knows
    // *why* a snapshot was taken. For Bash, that's the command itself.
    let msg = match trigger {
        t if t == snapshot::trigger::PRE_BASH => payload
            .get("tool_input")
            .and_then(|t| t.get("command"))
            .and_then(Value::as_str)
            .map(|c| truncate_one_line(c, 80)),
        t if t == snapshot::trigger::PRE_EDIT => payload
            .get("tool_input")
            .and_then(|t| t.get("file_path"))
            .and_then(Value::as_str)
            .map(|p| p.to_string()),
        _ => None,
    };

    // Best-effort. Failures to snapshot must NEVER block Claude Code.
    let outcome = snapshot::snap(
        &repo,
        SnapOpts {
            trigger,
            message: msg,
            force: false,
        },
    );

    // Visibility: write a one-liner to stderr so users see hooks fire.
    // stderr is shown in Claude Code's hook output without being parsed as
    // a control response (stdout JSON is reserved for permission decisions).
    if let Ok(crate::snapshot::SnapOutcome::Created(rec)) = outcome {
        let stats = if rec.clean {
            "clean".to_string()
        } else {
            format!("+{}/-{}", rec.files_added, rec.files_deleted)
        };
        let suffix = rec
            .message
            .as_deref()
            .map(|m| format!(" — {}", m))
            .unwrap_or_default();
        eprintln!(
            "📸 claude-oops: {} ({}, {}){}",
            rec.id, rec.trigger, stats, suffix
        );
    }
    Ok(())
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let one_line: String = s.chars().take_while(|c| *c != '\n').collect();
    if one_line.chars().count() <= max {
        one_line
    } else {
        let mut out: String = one_line.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_bash_recognized() {
        assert!(is_dangerous_bash("rm -rf node_modules"));
        assert!(is_dangerous_bash("cd /tmp && rm -rf ./build"));
        assert!(is_dangerous_bash("git reset --hard HEAD~5"));
        assert!(is_dangerous_bash("git clean -fd"));
        assert!(is_dangerous_bash("find . -name '*.log' -delete"));
        assert!(is_dangerous_bash("ls | xargs rm"));
        assert!(is_dangerous_bash("mkfs.ext4 /dev/sda1"));
    }

    #[test]
    fn safe_bash_not_flagged() {
        // The matcher errs on the side of over-snapshotting — quoted strings
        // that *contain* "rm -rf" will trigger, which is fine.
        assert!(!is_dangerous_bash("ls -la"));
        assert!(!is_dangerous_bash("cargo test"));
        assert!(!is_dangerous_bash("git status"));
        assert!(!is_dangerous_bash("npm install"));
        assert!(!is_dangerous_bash("git reset --soft HEAD~1"));
    }

    #[test]
    fn classify_edit_returns_pre_edit() {
        let p = json!({"tool_name": "Edit", "tool_input": {}});
        assert_eq!(classify_pre_tool_use(&p), Some("pre-edit"));
    }

    #[test]
    fn classify_safe_bash_returns_none() {
        let p = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
        assert_eq!(classify_pre_tool_use(&p), None);
    }

    #[test]
    fn classify_dangerous_bash_returns_pre_bash() {
        let p = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /tmp/foo"}
        });
        assert_eq!(classify_pre_tool_use(&p), Some("pre-bash"));
    }

    #[test]
    fn entry_is_ours_detects_claude_oops_command() {
        let ours = json!({
            "matcher": "*",
            "hooks": [{"type": "command", "command": "claude-oops snap"}]
        });
        let theirs = json!({
            "matcher": "*",
            "hooks": [{"type": "command", "command": "echo hello"}]
        });
        assert!(entry_is_ours(&ours));
        assert!(!entry_is_ours(&theirs));
    }
}
