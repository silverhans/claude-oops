# claude-oops

[![Crates.io](https://img.shields.io/crates/v/claude-oops.svg?logo=rust&color=orange)](https://crates.io/crates/claude-oops)
[![CI](https://github.com/silverhans/claude-oops/actions/workflows/ci.yml/badge.svg)](https://github.com/silverhans/claude-oops/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)](https://github.com/silverhans/claude-oops/releases/latest)

> An undo button for Claude Code.

`claude-oops` takes a git-stash-style snapshot of your working tree before
risky operations and restores it in two seconds when something goes wrong.
Snapshots are stored as git refs — local, free, and gone the moment you
delete the repo.

```text
$ claude-oops list
┌─────────┬────────────┬───────────────┬───────┬─────────────────────────┐
│ ID      │ AGE        │ TRIGGER       │ FILES │ MESSAGE                 │
╞═════════╪════════════╪═══════════════╪═══════╪═════════════════════════╡
│ abc123f │ 2 min ago  │ pre-bash      │ +0/-12│ rm -rf node_modules     │
│ def4561 │ 18 min ago │ pre-edit      │ +3/-1 │ src/auth.rs             │
│ ghi789a │ 1h ago     │ session-start │ —     │ Started: "fix login"    │
│ jkl012b │ 3h ago     │ manual        │ —     │ before refactor         │
└─────────┴────────────┴───────────────┴───────┴─────────────────────────┘

$ claude-oops to abc123f --force
restored to abc123f
```

## Why

Everyone running Claude Code on a real codebase has, at least once, watched it
helpfully clean up a directory by removing files they needed. `git stash` only
covers tracked files; uncommitted untracked work is lost. `git reflog` only
helps if Claude already committed. `claude-oops` snapshots *the entire
working tree* — tracked plus untracked, respecting `.gitignore` — at the
moments where things tend to go wrong.

## "If I need git anyway, why not just use git directly?"

Fair question. claude-oops isn't a storage tool — it's an *automation layer*
on top of git. Storage is git, and that's a feature: free, deduplicated by
the object store, no extra dependency to install. The value is in three
things bare git won't do for you:

1. **Capture untracked files without disturbing the working tree.**
   `git stash` (default) ignores untracked. `git stash -u` captures them but
   resets the working tree — that's not what you want a background tool to
   do silently. claude-oops builds a private temp index and writes a tree
   from it, leaving your real index alone.
2. **Snapshot at the right moments, automatically.** With bare git you'd have
   to remember to `git stash` before every risky operation. Nobody does that.
   claude-oops hooks into Claude Code's `SessionStart` and `PreToolUse` events
   and snapshots before `Edit`/`Write` and before any `Bash` command matching
   a curated dangerous-pattern list — with idempotency + cooldown so it
   doesn't spam.
3. **Restore in two commands without spelunking the reflog.** `claude-oops list`
   gives you a labelled, time-sorted table (with the triggering command);
   `claude-oops to <id>` (or per-file: `to <id> -- src/auth.rs`) puts things
   back. The bare-git equivalent is some combination of `git reflog` +
   `git stash list` + `git checkout-index` + tree-ish syntax — workable when
   you're calm, less so when you've just realized half your repo is gone.

Requiring git isn't really a requirement: git is on every dev machine, and
`git init` takes a second. The point of claude-oops is that storage is the
boring part — automation and ergonomics around it are the actual product.

## Install

Both commands go in your terminal. The first downloads the binary; the
second is a subcommand of that same binary that wires up the hooks.

```bash
cargo install claude-oops          # downloads the binary into ~/.cargo/bin/
claude-oops install                # patches ~/.claude/settings.json with hooks
                                   # and ships ~/.claude/commands/oops.md
```

After `install`, every Claude Code session will:

- snapshot at session start (one baseline);
- snapshot before each `Edit` / `Write` (rate-limited to one snapshot per
  2 minutes);
- snapshot before `Bash` commands matching dangerous patterns
  (`rm -rf`, `git reset --hard`, `find … -delete`, etc.).

You can take a manual snapshot at any time:

```bash
claude-oops snap -m "before refactor"
```

## Restoring

```bash
claude-oops list                 # see what you have
claude-oops show abc123f         # which files would change on restore
claude-oops diff abc123f         # full diff vs the snapshot
claude-oops to abc123f           # restore (asks for confirmation)
claude-oops to abc123f --force   # skip the confirmation prompt
claude-oops to abc123f -- src/auth.rs   # restore one file only
claude-oops to abc123f -- src/          # restore a subtree only
```

Per-file restore leaves everything else in your working tree alone — useful
when only one file went sideways. Files that exist in your working tree but
not in the snapshot are deleted (that's the snapshot's state for that path).

Inside a Claude Code session, type `/oops` instead — the slash command
runs `claude-oops list` and helps you pick a snapshot to restore without
switching to a terminal.

`to` updates the working tree only — your HEAD and commit history stay put.
The snapshot itself isn't consumed; you can restore again, or jump to a
different one.

## Commands

| Command | Description |
| --- | --- |
| `snap [-m MSG]`  | Take a manual snapshot (no idempotency check). |
| `list [--json] [--limit N]` | List snapshots in this repo. |
| `show <id>`      | List the files that would change on restore. |
| `diff <id>`      | Full diff between working tree and snapshot. |
| `to <id> [-f] [-- PATHS]` | Restore working tree (or specific paths). |
| `drop <id>`      | Delete a snapshot ref + remove from index. |
| `clean`          | Apply retention: keep last 30 OR < 7 days old. |
| `install`        | Add hooks to `~/.claude/settings.json` and ship `/oops`. |
| `uninstall`      | Remove the hooks and `/oops` we installed. |
| `status`         | Snapshot count, latest, index size. |

## How it works

- **Storage.** Each snapshot is a real git commit object referenced by
  `refs/claude-oops/<id>`. Git GC won't reap it because the ref keeps it
  alive. A JSONL index at `.git/claude-oops/index.jsonl` records the
  metadata you see in `list`.
- **Capture.** We don't use `git stash create` (it ignores untracked files).
  Instead we build a private temporary index, `git add -A` into it, and
  `git write-tree` — which gives us a tree object containing the entire
  working state without disturbing your real index.
- **Restore.** `git read-tree -m -u <tree>` then `git checkout-index -a -f`.
  HEAD is never moved.
- **Idempotency.** Auto-triggered snapshots compare the captured tree SHA
  against the most recent snapshot's; identical means skip. Manual `snap`
  ignores this — if you ask, you get a snapshot.

## Limits

- Requires a git repo. Snapshots outside a git repo are not supported in
  v0.1 (a tar fallback is on the roadmap).
- `.gitignore`d files are not captured. If `node_modules` is in your
  `.gitignore`, removing it will trigger a snapshot but the snapshot won't
  include the directory itself — restore won't bring it back. (`npm install`
  will, though.)

## Configuration

There is no config file. The defaults are:

- 2-minute cooldown for `Edit`/`Write` snapshots.
- Retention: keep last 30 OR snapshots from the last 7 days, whichever is
  more permissive.
- Settings file location: `~/.claude/settings.json` (override via
  `CLAUDE_OOPS_SETTINGS`).

## Building from source

```bash
git clone https://github.com/silverhans/claude-oops
cd claude-oops
cargo install --path .
```

Quality bar: `cargo test`, `cargo clippy --all-targets -- -D warnings`, and
`cargo fmt --check` are all clean on every commit. CI runs them on macOS,
Linux, and Windows.

## License

MIT.

---

<details>
<summary><b>Show HN draft</b></summary>

> **Show HN: claude-oops — undo button for Claude Code**
>
> I built this after the third time Claude Code helpfully cleaned up my
> repo by removing files I needed. claude-oops takes a git-stash-style
> snapshot before risky operations and restores in two seconds.
>
> Install: `cargo install claude-oops && claude-oops install`. From then
> on every Edit/Write/dangerous Bash gets snapshotted. `claude-oops list`
> shows them; `claude-oops to <id>` restores.
>
> Source: <https://github.com/silverhans/claude-oops>. Built in Rust, single
> binary, ~1 MB. Storage is git refs, so it's local, free, and survives
> reboots.
>
> Happy to take design feedback — particularly on the hook trigger
> heuristics and the dangerous-bash pattern list.

</details>
