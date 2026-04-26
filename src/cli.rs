//! CLI surface — clap derive structs.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "claude-oops",
    version,
    about = "Automatic safety net for Claude Code — snapshot before risky ops, restore in seconds.",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Take an explicit snapshot. Always runs (no idempotency check).
    Snap {
        /// Optional human-readable message.
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Override the trigger label (used by hook scripts).
        #[arg(long, default_value = "manual")]
        trigger: String,
        /// Suppress informational output.
        #[arg(long)]
        quiet: bool,
    },

    /// List snapshots for this repo.
    List {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Limit number of rows shown.
        #[arg(long)]
        limit: Option<usize>,
    },

    /// Show diff between working tree and a snapshot.
    Diff {
        /// Snapshot id (or unambiguous prefix).
        id: String,
    },

    /// Show the file-level summary of a snapshot (which files changed).
    Show {
        /// Snapshot id (or unambiguous prefix).
        id: String,
    },

    /// Restore the working tree to a snapshot.
    To {
        /// Snapshot id (or unambiguous prefix).
        id: String,
        /// Skip confirmation prompt.
        #[arg(short = 'f', long, alias = "yes")]
        force: bool,
    },

    /// Delete a snapshot.
    Drop {
        /// Snapshot id.
        id: String,
    },

    /// Apply retention policy: keep last 30 OR last 7 days.
    Clean,

    /// Install hooks into ~/.claude/settings.json.
    Install,

    /// Remove hooks we previously installed.
    Uninstall,

    /// Print project status (count, latest, disk usage).
    Status,

    /// Internal: hook entry point invoked by Claude Code on PreToolUse.
    /// Reads JSON from stdin per the Claude Code hook protocol.
    #[command(name = "_hook-pre-tool-use", hide = true)]
    HookPreToolUse,
}
