# remem0

Rust CLI with a built-in TUI configuration flow.

## Run

```sh
cargo run
```

`cargo run` opens the interactive TUI. You can also call it explicitly:

```sh
cargo run -- tui
```

## Config Commands

```sh
cargo run -- config init
cargo run -- config path
cargo run -- config show
cargo run -- config set profile-name local
cargo run -- config set data-dir /tmp/remem0
cargo run -- config set enable-sync true
cargo run -- config set editor vim
cargo run -- config reset
```

The config file is stored in the platform-specific user config directory.
