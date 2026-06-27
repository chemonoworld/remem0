mod cli;
mod config;
mod tui;

use std::path::PathBuf;

use clap::Parser;
use cli::{Cli, Command, ConfigCommand, ConfigKey};
use color_eyre::eyre::{Result, eyre};
use config::{AppConfig, ConfigStore};

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    let store = ConfigStore::new()?;

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(store),
        Command::Config { command } => run_config_command(store, command),
    }
}

fn run_config_command(store: ConfigStore, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Init => {
            let config = store.ensure_exists()?;
            println!("created {}", store.path().display());
            print_config(&config)?;
        }
        ConfigCommand::Path => {
            println!("{}", store.path().display());
        }
        ConfigCommand::Show => {
            let config = store.load_or_default()?;
            print_config(&config)?;
        }
        ConfigCommand::Set { key, value } => {
            let mut config = store.load_or_default()?;
            set_config_value(&mut config, key, value)?;
            store.save(&config)?;
            println!("updated {} in {}", key, store.path().display());
        }
        ConfigCommand::Reset => {
            let config = store.reset()?;
            println!("reset {}", store.path().display());
            print_config(&config)?;
        }
    }

    Ok(())
}

fn set_config_value(config: &mut AppConfig, key: ConfigKey, value: String) -> Result<()> {
    match key {
        ConfigKey::ProfileName => config.profile_name = value,
        ConfigKey::DataDir => config.data_dir = PathBuf::from(value),
        ConfigKey::EnableSync => config.enable_sync = parse_bool(&value)?,
        ConfigKey::Editor => {
            let value = value.trim();
            config.editor = (!value.is_empty()).then(|| value.to_string());
        }
    }

    Ok(())
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Ok(false),
        other => Err(eyre!(
            "expected a boolean value like true/false, yes/no, on/off, or 1/0; got {other:?}"
        )),
    }
}

fn print_config(config: &AppConfig) -> Result<()> {
    println!("{}", toml::to_string_pretty(config)?);
    Ok(())
}
