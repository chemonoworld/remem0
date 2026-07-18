use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    str::FromStr,
};

use color_eyre::eyre::{Result, WrapErr, eyre};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    memory::{self, Memory, MemoryScope, MemoryStatus},
    semantic::{self, SemanticExtraction, SemanticFact},
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConflictKind {
    ExactActiveDuplicate,
    ExclusiveCurrent,
}

impl ConflictKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ExactActiveDuplicate => "exact-active-duplicate",
            Self::ExclusiveCurrent => "exclusive-current-conflict",
        }
    }

    fn from_label(value: &str) -> Result<Self> {
        match value {
            "exact-active-duplicate" => Ok(Self::ExactActiveDuplicate),
            "exclusive-current-conflict" => Ok(Self::ExclusiveCurrent),
            other => Err(eyre!("unknown semantic conflict kind {other:?}")),
        }
    }
}

impl fmt::Display for ConflictKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub id: String,
    pub kind: ConflictKind,
    pub scope: MemoryScope,
    pub subject_id: Option<String>,
    pub subject: Option<String>,
    pub relation: Option<String>,
    pub members: Vec<ConflictMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConflictMember {
    pub memory_id: String,
    pub memory_path: String,
    pub memory_title: String,
    pub excerpt: String,
    pub fact: Option<ConflictFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConflictFact {
    pub id: String,
    pub object_id: Option<String>,
    pub object_value: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub learned_at: String,
    pub expired_at: Option<String>,
    pub confidence: Option<f64>,
    pub line_number: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub conflict_id: String,
    pub kept_id: String,
    pub archived_memory_ids: Vec<String>,
    pub expired_fact_ids: Vec<String>,
    pub expired_at: Option<i64>,
}

pub fn detect_current(
    memories: &[Memory],
    extractions: &BTreeMap<String, SemanticExtraction>,
) -> Result<Vec<Conflict>> {
    detect_at(memories, extractions, semantic::now_unix_seconds())
}

pub(crate) fn detect_at(
    memories: &[Memory],
    extractions: &BTreeMap<String, SemanticExtraction>,
    at: i64,
) -> Result<Vec<Conflict>> {
    let mut conflicts = exact_active_duplicates(memories);
    conflicts.extend(exclusive_current_conflicts(memories, extractions, at)?);
    conflicts.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.scope.to_string().cmp(&right.scope.to_string()))
            .then_with(|| left.subject_id.cmp(&right.subject_id))
            .then_with(|| left.relation.cmp(&right.relation))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(conflicts)
}

fn exact_active_duplicates(memories: &[Memory]) -> Vec<Conflict> {
    let mut groups = BTreeMap::<(String, String), BTreeMap<String, &Memory>>::new();
    for memory in memories
        .iter()
        .filter(|memory| memory.metadata.status == MemoryStatus::Active)
    {
        let key = (
            memory.metadata.scope.to_string(),
            memory::canonical_body(&memory.body),
        );
        groups
            .entry(key)
            .or_default()
            .insert(memory.metadata.id.clone(), memory);
    }

    groups
        .into_iter()
        .filter_map(|((scope, body), memories)| {
            if memories.len() < 2 {
                return None;
            }
            let scope_value = memories.values().next().unwrap().metadata.scope;
            let members = memories
                .into_values()
                .map(|memory| memory_member(memory, None, semantic::excerpt(&memory.body)))
                .collect();
            Some(Conflict {
                id: semantic::stable_id(
                    "conflict",
                    &format!("exact-active-duplicate\n{scope}\n{body}"),
                ),
                kind: ConflictKind::ExactActiveDuplicate,
                scope: scope_value,
                subject_id: None,
                subject: None,
                relation: None,
                members,
            })
        })
        .collect()
}

