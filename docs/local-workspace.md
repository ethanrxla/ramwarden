# Local workspace

Use `/Users/ethanrisden/ramwarden` as the single working folder on this Mac.
`web/` contains the website and `crates/ramwarden-cloud/` contains the MongoDB
storage code imported from the former PR6 checkout. It is a separate server-side
crate; its credentials do not ship with the desktop app.

The former `ramwarden-trunk` and `ramwarden-pr6` folders were additional Git
worktrees, not separate projects. Trunk held an abandoned merge whose parents
already occur in the active checkout's history. PR6 contained database commits
that had not yet been incorporated here.

Before consolidation, full source archives (including ignored secrets), Git
history, and the unfinished merge's index/metadata were saved under:
`/Users/ethanrisden/Documents/RamWarden-backups/2026-11-01-014729`.
These backups are private (directory mode 700, archives mode 600) and contain
SHA-256 checksums. Build caches are excluded and can be regenerated. The
`codex/archive-pr6-20261101` branch retains PR6's committed history; `reconcile`
retains the old trunk branch. The history bundle can restore committed refs,
and the source archives preserve working changes. No branch was deleted.
