# Repository instructions for coding agents

This file applies to the entire repository.

Before changing anything, read [`HOUSERULES.md`](HOUSERULES.md) completely.
It is the authoritative rulebook for architecture, language design, testing,
coordination, commits, and dependencies; do not duplicate or override those
rules here.

Also inspect:

- `git status` before editing, because several agents may share one worktree;
- the latest entries in [`chat.md`](chat.md) for active ownership and handoffs;
- [`TODO.md`](TODO.md) for current work rather than completed history.

Append a dated entry to `chat.md` before touching a shared subsystem named in
the house rules. Preserve other agents' uncommitted changes and never include
them in a commit merely because they are present in the worktree.

