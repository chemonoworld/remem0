mod cli;
mod config;
mod doctor;
mod frontmatter;
mod index;
mod memory;
mod policy;
mod search;
mod transaction;
mod tui;
mod workspace;

use std::{env, path::Path, process::Command as ProcessCommand};

use clap::Parser;
use cli::{Cli, Command, ConfigCommand, ConfigKey, ProfileCommand};
use color_eyre::eyre::{Result, eyre};
use config::{AppConfig, ConfigStore, ProfileConfig, StorageMode, normalize_root};
use memory::{CreateMemoryInput, MemoryFilter};
use transaction::{ExternalChangePolicy, TransactionOptions};
use workspace::Workspace;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let store = ConfigStore::new()?;

    match cli.command.unwrap_or(Command::Configure) {
        Command::Configure | Command::Tui => tui::run(store),
        Command::Init(args) => run_init(store, args),
        Command::Profile { command } => run_profile_command(store, command),
        Command::Add(args) => {
            let workspace = active_workspace(&store)?;
            let body = args.body();
            let memory_type = args.resolved_type();
            let scope = args.scope;
            let kind = args.kind;
            let tags = args.tags;
            let source = args.source;
            let agent = args.agent;
            let session = args.session;
            let input = CreateMemoryInput::new(memory_type, scope, kind, body)
                .tags(tags)
                .source(Some(source))
                .agent(agent)
                .session(session);
            let (memory, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    workspace.init()?;
                    let memory = memory::create_memory(&workspace, input)?;
                    Ok((
                        memory.clone(),
                        format!("rem: add {memory_type} memory {}", memory.metadata.id),
                    ))
                },
            )?;
            println!(
                "added {} {}",
                memory.metadata.memory_type, memory.metadata.id
            );
            Ok(())
        }
        Command::List(args) => {
            let workspace = active_workspace(&store)?;
            let memories = memory::list_memories(
                &workspace,
                &MemoryFilter {
                    memory_type: args.resolved_type(),
                    scope: args.scope,
                    kind: args.kind,
                    tag: args.tag,
                    include_archived: args.all,
                },
            )?;
            for memory in memories {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    memory.metadata.id,
                    memory.metadata.memory_type,
                    memory.metadata.scope,
                    memory.metadata.kind,
                    memory.title()
                );
            }
            Ok(())
        }
        Command::Show(args) => {
            let workspace = active_workspace(&store)?;
            let memory = memory::find_memory(&workspace, &args.id, true)?;
            print!("{}", memory.to_markdown());
            Ok(())
        }
        Command::Edit(args) => {
            let workspace = active_workspace(&store)?;
            let config = store.load_or_default()?;
            let editor = config
                .editor
                .or_else(|| env::var("EDITOR").ok())
                .unwrap_or_else(|| "vi".to_string());
            let id = args.id;
            let (path, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    let memory = memory::find_memory(&workspace, &id, true)?;
                    let path = memory
                        .path
                        .ok_or_else(|| eyre!("memory has no filesystem path"))?;
                    let status = run_editor(&editor, &path)?;
                    if !status.success() {
                        return Err(eyre!("editor exited with status {status}"));
                    }
                    Ok((path, format!("rem: update memory {id}")))
                },
            )?;
            println!("edited {}", path.display());
            Ok(())
        }
        Command::Update(args) => {
            let workspace = active_workspace(&store)?;
            let body = args.body();
            let id = args.id;
            let append = args.append;
            let (memory, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    let memory = memory::update_memory(&workspace, &id, body, append)?;
                    Ok((
                        memory.clone(),
                        format!("rem: update memory {}", memory.metadata.id),
                    ))
                },
            )?;
            println!("updated {}", memory.metadata.id);
            Ok(())
        }
        Command::Delete(args) => {
            let workspace = active_workspace(&store)?;
            let id = args.id;
            let hard = args.hard;
            let (memory, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    let memory = memory::delete_memory(&workspace, &id, hard)?;
                    let verb = if hard { "delete" } else { "archive" };
                    Ok((
                        memory.clone(),
                        format!("rem: {verb} memory {}", memory.metadata.id),
                    ))
                },
            )?;
            if hard {
                println!("deleted {}", memory.metadata.id);
            } else {
                println!("archived {}", memory.metadata.id);
            }
            Ok(())
        }
        Command::Promote(args) => {
            let workspace = active_workspace(&store)?;
            let body_override = args.body_override();
            let id = args.id;
            let (memory, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    let memory = memory::promote_memory(&workspace, &id, body_override)?;
                    let from = memory.metadata.promoted_from.clone().unwrap_or(id.clone());
                    Ok((
                        memory.clone(),
                        format!("rem: promote memory {from} to {}", memory.metadata.id),
                    ))
                },
            )?;
            println!("promoted {}", memory.metadata.id);
            Ok(())
        }
        Command::Commit(args) => run_commit(&store, args),
        Command::Search(args) => {
            let workspace = active_workspace(&store)?;
            let mode = if args.has_explicit_mode() {
                args.resolved_mode()
            } else {
                let config = store.load_or_default()?;
                search::SearchMode::parse_config_value(&config.default_search)?
            };
            let results = search::search(&workspace, &args.query(), mode)?;
            for result in results {
                println!(
                    "{}\t{}\t{}\t{}\t{:.3}\t{}",
                    result.id,
                    result.memory_type,
                    result.source,
                    result.title,
                    result.score,
                    result.path
                );
            }
            Ok(())
        }
        Command::Rebuild => {
            let workspace = active_workspace(&store)?;
            let report = index::rebuild(&workspace)?;
            println!(
                "rebuilt {} indexed={} diagnostics={}",
                report.index_path, report.indexed, report.diagnostics
            );
            Ok(())
        }
        Command::Doctor => {
            let config = store.load_or_default()?;
            println!("config: {}", store.path().display());
            if config.profiles.is_empty() {
                println!("warn\tno profiles configured; run `rem init --root <path>`");
                return Ok(());
            }
            let profile = match config.active_profile() {
                Ok(profile) => profile.clone(),
                Err(err) => {
                    println!("warn\t{err}");
                    return Ok(());
                }
            };
            let workspace = Workspace::new(&profile);
            println!("active_profile: {}", config.active_profile);
            println!("root: {}", profile.root.display());
            for finding in doctor::run(&workspace, profile.storage)? {
                println!("{}\t{}", finding.level, finding.message);
            }
            Ok(())
        }
        Command::Config { command } => run_config_command(store, command),
    }
}

