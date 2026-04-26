# Changelog

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
