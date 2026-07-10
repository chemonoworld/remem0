# Local Deployment

Install `rem` as a user-level Cargo binary so it is available from any terminal
directory.

## Install

From this repository:

```sh
./scripts/install-local.sh
```

The script runs the equivalent command below and installs `rem` to
`$HOME/.cargo/bin/rem`:

```sh
cargo install --path . --locked --offline --force
```

`--offline` keeps local deployment from waiting on crates.io. Run `cargo build`
once with network access first if the required dependencies are not already in
your local Cargo cache.

Verify the installation from a different directory:

```sh
cd /tmp
command -v rem
rem --help
```

If `command -v rem` returns nothing, add Cargo's user binary directory to your
shell startup file and open a new terminal:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

For zsh, place that line in `~/.zshrc`.

## Update

After changing or pulling this repository, rerun the install script:

```sh
git pull --ff-only
./scripts/install-local.sh
```

`--force` replaces the existing installed `rem` binary with the current build.

## Uninstall

```sh
cargo uninstall rem
```