fn run_init(store: ConfigStore, args: cli::InitArgs) -> Result<()> {
    let mut config = store.load_or_default()?;
    let profile_name = args.profile.unwrap_or(config.active_profile.clone());
    let root = match args.root {
        Some(root) => normalize_root(root),
        None => match config.profile(&profile_name) {
            Ok(profile) => profile.root.clone(),
            Err(_) => env::current_dir()?.join("rem"),
        },
    };
    let storage = args
        .storage
        .or_else(|| {
            config
                .profile(&profile_name)
                .ok()
                .map(|profile| profile.storage)
        })
        .unwrap_or(StorageMode::Local);

    let profile = ProfileConfig {
        name: profile_name.clone(),
        root,
        storage,
    };
    let workspace = Workspace::new(&profile);
    transaction::run_mutation(
        &workspace,
        "rem: initialize vault",
        transaction_options(args.tx)?,
        || workspace.init(),
    )?;

    config.upsert_profile(profile.clone());
    config.active_profile = profile_name;
    store.save(&config)?;

    println!("initialized {}", workspace.root().display());
    Ok(())
}

fn run_commit(store: &ConfigStore, args: cli::CommitArgs) -> Result<()> {
    let workspace = active_workspace(store)?;

    if args.dry_run {
        let report = transaction::dry_run(&workspace)?;
        println!("dry-run changes={}", report.changed_paths.len());
        for path in &report.changed_paths {
            println!("would commit\t{path}");
        }
        println!(
            "dry-run reindex indexed={} diagnostics={}",
            report.indexed, report.diagnostics
        );
        if report.diagnostics > 0 {
            return Err(eyre!(
                "reindex produced {} diagnostics; commit would fail",
                report.diagnostics
            ));
        }
        return Ok(());
    }

    let message = args
        .message
        .unwrap_or_else(|| "rem: commit vault changes".to_string());
    let (_, outcome) = transaction::run_mutation(
        &workspace,
        &message,
        transaction_options_from_parts(
            args.accept_external,
            args.restore_external,
            args.review,
            args.non_interactive,
        )?,
        || Ok(()),
    )?;

    if let Some(commit_id) = outcome.commit_id {
        println!(
            "committed {} changes={} indexed={}",
            commit_id,
            outcome.changed_paths.len(),
            outcome.indexed
        );
    } else {
        println!("nothing to commit indexed={}", outcome.indexed);
    }

    Ok(())
}

