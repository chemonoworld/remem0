use std::fmt;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "remem0",
    version,
    about = "A Rust CLI with a built-in TUI configuration flow."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Open the interactive configuration TUI.")]
    Tui,
    #[command(about = "Read or update the local configuration file.")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Create the config file if it does not exist.")]
    Init,
    #[command(about = "Print the config file path.")]
    Path,
    #[command(about = "Print the current config as TOML.")]
    Show,
    #[command(about = "Update a single config value.")]
    Set {
        #[arg(value_enum)]
        key: ConfigKey,
        value: String,
    },
    #[command(about = "Reset config values to defaults.")]
    Reset,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ConfigKey {
    #[value(name = "profile-name")]
    ProfileName,
    #[value(name = "data-dir")]
    DataDir,
    #[value(name = "enable-sync")]
    EnableSync,
    Editor,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ProfileName => "profile-name",
            Self::DataDir => "data-dir",
            Self::EnableSync => "enable-sync",
            Self::Editor => "editor",
        };

        f.write_str(name)
    }
}
