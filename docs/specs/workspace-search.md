# Workspace text search

`search_text(path, query)` searches the folder selected for the current companion.
It is a literal, case-sensitive search with a nonempty, single-line query of at
most 200 bytes. Results contain relative paths, one-based line numbers and text
excerpts. It can be proposed alone or as a step in a reviewed workflow.

Search uses the existing capability-based text reader, excluding credential
paths and symlinks. It skips binary files, non-UTF-8 files and files over 16 KB.
Recursive traversal skips `target`, `node_modules`, `.venv` and `vendor`; an
explicit search rooted inside one of those directories can inspect it.

A request inspects at most 500 directory entries, checks at most 100 files,
descends four directory levels and returns at most 100 matching lines. It checks
a two-second scan budget between filesystem operations. Excerpts are limited to
300 characters and carry `text_truncated` when clipped. Results include counts,
limits and a `limited` flag. Incomplete or unreadable searches must never be
reported as proving that no match exists.

Single searches retain the existing explicit current-request and folder-scoped
Auto policy for read tools. Multi-step workflows always require manual approval.
File-oriented searches for words such as `alert` or `port` do not automatically
attach unrelated security-event records to the companion request.
