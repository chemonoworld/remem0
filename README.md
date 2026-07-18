# rem

`rem` is a local-first Markdown memory CLI for a human-readable second brain
plus local/coding agents.

## Model

- Markdown is the canonical source of truth.
- Git is the canonical transaction boundary. Every configured vault must be the
  root of a Git working tree with a GitHub or GitLab `origin` remote.
- SQLite FTS5/BM25 is a rebuildable vault-local cache. Memory mutations rebuild
  the cache inside the same transaction before Git commit.
- The same SQLite cache also stores a derived temporal semantic graph MVP:
  source episodes, entities, facts, source links, and a small controlled
  relation ontology. Markdown remains canonical; no external graph database is
  required.
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
Memory Markdown files must be regular files inside the vault; `rem` refuses to
mutate a symlinked memory because Git would only record the link, not its target
content.
If the vault already has uncommitted files during init, pass
`--accept-external` to include them in the initialization commit or
`--restore-external` to discard them first.

## Local Installation

Install `rem` as a user-level command with:

```sh
./scripts/install-local.sh
```

This installs to `$HOME/.cargo/bin/rem`, which must be on `PATH`. See
[Local Deployment](docs/local-deployment.md) for install, update, PATH, and
uninstall instructions.

## Commands

`rem --help` groups the command surface by purpose: setup and configuration,
reading and search, creating and changing memories, review and maintenance, and
command help. Existing command names remain available directly, and
`rem help <command>` shows the detailed options for one command.

```sh
cargo run -- add --short --tag rust $'# Decision\nUse Markdown as canonical memory.'
cargo run -- list --short
cargo run -- show <id-or-prefix>
cargo run -- append <id-or-prefix> "New supporting context."
cargo run -- update <id-or-prefix> $'# Updated\nSQLite is cache only.'
cargo run -- supersede <id-or-prefix> $'# Replacement\nUse a newer decision.'
cargo run -- review --non-interactive $'# Proposed\n@fact User | PREFERS | Helix'
cargo run -- promote <short-id-or-prefix>
cargo run -- delete <id-or-prefix>
cargo run -- commit --message "sync manual vault edits"
cargo run -- rebuild
cargo run -- search "Markdown"
cargo run -- search --bm25 "SQLite"
cargo run -- facts --entity User
cargo run -- facts --at 2025-04-01
cargo run -- doctor
```

### Color output

`rem` uses semantic colors for status labels, memory IDs and types, metadata,
paths, scores, review actions, diagnostics, Markdown/TOML documents, and Git
diffs. Color is enabled automatically for terminal output and disabled when
stdout or stderr is redirected, so tab-separated output remains safe for
scripts.

Use the global option before or after a command when an explicit policy is
needed:

```sh
rem --color auto list
rem search mTLS --color always
rem --color never doctor
```

`NO_COLOR=1` or `CLICOLOR=0` disables color in `auto` mode.
`CLICOLOR_FORCE=1` or `FORCE_COLOR=1` enables it even when redirected.
Explicit `--color always` and `--color never` take precedence over environment
variables.

The examples use Bash/zsh ANSI-C quoting (`$'...'`) so `\n` becomes a real
newline. Plain double quotes keep `\n` as literal text, which means `@fact`
must instead be entered on an actual new line.

`rem commit` validates pending Markdown changes, rebuilds SQLite through a temp
index, atomically replaces the local cache, and creates one Git commit. In
non-interactive scripts, use `--accept-external` to include existing manual
changes or `--restore-external` to discard them.

Only one `rem` transaction may run against a vault at a time. A competing
command fails without writing and reports `.rem/tx/active.lock`. If a crashed
process leaves that lock behind, run `rem doctor`, confirm that no `rem`
process is still using the vault, and only then remove the stale lock.

### Explicit memory actions

Each write action has one meaning:

- `add` creates a new memory. Supplying `--source` and `--source-id` makes the
  event idempotent: the identical body returns `no-op`; a changed body tells
  the caller to use `update <id>` explicitly.
- `append <id> <TEXT>` adds a follow-up paragraph while preserving the current
  body.
