# Changelog

## Unreleased

- Initial implementation: snapshot, list, diff, restore, drop.
- Storage as git refs under `refs/claude-oops/<id>` plus a JSONL index in
  `.git/claude-oops/index.jsonl`.
- Idempotency check: auto-triggered snapshots skip when the working tree's
  tree SHA is unchanged.