fn exclusive_current_conflicts(
    memories: &[Memory],
    extractions: &BTreeMap<String, SemanticExtraction>,
    at: i64,
) -> Result<Vec<Conflict>> {
    let mut groups = BTreeMap::<SemanticKey, BTreeMap<(String, String), FactCandidate<'_>>>::new();

    for memory in memories
        .iter()
        .filter(|memory| memory.metadata.status == MemoryStatus::Active)
    {
        let Some(extraction) = extractions.get(&memory.metadata.id) else {
            continue;
        };
        for fact in &extraction.facts {
            if fact.source_memory_id != memory.metadata.id {
                return Err(eyre!(
                    "semantic fact {} belongs to memory {}, not {}",
                    fact.id,
                    fact.source_memory_id,
                    memory.metadata.id
                ));
            }
            if !relation_is_exclusive(&fact.relation)
                || !semantic::semantic_fact_is_valid_at(fact, at)?
            {
                continue;
            }
            let key = SemanticKey {
                scope: memory.metadata.scope.to_string(),
                subject_id: fact.subject_id.clone(),
                relation: fact.relation.clone(),
            };
            groups.entry(key).or_default().insert(
                (memory.metadata.id.clone(), fact.id.clone()),
                FactCandidate {
                    memory,
                    extraction,
                    fact,
                },
            );
        }
    }

    let mut conflicts = Vec::new();
    for (key, candidates) in groups {
        if candidates.len() < 2 {
            continue;
        }
        let first = candidates.values().next().unwrap().fact;
        if candidates
            .values()
            .skip(1)
            .all(|candidate| semantic::semantic_objects_match(first, candidate.fact))
        {
            continue;
        }

        let scope = candidates.values().next().unwrap().memory.metadata.scope;
        let subject = candidates
            .values()
            .filter_map(|candidate| {
                candidate
                    .extraction
                    .entities
                    .iter()
                    .find(|entity| entity.id == key.subject_id)
                    .map(|entity| entity.canonical_name.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .next()
            .unwrap_or_else(|| key.subject_id.clone());
        let members = candidates
            .into_values()
            .map(|candidate| {
                memory_member(
                    candidate.memory,
                    Some(conflict_fact(candidate.fact)),
                    candidate.extraction.episode.excerpt.clone(),
                )
            })
            .collect();
        conflicts.push(Conflict {
            id: semantic::stable_id(
                "conflict",
                &format!(
                    "exclusive-current-conflict\n{}\n{}\n{}",
                    key.scope, key.subject_id, key.relation
                ),
            ),
            kind: ConflictKind::ExclusiveCurrent,
            scope,
            subject_id: Some(key.subject_id),
            subject: Some(subject),
            relation: Some(key.relation),
            members,
        });
    }
    Ok(conflicts)
}

fn relation_is_exclusive(relation: &str) -> bool {
    semantic::relation_is_exclusive_current(relation)
}

fn memory_member(memory: &Memory, fact: Option<ConflictFact>, excerpt: String) -> ConflictMember {
    ConflictMember {
        memory_id: memory.metadata.id.clone(),
        memory_path: memory
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("<memory:{}>", memory.metadata.id)),
        memory_title: memory.title(),
        excerpt,
        fact,
    }
}

fn conflict_fact(fact: &SemanticFact) -> ConflictFact {
    ConflictFact {
        id: fact.id.clone(),
        object_id: fact.object_id.clone(),
        object_value: fact.object_value.clone(),
        valid_from: fact.valid_from.clone(),
        valid_to: fact.valid_to.clone(),
        learned_at: fact.learned_at.clone(),
        expired_at: fact.expired_at.clone(),
        confidence: fact.confidence,
        line_number: fact.line_number,
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticKey {
    scope: String,
    subject_id: String,
    relation: String,
}

#[derive(Clone, Copy)]
struct FactCandidate<'a> {
    memory: &'a Memory,
    extraction: &'a SemanticExtraction,
    fact: &'a SemanticFact,
}

pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
CREATE TABLE semantic_conflicts (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  scope TEXT NOT NULL,
  subject_id TEXT,
  subject TEXT,
  relation TEXT,
  member_count INTEGER NOT NULL
);

CREATE TABLE semantic_conflict_members (
  conflict_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  memory_id TEXT NOT NULL,
  memory_path TEXT NOT NULL,
  memory_title TEXT NOT NULL,
  excerpt TEXT NOT NULL,
  fact_id TEXT,
  object_id TEXT,
  object_value TEXT,
  valid_from TEXT,
  valid_to TEXT,
  learned_at TEXT,
  expired_at TEXT,
  confidence REAL,
  line_number INTEGER,
  PRIMARY KEY (conflict_id, ordinal),
  FOREIGN KEY (conflict_id) REFERENCES semantic_conflicts(id),
  FOREIGN KEY (memory_id) REFERENCES memories(id),
  FOREIGN KEY (fact_id) REFERENCES semantic_facts(id)
);

CREATE INDEX semantic_conflicts_kind_scope_idx
  ON semantic_conflicts(kind, scope);

CREATE INDEX semantic_conflict_members_memory_idx
  ON semantic_conflict_members(memory_id);
"#,
    )?;
    Ok(())
}

pub fn insert_conflicts(conn: &Connection, conflicts: &[Conflict]) -> Result<()> {
    for conflict in conflicts {
        conn.execute(
            "INSERT INTO semantic_conflicts (
                id, kind, scope, subject_id, subject, relation, member_count
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &conflict.id,
                conflict.kind.label(),
                conflict.scope.to_string(),
                conflict.subject_id.as_deref(),
                conflict.subject.as_deref(),
                conflict.relation.as_deref(),
                conflict.members.len() as i64,
            ],
        )?;
        for (ordinal, member) in conflict.members.iter().enumerate() {
            let fact = member.fact.as_ref();
            conn.execute(
                "INSERT INTO semantic_conflict_members (
                    conflict_id, ordinal, memory_id, memory_path, memory_title, excerpt,
                    fact_id, object_id, object_value, valid_from, valid_to, learned_at,
                    expired_at, confidence, line_number
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    &conflict.id,
                    ordinal as i64,
                    &member.memory_id,
                    &member.memory_path,
                    &member.memory_title,
                    &member.excerpt,
                    fact.map(|fact| fact.id.as_str()),
                    fact.and_then(|fact| fact.object_id.as_deref()),
                    fact.map(|fact| fact.object_value.as_str()),
                    fact.and_then(|fact| fact.valid_from.as_deref()),
                    fact.and_then(|fact| fact.valid_to.as_deref()),
                    fact.map(|fact| fact.learned_at.as_str()),
                    fact.and_then(|fact| fact.expired_at.as_deref()),
                    fact.and_then(|fact| fact.confidence),
                    fact.map(|fact| fact.line_number as i64),
                ],
            )?;
        }
    }
    Ok(())
}

