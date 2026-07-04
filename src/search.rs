use std::{collections::BTreeMap, fmt, fs};

use clap::ValueEnum;
use color_eyre::eyre::{Result, eyre};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{memory, workspace::Workspace};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SearchMode {
    Auto,
    Grep,
    Bm25,
    Vector,
    All,
}

impl SearchMode {
    pub fn parse_config_value(value: &str) -> Result<Self> {
        match value.trim() {
            "auto" => Ok(Self::Auto),
            "grep" => Ok(Self::Grep),
            "bm25" => Ok(Self::Bm25),
            "vector" => Ok(Self::Vector),
            "all" => Ok(Self::All),
            other => Err(eyre!(
                "invalid search mode {other:?}; expected auto, grep, bm25, vector, or all"
            )),
        }
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Grep => f.write_str("grep"),
            Self::Bm25 => f.write_str("bm25"),
            Self::Vector => f.write_str("vector"),
            Self::All => f.write_str("all"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub id: String,
    pub memory_type: String,
    pub title: String,
    pub path: String,
    pub source: String,
    pub score: f64,
}

pub fn search(workspace: &Workspace, query: &str, mode: SearchMode) -> Result<Vec<SearchResult>> {
    if query.trim().is_empty() {
        return Err(eyre!("search query cannot be empty"));
    }

    match mode {
        SearchMode::Grep => grep(workspace, query),
        SearchMode::Bm25 => bm25(workspace, query),
        SearchMode::Auto => merge(grep(workspace, query)?, bm25_if_indexed(workspace, query)?),
        SearchMode::All => merge(grep(workspace, query)?, bm25_if_indexed(workspace, query)?),
        SearchMode::Vector => Err(eyre!("vector search is not configured in v1")),
    }
}

fn grep(workspace: &Workspace, query: &str) -> Result<Vec<SearchResult>> {
    let needle = query.to_lowercase();
    let mut results = Vec::new();
    for path in memory::memory_paths(workspace, false)? {
        let memory = match memory::read_memory(&path) {
            Ok(memory) => memory,
            Err(err) => {
                eprintln!("warn\t{}: {err}", path.display());
                continue;
            }
        };
        let haystack = format!(
            "{}\n{}\n{}",
            memory.title(),
            memory.body,
            memory.metadata.tags.join(" ")
        )
        .to_lowercase();
        let score = haystack.matches(&needle).count() as f64;
        if score > 0.0 {
            let title = memory.title();
            results.push(SearchResult {
                id: memory.metadata.id.clone(),
                memory_type: memory.metadata.memory_type.to_string(),
                title,
                path: path.display().to_string(),
                source: "grep".to_string(),
                score,
            });
        } else if fs::read_to_string(&path)
            .unwrap_or_default()
            .to_lowercase()
            .contains(&needle)
        {
            let title = memory.title();
            results.push(SearchResult {
                id: memory.metadata.id.clone(),
                memory_type: memory.metadata.memory_type.to_string(),
                title,
                path: path.display().to_string(),
                source: "grep".to_string(),
                score: 1.0,
            });
        }
    }
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.id.cmp(&right.id))
    });
    Ok(results)
}

fn bm25_if_indexed(workspace: &Workspace, query: &str) -> Result<Vec<SearchResult>> {
    if workspace.index_path().exists() {
        match bm25(workspace, query) {
            Ok(results) => Ok(results),
            Err(err) => {
                eprintln!(
                    "warn\t{}: {err}; run `rem rebuild`",
                    workspace.index_path().display()
                );
                Ok(Vec::new())
            }
        }
    } else {
        Ok(Vec::new())
    }
}

fn bm25(workspace: &Workspace, query: &str) -> Result<Vec<SearchResult>> {
    if !workspace.index_path().exists() {
        return Err(eyre!(
            "search index does not exist; run `rem rebuild` before BM25 search"
        ));
    }
    let conn = Connection::open(workspace.index_path())?;
    let Some(fts_query) = sanitize_fts_query(query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT m.id, m.memory_type, m.title, m.path, bm25(memories_fts) AS score
         FROM memories_fts
         JOIN memories m ON m.id = memories_fts.id
         WHERE memories_fts MATCH ?1 AND m.status = 'active'
         ORDER BY score ASC, m.id ASC
         LIMIT 50",
    )?;
    let rows = stmt.query_map(params![fts_query], |row| {
        let raw_score = row.get::<_, f64>(4).unwrap_or(0.0);
        Ok(SearchResult {
            id: row.get(0)?,
            memory_type: row.get(1)?,
            title: row.get(2)?,
            path: row.get(3)?,
            source: "bm25".to_string(),
            score: (-raw_score).max(0.0),
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn merge(left: Vec<SearchResult>, right: Vec<SearchResult>) -> Result<Vec<SearchResult>> {
    let mut by_id = BTreeMap::<String, SearchResult>::new();
    for mut result in left.into_iter().chain(right) {
        if let Some(existing) = by_id.get_mut(&result.id) {
            existing.source = format!("{},{}", existing.source, result.source);
            existing.score += result.score.abs().max(1.0);
        } else {
            result.score = result.score.abs().max(1.0);
            by_id.insert(result.id.clone(), result);
        }
    }
    let mut results = by_id.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.id.cmp(&right.id))
    });
    Ok(results)
}

fn sanitize_fts_query(query: &str) -> Option<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }

    if terms.is_empty() {
        None
    } else {
        Some(
            terms
                .into_iter()
                .map(|term| format!("\"{term}\""))
                .collect::<Vec<_>>()
                .join(" AND "),
        )
    }
}
