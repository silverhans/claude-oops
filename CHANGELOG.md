# Changelog

## Unreleased

- **Per-file restore.** `claude-oops to <id> -- src/auth.rs` restores only
  the named paths from the snapshot, leaving everything else in the
  working tree alone. Pathspecs work too: `to <id> -- src/` for a whole
  directory. Files that exist in the working tree but not in the snapshot
  are deleted (that's what "restore the snapshot's state for this path"
  means). Paths are resolved relative to your current directory, so
  running from a subdirectory works as expected.
- CI: granted `contents: write` to the release job so `softprops/action-gh-release`
  can publish.

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