pub fn count(conn: &Connection) -> Result<usize> {
    let count = conn.query_row("SELECT COUNT(*) FROM semantic_conflicts", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(count as usize)
}

pub fn index_has_schema(conn: &Connection) -> Result<bool> {
    let conflicts = table_exists(conn, "semantic_conflicts")?;
    let members = table_exists(conn, "semantic_conflict_members")?;
    Ok(conflicts && members)
}

pub fn open_index(index_path: &Path) -> Result<Connection> {
    if !index_path.is_file() {
        return Err(eyre!(
            "semantic conflict index missing at {}; run `rem rebuild`",
            index_path.display()
        ));
    }
    let conn = Connection::open(index_path)
        .wrap_err_with(|| format!("failed to open conflict index {}", index_path.display()))?;
    if !index_has_schema(&conn)? {
        return Err(eyre!(
            "semantic conflict cache schema missing; run `rem rebuild`"
        ));
    }
    Ok(conn)
}

pub fn query(
    conn: &Connection,
    kind: Option<ConflictKind>,
    scope: Option<MemoryScope>,
) -> Result<Vec<Conflict>> {
    if !index_has_schema(conn)? {
        return Err(eyre!(
            "semantic conflict cache schema missing; run `rem rebuild`"
        ));
    }

    let mut statement = conn.prepare(
        "SELECT id, kind, scope, subject_id, subject, relation, member_count
         FROM semantic_conflicts
         ORDER BY kind, scope, subject_id, relation, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;

    let mut conflicts = Vec::new();
    for row in rows {
        let (id, kind_label, scope_label, subject_id, subject, relation, member_count) = row?;
        let conflict_kind = ConflictKind::from_label(&kind_label)
            .wrap_err_with(|| format!("invalid conflict {id}"))?;
        let conflict_scope = MemoryScope::from_str(&scope_label)
            .wrap_err_with(|| format!("invalid scope for conflict {id}"))?;
        if kind.is_some_and(|kind| kind != conflict_kind)
            || scope.is_some_and(|scope| scope != conflict_scope)
        {
            continue;
        }
        if member_count < 2 {
            return Err(eyre!(
                "semantic conflict cache is invalid: conflict {id} has member_count={member_count}; run `rem rebuild`"
            ));
        }

        let members = load_members(conn, &id)?;
        if members.len() != usize::try_from(member_count).unwrap_or(usize::MAX) {
            return Err(eyre!(
                "semantic conflict cache is invalid: conflict {id} declares {member_count} members but stores {}; run `rem rebuild`",
                members.len()
            ));
        }
        match conflict_kind {
            ConflictKind::ExactActiveDuplicate => {
                if subject_id.is_some() || subject.is_some() || relation.is_some() {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: duplicate conflict {id} has semantic key fields; run `rem rebuild`"
                    ));
                }
                if members.iter().any(|member| member.fact.is_some()) {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: duplicate conflict {id} has fact evidence; run `rem rebuild`"
                    ));
                }
            }
            ConflictKind::ExclusiveCurrent => {
                if subject_id.is_none() || subject.is_none() || relation.is_none() {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: exclusive conflict {id} is missing its semantic key; run `rem rebuild`"
                    ));
                }
                if members.iter().any(|member| member.fact.is_none()) {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: exclusive conflict {id} is missing fact evidence; run `rem rebuild`"
                    ));
                }
            }
        }
        conflicts.push(Conflict {
            id,
            kind: conflict_kind,
            scope: conflict_scope,
            subject_id,
            subject,
            relation,
            members,
        });
    }
    Ok(conflicts)
}

