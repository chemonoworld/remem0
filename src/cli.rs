use std::{fmt, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    config::StorageMode,
    memory::{MemoryKind, MemoryScope, MemoryType},
    search::SearchMode,
};

#[derive(Debug, Parser)]
#[command(
    name = "rem",
    version,
    about = "Local-first Markdown memory CLI for humans and agents."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Open the interactive configuration TUI.")]
    Configure,
    #[command(about = "Initialize the active or provided memory vault.")]
    Init(InitArgs),
    #[command(about = "Manage global profiles.")]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    #[command(about = "Add a short-term or long-term memory.")]
    Add(MemoryWriteArgs),
    #[command(about = "List memories.")]
    List(ListArgs),
    #[command(about = "Show one memory by id or unique prefix.")]
    Show(IdArgs),
    #[command(about = "Open a memory in $EDITOR.")]
    Edit(EditArgs),
    #[command(about = "Update a memory body.")]
    Update(UpdateArgs),
    #[command(about = "Archive or delete a memory.")]
    Delete(DeleteArgs),
    #[command(about = "Promote a short-term memory to long-term memory.")]
    Promote(PromoteArgs),
    #[command(about = "Validate, reindex, and Git commit vault changes.")]
    Commit(CommitArgs),
    #[command(about = "Search memories.")]
    Search(SearchArgs),
    #[command(about = "List derived temporal semantic facts.")]
    Facts(FactsArgs),
    #[command(about = "Rebuild the vault-local SQLite search index.")]
    Rebuild,
    #[command(about = "Inspect configuration, vault, policy, and index state.")]
    Doctor,
    #[command(about = "Read or update low-level global configuration.")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    #[arg(long)]
    pub root: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub storage: Option<StorageMode>,
    #[arg(long)]
    pub profile: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    #[command(about = "List configured profiles.")]
    List,
    #[command(about = "Show the active or named profile.")]
    Show { name: Option<String> },
    #[command(about = "Add or replace a profile.")]
    Add {
        name: String,
        root: PathBuf,
        #[arg(long, value_enum, default_value_t = StorageMode::Local)]
        storage: StorageMode,
    },
    #[command(about = "Set the active profile.")]
    Use { name: String },
}

#[derive(Debug, Args)]
pub struct MemoryWriteArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    #[arg(long, conflicts_with = "long")]
    pub short: bool,
    #[arg(long, conflicts_with = "short")]
    pub long: bool,
    #[arg(long = "type", value_enum)]
    pub memory_type: Option<MemoryType>,
    #[arg(long, value_enum, default_value_t = MemoryScope::User)]
    pub scope: MemoryScope,
    #[arg(long, value_enum, default_value_t = MemoryKind::Note)]
    pub kind: MemoryKind,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long, default_value = "cli")]
    pub source: String,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl MemoryWriteArgs {
    pub fn resolved_type(&self) -> MemoryType {
        if self.long {
            MemoryType::Long
        } else if self.short {
            MemoryType::Short
        } else {
            self.memory_type.unwrap_or(MemoryType::Short)
        }
    }

    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long, conflicts_with = "long")]
    pub short: bool,
    #[arg(long, conflicts_with = "short")]
    pub long: bool,
    #[arg(long = "type", value_enum)]
    pub memory_type: Option<MemoryType>,
    #[arg(long, value_enum)]
    pub scope: Option<MemoryScope>,
    #[arg(long, value_enum)]
    pub kind: Option<MemoryKind>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long)]
    pub all: bool,
}

impl ListArgs {
    pub fn resolved_type(&self) -> Option<MemoryType> {
        if self.long {
            Some(MemoryType::Long)
        } else if self.short {
            Some(MemoryType::Short)
        } else {
            self.memory_type
        }
    }
}

#[derive(Debug, Args)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(long)]
    pub append: bool,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl UpdateArgs {
    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(long)]
    pub hard: bool,
}

#[derive(Debug, Args)]
pub struct PromoteArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl PromoteArgs {
    pub fn body_override(&self) -> Option<String> {
        let body = self.text.join(" ").trim().to_string();
        (!body.is_empty()).then_some(body)
    }
}

#[derive(Clone, Copy, Debug, Default, Args)]
pub struct MutationArgs {
    #[arg(long, conflicts_with = "restore_external")]
    pub accept_external: bool,
    #[arg(long, conflicts_with = "accept_external")]
    pub restore_external: bool,
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, Args)]
pub struct CommitArgs {
    #[arg(long, short = 'm')]
    pub message: Option<String>,
    #[arg(long, conflicts_with = "restore_external")]
    pub accept_external: bool,
    #[arg(long, conflicts_with = "accept_external")]
    pub restore_external: bool,
    #[arg(
        long,
        conflicts_with_all = ["accept_external", "restore_external", "non_interactive"]
    )]
    pub review: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    #[arg(long)]
    pub grep: bool,
    #[arg(long)]
    pub bm25: bool,
    #[arg(long)]
    pub vector: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_enum)]
    pub mode: Option<SearchMode>,
    #[arg(value_name = "QUERY")]
    pub query: Vec<String>,
}

#[derive(Debug, Args)]
pub struct FactsArgs {
    #[arg(long)]
    pub entity: Option<String>,
    #[arg(long)]
    pub relation: Option<String>,
    #[arg(long, conflicts_with = "all", allow_hyphen_values = true)]
    pub at: Option<String>,
    #[arg(long, conflicts_with = "at")]
    pub all: bool,
    #[arg(long)]
    pub source: bool,
}

impl SearchArgs {
    pub fn has_explicit_mode(&self) -> bool {
        self.all || self.vector || self.bm25 || self.grep || self.mode.is_some()
    }

    pub fn resolved_mode(&self) -> SearchMode {
        if self.all {
            SearchMode::All
        } else if self.vector {
            SearchMode::Vector
        } else if self.bm25 {
            SearchMode::Bm25
        } else if self.grep {
            SearchMode::Grep
        } else {
            self.mode.unwrap_or(SearchMode::Auto)
        }
    }

    pub fn query(&self) -> String {
        self.query.join(" ").trim().to_string()
    }
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Create the global config file if it does not exist.")]
    Init,
    #[command(about = "Print the global config file path.")]
    Path,
    #[command(about = "Print the current config as TOML.")]
    Show,
    #[command(about = "Update a single config value.")]
    Set {
        #[arg(value_enum)]
        key: ConfigKey,
        value: String,
    },
    #[command(about = "Reset global config values to defaults.")]
    Reset,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ConfigKey {
    #[value(name = "active-profile")]
    ActiveProfile,
    #[value(name = "default-search")]
    DefaultSearch,
    Editor,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ActiveProfile => "active-profile",
            Self::DefaultSearch => "default-search",
            Self::Editor => "editor",
        };

        f.write_str(name)
    }
}
