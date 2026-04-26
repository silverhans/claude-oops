# claude-oops

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

## Install

```bash
cargo install claude-oops
claude-oops install   # patches ~/.claude/settings.json with hooks
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
claude-oops diff abc123f         # what changed since the snapshot
claude-oops to abc123f           # restore (asks for confirmation)
claude-oops to abc123f --force   # skip the confirmation prompt
```

`to` updates the working tree only — your HEAD and commit history stay put.
The snapshot itself isn't consumed; you can restore again, or jump to a
different one.

## Commands

| Command | Description |
| --- | --- |
| `snap [-m MSG]`  | Take a manual snapshot (no idempotency check). |
| `list [--json] [--limit N]` | List snapshots in this repo. |
| `diff <id>`      | Diff working tree vs snapshot. |
| `to <id> [-f]`   | Restore working tree to snapshot. |
| `drop <id>`      | Delete a snapshot ref + remove from index. |
| `clean`          | Apply retention: keep last 30 OR < 7 days old. |
| `install`        | Add hooks to `~/.claude/settings.json`. |
| `uninstall`      | Remove only the hooks we installed. |
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
git clone https://github.com/<user>/claude-oops
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
> Source: <https://github.com/USER/claude-oops>. Built in Rust, single
> binary, ~1 MB. Storage is git refs, so it's local, free, and survives
> reboots.
>
> Happy to take design feedback — particularly on the hook trigger
> heuristics and the dangerous-bash pattern list.

</details>
