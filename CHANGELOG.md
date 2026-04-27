# Changelog

## v0.4.0

Two improvements driven by feedback from early users — both close real
gaps in the snapshot strategy.

- **`Stop` hook: snapshot after every Claude turn.** Up to v0.3.x snapshots
  were strictly *before* risky operations (PreToolUse hook + a curated
  dangerous-bash matcher). That assumed Claude could recognise its own
  destructive moves — which it often can't. The Stop hook fires when
  Claude finishes a turn and is content-agnostic: every completed turn
  becomes an atomic recoverable unit, no matter what was inside.
  Idempotency suppresses snapshots for chat-only turns, so chatty
  sessions don't fill up with noise.
- **Especially valuable in agent-loop setups** where Claude operates
  autonomously without git access. The new framing in the README:
  *git semantics without giving Claude the git CLI* — claude-oops
  provides the recovery layer outside Claude's reach.
- **Expanded dangerous-bash patterns:** `sed -i`, `awk -i inplace`,
  `perl -i` / `perl -pi`, `git apply`, `truncate -s 0`. These were all
  destructive operations the matcher used to miss.

## v0.3.4

Three real bugs found by the first user trying `/oops` on a real project:

- **Restore now actually overwrites conflicting local changes.** Previously
  `claude-oops to <id>` used `git read-tree -m -u`, which is merge-aware
  and refuses to clobber uncommitted local edits — but that's exactly what
  the user just confirmed they want to do. Now we use a private temp
  index + `checkout-index -a -f`, which force-overwrites. Per-file restore
  was already doing this correctly; whole-tree restore now matches.
- **Whole-tree restore deletes working-tree files that aren't in the
  snapshot** (tracked + untracked, respecting `.gitignore`), so the working
  tree ends up matching the snapshot exactly. Previously these files
  were left stranded.
- **`confirm()` errors clearly when stdin is empty + non-TTY**, instead of
  silently aborting. Slash commands invoke the binary without a TTY,
  and the silent abort masqueraded as "user declined". Now: clear
  "pass --force" message.
- **`/oops` slash command now runs `claude-oops show <id>` first** to
  preview the change, asks the user in chat, then runs `to <id> --force`.
  No more confusing double-confirm flow where Claude tries `yes |` because
  the binary kept aborting.

## v0.3.3

- `claude-oops list` and `status` no longer fail when run outside a git
  repository — they print a friendly note and exit 0. This was making
  the `/oops` slash command blow up in any non-git project.
- `claude-oops snap --quiet` (used by the SessionStart hook) also exits 0
  in non-git directories instead of failing the hook.
- `claude-oops snap` (manual, non-quiet) still errors loudly — explicit
  user action deserves an explicit error.

## v0.3.2

- Per-file restore: rewrite path resolution. Use `git rev-parse --show-prefix`
  from the user's cwd instead of comparing absolute paths — Windows reports
  cwd with 8.3 short names (`RUNNER~1`) while git uses long names
  (`runneradmin`), and a lexical strip-prefix between them fails.
  Now we work entirely in repo-relative space; absolute paths are
  rejected with a helpful error.

## v0.3.1

- (yanked — Windows path resolution still broken; superseded by v0.3.2)
- Attempted Windows pathspec fix: emit forward slashes regardless of OS.
  Insufficient on its own — see v0.3.2.

## v0.3.0 (yanked — broken on Windows)

- **Per-file restore.** `claude-oops to <id> -- src/auth.rs` restores only
  the named paths from the snapshot, leaving everything else in the
  working tree alone. Pathspecs work too: `to <id> -- src/` for a whole
  directory. Files that exist in the working tree but not in the snapshot
  are deleted (that's what "restore the snapshot's state for this path"
  means). Paths are resolved relative to your current directory, so
  running from a subdirectory works as expected.
- CI: granted `contents: write` to the release job so `softprops/action-gh-release`
  can publish.
- Published to crates.io: `cargo install claude-oops` now works.

## v0.2.0

- New `claude-oops show <id>` — file-level summary of what `to <id>` would
  change. Color-coded `A`/`M`/`D` like `git status`.
- `claude-oops install` now also writes a `/oops` slash command to
  `~/.claude/commands/oops.md` so you can list and restore snapshots
  without leaving your Claude Code session.
- `_hook-pre-tool-use` now writes a one-line announcement to stderr each
  time it takes a snapshot, so users can see hooks firing.
- Hook auto-snapshots now record a useful default message (file path for
  Edit/Write, command for Bash) instead of `null`.
- `uninstall` removes the `/oops` slash command unless the user edited it.
- Test fixtures pin `core.autocrlf=false`; fixes Windows CI flake.

## v0.1.0

- `snap`, `list`, `diff`, `to`, `drop`, `clean`, `install`, `uninstall`,
  `status`, plus the internal `_hook-pre-tool-use` entry point.
- Storage as git refs under `refs/claude-oops/<id>` plus a JSONL index in
  `.git/claude-oops/index.jsonl`.
- Working-tree capture via a private temporary index — includes untracked
  files (respecting `.gitignore`) without disturbing the user's index.
- Auto-snapshot triggers: SessionStart, PreToolUse on Edit/Write (with a
  2-min cooldown), PreToolUse on Bash matching a curated dangerous-pattern
  list (`rm -rf`, `git reset --hard`, `find … -delete`, etc.).
- Retention: keep last 30 OR snapshots from the last 7 days, whichever is
  more permissive.
- Cross-platform CI on macOS, Linux, Windows. Release workflow builds
  binaries for x86_64 + aarch64 darwin, x86_64-linux-gnu, x86_64-windows.
