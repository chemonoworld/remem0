use std::{
    fmt::{self, Write as _},
    path::PathBuf,
};

use clap::{
    Args, Command as ClapCommand, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
};

use crate::{
    config::StorageMode,
    conflict::ConflictKind,
    memory::{MemoryKind, MemoryScope, MemoryType},
    output::ColorChoice,
    search::SearchMode,
};

const ROOT_HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

{before-help}Options:
{options}";

struct CommandCategory {
    heading: &'static str,
    commands: &'static [&'static str],
}

const COMMAND_CATEGORIES: &[CommandCategory] = &[
    CommandCategory {
        heading: "Setup & Configuration",
        commands: &["configure", "init", "profile", "config"],
    },
    CommandCategory {
        heading: "Read & Search",
        commands: &["list", "show", "search", "facts"],
    },
    CommandCategory {
        heading: "Create & Change",
        commands: &[
            "add",
            "append",
            "update",
            "edit",
            "supersede",
            "promote",
            "delete",
        ],
    },
    CommandCategory {
        heading: "Review & Maintenance",
        commands: &["review", "conflict", "commit", "rebuild", "doctor"],
    },
    CommandCategory {
        heading: "Help",
        commands: &["help"],
    },
];

#[derive(Debug, Parser)]
#[command(
    name = "rem",
    version,
    about = "Local-first Markdown memory CLI for humans and agents."
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = ColorChoice::Auto,
        help = "Control when ANSI color is used."
    )]
    pub color: ColorChoice,
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn parse_grouped() -> Self {
        let matches = Self::grouped_command().get_matches();
        Self::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
    }

    fn grouped_command() -> ClapCommand {
        let mut command = <Self as CommandFactory>::command();
        command.build();
        let grouped_help = render_grouped_commands(&command);

        command
            .before_help(grouped_help)
            .help_template(ROOT_HELP_TEMPLATE)
    }
}

fn render_grouped_commands(command: &ClapCommand) -> String {
    let visible_commands = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .collect::<Vec<_>>();
    let name_width = visible_commands
        .iter()
        .map(|subcommand| subcommand.get_name().len())
        .max()
        .unwrap_or_default();
    let mut help = String::new();

    for category in COMMAND_CATEGORIES {
        let commands = category
            .commands
            .iter()
            .filter_map(|name| {
                visible_commands
                    .iter()
                    .find(|subcommand| subcommand.get_name() == *name)
            })
            .collect::<Vec<_>>();
        if commands.is_empty() {
            continue;
        }

        let _ = writeln!(help, "{}:", category.heading);
        for subcommand in commands {
            let about = subcommand
                .get_about()
                .map(ToString::to_string)
                .unwrap_or_default();
            let _ = writeln!(help, "  {:name_width$}  {about}", subcommand.get_name());
        }
        help.push('\n');
    }

    let uncategorized = visible_commands
        .iter()
        .filter(|subcommand| {
            !COMMAND_CATEGORIES
                .iter()
                .any(|category| category.commands.contains(&subcommand.get_name()))
        })
        .collect::<Vec<_>>();
    if !uncategorized.is_empty() {
        let _ = writeln!(help, "Other:");
        for subcommand in uncategorized {
            let about = subcommand
                .get_about()
                .map(ToString::to_string)
                .unwrap_or_default();
            let _ = writeln!(help, "  {:name_width$}  {about}", subcommand.get_name());
        }
        help.push('\n');
    }

    help.trim_end().to_owned()
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
    #[command(about = "Append a follow-up to an existing memory body.")]
    Append(AppendArgs),
    #[command(about = "Review a proposed memory action without writing.")]
    Review(ReviewArgs),
    #[command(about = "Create a replacement memory and mark an active memory as superseded.")]
    Supersede(SupersedeArgs),
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
    #[command(
        visible_alias = "conflicts",
        about = "Inspect and resolve derived semantic conflicts."
    )]
    Conflict {
        #[command(subcommand)]
        command: ConflictCommand,
    },
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
    pub source_id: Option<String>,
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
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl UpdateArgs {
    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }
}

#[derive(Debug, Args)]
pub struct AppendArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl AppendArgs {
    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long, value_enum, default_value_t = MemoryScope::User)]
    pub scope: MemoryScope,
    #[arg(long)]
    pub non_interactive: bool,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl ReviewArgs {
    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }
}

#[derive(Debug, Args)]
pub struct SupersedeArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
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
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub source_id: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,
}

