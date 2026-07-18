use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{Result, WrapErr};
use rusqlite::{Connection, params};

use crate::{conflict, memory, semantic, workspace::Workspace};

pub fn rebuild(workspace: &Workspace) -> Result<RebuildReport> {
    fs::create_dir_all(workspace.cache_dir())
        .wrap_err_with(|| format!("failed to create {}", workspace.cache_dir().display()))?;
    let index_path = workspace.index_path();
    let temp_path = temp_index_path(workspace, "rebuild");
    let report = rebuild_to_path(workspace, &temp_path)?;
    fs::rename(&temp_path, &index_path).wrap_err_with(|| {
        format!(
            "failed to replace {} with {}",
            index_path.display(),
            temp_path.display()
        )
    })?;

    Ok(RebuildReport {
        index_path: index_path.display().to_string(),
        ..report
    })
}

pub fn rebuild_to_path(workspace: &Workspace, index_path: &Path) -> Result<RebuildReport> {
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    if index_path.exists() {
        fs::remove_file(index_path)
            .wrap_err_with(|| format!("failed to remove {}", index_path.display()))?;
    }

    let conn = Connection::open(index_path)
        .wrap_err_with(|| format!("failed to open {}", index_path.display()))?;
    create_schema(&conn)?;

    let mut diagnostics = 0usize;
    let mut winners = BTreeMap::<String, memory::Memory>::new();
    for path in memory::memory_paths(workspace, true)? {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                insert_diagnostic(
                    &conn,
                    path.display().to_string(),
                    "error",
                    "memory file must be a regular vault file, not a symlink".to_string(),
                )?;
                diagnostics += 1;
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                insert_diagnostic(&conn, path.display().to_string(), "error", err.to_string())?;
                diagnostics += 1;
                continue;
            }
        }
        match memory::read_memory(&path) {
            Ok(memory) => {
                let id = memory.metadata.id.clone();
                if let Some(existing) = winners.get(&id) {
                    diagnostics += 1;
                    if memory::effective_priority(&memory) < memory::effective_priority(existing) {
                        insert_diagnostic(
                            &conn,
                            memory_path_label(existing),
                            "error",
                            format!(
                                "duplicate memory id {id}; ignored in favor of {}",
                                path.display()
                            ),
                        )?;
                        winners.insert(id, memory);
                    } else {
                        insert_diagnostic(
                            &conn,
                            path.display().to_string(),
                            "error",
                            format!("duplicate memory id {id}"),
                        )?;
                    }
                } else {
                    winners.insert(id, memory);
                }
            }
            Err(err) => {
                insert_diagnostic(&conn, path.display().to_string(), "error", err.to_string())?;
                diagnostics += 1;
            }
        }
    }

    let mut source_identities = BTreeMap::<(String, String), (String, String)>::new();
    for memory in winners.values() {
        let (Some(source), Some(source_id)) = (
            memory.metadata.source.as_ref(),
            memory.metadata.source_id.as_ref(),
        ) else {
            continue;
        };
        let key = (source.clone(), source_id.clone());
        let path = memory_path_label(memory);
        if let Some((existing_id, existing_path)) = source_identities.get(&key) {
            diagnostics += 1;
            insert_diagnostic(
                &conn,
                path,
                "error",
                format!(
                    "duplicate source identity {source:?}/{source_id}; already used by memory {existing_id} at {existing_path}"
                ),
            )?;
        } else {
            source_identities.insert(key, (memory.metadata.id.clone(), path));
        }
    }

    let indexed_memories = winners.into_values().collect::<Vec<_>>();
    let indexed = indexed_memories.len();
    let mut extractions = BTreeMap::new();
    for memory in &indexed_memories {
        insert_memory(&conn, memory)?;
        let extraction = semantic::extract(memory).and_then(|extraction| {
            semantic::insert_extraction(&conn, &extraction, &memory_path_label(memory))?;
            Ok(extraction)
        });
        match extraction {
            Ok(extraction) => {
                extractions.insert(memory.metadata.id.clone(), extraction);
            }
            Err(err) => {
                diagnostics += 1;
                insert_diagnostic(
                    &conn,
                    memory_path_label(memory),
                    "error",
                    format!("semantic cache extraction failed: {err}"),
                )?;
            }
        }
    }
    semantic::validate_no_duplicate_fact_ids(&conn)?;
    let mut conflicts = conflict::detect_current(&indexed_memories, &extractions)?;
    for diagnostic in conflict::apply_acceptances(workspace, &mut conflicts)? {
        diagnostics += 1;
        insert_diagnostic(&conn, diagnostic.path, "error", diagnostic.message)?;
    }
    conflict::insert_conflicts(&conn, &conflicts)?;
    let (semantic_entities, semantic_episodes, semantic_facts) = semantic::fact_counts(&conn)?;
    let semantic_conflicts = conflict::count(&conn)?;

    Ok(RebuildReport {
        indexed,
        diagnostics,
        semantic_entities,
        semantic_episodes,
        semantic_facts,
        semantic_conflicts,
        index_path: index_path.display().to_string(),
    })
}

