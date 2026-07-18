mod cli;
mod config;
mod conflict;
mod doctor;
mod frontmatter;
mod index;
mod memory;
mod output;
mod policy;
mod review;
mod search;
mod semantic;
mod transaction;
mod tui;
mod workspace;

use std::{env, fs, path::Path, process::Command as ProcessCommand};

use cli::{Cli, Command, ConfigCommand, ConfigKey, ConflictCommand, ProfileCommand};
use color_eyre::eyre::{Result, eyre};
use config::{AppConfig, ConfigStore, ProfileConfig, StorageMode, normalize_root};
use memory::{CreateMemoryInput, MemoryFilter};
use output::Tone;
use transaction::{ExternalChangePolicy, TransactionOptions};
use workspace::Workspace;

fn main() {
    if let Err(err) = run() {
        output::error(err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse_grouped();
    output::configure(cli.color);
    let store = ConfigStore::new()?;

    match cli.command.unwrap_or(Command::Configure) {
        Command::Configure => tui::run(store),
        Command::Init(args) => run_init(store, args),
        Command::Profile { command } => run_profile_command(store, command),
        Command::Add(args) => {
            let workspace = active_workspace(&store)?;
            validate_mutation_workspace(&workspace)?;
            let body = args.body();
            let memory_type = args.resolved_type();
            let scope = args.scope;
            let kind = args.kind;
            let tags = args.tags;
            let source = args.source;
            let source_id = args.source_id;
            let agent = args.agent;
            let session = args.session;
            if let Some(source_id) = &source_id {
                let matches =
                    memory::find_memories_by_source_identity(&workspace, &source, source_id)?;
                match matches.len() {
                    0 => {}
                    1 => {
                        let existing = &matches[0];
                        if existing.metadata.status != memory::MemoryStatus::Active {
                            return Err(eyre!(
                                "source identity {:?}/{} belongs to {} memory {}; use an explicit action instead of add",
                                source,
                                source_id,
                                existing.metadata.status,
                                existing.metadata.id
                            ));
                        }
                        if memory::bodies_match(&existing.body, &body) {
                            output::line(format!(
                                "{} {} {}",
                                output::paint("no-op", Tone::Warning),
                                output::paint(&existing.metadata.id, Tone::Id),
                                output::key_value("reason", "source-identity", Tone::Muted)
                            ));
                            return Ok(());
                        }
                        return Err(eyre!(
                            "source identity {:?}/{} already belongs to memory {}; run `rem update {}` to change it",
                            source,
                            source_id,
                            existing.metadata.id,
                            existing.metadata.id
                        ));
                    }
                    count => {
                        return Err(eyre!(
                            "source identity {:?}/{} matches {count} memories; resolve duplicate source_id values before adding",
                            source,
                            source_id
                        ));
                    }
                }
            }
            let input = CreateMemoryInput::new(memory_type, scope, kind, body)
                .tags(tags)
                .source(Some(source))
                .source_id(source_id)
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
            let memory_type = memory.metadata.memory_type.to_string();
            output::line(output::action(
                "added",
                format!(
                    "{} {}",
                    output::paint(&memory_type, output::memory_type_tone(&memory_type)),
                    output::paint(&memory.metadata.id, Tone::Id)
                ),
                Tone::Success,
            ));
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
            let rows = memories
                .into_iter()
                .map(|memory| {
                    let memory_type = memory.metadata.memory_type.to_string();
                    let memory_type_tone = output::memory_type_tone(&memory_type);
                    let title = memory.title();
                    [
                        (memory.metadata.id, Tone::Id),
                        (memory_type, memory_type_tone),
                        (memory.metadata.scope.to_string(), Tone::Scope),
                        (memory.metadata.kind.to_string(), Tone::Kind),
                        (title, Tone::Title),
                    ]
                })
                .collect::<Vec<_>>();

            if output::stdout_is_terminal() {
                let headers = ["ID", "TYPE", "SCOPE", "KIND", "TITLE"];
                let widths = std::array::from_fn(|index| {
                    rows.iter()
                        .map(|row| row[index].0.chars().count())
                        .chain([headers[index].len()])
                        .max()
                        .unwrap_or_default()
                });
                output::line(output::table_row(
                    headers.map(|header| (header.to_string(), Tone::Key)),
                    widths,
                ));
                for row in rows {
                    output::line(output::table_row(row, widths));
                }
            } else {
                for row in rows {
                    output::line(output::row(row));
                }
            }
            Ok(())
        }
        Command::Show(args) => {
            let workspace = active_workspace(&store)?;
            let memory = memory::find_memory(&workspace, &args.id, true)?;
            output::markdown(&memory.to_markdown());
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
                    memory::ensure_active_memory(&memory, "edited")?;
                    let path = memory
                        .path
                        .ok_or_else(|| eyre!("memory has no filesystem path"))?;
                    memory::ensure_mutable_memory_path(&path)?;
                    let status = run_editor(&editor, &path)?;
                    if !status.success() {
                        return Err(eyre!("editor exited with status {status}"));
                    }
                    Ok((path, format!("rem: update memory {id}")))
                },
            )?;
            output::line(output::action(
                "edited",
                output::paint(path.display(), Tone::Path),
                Tone::Success,
            ));
            Ok(())
        }
        Command::Update(args) => {
            let workspace = active_workspace(&store)?;
            validate_mutation_workspace(&workspace)?;
            let body = args.body();
            let id = args.id;
            let existing = memory::find_memory(&workspace, &id, false)?;
            memory::ensure_active_memory(&existing, "updated")?;
            if memory::bodies_match(&existing.body, &body) {
                output::line(format!(
                    "{} {} {}",
                    output::paint("no-op", Tone::Warning),
                    output::paint(&existing.metadata.id, Tone::Id),
                    output::key_value("reason", "unchanged-body", Tone::Muted)
                ));
                return Ok(());
            }
            let id = existing.metadata.id;
            let (memory, _) = transaction::run_mutation_with_message(
                &workspace,
                transaction_options(args.tx)?,
                || {
                    let memory = memory::update_memory(&workspace, &id, body, false)?;
                    Ok((
                        memory.clone(),
                        format!("rem: update memory {}", memory.metadata.id),
                    ))
                },
            )?;
            output::line(output::action(
                "updated",
                output::paint(&memory.metadata.id, Tone::Id),
                Tone::Success,
            ));
            Ok(())
        }
        Command::Append(args) => run_append(&store, args),
        Command::Review(args) => run_review(&store, args),
        Command::Supersede(args) => run_supersede(&store, args),
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
                output::line(output::action(
                    "deleted",
                    output::paint(&memory.metadata.id, Tone::Id),
                    Tone::Error,
                ));
            } else {
                output::line(output::action(
                    "archived",
                    output::paint(&memory.metadata.id, Tone::Id),
                    Tone::Warning,
                ));
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
            output::line(output::action(
                "promoted",
                output::paint(&memory.metadata.id, Tone::Id),
                Tone::Success,
            ));
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
                output::line(output::row([
                    (result.id, Tone::Id),
                    (
                        result.memory_type.clone(),
                        output::memory_type_tone(&result.memory_type),
                    ),
                    (result.source, Tone::Source),
                    (result.title, Tone::Title),
                    (format!("{:.3}", result.score), Tone::Number),
                    (result.path, Tone::Path),
                ]));
            }
            Ok(())
        }
        Command::Facts(args) => {
            let workspace = active_workspace(&store)?;
            let facts = search::facts(
                &workspace,
                semantic::FactQuery {
                    entity: args.entity,
                    relation: args.relation,
                    at: args.at,
                    include_expired: args.all,
                },
            )?;
            for fact in facts {
                if args.source {
                    output::line(output::row([
                        (fact.id, Tone::Id),
                        (fact.subject, Tone::Title),
                        (fact.relation, Tone::Kind),
                        (fact.object, Tone::Value),
                        (
                            fact.valid_from.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (
                            fact.valid_to.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (
                            fact.expired_at.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (fact.source_memory_id, Tone::Id),
                        (
                            fact.confidence
                                .map(|confidence| confidence.to_string())
                                .unwrap_or_default(),
                            Tone::Number,
                        ),
                        (fact.source_path, Tone::Path),
                        (fact.episode_id, Tone::Source),
                        (fact.excerpt, Tone::Muted),
                    ]));
                } else {
                    output::line(output::row([
                        (fact.id, Tone::Id),
                        (fact.subject, Tone::Title),
                        (fact.relation, Tone::Kind),
                        (fact.object, Tone::Value),
                        (
                            fact.valid_from.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (
                            fact.valid_to.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (
                            fact.expired_at.as_deref().unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                        (fact.learned_at, Tone::Muted),
                        (
                            fact.confidence
                                .map(|confidence| confidence.to_string())
                                .unwrap_or_default(),
                            Tone::Number,
                        ),
                        (fact.source_memory_id, Tone::Id),
                    ]));
                }
            }
            Ok(())
        }
        Command::Conflict { command } => run_conflict_command(&store, command),
        Command::Rebuild => {
            let workspace = active_workspace(&store)?;
            let report = transaction::rebuild_index(&workspace)?;
            output::line(output::action(
                "rebuilt",
                format!(
                    "{} {} {} {} {} {} {}",
                    output::paint(&report.index_path, Tone::Path),
                    output::key_value("indexed", report.indexed, Tone::Number),
                    output::key_value("diagnostics", report.diagnostics, Tone::Number),
                    output::key_value("semantic_entities", report.semantic_entities, Tone::Number),
                    output::key_value("semantic_episodes", report.semantic_episodes, Tone::Number),
                    output::key_value("semantic_facts", report.semantic_facts, Tone::Number),
                    output::key_value(
                        "semantic_conflicts",
                        report.semantic_conflicts,
                        Tone::Number
                    )
                ),
                Tone::Success,
            ));
            Ok(())
        }
        Command::Doctor => {
            let config = store.load_or_default()?;
            output::line(output::colon_value(
                "config",
                store.path().display(),
                Tone::Path,
            ));
            if config.profiles.is_empty() {
                output::line(output::row([
                    ("warn".to_string(), Tone::Warning),
                    (
                        "no profiles configured; run `rem init --root <path>`".to_string(),
                        Tone::Value,
                    ),
                ]));
                return Ok(());
            }
            let profile = match config.active_profile() {
                Ok(profile) => profile.clone(),
                Err(err) => {
                    output::line(output::row([
                        ("warn".to_string(), Tone::Warning),
                        (err.to_string(), Tone::Value),
                    ]));
                    return Ok(());
                }
            };
            let workspace = Workspace::new(&profile);
            output::line(output::colon_value(
                "active_profile",
                config.active_profile,
                Tone::Id,
            ));
            output::line(output::colon_value(
                "root",
                profile.root.display(),
                Tone::Path,
            ));
            for finding in doctor::run(&workspace, profile.storage)? {
                let tone = match finding.level {
                    doctor::DoctorLevel::Ok => Tone::Success,
                    doctor::DoctorLevel::Warn => Tone::Warning,
                };
                output::line(output::row([
                    (finding.level.to_string(), tone),
                    (finding.message, Tone::Value),
                ]));
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

    output::line(output::action(
        "initialized",
        output::paint(workspace.root().display(), Tone::Path),
        Tone::Success,
    ));
    Ok(())
}

fn run_review(store: &ConfigStore, args: cli::ReviewArgs) -> Result<()> {
    let workspace = active_workspace(store)?;
    let body = args.body();
    let plan = review::plan(&workspace, args.id.as_deref(), &body, args.scope)?;
    review::print_plan(&plan);

    let action = if args.non_interactive {
        plan.suggested_action
    } else {
        review::prompt_for_action(&plan)?
    };
    if action == review::ReviewAction::Abort {
        return Err(eyre!("memory review aborted"));
    }

    output::line(format!(
        "{} {} {} {}",
        output::paint("review", Tone::Info),
        output::key_value(
            "action",
            action.label(),
            output::action_tone(action.label())
        ),
        output::key_value("target", plan.target_id().unwrap_or("-"), Tone::Id),
        output::key_value("reason", plan.reason, Tone::Muted)
    ));
    Ok(())
}

fn run_conflict_command(store: &ConfigStore, command: ConflictCommand) -> Result<()> {
    let workspace = active_workspace(store)?;
    match command {
        ConflictCommand::List(args) => {
            let conn = conflict::open_index(&workspace.index_path())?;
            let conflicts = conflict::query(&conn, args.kind.map(Into::into), args.scope)?;
            let rows = conflicts
                .into_iter()
                .map(|conflict| {
                    [
                        (conflict.id, Tone::Id),
                        (conflict.kind.to_string(), Tone::Kind),
                        (conflict.scope.to_string(), Tone::Scope),
                        (
                            conflict.subject.unwrap_or_else(|| "-".to_string()),
                            Tone::Title,
                        ),
                        (
                            conflict.relation.unwrap_or_else(|| "-".to_string()),
                            Tone::Kind,
                        ),
                        (conflict.members.len().to_string(), Tone::Number),
                    ]
                })
                .collect::<Vec<_>>();
            if output::stdout_is_terminal() {
                let headers = ["ID", "KIND", "SCOPE", "SUBJECT", "RELATION", "MEMBERS"];
                let widths = std::array::from_fn(|index| {
                    rows.iter()
                        .map(|row| row[index].0.chars().count())
                        .chain([headers[index].len()])
                        .max()
                        .unwrap_or_default()
                });
                output::line(output::table_row(
                    headers.map(|header| (header.to_string(), Tone::Key)),
                    widths,
                ));
                for row in rows {
                    output::line(output::table_row(row, widths));
                }
            } else {
                for row in rows {
                    output::line(output::row(row));
                }
            }
            Ok(())
        }
        ConflictCommand::Show(args) => {
            let conn = conflict::open_index(&workspace.index_path())?;
            let conflict = conflict::find(&conn, &args.id)?;
            print_conflict(&conflict);
            Ok(())
        }
        ConflictCommand::Resolve(args) => run_conflict_resolve(&workspace, args),
    }
}

fn print_conflict(conflict: &conflict::Conflict) {
    output::line(output::colon_value("id", &conflict.id, Tone::Id));
    output::line(output::colon_value("kind", conflict.kind, Tone::Kind));
    output::line(output::colon_value("scope", conflict.scope, Tone::Scope));
    output::line(output::colon_value(
        "subject_id",
        conflict.subject_id.as_deref().unwrap_or("-"),
        Tone::Id,
    ));
    output::line(output::colon_value(
        "subject",
        conflict.subject.as_deref().unwrap_or("-"),
        Tone::Title,
    ));
    output::line(output::colon_value(
        "relation",
        conflict.relation.as_deref().unwrap_or("-"),
        Tone::Kind,
    ));
    output::line(output::colon_value(
        "members",
        conflict.members.len(),
        Tone::Number,
    ));
    for (index, member) in conflict.members.iter().enumerate() {
        output::line("");
        output::line(output::colon_value("member", index + 1, Tone::Number));
        output::line(output::colon_value(
            "memory_id",
            &member.memory_id,
            Tone::Id,
        ));
        output::line(output::colon_value(
            "memory_path",
            &member.memory_path,
            Tone::Path,
        ));
        output::line(output::colon_value(
            "memory_title",
            &member.memory_title,
            Tone::Title,
        ));
        output::line(output::colon_value("excerpt", &member.excerpt, Tone::Muted));
        if let Some(fact) = &member.fact {
            output::line(output::colon_value("fact_id", &fact.id, Tone::Id));
            output::line(output::colon_value(
                "object_id",
                fact.object_id.as_deref().unwrap_or("-"),
                Tone::Id,
            ));
            output::line(output::colon_value(
                "object",
                &fact.object_value,
                Tone::Value,
            ));
            output::line(output::colon_value(
                "valid_from",
                fact.valid_from.as_deref().unwrap_or("-"),
                Tone::Muted,
            ));
            output::line(output::colon_value(
                "valid_to",
                fact.valid_to.as_deref().unwrap_or("-"),
                Tone::Muted,
            ));
            output::line(output::colon_value(
                "learned_at",
                &fact.learned_at,
                Tone::Muted,
            ));
            output::line(output::colon_value(
                "expired_at",
                fact.expired_at.as_deref().unwrap_or("-"),
                Tone::Muted,
            ));
            output::line(output::colon_value(
                "confidence",
                fact.confidence
                    .map(|confidence| confidence.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                Tone::Number,
            ));
            output::line(output::colon_value("line", fact.line_number, Tone::Number));
        }
    }
}

fn run_conflict_resolve(workspace: &Workspace, args: cli::ConflictResolveArgs) -> Result<()> {
    let requested_expiration = args
        .at
        .as_deref()
        .map(semantic::parse_time_to_unix_seconds)
        .transpose()?;
    let conflict_id = args.id;
    let keep = args.keep;
    let options = transaction_options(args.tx)?;
    let ((resolution, kind), outcome) = transaction::run_mutation_with_message_checked(
        workspace,
        options,
        || {
            let current = refreshed_conflict(workspace, &conflict_id)?;
            let kind = current.kind;
            let resolution =
                conflict::apply_resolution(workspace, &current, &keep, requested_expiration)?;
            let message = match kind {
                conflict::ConflictKind::ExactActiveDuplicate => format!(
                    "rem: resolve duplicate conflict {} keep memory {}",
                    current.id, resolution.kept_id
                ),
                conflict::ConflictKind::ExclusiveCurrent => format!(
                    "rem: resolve semantic conflict {} keep fact {}",
                    current.id, resolution.kept_id
                ),
            };
            Ok(((resolution, kind), message))
        },
        |_| {
            let conn = conflict::open_index(&workspace.index_path())?;
            if conflict::find_exact(&conn, &conflict_id)?.is_some() {
                return Err(eyre!(
                    "conflict {conflict_id} remains after resolution; rolled back without committing"
                ));
            }
            Ok(())
        },
    )?;

    output::line(format!(
        "{} {} {} {} {} {}",
        output::paint("resolved", Tone::Success),
        output::paint(&resolution.conflict_id, Tone::Id),
        output::key_value("kind", kind, Tone::Kind),
        output::key_value("kept", &resolution.kept_id, Tone::Id),
        output::key_value(
            "archived",
            resolution.archived_memory_ids.len(),
            Tone::Number
        ),
        output::key_value("expired", resolution.expired_fact_ids.len(), Tone::Number)
    ));
    if let Some(expired_at) = resolution.expired_at {
        output::line(output::colon_value("expired_at", expired_at, Tone::Muted));
    }
    for memory_id in &resolution.archived_memory_ids {
        output::line(output::row([
            ("archived".to_string(), Tone::Warning),
            (memory_id.clone(), Tone::Id),
        ]));
    }
    for fact_id in &resolution.expired_fact_ids {
        output::line(output::row([
            ("expired".to_string(), Tone::Warning),
            (fact_id.clone(), Tone::Id),
        ]));
    }
    if let Some(commit_id) = outcome.commit_id {
        output::line(output::colon_value("commit", commit_id, Tone::Id));
    }
    Ok(())
}

fn refreshed_conflict(workspace: &Workspace, id_or_prefix: &str) -> Result<conflict::Conflict> {
    let temp_path = index::temp_index_path(workspace, "conflict-resolve");
    let result = (|| {
        let report = index::rebuild_to_path(workspace, &temp_path)?;
        if report.diagnostics > 0 {
            let details = index::diagnostic_messages(&temp_path)?.join("\n");
            return Err(eyre!(
                "reindex produced {} diagnostics; fix malformed or duplicate memory files before resolving conflicts:\n{}",
                report.diagnostics,
                details
            ));
        }
        let conn = conflict::open_index(&temp_path)?;
        conflict::find(&conn, id_or_prefix)
    })();
    match fs::remove_file(&temp_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) if result.is_ok() => {
            return Err(eyre!(
                "failed to remove temporary conflict index {}: {err}",
                temp_path.display()
            ));
        }
        Err(_) => {}
    }
    result
}

fn run_append(store: &ConfigStore, args: cli::AppendArgs) -> Result<()> {
    let workspace = active_workspace(store)?;
    validate_mutation_workspace(&workspace)?;
    let body = args.body();
    let existing = memory::find_memory(&workspace, &args.id, false)?;
    memory::ensure_active_memory(&existing, "appended")?;
    let id = existing.metadata.id;
    let (memory, _) =
        transaction::run_mutation_with_message(&workspace, transaction_options(args.tx)?, || {
            let memory = memory::update_memory(&workspace, &id, body, true)?;
            Ok((
                memory.clone(),
                format!("rem: append memory {}", memory.metadata.id),
            ))
        })?;
    output::line(output::action(
        "appended",
        output::paint(&memory.metadata.id, Tone::Id),
        Tone::Success,
    ));
    Ok(())
}

fn run_supersede(store: &ConfigStore, args: cli::SupersedeArgs) -> Result<()> {
    let workspace = active_workspace(store)?;
    validate_mutation_workspace(&workspace)?;
    let source = memory::find_memory(&workspace, &args.id, true)?;
    memory::ensure_active_memory(&source, "superseded")?;

    let body = args.body();
    if memory::bodies_match(&source.body, &body) && !args.has_metadata_overrides() {
        output::line(format!(
            "{} {} {}",
            output::paint("no-op", Tone::Warning),
            output::paint(&source.metadata.id, Tone::Id),
            output::key_value("reason", "unchanged-body", Tone::Muted)
        ));
        return Ok(());
    }
    let memory_type = args.resolved_type(source.metadata.memory_type);
    let scope = args.scope.unwrap_or(source.metadata.scope);
    let kind = args.kind.unwrap_or(source.metadata.kind);
    let tags = if args.tags.is_empty() {
        source.metadata.tags.clone()
    } else {
        args.tags
    };
    let new_source = args.source.unwrap_or_else(|| "supersede".to_string());
    if let Some(source_id) = &args.source_id {
        let matches = memory::find_memories_by_source_identity(&workspace, &new_source, source_id)?;
        if let Some(existing) = matches.first() {
            return Err(eyre!(
                "source identity {:?}/{} already belongs to memory {}; use an explicit update or a new source_id",
                new_source,
                source_id,
                existing.metadata.id
            ));
        }
    }
    let input = CreateMemoryInput::new(memory_type, scope, kind, body)
        .tags(tags)
        .source(Some(new_source))
        .source_id(args.source_id)
        .agent(args.agent.or(source.metadata.agent.clone()))
        .session(args.session.or(source.metadata.session.clone()));
    let source_id = source.metadata.id;
    let ((_, replacement), _) =
        transaction::run_mutation_with_message(&workspace, transaction_options(args.tx)?, || {
            workspace.init()?;
            let (superseded, replacement) =
                memory::supersede_memory(&workspace, &source_id, input)?;
            Ok((
                (superseded, replacement.clone()),
                format!(
                    "rem: supersede memory {} with {}",
                    source_id, replacement.metadata.id
                ),
            ))
        })?;
    output::line(format!(
        "{} {} {} {}",
        output::paint("superseded", Tone::Success),
        output::paint(&source_id, Tone::Id),
        output::paint("with", Tone::Muted),
        output::paint(&replacement.metadata.id, Tone::Id)
    ));
    Ok(())
}

fn run_commit(store: &ConfigStore, args: cli::CommitArgs) -> Result<()> {
    let workspace = active_workspace(store)?;

    if args.dry_run {
        let report = transaction::dry_run(&workspace)?;
        output::line(format!(
            "{} {}",
            output::paint("dry-run", Tone::Info),
            output::key_value("changes", report.changed_paths.len(), Tone::Number)
        ));
        for path in &report.changed_paths {
            output::line(output::row([
                ("would commit".to_string(), Tone::Info),
                (path.to_string(), Tone::Path),
            ]));
        }
        output::line(format!(
            "{} {} {} {}",
            output::paint("dry-run", Tone::Info),
            output::paint("reindex", Tone::Muted),
            output::key_value("indexed", report.indexed, Tone::Number),
            output::key_value("diagnostics", report.diagnostics, Tone::Number)
        ));
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
        output::line(output::action(
            "committed",
            format!(
                "{} {} {}",
                output::paint(commit_id, Tone::Id),
                output::key_value("changes", outcome.changed_paths.len(), Tone::Number),
                output::key_value("indexed", outcome.indexed, Tone::Number)
            ),
            Tone::Success,
        ));
    } else {
        output::line(format!(
            "{} {} {}",
            output::paint("nothing", Tone::Warning),
            output::paint("to commit", Tone::Muted),
            output::key_value("indexed", outcome.indexed, Tone::Number)
        ));
    }

    Ok(())
}

fn run_profile_command(store: ConfigStore, command: ProfileCommand) -> Result<()> {
    let mut config = store.load_or_default()?;
    match command {
        ProfileCommand::List => {
            if config.profiles.is_empty() {
                output::line(output::paint(
                    "no profiles configured; run `rem init --root <path>`",
                    Tone::Warning,
                ));
            }
            for profile in &config.profiles {
                let active = if profile.name == config.active_profile {
                    "*"
                } else {
                    " "
                };
                let marker_tone = if active == "*" {
                    Tone::Success
                } else {
                    Tone::Muted
                };
                output::line(format!(
                    "{} {}\t{}\t{}",
                    output::paint(active, marker_tone),
                    output::paint(&profile.name, Tone::Id),
                    output::paint(profile.storage, Tone::Source),
                    output::paint(profile.root.display(), Tone::Path)
                ));
            }
        }
        ProfileCommand::Show { name } => {
            let name = name.unwrap_or_else(|| config.active_profile.clone());
            let profile = config.profile(&name)?;
            output::line(format!(
                "{} = {}",
                output::paint("name", Tone::Key),
                output::paint(&profile.name, Tone::Id)
            ));
            output::line(format!(
                "{} = {}",
                output::paint("storage", Tone::Key),
                output::paint(profile.storage, Tone::Source)
            ));
            output::line(format!(
                "{} = {}",
                output::paint("root", Tone::Key),
                output::paint(profile.root.display(), Tone::Path)
            ));
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
            output::line(output::action(
                "added profile",
                output::paint(name, Tone::Id),
                Tone::Success,
            ));
        }
        ProfileCommand::Use { name } => {
            config.profile(&name)?;
            config.active_profile = name.clone();
            store.save(&config)?;
            output::line(output::action(
                "active profile",
                output::paint(name, Tone::Id),
                Tone::Success,
            ));
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
            output::line(output::action(
                "created",
                output::paint(store.path().display(), Tone::Path),
                Tone::Success,
            ));
            print_config(&config)?;
        }
        ConfigCommand::Path => {
            output::line(output::paint(store.path().display(), Tone::Path));
        }
        ConfigCommand::Show => {
            let config = store.load_or_default()?;
            print_config(&config)?;
        }
        ConfigCommand::Set { key, value } => {
            let mut config = store.load_or_default()?;
            set_config_value(&mut config, key, value)?;
            store.save(&config)?;
            output::line(format!(
                "{} {} {} {}",
                output::paint("updated", Tone::Success),
                output::paint(key, Tone::Key),
                output::paint("in", Tone::Muted),
                output::paint(store.path().display(), Tone::Path)
            ));
        }
        ConfigCommand::Reset => {
            let config = store.reset()?;
            output::line(output::action(
                "reset",
                output::paint(store.path().display(), Tone::Path),
                Tone::Warning,
            ));
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

fn validate_mutation_workspace(workspace: &Workspace) -> Result<()> {
    transaction::validate_git_vault(workspace.root()).map(|_| ())
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
    output::toml(&toml::to_string_pretty(config)?);
    output::line("");
    Ok(())
}