fn run_profile_command(store: ConfigStore, command: ProfileCommand) -> Result<()> {
    let mut config = store.load_or_default()?;
    match command {
        ProfileCommand::List => {
            if config.profiles.is_empty() {
                println!("no profiles configured; run `rem init --root <path>`");
            }
            for profile in &config.profiles {
                let active = if profile.name == config.active_profile {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{} {}\t{}\t{}",
                    active,
                    profile.name,
                    profile.storage,
                    profile.root.display()
                );
            }
        }
        ProfileCommand::Show { name } => {
            let name = name.unwrap_or_else(|| config.active_profile.clone());
            let profile = config.profile(&name)?;
            println!("name = {}", profile.name);
            println!("storage = {}", profile.storage);
            println!("root = {}", profile.root.display());
        }
        ProfileCommand::Add {
            name,
            root,
            storage,
        } => {
            let root = normalize_root(root);
            transaction::validate_git_vault(&root)?;
            config.upsert_profile(ProfileConfig {
                name: name.clone(),
                root,
                storage,
            });
            if config.profiles.len() == 1 {
                config.active_profile = name.clone();
            }
            store.save(&config)?;
            println!("added profile {name}");
        }
        ProfileCommand::Use { name } => {
            config.profile(&name)?;
            config.active_profile = name.clone();
            store.save(&config)?;
            println!("active profile {name}");
        }
    }
    Ok(())
}

fn transaction_options(args: cli::MutationArgs) -> Result<TransactionOptions> {
    transaction_options_from_parts(
        args.accept_external,
        args.restore_external,
        false,
        args.non_interactive,
    )
}

fn transaction_options_from_parts(
    accept_external: bool,
    restore_external: bool,
    review: bool,
    non_interactive: bool,
) -> Result<TransactionOptions> {
    let selected = [accept_external, restore_external, review]
        .into_iter()
        .filter(|selected| *selected)
        .count();
    if selected > 1 {
        return Err(eyre!("choose only one external-change policy"));
    }
    if review && non_interactive {
        return Err(eyre!("--review requires interactive input"));
    }

    let external_policy = match (accept_external, restore_external, review) {
        (true, false, false) => ExternalChangePolicy::Accept,
        (false, true, false) => ExternalChangePolicy::Restore,
        (false, false, true) => ExternalChangePolicy::Review,
        (false, false, false) => ExternalChangePolicy::Prompt,
        _ => return Err(eyre!("choose only one external-change policy")),
    };

    Ok(TransactionOptions {
        external_policy,
        non_interactive,
    })
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
        ConfigKey::ActiveProfile => {
            config.profile(&value)?;
            config.active_profile = value;
        }
        ConfigKey::DefaultSearch => {
            config.default_search = search::SearchMode::parse_config_value(&value)?.to_string()
        }
        ConfigKey::Editor => {
            let value = value.trim();
            config.editor = (!value.is_empty()).then(|| value.to_string());
        }
    }

    Ok(())
}

fn active_profile(store: &ConfigStore) -> Result<(AppConfig, ProfileConfig)> {
    let config = store.load_or_default()?;
    let profile = config.active_profile()?.clone();
    Ok((config, profile))
}

fn active_workspace(store: &ConfigStore) -> Result<Workspace> {
    let (_, profile) = active_profile(store)?;
    Ok(Workspace::new(&profile))
}

fn run_editor(editor: &str, path: &Path) -> Result<std::process::ExitStatus> {
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| eyre!("editor command cannot be empty"))?;

    ProcessCommand::new(program)
        .args(parts)
        .arg(path)
        .status()
        .map_err(Into::into)
}

fn print_config(config: &AppConfig) -> Result<()> {
    println!("{}", toml::to_string_pretty(config)?);
    Ok(())
}