pub fn temp_index_path(workspace: &Workspace, label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    workspace.cache_dir().join(format!(
        "index.sqlite.tmp-{label}-{}-{nanos}",
        process::id()
    ))
}

pub fn diagnostics_count(workspace: &Workspace) -> Result<usize> {
    if !workspace.index_path().exists() {
        return Ok(0);
    }
    let conn = Connection::open(workspace.index_path())?;
    let count = conn.query_row("SELECT COUNT(*) FROM diagnostics", [], |row| row.get(0))?;
    Ok(count)
}

pub fn diagnostic_messages(index_path: &Path) -> Result<Vec<String>> {
    let conn = Connection::open(index_path)
        .wrap_err_with(|| format!("failed to open {}", index_path.display()))?;
    let mut stmt = conn.prepare(
        "SELECT path, severity, message FROM diagnostics ORDER BY path ASC, severity ASC, message ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(format!(
            "{} [{}]: {}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE memories (
  id TEXT PRIMARY KEY,
  memory_type TEXT NOT NULL,
  scope TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  path TEXT NOT NULL,
  tags TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  source TEXT,
  source_id TEXT,
  agent TEXT,
  session TEXT,
  confidence TEXT,
  promoted_from TEXT,
  supersedes TEXT
);

CREATE VIRTUAL TABLE memories_fts USING fts5(
  id UNINDEXED,
  title,
  body,
  tags,
  scope,
  kind
);

CREATE TABLE diagnostics (
  path TEXT NOT NULL,
  severity TEXT NOT NULL,
  message TEXT NOT NULL
);
"#,
    )?;
    semantic::create_schema(conn)?;
    conflict::create_schema(conn)?;
    Ok(())
}

fn insert_memory(conn: &Connection, memory: &memory::Memory) -> Result<()> {
    let path = memory_path_label(memory);
    conn.execute(
        "INSERT INTO memories (
            id, memory_type, scope, kind, status, title, body, path,
            tags, created_at, updated_at, source, source_id, agent, session,
            confidence, promoted_from, supersedes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            &memory.metadata.id,
            memory.metadata.memory_type.to_string(),
            memory.metadata.scope.to_string(),
            memory.metadata.kind.to_string(),
            memory.metadata.status.to_string(),
            memory.title(),
            &memory.body,
            path,
            memory.metadata.tags.join(","),
            &memory.metadata.created_at,
            &memory.metadata.updated_at,
            memory.metadata.source.as_deref(),
            memory.metadata.source_id.as_deref(),
            memory.metadata.agent.as_deref(),
            memory.metadata.session.as_deref(),
            memory.metadata.confidence.as_deref(),
            memory.metadata.promoted_from.as_deref(),
            memory.metadata.supersedes.join(","),
        ],
    )?;
    conn.execute(
        "INSERT INTO memories_fts (id, title, body, tags, scope, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &memory.metadata.id,
            memory.title(),
            &memory.body,
            memory.metadata.tags.join(" "),
            memory.metadata.scope.to_string(),
            memory.metadata.kind.to_string(),
        ],
    )?;
    Ok(())
}

fn insert_diagnostic(
    conn: &Connection,
    path: String,
    severity: &'static str,
    message: String,
) -> Result<()> {
    conn.execute(
        "INSERT INTO diagnostics (path, severity, message) VALUES (?1, ?2, ?3)",
        params![path, severity, message],
    )?;
    Ok(())
}

fn memory_path_label(memory: &memory::Memory) -> String {
    memory
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("<memory:{}>", memory.metadata.id))
}

#[derive(Clone, Debug)]
pub struct RebuildReport {
    pub indexed: usize,
    pub diagnostics: usize,
    pub semantic_entities: usize,
    pub semantic_episodes: usize,
    pub semantic_facts: usize,
    pub semantic_conflicts: usize,
    pub index_path: String,
}