pub fn find(conn: &Connection, id_or_prefix: &str) -> Result<Conflict> {
    if id_or_prefix.trim().is_empty() {
        return Err(eyre!("conflict id or prefix cannot be empty"));
    }
    let conflicts = query(conn, None, None)?;
    if let Some(conflict) = conflicts
        .iter()
        .find(|conflict| conflict.id == id_or_prefix)
    {
        return Ok(conflict.clone());
    }
    let mut matches = conflicts
        .into_iter()
        .filter(|conflict| conflict.id.starts_with(id_or_prefix))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(eyre!("no semantic conflict found for {id_or_prefix:?}")),
        1 => Ok(matches.remove(0)),
        count => Err(eyre!(
            "semantic conflict prefix {id_or_prefix:?} is ambiguous across {count} conflicts"
        )),
    }
}

pub fn find_exact(conn: &Connection, id: &str) -> Result<Option<Conflict>> {
    Ok(query(conn, None, None)?
        .into_iter()
        .find(|conflict| conflict.id == id))
}

pub fn apply_resolution(
    workspace: &Workspace,
    conflict: &Conflict,
    keep: &str,
    requested_expiration: Option<i64>,
) -> Result<Resolution> {
    match conflict.kind {
        ConflictKind::ExactActiveDuplicate => {
            if requested_expiration.is_some() {
                return Err(eyre!("--at only applies to exclusive-current conflicts"));
            }
            let memory_ids = conflict
                .members
                .iter()
                .map(|member| member.memory_id.as_str())
                .collect::<Vec<_>>();
            let kept_id = select_member_id(&memory_ids, keep, "memory")?;
            let mut archived_memory_ids = Vec::new();
            for member in &conflict.members {
                if member.memory_id == kept_id {
                    continue;
                }
                let archived = memory::delete_memory(workspace, &member.memory_id, false)?;
                archived_memory_ids.push(archived.metadata.id);
            }
            Ok(Resolution {
                conflict_id: conflict.id.clone(),
                kept_id,
                archived_memory_ids,
                expired_fact_ids: Vec::new(),
                expired_at: None,
            })
        }
        ConflictKind::ExclusiveCurrent => {
            let expiration = requested_expiration.unwrap_or_else(semantic::now_unix_seconds);
            let now = semantic::now_unix_seconds();
            if expiration > now {
                return Err(eyre!(
                    "conflict expiration time {expiration} is in the future; choose a time at or before {now}"
                ));
            }
            let fact_ids = conflict
                .members
                .iter()
                .filter_map(|member| member.fact.as_ref().map(|fact| fact.id.as_str()))
                .collect::<Vec<_>>();
            let kept_id = select_member_id(&fact_ids, keep, "fact")?;
            let kept_fact = conflict
                .members
                .iter()
                .filter_map(|member| member.fact.as_ref())
                .find(|fact| fact.id == kept_id)
                .ok_or_else(|| eyre!("kept fact {kept_id} disappeared from conflict evidence"))?;

            let mut by_memory = BTreeMap::<String, BTreeSet<String>>::new();
            let mut expired_fact_ids = Vec::new();
            for member in &conflict.members {
                let fact = member.fact.as_ref().ok_or_else(|| {
                    eyre!("exclusive conflict {} has non-fact evidence", conflict.id)
                })?;
                if semantic::semantic_object_parts_match(
                    kept_fact.object_id.as_deref(),
                    &kept_fact.object_value,
                    fact.object_id.as_deref(),
                    &fact.object_value,
                ) {
                    continue;
                }
                by_memory
                    .entry(member.memory_id.clone())
                    .or_default()
                    .insert(fact.id.clone());
                expired_fact_ids.push(fact.id.clone());
            }
            if expired_fact_ids.is_empty() {
                return Err(eyre!(
                    "conflict {} has no competing facts to expire",
                    conflict.id
                ));
            }

            for (memory_id, fact_ids) in by_memory {
                let source = memory::find_memory(workspace, &memory_id, false)?;
                memory::ensure_active_memory(&source, "used for conflict resolution")?;
                let body = semantic::expire_facts(&source, &fact_ids, expiration)?;
                memory::update_memory(workspace, &memory_id, body, false)?;
            }
            expired_fact_ids.sort();
            Ok(Resolution {
                conflict_id: conflict.id.clone(),
                kept_id,
                archived_memory_ids: Vec::new(),
                expired_fact_ids,
                expired_at: Some(expiration),
            })
        }
    }
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn load_members(conn: &Connection, conflict_id: &str) -> Result<Vec<ConflictMember>> {
    let mut statement = conn.prepare(
        "SELECT ordinal, memory_id, memory_path, memory_title, excerpt,
                fact_id, object_id, object_value, valid_from, valid_to, learned_at,
                expired_at, confidence, line_number
         FROM semantic_conflict_members
         WHERE conflict_id = ?1
         ORDER BY ordinal",
    )?;
    let rows = statement.query_map([conflict_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<f64>>(12)?,
            row.get::<_, Option<i64>>(13)?,
        ))
    })?;

    let mut members = Vec::new();
    for row in rows {
        let (
            ordinal,
            memory_id,
            memory_path,
            memory_title,
            excerpt,
            fact_id,
            object_id,
            object_value,
            valid_from,
            valid_to,
            learned_at,
            expired_at,
            confidence,
            line_number,
        ) = row?;
        let expected_ordinal = i64::try_from(members.len()).unwrap_or(i64::MAX);
        if ordinal != expected_ordinal {
            return Err(eyre!(
                "semantic conflict cache is invalid: conflict {conflict_id} has ordinal {ordinal}, expected {expected_ordinal}; run `rem rebuild`"
            ));
        }
        if confidence.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(eyre!(
                "semantic conflict cache is invalid: conflict {conflict_id} has invalid confidence; run `rem rebuild`"
            ));
        }
        let fact = match fact_id {
            Some(id) => {
                let line_number = usize::try_from(line_number.ok_or_else(|| {
                    eyre!(
                        "semantic conflict cache is invalid: fact evidence in {conflict_id} lacks line_number; run `rem rebuild`"
                    )
                })?)
                .map_err(|_| {
                    eyre!(
                        "semantic conflict cache is invalid: fact evidence in {conflict_id} has a negative line_number; run `rem rebuild`"
                    )
                })?;
                if line_number == 0 {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: fact evidence in {conflict_id} has line_number 0; run `rem rebuild`"
                    ));
                }
                Some(ConflictFact {
                    id,
                    object_id,
                    object_value: object_value.ok_or_else(|| {
                        eyre!(
                            "semantic conflict cache is invalid: fact evidence in {conflict_id} lacks object_value; run `rem rebuild`"
                        )
                    })?,
                    valid_from,
                    valid_to,
                    learned_at: learned_at.ok_or_else(|| {
                        eyre!(
                            "semantic conflict cache is invalid: fact evidence in {conflict_id} lacks learned_at; run `rem rebuild`"
                        )
                    })?,
                    expired_at,
                    confidence,
                    line_number,
                })
            }
            None => {
                if object_id.is_some()
                    || object_value.is_some()
                    || valid_from.is_some()
                    || valid_to.is_some()
                    || learned_at.is_some()
                    || expired_at.is_some()
                    || confidence.is_some()
                    || line_number.is_some()
                {
                    return Err(eyre!(
                        "semantic conflict cache is invalid: non-fact evidence in {conflict_id} has fact fields; run `rem rebuild`"
                    ));
                }
                None
            }
        };
        members.push(ConflictMember {
            memory_id,
            memory_path,
            memory_title,
            excerpt,
            fact,
        });
    }
    Ok(members)
}