- `update <id> <TEXT>` replaces the body. An unchanged body returns `no-op`.
- `supersede <id> <TEXT>` creates a replacement memory and records the old ID
  in its `supersedes` frontmatter. The old memory becomes `superseded` and is
  retained as immutable provenance.
- `review` never writes. It reports a candidate and a recommended action; the
  caller must subsequently run `add`, `append`, `update`, or `supersede`.

`review --id <id> <TEXT>` evaluates an explicit update target. Without `--id`,
`review --scope <user|project|agent|session> <TEXT>` first detects an exact
body duplicate, then compares current `@fact` directives in the same scope.
It recommends `append` for the same fact, `supersede` for a changed
exclusive-current relation such as `PREFERS` or `WORKS_AT`, and `add` for
compatible facts or ambiguous candidates. It uses deterministic Markdown fact
parsing, not embeddings, and does not alter Markdown, SQLite, or Git.

Use `cargo run -- commit --review` to inspect dirty Git working-tree changes
before committing. The review flow can show diffs, include all changes, restore
all changes, or walk each file and choose include/restore. If Git reports
unmerged conflict states, resolve them with Git/editor tooling first and then
rerun `rem commit --review`.

Git conflict handling and semantic review remain separate. `rem review` covers
only deterministic fact candidates. Duplicate memory IDs, sync-conflict copies,
entity resolution, vector/LLM similarity, and automatic temporal validity
closure remain future semantic-review work; Git unmerged states must still be
resolved with Git/editor tooling before `rem commit`.

`rem search` uses the configured `default-search` mode when no explicit search
flag is provided. Explicit BM25 search requires a current index; run
`cargo run -- rebuild` for a local cache refresh without creating a Git commit.

Semantic facts are derived from explicit Markdown body directives:

```md
@fact User | PREFERS | Adidas | valid_from=2025-01-10 | valid_to=2025-04-02
@fact User | PREFERS | Puma | valid_from=2025-04-02
@fact User | USES | LegacyTool | valid_from=2020-01-01 | expired_at=2024-01-01
```

Allowed relations are intentionally small and controlled: `PREFERS`,
`DISLIKES`, `USES`, `WORKS_AT`, `HAS_PROJECT`, `PART_OF`, `SUPERSEDES`, and
`MENTIONS`. `rem facts` lists current facts by default, `--at <time>` shows a
historical view, `--all` includes closed/expired facts, and `--source` includes
episode provenance. `--at` and `--all` are mutually exclusive so a historical
state query cannot silently ignore either filter. Semantic time values must be
signed 64-bit unix seconds, zero-padded `YYYY-MM-DD`, or
`YYYY-MM-DDTHH:MM:SSZ`; historical queries normalize both accepted formats to
the same instant.

`rem facts` emits tab-separated rows. Its normal temporal fields are
`valid_from`, `valid_to`, `expired_at`, then `learned_at`; with `--source`, the
remaining fields identify the source memory, path, episode, and excerpt.

`cargo run -- configure` opens the TUI configuration flow. `storage-mode` and
`default-search` use an option picker rather than free-form text. `profile-root`
still accepts a typed path; select that field and press `Ctrl-F` to open a fuzzy
directory finder that recursively searches readable, non-symlink directories
under the user's `$HOME` in the background. Its editable path filter starts
pre-filled with `$HOME`, so you can backspace from that path or press `Ctrl-A`
to replace it. Every character (including `j` and `k`) narrows the results.
Press `Tab` or `Shift-Tab` to move focus to the path list; arrows or `j`/`k`
then move the path cursor. Press `Tab` again to return to the filter and
`Enter` to choose the current path.

`S`/`s` writes the configuration independently of vault initialization, so a
new root can be recorded before it is a valid Git-backed vault. `I` attempts
vault initialization and reports any Git validation problem in the TUI without
discarding unsaved configuration changes.

Non-interactive profile commands are available for scripts and tests:

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

On Unix hosts with `expect` installed, `cargo test` also runs a real PTY
regression for the first-run Configure flow, including the root finder, option
pickers, and uppercase `S` save shortcut. The test reports a skip when that
system PTY helper is unavailable.
