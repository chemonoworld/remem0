# rem

`rem` is a local-first Markdown memory CLI for a human-readable second brain
plus local/coding agents.

## Model

- Markdown is the canonical source of truth.
- Git is the canonical transaction boundary. Every configured vault must be the
  root of a Git working tree with a GitHub or GitLab `origin` remote.
- SQLite FTS5/BM25 is a rebuildable vault-local cache. Memory mutations rebuild
  the cache inside the same transaction before Git commit.
- Global config lives under `$HOME/.rem` or `REM_HOME` when set.
- Each vault stores memories and cache under the configured root.

```text
<vault>/
  memories/
    short/
    long/
  policies/
  inbox/
  archive/
  .rem/
    cache/
    tx/
```

## Setup

```sh
mkdir -p ~/rem-vault
git -C ~/rem-vault init
git -C ~/rem-vault remote add origin git@github.com:you/rem-vault.git
cargo run -- init --root ~/rem-vault --storage git
cargo run -- profile list
cargo run -- doctor
```

For Obsidian-backed storage, point `--root` at the Obsidian vault, but keep the
same Git requirement. Obsidian Sync can move Markdown between devices; Git
remains the transaction log.

```sh
cargo run -- init --root ~/Documents/MyVault --storage obsidian
```

`rem` automatically keeps `.rem/cache/` and `.rem/tx/` out of Git. SQLite stays
local and rebuildable; Markdown plus Git commits are the durable state.
If the vault already has uncommitted files during init, pass
`--accept-external` to include them in the initialization commit or
`--restore-external` to discard them first.

## Commands

```sh
cargo run -- add --short --tag rust "# Decision\nUse Markdown as canonical memory."
cargo run -- list --short
cargo run -- show <id-or-prefix>
cargo run -- update <id-or-prefix> "# Updated\nSQLite is cache only."
cargo run -- promote <short-id-or-prefix>
cargo run -- delete <id-or-prefix>
cargo run -- commit --message "sync manual vault edits"
cargo run -- rebuild
cargo run -- search "Markdown"
cargo run -- search --bm25 "SQLite"
cargo run -- doctor
```

`rem commit` validates pending Markdown changes, rebuilds SQLite through a temp
index, atomically replaces the local cache, and creates one Git commit. In
non-interactive scripts, use `--accept-external` to include existing manual
changes or `--restore-external` to discard them.

Use `cargo run -- commit --review` to inspect dirty Git working-tree changes
before committing. The review flow can show diffs, include all changes, restore
all changes, or walk each file and choose include/restore. If Git reports
unmerged conflict states, resolve them with Git/editor tooling first and then
rerun `rem commit --review`.

Semantic memory conflict review is intentionally separate and still TODO:
duplicate memory IDs, sync-conflict copies, and contradictory long-term memories
should get a dedicated memory-aware review workflow rather than being solved by
the Git dirty-state review.

`rem search` uses the configured `default-search` mode when no explicit search
flag is provided. Explicit BM25 search requires a current index; run
`cargo run -- rebuild` for a local cache refresh without creating a Git commit.

`cargo run -- configure` opens the TUI configuration flow. Non-interactive
profile commands are available for scripts and tests:

```sh
cargo run -- profile add work ~/work-vault --storage git
cargo run -- profile use work
cargo run -- profile show
```

## Tests

```sh
cargo fmt --check
cargo check
cargo test
```