fn select_member_id(candidates: &[&str], selector: &str, label: &str) -> Result<String> {
    if selector.trim().is_empty() {
        return Err(eyre!("kept {label} id or prefix cannot be empty"));
    }
    if let Some(candidate) = candidates.iter().find(|candidate| **candidate == selector) {
        return Ok((*candidate).to_string());
    }
    let matches = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.starts_with(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(eyre!(
            "{selector:?} does not identify a {label} in this conflict"
        )),
        [candidate] => Ok((*candidate).to_string()),
        _ => Err(eyre!(
            "{label} prefix {selector:?} is ambiguous in this conflict"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use rusqlite::Connection;

    use super::{
        ConflictKind, create_schema, detect_at, find, insert_conflicts, query, select_member_id,
    };
    use crate::{
        memory::{Memory, MemoryKind, MemoryMetadata, MemoryScope, MemoryStatus, MemoryType},
        semantic::{self, SemanticExtraction},
    };

    fn memory(id: &str, scope: MemoryScope, status: MemoryStatus, body: &str) -> Memory {
        Memory {
            metadata: MemoryMetadata {
                id: id.to_string(),
                memory_type: MemoryType::Long,
                scope,
                kind: MemoryKind::Fact,
                status,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                source_id: None,
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: body.to_string(),
            path: Some(PathBuf::from(format!("/vault/memories/long/{id}.md"))),
        }
    }

    fn extractions(memories: &[Memory]) -> BTreeMap<String, SemanticExtraction> {
        memories
            .iter()
            .map(|memory| {
                (
                    memory.metadata.id.clone(),
                    semantic::extract(memory).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn exact_duplicates_are_scope_status_and_order_aware() {
        let first = memory(
            "a",
            MemoryScope::Project,
            MemoryStatus::Active,
            "# Same\r\nbody\r\n",
        );
        let second = memory(
            "b",
            MemoryScope::Project,
            MemoryStatus::Active,
            "# Same\nbody",
        );
        let other_scope = memory("c", MemoryScope::User, MemoryStatus::Active, "# Same\nbody");
        let archived = memory(
            "d",
            MemoryScope::Project,
            MemoryStatus::Archived,
            "# Same\nbody",
        );
        let memories = vec![
            first.clone(),
            archived,
            second.clone(),
            other_scope,
            first.clone(),
        ];
        let conflicts = detect_at(&memories, &BTreeMap::new(), 100).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, ConflictKind::ExactActiveDuplicate);
        assert_eq!(conflicts[0].scope, MemoryScope::Project);
        assert_eq!(
            conflicts[0]
                .members
                .iter()
                .map(|member| member.memory_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(conflicts[0].id.starts_with("conflict-"));

        let reversed = memories.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(
            conflicts,
            detect_at(&reversed, &BTreeMap::new(), 100).unwrap()
        );
    }

    #[test]
    fn exclusive_current_conflicts_are_grouped_with_stable_evidence() {
        let first = memory(
            "a",
            MemoryScope::User,
            MemoryStatus::Active,
            "# Editor A\n@fact User | PREFERS | Vim | valid_from=1",
        );
        let second = memory(
            "b",
            MemoryScope::User,
            MemoryStatus::Active,
            "# Editor B\n@fact user | prefers | Helix | valid_from=2",
        );
        let memories = vec![second.clone(), first.clone()];
        let conflicts = detect_at(&memories, &extractions(&memories), 100).unwrap();

        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.kind, ConflictKind::ExclusiveCurrent);
        assert_eq!(conflict.scope, MemoryScope::User);
        assert_eq!(conflict.subject.as_deref(), Some("User"));
        assert_eq!(conflict.relation.as_deref(), Some("PREFERS"));
        assert_eq!(
            conflict
                .members
                .iter()
                .map(|member| member.fact.as_ref().unwrap().object_value.as_str())
                .collect::<Vec<_>>(),
            ["Vim", "Helix"]
        );
        assert!(
            conflict
                .members
                .iter()
                .all(|member| !member.excerpt.is_empty())
        );

        let third = memory(
            "c",
            MemoryScope::User,
            MemoryStatus::Active,
            "# Editor C\n@fact User | PREFERS | Emacs | valid_from=3",
        );
        let expanded = vec![first, second, third];
        let expanded_conflicts = detect_at(&expanded, &extractions(&expanded), 100).unwrap();
        assert_eq!(expanded_conflicts.len(), 1);
        assert_eq!(expanded_conflicts[0].id, conflict.id);
        assert_eq!(expanded_conflicts[0].members.len(), 3);
    }

    #[test]
    fn same_object_multi_current_scope_and_inactive_memories_do_not_conflict() {
        let memories = vec![
            memory(
                "same-a",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Same A\n@fact User | PREFERS | Vim | valid_from=1",
            ),
            memory(
                "same-b",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Same B\n@fact user | prefers | vim | valid_from=2",
            ),
            memory(
                "other-scope",
                MemoryScope::Project,
                MemoryStatus::Active,
                "# Other scope\n@fact User | PREFERS | Helix | valid_from=1",
            ),
            memory(
                "tool-a",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Tool A\n@fact User | USES | Git | valid_from=1",
            ),
            memory(
                "tool-b",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Tool B\n@fact User | USES | SQLite | valid_from=1",
            ),
            memory(
                "superseded",
                MemoryScope::User,
                MemoryStatus::Superseded,
                "# Old\n@fact User | PREFERS | Emacs | valid_from=1",
            ),
        ];

        assert!(
            detect_at(&memories, &extractions(&memories), 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn value_objects_normalize_but_mixed_entity_value_evidence_stays_conservative() {
        let values = vec![
            memory(
                "value-a",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Value A\n@fact User | PREFERS | Vim | object_kind=value | valid_from=1",
            ),
            memory(
                "value-b",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Value B\n@fact user | prefers |  VIM | object_kind=value | valid_from=2",
            ),
        ];
        assert!(
            detect_at(&values, &extractions(&values), 100)
                .unwrap()
                .is_empty()
        );

        let mixed = vec![
            values[0].clone(),
            memory(
                "entity",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Entity\n@fact User | PREFERS | Vim | object_kind=entity | valid_from=3",
            ),
        ];
        let conflicts = detect_at(&mixed, &extractions(&mixed), 100).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, ConflictKind::ExclusiveCurrent);
        assert_eq!(conflicts[0].members.len(), 2);
    }

    #[test]
    fn temporal_boundaries_exclude_closed_expired_and_future_facts() {
        let memories = vec![
            memory(
                "closed",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Closed\n@fact User | WORKS_AT | OldCo | valid_from=1 | valid_to=100",
            ),
            memory(
                "expired",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Expired\n@fact User | WORKS_AT | ExpiredCo | valid_from=1 | expired_at=100",
            ),
            memory(
                "future",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Future\n@fact User | WORKS_AT | FutureCo | valid_from=101",
            ),
            memory(
                "current",
                MemoryScope::User,
                MemoryStatus::Active,
                "# Current\n@fact User | WORKS_AT | CurrentCo | valid_from=100",
            ),
        ];

        assert!(
            detect_at(&memories, &extractions(&memories), 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn usually_exclusive_facts_can_conflict_inside_one_memory() {
        let memories = vec![memory(
            "employment",
            MemoryScope::User,
            MemoryStatus::Active,
            "# Employment\n@fact User | WORKS_AT | OldCo | valid_from=1\n@fact User | WORKS_AT | NewCo | valid_from=2",
        )];
        let conflicts = detect_at(&memories, &extractions(&memories), 100).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, ConflictKind::ExclusiveCurrent);
        assert_eq!(conflicts[0].relation.as_deref(), Some("WORKS_AT"));
        assert_eq!(conflicts[0].members.len(), 2);
        assert!(
            conflicts[0]
                .members
                .iter()
                .all(|member| member.memory_id == "employment")
        );
        assert_ne!(
            conflicts[0].members[0].fact.as_ref().unwrap().id,
            conflicts[0].members[1].fact.as_ref().unwrap().id
        );
    }

    #[test]
    fn mismatched_extraction_provenance_is_rejected() {
        let memories = vec![memory(
            "source",
            MemoryScope::User,
            MemoryStatus::Active,
            "# Source\n@fact User | PREFERS | Vim | valid_from=1",
        )];
        let mut extracted = extractions(&memories);
        extracted.get_mut("source").unwrap().facts[0].source_memory_id = "other".to_string();

        let error = detect_at(&memories, &extracted, 100)
            .unwrap_err()
            .to_string();
        assert!(error.contains("belongs to memory other, not source"));
    }

    #[test]
    fn derived_conflicts_persist_with_ordered_members() {
        let memories = vec![
            memory(
                "a",
                MemoryScope::User,
                MemoryStatus::Active,
                "# A\n@fact User | PREFERS | Vim | valid_from=1",
            ),
            memory(
                "b",
                MemoryScope::User,
                MemoryStatus::Active,
                "# B\n@fact User | PREFERS | Helix | valid_from=1",
            ),
        ];
        let conflicts = detect_at(&memories, &extractions(&memories), 100).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY);
             CREATE TABLE semantic_facts (id TEXT PRIMARY KEY);",
        )
        .unwrap();
        create_schema(&conn).unwrap();
        let foreign_keys = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        for conflict in &conflicts {
            for member in &conflict.members {
                conn.execute(
                    "INSERT OR IGNORE INTO memories (id) VALUES (?1)",
                    [&member.memory_id],
                )
                .unwrap();
                if let Some(fact) = &member.fact {
                    conn.execute(
                        "INSERT OR IGNORE INTO semantic_facts (id) VALUES (?1)",
                        [&fact.id],
                    )
                    .unwrap();
                }
            }
        }
        insert_conflicts(&conn, &conflicts).unwrap();

        assert_eq!(query(&conn, None, None).unwrap(), conflicts);
        assert_eq!(
            query(&conn, Some(ConflictKind::ExactActiveDuplicate), None).unwrap(),
            Vec::new()
        );
        assert_eq!(find(&conn, "conflict-").unwrap(), conflicts[0]);

        let count = super::count(&conn).unwrap();
        let member_count = conn
            .query_row(
                "SELECT member_count FROM semantic_conflicts WHERE id = ?1",
                [&conflicts[0].id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let members = conn
            .prepare(
                "SELECT memory_id FROM semantic_conflict_members ORDER BY conflict_id, ordinal",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        assert_eq!(count, 1);
        assert_eq!(member_count, 2);
        assert_eq!(members, ["a", "b"]);

        conn.execute(
            "UPDATE semantic_conflicts SET member_count = 3 WHERE id = ?1",
            [&conflicts[0].id],
        )
        .unwrap();
        let error = query(&conn, None, None).unwrap_err().to_string();
        assert!(error.contains("declares 3 members but stores 2"));
    }

    #[test]
    fn derived_conflict_members_require_indexed_sources() {
        let memories = vec![
            memory(
                "a",
                MemoryScope::User,
                MemoryStatus::Active,
                "# A\n@fact User | PREFERS | Vim | valid_from=1",
            ),
            memory(
                "b",
                MemoryScope::User,
                MemoryStatus::Active,
                "# B\n@fact User | PREFERS | Helix | valid_from=1",
            ),
        ];
        let conflicts = detect_at(&memories, &extractions(&memories), 100).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY);
             CREATE TABLE semantic_facts (id TEXT PRIMARY KEY);",
        )
        .unwrap();
        create_schema(&conn).unwrap();

        let error = insert_conflicts(&conn, &conflicts).unwrap_err().to_string();
        assert!(error.contains("FOREIGN KEY constraint failed"));
    }

    #[test]
    fn member_selection_prefers_exact_ids_and_rejects_ambiguous_prefixes() {
        let candidates = ["memory-a", "memory-ab", "other"];
        assert_eq!(
            select_member_id(&candidates, "memory-a", "memory").unwrap(),
            "memory-a"
        );
        assert!(
            select_member_id(&candidates, "memory-", "memory")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(
            select_member_id(&candidates, "missing", "memory")
                .unwrap_err()
                .to_string()
                .contains("does not identify")
        );
    }
}
