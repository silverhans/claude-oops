//! Entry point — parses CLI args and dispatches to subcommand handlers.

mod cli;
mod format;
mod git;
mod hooks;
mod retention;
mod snapshot;
mod storage;

use anyhow::{Context, Result};
use clap::Parser;
use std::io::Write;

use crate::cli::{Cli, Cmd};
use crate::git::GitRepo;
use crate::snapshot::{SnapOpts, SnapOutcome};

fn main() {
    if let Err(e) = run() {
        eprintln!("claude-oops: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Cli::parse();
    let cwd = std::env::current_dir().context("could not read current directory")?;

    match args.command {
        Cmd::Snap {
            message,
            trigger,
            quiet,
        } => {
            // SessionStart hook fires `snap --quiet`; outside a git repo
            // we should exit silently rather than failing the hook.
            let repo = match GitRepo::discover(&cwd) {
                Ok(r) => r,
                Err(e) => {
                    if !quiet {
                        return Err(e);
                    }
                    return Ok(());
                }
            };
            let outcome = snapshot::snap(
                &repo,
                SnapOpts {
                    trigger: &trigger,
                    message,
                    force: trigger == "manual",
                },
            )?;
            match outcome {
                SnapOutcome::Created(rec) if !quiet => {
                    println!(
                        "snapshot {} ({}, {})",
                        rec.id,
                        rec.trigger,
                        if rec.clean {
                            "clean tree".to_string()
                        } else {
                            format!("+{}/-{}", rec.files_added, rec.files_deleted)
                        }
                    );
                }
                SnapOutcome::Skipped(rec) if !quiet => {
                    println!("no change since {} ({})", rec.id, rec.trigger);
                }
                SnapOutcome::NoCommits if !quiet => {
                    println!("no commits in repo yet — nothing to snapshot");
                }
                _ => {}
            }
            Ok(())
        }

        Cmd::List { json, limit } => {
            // `list` is a read-only query — outside a git repo, just say so
            // and exit 0, so that integrations like the /oops slash command
            // don't blow up in non-git projects.
            let repo = match GitRepo::discover(&cwd) {
                Ok(r) => r,
                Err(_) => {
                    if json {
                        println!("[]");
                    } else {
                        println!(
                            "no snapshots — this directory is not a git repository \
                             (run `git init` to enable claude-oops)"
                        );
                    }
                    return Ok(());
                }
            };
            let mut recs = storage::read_all(&repo)?;
            if let Some(n) = limit {
                if recs.len() > n {
                    let start = recs.len() - n;
                    recs = recs.split_off(start);
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&recs)?);
            } else if recs.is_empty() {
                println!("no snapshots yet — run `claude-oops snap` to take one");
            } else {
                println!("{}", format::list_table(&recs));
            }
            Ok(())
        }

        Cmd::Diff { id } => {
            let repo = GitRepo::discover(&cwd)?;
            let recs = storage::read_all(&repo)?;
            let rec = storage::find_by_id(&recs, &id)?.clone();
            snapshot::diff(&repo, &rec)
        }

        Cmd::Show { id } => {
            let repo = GitRepo::discover(&cwd)?;
            let recs = storage::read_all(&repo)?;
            let rec = storage::find_by_id(&recs, &id)?.clone();
            let rows = snapshot::show_files(&repo, &rec)?;
            println!("{}", format::show_files_block(&rows));
            Ok(())
        }

        Cmd::To { id, force, paths } => {
            let repo = GitRepo::discover(&cwd)?;
            let recs = storage::read_all(&repo)?;
            let rec = storage::find_by_id(&recs, &id)?.clone();
            let label = rec.message.as_deref().unwrap_or(&rec.trigger);

            if paths.is_empty() {
                // Whole-tree restore (legacy behavior).
                if !force
                    && !confirm(&format!(
                        "Restore working tree to snapshot {} ({})? Local changes will be overwritten.",
                        rec.id, label,
                    ))?
                {
                    println!("aborted");
                    return Ok(());
                }
                snapshot::restore(&repo, &rec)?;
                println!("restored to {}", rec.id);
            } else {
                // Per-file restore.
                let resolved: Vec<String> = paths
                    .iter()
                    .map(|p| snapshot::resolve_path(&repo, &cwd, p))
                    .collect::<Result<Vec<_>>>()?;
                if !force
                    && !confirm(&format!(
                        "Restore {} path{} from snapshot {} ({})? Working-tree versions will be overwritten.",
                        resolved.len(),
                        if resolved.len() == 1 { "" } else { "s" },
                        rec.id,
                        label,
                    ))?
                {
                    println!("aborted");
                    return Ok(());
                }
                let report = snapshot::restore_paths(&repo, &rec, &resolved)?;
                println!(
                    "restored {} file(s), deleted {} file(s) from {}",
                    report.restored.len(),
                    report.deleted.len(),
                    rec.id
                );
            }
            Ok(())
        }

        Cmd::Drop { id } => {
            let repo = GitRepo::discover(&cwd)?;
            let rec = snapshot::drop(&repo, &id)?;
            println!("dropped {}", rec.id);
            Ok(())
        }

        Cmd::Clean => {
            let repo = GitRepo::discover(&cwd)?;
            let report = retention::clean(&repo)?;
            println!(
                "kept {} snapshots, deleted {}",
                report.kept,
                report.deleted.len()
            );
            Ok(())
        }
        Cmd::Install => {
            let report = hooks::install()?;
            println!("hooks       → {}", report.settings.display());
            println!("/oops cmd   → {}", report.slash_command.display());
            Ok(())
        }
        Cmd::Uninstall => {
            let report = hooks::uninstall()?;
            println!("hooks removed from {}", report.settings.display());
            match report.removed_slash_command {
                Some(p) => println!("/oops cmd removed: {}", p.display()),
                None => println!("/oops cmd: not removed (missing or user-modified)"),
            }
            Ok(())
        }
        Cmd::Status => {
            let repo = match GitRepo::discover(&cwd) {
                Ok(r) => r,
                Err(_) => {
                    println!(
                        "not a git repository — claude-oops is dormant here \
                         (run `git init` to enable it)"
                    );
                    return Ok(());
                }
            };
            let recs = storage::read_all(&repo)?;
            let index_bytes = storage::index_path(&repo)
                .ok()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .unwrap_or(0);
            println!("{}", format::status_summary(&recs, index_bytes));
            Ok(())
        }
        Cmd::HookPreToolUse => hooks::run_pre_tool_use_hook(),
    }
}

/// Y/n prompt. Returns true on yes.
fn confirm(msg: &str) -> Result<bool> {
    print!("{} [y/N] ", msg);
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let answer = buf.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}