impl SupersedeArgs {
    pub fn resolved_type(&self, source: MemoryType) -> MemoryType {
        if self.long {
            MemoryType::Long
        } else if self.short {
            MemoryType::Short
        } else {
            self.memory_type.unwrap_or(source)
        }
    }

    pub fn body(&self) -> String {
        self.text.join(" ").trim().to_string()
    }

    pub fn has_metadata_overrides(&self) -> bool {
        self.short
            || self.long
            || self.memory_type.is_some()
            || self.scope.is_some()
            || self.kind.is_some()
            || !self.tags.is_empty()
            || self.source.is_some()
            || self.source_id.is_some()
            || self.agent.is_some()
            || self.session.is_some()
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

#[derive(Debug, Subcommand)]
pub enum ConflictCommand {
    #[command(about = "List semantic conflicts from the local index.")]
    List(ConflictListArgs),
    #[command(about = "Show conflict evidence by id or unique prefix.")]
    Show(IdArgs),
    #[command(about = "Accept the current conflict evidence as intentional.")]
    Accept(ConflictAcceptArgs),
    #[command(about = "Resolve a conflict atomically and commit the Markdown writeback.")]
    Resolve(ConflictResolveArgs),
}

#[derive(Debug, Args)]
pub struct ConflictListArgs {
    #[arg(long, value_enum)]
    pub kind: Option<ConflictKindArg>,
    #[arg(long, value_enum)]
    pub scope: Option<MemoryScope>,
    #[arg(long, help = "Include accepted conflicts.")]
    pub all: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ConflictKindArg {
    #[value(name = "exact-active-duplicate", alias = "duplicate")]
    ExactActiveDuplicate,
    #[value(name = "exclusive-current-conflict", alias = "exclusive-current")]
    ExclusiveCurrent,
}

impl From<ConflictKindArg> for ConflictKind {
    fn from(value: ConflictKindArg) -> Self {
        match value {
            ConflictKindArg::ExactActiveDuplicate => Self::ExactActiveDuplicate,
            ConflictKindArg::ExclusiveCurrent => Self::ExclusiveCurrent,
        }
    }
}

#[derive(Debug, Args)]
pub struct ConflictAcceptArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConflictResolveArgs {
    #[command(flatten)]
    pub tx: MutationArgs,
    pub id: String,
    #[arg(
        long,
        value_name = "MEMORY_OR_FACT_ID",
        help = "Memory id for a duplicate conflict, or fact id for an exclusive-current conflict."
    )]
    pub keep: String,
    #[arg(
        long,
        allow_hyphen_values = true,
        value_name = "TIME",
        help = "Expiration instant for competing facts (unix seconds or ISO UTC; defaults to now)."
    )]
    pub at: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn supersede_args() -> SupersedeArgs {
        SupersedeArgs {
            tx: MutationArgs::default(),
            id: "m1".to_string(),
            short: false,
            long: false,
            memory_type: None,
            scope: None,
            kind: None,
            tags: Vec::new(),
            source: None,
            source_id: None,
            agent: None,
            session: None,
            text: vec!["same body".to_string()],
        }
    }

    #[test]
    fn supersede_metadata_override_detection_is_explicit() {
        let mut args = supersede_args();
        assert!(!args.has_metadata_overrides());

        args.kind = Some(MemoryKind::Decision);
        assert!(args.has_metadata_overrides());
        args.kind = None;
        args.source_id = Some("event-2".to_string());
        assert!(args.has_metadata_overrides());
        args.source_id = None;
        args.tags.push("durable".to_string());
        assert!(args.has_metadata_overrides());
    }

    #[test]
    fn command_categories_cover_every_visible_subcommand_once() {
        let mut command = <Cli as CommandFactory>::command();
        command.build();

        for subcommand in command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
        {
            let category_count = COMMAND_CATEGORIES
                .iter()
                .filter(|category| category.commands.contains(&subcommand.get_name()))
                .count();
            assert_eq!(
                category_count,
                1,
                "top-level command {:?} must belong to exactly one help category",
                subcommand.get_name()
            );
        }

        for category in COMMAND_CATEGORIES {
            for name in category.commands {
                assert!(
                    command
                        .get_subcommands()
                        .any(|subcommand| !subcommand.is_hide_set()
                            && subcommand.get_name() == *name),
                    "help category {:?} references missing command {name:?}",
                    category.heading
                );
            }
        }
    }
}
