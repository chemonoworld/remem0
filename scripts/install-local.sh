#!/usr/bin/env sh
set -eu

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required; install Rust with rustup first" >&2
  exit 1
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec cargo install --path "$repo_root" --locked --force
