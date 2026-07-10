use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{Result, eyre};
use rusqlite::{Connection, OptionalExtension, params};

use crate::memory;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

#[derive(Clone, Debug)]
pub struct SemanticExtraction {
    pub entities: Vec<SemanticEntity>,
    pub episode: SemanticEpisode,
    pub facts: Vec<SemanticFact>,
}

#[derive(Clone, Debug)]
pub struct SemanticEntity {
    pub id: String,
    pub canonical_name: String,
    pub kind: String,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct SemanticEpisode {
    pub id: String,
    pub source_memory_id: String,
    pub kind: String,
    pub created_at: String,
    pub text_hash: String,
    pub excerpt: String,
}

#[derive(Clone, Debug)]
pub struct SemanticFact {
    pub id: String,
    pub subject_id: String,
    pub relation: String,
    pub object_id: Option<String>,
    pub object_value: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub learned_at: String,
    pub expired_at: Option<String>,
    pub time_kind: TimeKind,
    pub source_memory_id: String,
    pub confidence: Option<f64>,
    pub episode_id: String,
    pub line_number: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub object_kind: &'static str,
    pub cardinality: &'static str,
    pub invalidation_policy: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Debug)]
pub struct FactQuery {
    pub entity: Option<String>,
    pub relation: Option<String>,
    pub at: Option<String>,
    pub include_expired: bool,
}

#[derive(Clone, Debug)]
pub struct FactRow {
    pub id: String,
    pub subject: String,
    pub relation: String,
    pub object: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub expired_at: Option<String>,
    pub learned_at: String,
    pub source_memory_id: String,
    pub source_path: String,
    pub episode_id: String,
    pub excerpt: String,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug)]
struct FactCandidate {
    row: FactRow,
    time_kind: TimeKind,
    sort_time: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeKind {
    Unix,
    IsoDate,
}

impl fmt::Display for TimeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unix => f.write_str("unix"),
            Self::IsoDate => f.write_str("iso-date"),
        }
    }
}

impl TimeKind {
    fn from_storage(value: &str) -> Result<Self> {
        match value {
            "unix" => Ok(Self::Unix),
            "iso-date" => Ok(Self::IsoDate),
            other => Err(eyre!("unknown semantic time kind {other:?}")),
        }
    }
}

pub fn relation_specs() -> &'static [RelationSpec] {
    &[
        RelationSpec {
            name: "PREFERS",
            label: "prefers",
            object_kind: "preference",
            cardinality: "exclusive-current",
            invalidation_policy: "review-close-previous",
            description: "A current or historical preference for an entity or value.",
        },
        RelationSpec {
            name: "DISLIKES",
            label: "dislikes",
            object_kind: "preference",
            cardinality: "multi-current",
            invalidation_policy: "review",
            description: "A current or historical negative preference.",
        },
        RelationSpec {
            name: "USES",
            label: "uses",
            object_kind: "tool",
            cardinality: "multi-current",
            invalidation_policy: "none",
            description: "A tool, product, service, or system being used.",
        },
        RelationSpec {
            name: "WORKS_AT",
            label: "works at",
            object_kind: "organization",
            cardinality: "usually-exclusive-current",
            invalidation_policy: "review-close-previous",
            description: "An employment or organizational affiliation.",
        },
        RelationSpec {
            name: "HAS_PROJECT",
            label: "has project",
            object_kind: "project",
            cardinality: "multi-current",
            invalidation_policy: "none",
            description: "A project associated with the subject.",
        },
        RelationSpec {
            name: "PART_OF",
            label: "part of",
            object_kind: "entity",
            cardinality: "multi-current",
            invalidation_policy: "none",
            description: "A containment or membership relationship.",
        },
        RelationSpec {
            name: "SUPERSEDES",
            label: "supersedes",
            object_kind: "entity",
            cardinality: "multi-current",
            invalidation_policy: "none",
            description: "A version or fact explicitly supersedes another one.",
        },
        RelationSpec {
            name: "MENTIONS",
            label: "mentions",
            object_kind: "entity",
            cardinality: "multi-current",
            invalidation_policy: "none",
            description: "A weak source-grounded mention without invalidation behavior.",
        },
    ]
}

pub fn normalize_relation(value: &str) -> Result<&'static str> {
    let normalized = value.trim().replace([' ', '-'], "_").to_ascii_uppercase();
    relation_specs()
        .iter()
        .find(|spec| spec.name == normalized)
        .map(|spec| spec.name)
        .ok_or_else(|| {
            let allowed = relation_specs()
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
                .join(", ");
            eyre!("unsupported semantic relation {value:?}; expected one of: {allowed}")
        })
}

pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE semantic_entities (
  id TEXT PRIMARY KEY,
  canonical_name TEXT NOT NULL,
  kind TEXT NOT NULL,
  summary TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE semantic_episodes (
  id TEXT PRIMARY KEY,
  source_memory_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  excerpt TEXT NOT NULL,
  source_path TEXT NOT NULL
);

CREATE TABLE semantic_facts (
  id TEXT PRIMARY KEY,
  subject_id TEXT NOT NULL,
  relation TEXT NOT NULL,
  object_id TEXT,
  object_value TEXT NOT NULL,
  valid_from TEXT,
  valid_to TEXT,
  learned_at TEXT NOT NULL,
  expired_at TEXT,
  time_kind TEXT NOT NULL,
  source_memory_id TEXT NOT NULL,
  confidence REAL,
  line_number INTEGER NOT NULL
);

CREATE TABLE semantic_fact_sources (
  fact_id TEXT NOT NULL,
  episode_id TEXT NOT NULL,
  PRIMARY KEY (fact_id, episode_id)
);

CREATE TABLE semantic_ontology_relations (
  name TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  object_kind TEXT NOT NULL,
  cardinality TEXT NOT NULL,
  invalidation_policy TEXT NOT NULL,
  description TEXT NOT NULL
);

CREATE INDEX semantic_facts_subject_relation_idx
  ON semantic_facts(subject_id, relation);

CREATE INDEX semantic_facts_validity_idx
  ON semantic_facts(valid_from, valid_to);
"#,
    )?;

    for spec in relation_specs() {
        conn.execute(
            "INSERT INTO semantic_ontology_relations (
                name, label, object_kind, cardinality, invalidation_policy, description
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                spec.name,
                spec.label,
                spec.object_kind,
                spec.cardinality,
                spec.invalidation_policy,
                spec.description,
            ],
        )?;
    }

    Ok(())
}

pub fn extract(memory: &memory::Memory) -> Result<SemanticExtraction> {
    let episode = SemanticEpisode {
        id: format!("episode-{}", memory.metadata.id),
        source_memory_id: memory.metadata.id.clone(),
        kind: memory.metadata.kind.to_string(),
        created_at: memory.metadata.created_at.clone(),
        text_hash: stable_id("text", &memory.body),
        excerpt: excerpt(&memory.body),
    };

    let mut facts = Vec::new();
    let mut entities = BTreeMap::<String, SemanticEntity>::new();
    let mut active_fence = None;
    let mut in_html_comment = false;
    for (line_index, line) in memory.body.lines().enumerate() {
        if let Some(fence) = active_fence {
            if closes_fence(line, fence) {
                active_fence = None;
            }
            continue;
        }
        if in_html_comment {
            if line.contains("-->") {
                in_html_comment = false;
            }
            continue;
        }
        if let Some(fence) = opening_fence(line) {
            active_fence = Some(fence);
            continue;
        }
        if line.contains("<!--") {
            in_html_comment = !line.contains("-->");
            continue;
        }
        let Some(raw_fact) = line.strip_prefix("@fact ") else {
            continue;
        };
        let directive = parse_fact_directive(raw_fact)
            .map_err(|err| eyre!("semantic fact line {}: {err}", line_index + 1))?;
        let relation = normalize_relation(&directive.relation)?;
        let subject_id = entity_id(&directive.subject);
        let object_id = directive
            .object_is_entity
            .then(|| entity_id(&directive.object));
        let learned_at = directive
            .learned_at
            .unwrap_or_else(|| memory.metadata.updated_at.clone());
        let time_kind = validate_fact_times(
            directive.valid_from.as_deref(),
            directive.valid_to.as_deref(),
            directive.expired_at.as_deref(),
            &learned_at,
        )
        .map_err(|err| eyre!("semantic fact line {}: {err}", line_index + 1))?;
        let confidence = match directive.confidence {
            Some(confidence) => Some(confidence),
            None => parse_confidence(memory.metadata.confidence.as_deref())
                .map_err(|err| eyre!("semantic fact line {}: {err}", line_index + 1))?,
        };
        upsert_entity(
            &mut entities,
            &subject_id,
            &directive.subject,
            "entity",
            memory,
        );
        if let Some(object_id) = &object_id {
            let object_kind = relation_specs()
                .iter()
                .find(|spec| spec.name == relation)
                .map(|spec| spec.object_kind)
                .unwrap_or("entity");
            upsert_entity(
                &mut entities,
                object_id,
                &directive.object,
                object_kind,
                memory,
            );
        }

        facts.push(SemanticFact {
            id: fact_id(
                &memory.metadata.id,
                line_index + 1,
                &directive.subject,
                relation,
                &directive.object,
                directive.valid_from.as_deref(),
                directive.valid_to.as_deref(),
            ),
            subject_id,
            relation: relation.to_string(),
            object_id,
            object_value: directive.object,
            valid_from: directive.valid_from,
            valid_to: directive.valid_to,
            learned_at,
            expired_at: directive.expired_at,
            time_kind,
            source_memory_id: memory.metadata.id.clone(),
            confidence,
            episode_id: episode.id.clone(),
            line_number: line_index + 1,
        });
    }

    Ok(SemanticExtraction {
        entities: entities.into_values().collect(),
        episode,
        facts,
    })
}

pub fn insert_extraction(
    conn: &Connection,
    extraction: &SemanticExtraction,
    source_path: &str,
) -> Result<()> {
    for entity in &extraction.entities {
        conn.execute(
            "INSERT INTO semantic_entities (
                id, canonical_name, kind, summary, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               canonical_name = excluded.canonical_name,
               kind = excluded.kind,
               summary = excluded.summary,
               updated_at = excluded.updated_at",
            params![
                &entity.id,
                &entity.canonical_name,
                &entity.kind,
                &entity.summary,
                &entity.created_at,
                &entity.updated_at,
            ],
        )?;
    }

    conn.execute(
        "INSERT INTO semantic_episodes (
            id, source_memory_id, kind, created_at, text_hash, excerpt, source_path
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &extraction.episode.id,
            &extraction.episode.source_memory_id,
            &extraction.episode.kind,
            &extraction.episode.created_at,
            &extraction.episode.text_hash,
            &extraction.episode.excerpt,
            source_path,
        ],
    )?;

    for fact in &extraction.facts {
        conn.execute(
            "INSERT INTO semantic_facts (
                id, subject_id, relation, object_id, object_value,
                valid_from, valid_to, learned_at, expired_at, time_kind, source_memory_id,
                confidence, line_number
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &fact.id,
                &fact.subject_id,
                &fact.relation,
                fact.object_id.as_deref(),
                &fact.object_value,
                fact.valid_from.as_deref(),
                fact.valid_to.as_deref(),
                &fact.learned_at,
                fact.expired_at.as_deref(),
                fact.time_kind.to_string(),
                &fact.source_memory_id,
                fact.confidence,
                fact.line_number as i64,
            ],
        )?;
        conn.execute(
            "INSERT INTO semantic_fact_sources (fact_id, episode_id) VALUES (?1, ?2)",
            params![&fact.id, &fact.episode_id],
        )?;
    }

    Ok(())
}

pub fn query_facts(conn: &Connection, query: &FactQuery) -> Result<Vec<FactRow>> {
    let mut sql = String::from(
        "SELECT
            f.id,
            s.canonical_name,
            f.relation,
            COALESCE(o.canonical_name, f.object_value),
            f.valid_from,
            f.valid_to,
            f.learned_at,
            f.source_memory_id,
            m.path,
            e.id,
            e.excerpt,
            f.confidence,
            f.expired_at,
            f.time_kind
         FROM semantic_facts f
         JOIN semantic_entities s ON s.id = f.subject_id
         LEFT JOIN semantic_entities o ON o.id = f.object_id
         JOIN semantic_fact_sources fs ON fs.fact_id = f.id
         JOIN semantic_episodes e ON e.id = fs.episode_id
         JOIN memories m ON m.id = f.source_memory_id
         WHERE m.status = 'active'",
    );

    let mut values = Vec::<String>::new();
    if query.entity.is_some() {
        let entity_param = push_param(&mut values, query.entity.clone().unwrap());
        write!(
            sql,
            " AND (LOWER(s.canonical_name) = LOWER({entity_param})
               OR LOWER(COALESCE(o.canonical_name, f.object_value)) = LOWER({entity_param}))"
        )
        .unwrap();
    }
    if query.relation.is_some() {
        let relation = normalize_relation(query.relation.as_deref().unwrap())?.to_string();
        let relation_param = push_param(&mut values, relation);
        write!(sql, " AND f.relation = {relation_param}").unwrap();
    }
    sql.push_str(" ORDER BY f.id ASC");

    let at = match query.at.as_deref() {
        Some(value) => Some(temporal_unix_seconds(value)?),
        None if !query.include_expired => Some(now_unix_seconds()),
        None => None,
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok((
            FactRow {
                id: row.get(0)?,
                subject: row.get(1)?,
                relation: row.get(2)?,
                object: row.get(3)?,
                valid_from: row.get(4)?,
                valid_to: row.get(5)?,
                expired_at: row.get(12)?,
                learned_at: row.get(6)?,
                source_memory_id: row.get(7)?,
                source_path: row.get(8)?,
                episode_id: row.get(9)?,
                excerpt: row.get(10)?,
                confidence: row.get(11)?,
            },
            row.get::<_, String>(13)?,
        ))
    })?;

    let mut facts = Vec::new();
    for row in rows {
        let (fact, time_kind) = row?;
        let mut candidate = FactCandidate {
            row: fact,
            time_kind: TimeKind::from_storage(&time_kind)?,
            sort_time: 0,
        };
        if let Some(at) = at
            && !fact_is_valid_at(&candidate, at)?
        {
            continue;
        }
        candidate.sort_time = fact_sort_time(&candidate)?;
        facts.push(candidate);
    }
    facts.sort_by(|left, right| {
        right
            .sort_time
            .cmp(&left.sort_time)
            .then(left.row.id.cmp(&right.row.id))
    });
    Ok(facts.into_iter().map(|candidate| candidate.row).collect())
}

pub fn index_has_semantic_schema(conn: &Connection) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'semantic_facts'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn parse_fact_directive(raw: &str) -> Result<FactDirective> {
    let mut parts = raw.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 3 {
        return Err(eyre!(
            "expected `@fact subject | RELATION | object | key=value ...`"
        ));
    }

    let subject = parts.remove(0).trim().to_string();
    let relation = parts.remove(0).trim().to_string();
    let object = parts.remove(0).trim().to_string();
    if subject.is_empty() || relation.is_empty() || object.is_empty() {
        return Err(eyre!("subject, relation, and object are required"));
    }

    let mut options = BTreeMap::<String, String>::new();
    for option in parts {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| eyre!("invalid option {option:?}; expected key=value"))?;
        let key = key.trim().replace('-', "_").to_ascii_lowercase();
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if key.is_empty() || value.is_empty() {
            return Err(eyre!(
                "invalid option {option:?}; key and value are required"
            ));
        }
        if options.insert(key.clone(), value).is_some() {
            return Err(eyre!("duplicate semantic fact option {key:?}"));
        }
    }

    let object_is_entity = options
        .remove("object_kind")
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "entity" => Ok(true),
            "value" => Ok(false),
            _ => Err(eyre!("object_kind must be either entity or value")),
        })
        .transpose()?
        .unwrap_or(true);
    let confidence = options
        .remove("confidence")
        .map(|value| value.parse::<f64>())
        .transpose()
        .map_err(|_| eyre!("confidence must be a number"))?;
    let valid_from = options.remove("valid_from");
    let valid_to = options.remove("valid_to");
    let learned_at = options.remove("learned_at");
    let expired_at = options.remove("expired_at");
    if let Some(confidence) = confidence
        && !(0.0..=1.0).contains(&confidence)
    {
        return Err(eyre!("confidence must be between 0.0 and 1.0"));
    }
    if !options.is_empty() {
        let unknown = options.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(eyre!("unknown semantic fact option(s): {unknown}"));
    }

    Ok(FactDirective {
        subject,
        relation,
        object,
        valid_from,
        valid_to,
        learned_at,
        expired_at,
        confidence,
        object_is_entity,
    })
}

fn validate_fact_times(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    expired_at: Option<&str>,
    learned_at: &str,
) -> Result<TimeKind> {
    let learned_kind = parse_time_kind(learned_at)?;
    let validity_values = [valid_from, valid_to, expired_at]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let Some(first_value) = validity_values.first() else {
        return Ok(learned_kind);
    };
    let validity_kind = parse_time_kind(first_value)?;
    for value in validity_values.iter().skip(1) {
        let kind = parse_time_kind(value)?;
        if kind != validity_kind {
            return Err(eyre!(
                "valid_from, valid_to, and expired_at must use the same time format"
            ));
        }
    }
    if let (Some(from), Some(to)) = (valid_from, valid_to)
        && compare_time_values(from, to, validity_kind)? != std::cmp::Ordering::Less
    {
        return Err(eyre!("valid_to must be later than valid_from"));
    }
    if let (Some(from), Some(expired)) = (valid_from, expired_at)
        && compare_time_values(from, expired, validity_kind)? != std::cmp::Ordering::Less
    {
        return Err(eyre!("expired_at must be later than valid_from"));
    }
    Ok(validity_kind)
}

fn parse_time_kind(value: &str) -> Result<TimeKind> {
    if value.is_empty() {
        return Err(eyre!("time value cannot be empty"));
    }
    if is_unix_seconds_literal(value) {
        parse_unix_seconds(value)?;
        return Ok(TimeKind::Unix);
    }
    validate_iso_date(value)?;
    Ok(TimeKind::IsoDate)
}

fn is_unix_seconds_literal(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_unix_seconds(value: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| eyre!("time value {value:?} must be signed 64-bit unix seconds"))
}

fn validate_iso_date(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if !(bytes.len() == 10 || bytes.len() == 20)
        || !bytes[0..4].iter().all(u8::is_ascii_digit)
        || bytes[4] != b'-'
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || bytes[7] != b'-'
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return Err(eyre!(
            "time value {value:?} must be unix seconds, zero-padded YYYY-MM-DD, or YYYY-MM-DDTHH:MM:SSZ"
        ));
    }
    if bytes.len() == 20
        && (bytes[10] != b'T'
            || !bytes[11..13].iter().all(u8::is_ascii_digit)
            || bytes[13] != b':'
            || !bytes[14..16].iter().all(u8::is_ascii_digit)
            || bytes[16] != b':'
            || !bytes[17..19].iter().all(u8::is_ascii_digit)
            || bytes[19] != b'Z')
    {
        return Err(eyre!(
            "time value {value:?} must be unix seconds, zero-padded YYYY-MM-DD, or YYYY-MM-DDTHH:MM:SSZ"
        ));
    }

    let year = value[0..4].parse::<u32>()?;
    let month = value[5..7].parse::<u32>()?;
    let day = value[8..10].parse::<u32>()?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
        return Err(eyre!("invalid calendar date {value:?}"));
    }
    if bytes.len() == 20 {
        let hour = value[11..13].parse::<u32>()?;
        let minute = value[14..16].parse::<u32>()?;
        let second = value[17..19].parse::<u32>()?;
        if hour > 23 || minute > 59 || second > 59 {
            return Err(eyre!("invalid time of day {value:?}"));
        }
    }
    Ok(())
}

fn compare_time_values(left: &str, right: &str, kind: TimeKind) -> Result<std::cmp::Ordering> {
    match kind {
        TimeKind::Unix => Ok(parse_unix_seconds(left)?.cmp(&parse_unix_seconds(right)?)),
        TimeKind::IsoDate => Ok(iso_to_unix_seconds(left)?.cmp(&iso_to_unix_seconds(right)?)),
    }
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn push_param(values: &mut Vec<String>, value: String) -> String {
    values.push(value);
    format!("?{}", values.len())
}

#[derive(Clone, Copy, Debug)]
struct Fence {
    marker: u8,
    width: usize,
}

fn opening_fence(line: &str) -> Option<Fence> {
    let (marker, width, _) = fence_parts(line)?;
    (width >= 3).then_some(Fence { marker, width })
}

fn closes_fence(line: &str, fence: Fence) -> bool {
    let Some((marker, width, tail)) = fence_parts(line) else {
        return false;
    };
    marker == fence.marker && width >= fence.width && tail.trim().is_empty()
}

fn fence_parts(line: &str) -> Option<(u8, usize, &str)> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = *rest.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let width = rest.bytes().take_while(|byte| *byte == marker).count();
    Some((marker, width, &rest[width..]))
}

#[derive(Clone, Debug)]
struct FactDirective {
    subject: String,
    relation: String,
    object: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
    learned_at: Option<String>,
    expired_at: Option<String>,
    confidence: Option<f64>,
    object_is_entity: bool,
}

fn upsert_entity(
    entities: &mut BTreeMap<String, SemanticEntity>,
    id: &str,
    name: &str,
    kind: &str,
    memory: &memory::Memory,
) {
    entities
        .entry(id.to_string())
        .or_insert_with(|| SemanticEntity {
            id: id.to_string(),
            canonical_name: name.trim().to_string(),
            kind: kind.to_string(),
            summary: format!("Derived from memory {}", memory.metadata.id),
            created_at: memory.metadata.created_at.clone(),
            updated_at: memory.metadata.updated_at.clone(),
        });
}

fn entity_id(name: &str) -> String {
    let normalized = normalize_entity_name(name);
    format!(
        "ent-{}-{}",
        short_slug(&normalized),
        stable_hash(&normalized)
    )
}

fn fact_id(
    memory_id: &str,
    line_number: usize,
    subject: &str,
    relation: &str,
    object: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> String {
    stable_id(
        "fact",
        &format!(
            "{memory_id}\n{line_number}\n{}\n{relation}\n{}\n{}\n{}",
            normalize_entity_name(subject),
            normalize_entity_name(object),
            valid_from.unwrap_or(""),
            valid_to.unwrap_or("")
        ),
    )
}

fn normalize_entity_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn short_slug(value: &str) -> String {
    let slug = value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch)
            } else if ch.is_whitespace() || ch == '-' || ch == '_' {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(24)
        .collect::<String>();
    if slug.is_empty() {
        "entity".to_string()
    } else {
        slug
    }
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}-{}", stable_hash(value))
}

fn stable_hash(value: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn excerpt(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with("@fact "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn parse_confidence(value: Option<&str>) -> Result<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let confidence = value
        .parse::<f64>()
        .map_err(|_| eyre!("confidence must be a number"))?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err(eyre!("confidence must be between 0.0 and 1.0"));
    }
    Ok(Some(confidence))
}

fn now_unix_seconds() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn temporal_unix_seconds(value: &str) -> Result<i64> {
    match parse_time_kind(value)? {
        TimeKind::Unix => parse_unix_seconds(value),
        TimeKind::IsoDate => iso_to_unix_seconds(value),
    }
}

fn iso_to_unix_seconds(value: &str) -> Result<i64> {
    validate_iso_date(value)?;
    let year = value[0..4].parse::<i32>()?;
    let month = value[5..7].parse::<u32>()?;
    let day = value[8..10].parse::<u32>()?;
    let days = days_from_civil(year, month, day);
    let seconds_of_day = if value.len() == 20 {
        let hour = value[11..13].parse::<i64>()?;
        let minute = value[14..16].parse::<i64>()?;
        let second = value[17..19].parse::<i64>()?;
        hour * 3_600 + minute * 60 + second
    } else {
        0
    };
    days.checked_mul(86_400)
        .and_then(|seconds| seconds.checked_add(seconds_of_day))
        .ok_or_else(|| eyre!("time value {value:?} is outside supported unix seconds"))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn fact_is_valid_at(candidate: &FactCandidate, at: i64) -> Result<bool> {
    if let Some(value) = candidate.row.valid_from.as_deref()
        && temporal_value_to_unix_seconds(value, candidate.time_kind)? > at
    {
        return Ok(false);
    }
    if let Some(value) = candidate.row.valid_to.as_deref()
        && temporal_value_to_unix_seconds(value, candidate.time_kind)? <= at
    {
        return Ok(false);
    }
    if let Some(value) = candidate.row.expired_at.as_deref()
        && temporal_value_to_unix_seconds(value, candidate.time_kind)? <= at
    {
        return Ok(false);
    }
    Ok(true)
}

fn fact_sort_time(candidate: &FactCandidate) -> Result<i64> {
    match candidate.row.valid_from.as_deref() {
        Some(value) => temporal_value_to_unix_seconds(value, candidate.time_kind),
        None => temporal_unix_seconds(&candidate.row.learned_at),
    }
}

fn temporal_value_to_unix_seconds(value: &str, kind: TimeKind) -> Result<i64> {
    match kind {
        TimeKind::Unix => parse_unix_seconds(value),
        TimeKind::IsoDate => iso_to_unix_seconds(value),
    }
}

pub fn fact_counts(conn: &Connection) -> Result<(usize, usize, usize)> {
    let entities = count_table(conn, "semantic_entities")?;
    let episodes = count_table(conn, "semantic_episodes")?;
    let facts = count_table(conn, "semantic_facts")?;
    Ok((entities, episodes, facts))
}

fn count_table(conn: &Connection, table: &str) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count = conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
    Ok(count as usize)
}

pub fn validate_no_duplicate_fact_ids(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id FROM semantic_facts")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = row?;
        if !seen.insert(id.clone()) {
            return Err(eyre!("duplicate semantic fact id {id}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        Memory, MemoryKind, MemoryMetadata, MemoryScope, MemoryStatus, MemoryType,
    };

    #[test]
    fn relation_whitelist_normalizes_supported_relations() {
        assert_eq!(normalize_relation("prefers").unwrap(), "PREFERS");
        assert_eq!(normalize_relation("works-at").unwrap(), "WORKS_AT");
        assert!(normalize_relation("loves").is_err());
    }

    #[test]
    fn parses_temporal_fact_directive() {
        let directive = parse_fact_directive(
            "User | PREFERS | Puma | valid_from=2025-04-02 | valid_to=2025-05-01 | confidence=0.7",
        )
        .unwrap();

        assert_eq!(directive.subject, "User");
        assert_eq!(directive.relation, "PREFERS");
        assert_eq!(directive.object, "Puma");
        assert_eq!(directive.valid_from.as_deref(), Some("2025-04-02"));
        assert_eq!(directive.valid_to.as_deref(), Some("2025-05-01"));
        assert_eq!(directive.confidence, Some(0.7));
    }

    #[test]
    fn validates_temporal_ranges_and_time_formats() {
        assert!(parse_time_kind("2025-04-02").is_ok());
        assert!(parse_time_kind("2025-04-02T03:04:05Z").is_ok());
        assert!(parse_time_kind("1712016000").is_ok());
        assert_eq!(parse_time_kind("-1").unwrap(), TimeKind::Unix);
        assert!(parse_time_kind("9223372036854775808").is_err());
        assert!(parse_time_kind("2025-4-2").is_err());
        assert!(parse_time_kind("2025-04-02T25:04:05Z").is_err());
        assert!(parse_time_kind("2025-02-29").is_err());
        assert!(parse_time_kind("2024-02-29").is_ok());

        let reversed =
            validate_fact_times(Some("2025-04-02"), Some("2025-01-10"), None, "1712016000")
                .unwrap_err()
                .to_string();
        assert!(reversed.contains("valid_to must be later"));

        let mixed = validate_fact_times(Some("2025-04-02"), Some("1712016000"), None, "1712016000")
            .unwrap_err()
            .to_string();
        assert!(mixed.contains("same time format"));

        let same_instant = validate_fact_times(
            Some("2025-04-02"),
            Some("2025-04-02T00:00:00Z"),
            None,
            "1712016000",
        )
        .unwrap_err()
        .to_string();
        assert!(same_instant.contains("valid_to must be later"));

        let expired_before_start =
            validate_fact_times(Some("2025-04-02"), None, Some("2025-04-01"), "1712016000")
                .unwrap_err()
                .to_string();
        assert!(expired_before_start.contains("expired_at must be later"));
    }

    #[test]
    fn converts_iso_times_to_unix_seconds() {
        assert_eq!(iso_to_unix_seconds("1970-01-01").unwrap(), 0);
        assert_eq!(iso_to_unix_seconds("2025-01-01").unwrap(), 1_735_689_600);
        assert_eq!(
            iso_to_unix_seconds("2025-01-01T12:00:00Z").unwrap(),
            1_735_732_800
        );
        assert_eq!(temporal_unix_seconds("-1").unwrap(), -1);
    }

    #[test]
    fn rejects_unknown_fact_options_and_bad_confidence() {
        let unknown =
            parse_fact_directive("User | PREFERS | Puma | valid_from=2025-04-02 | typo=true")
                .unwrap_err()
                .to_string();
        assert!(unknown.contains("unknown semantic fact option"));

        let bad_confidence = parse_fact_directive("User | PREFERS | Puma | confidence=2.0")
            .unwrap_err()
            .to_string();
        assert!(bad_confidence.contains("between 0.0 and 1.0"));

        let bad_metadata = parse_confidence(Some("2.0")).unwrap_err().to_string();
        assert!(bad_metadata.contains("between 0.0 and 1.0"));
    }

    #[test]
    fn rejects_empty_segments_and_duplicate_options() {
        let empty_segment = parse_fact_directive("User || PREFERS | Puma")
            .unwrap_err()
            .to_string();
        assert!(empty_segment.contains("subject, relation, and object are required"));

        let duplicate_option = parse_fact_directive(
            "User | PREFERS | Puma | valid_from=2025-01-01 | valid_from=2025-02-01",
        )
        .unwrap_err()
        .to_string();
        assert!(duplicate_option.contains("duplicate semantic fact option"));
    }

    #[test]
    fn extracts_episode_entities_and_facts_from_memory_body() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "abc123".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::User,
                kind: MemoryKind::Preference,
                status: MemoryStatus::Active,
                created_at: "2026-07-09T00:00:00Z".to_string(),
                updated_at: "2026-07-09T00:00:00Z".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("0.9".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: "# Preference\n@fact User | PREFERS | Puma | valid_from=2025-04-02".to_string(),
            path: None,
        };

        let extraction = extract(&memory).unwrap();
        assert_eq!(extraction.episode.id, "episode-abc123");
        assert_eq!(extraction.entities.len(), 2);
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].relation, "PREFERS");
        assert_eq!(
            extraction.facts[0].valid_from.as_deref(),
            Some("2025-04-02")
        );
        assert_eq!(extraction.facts[0].time_kind, TimeKind::IsoDate);
        assert_eq!(extraction.facts[0].confidence, Some(0.9));
    }

    #[test]
    fn ignores_fact_directives_inside_markdown_code_fences() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "abc123".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::User,
                kind: MemoryKind::Note,
                status: MemoryStatus::Active,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: "# Example\n```text\n@fact User | LOVES | Broken\n```\n    @fact User | LOVES | AlsoBroken\n@fact User | USES | SQLite"
                .to_string(),
            path: None,
        };

        let extraction = extract(&memory).unwrap();
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].relation, "USES");
    }

    #[test]
    fn honors_fence_marker_and_width_when_skipping_directives() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "fence-test".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::User,
                kind: MemoryKind::Note,
                status: MemoryStatus::Active,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: "# Example\n````text\n```\n@fact User | LOVES | CodeOnly\n````\n@fact User | USES | SQLite"
                .to_string(),
            path: None,
        };

        let extraction = extract(&memory).unwrap();
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].relation, "USES");
        assert_eq!(extraction.facts[0].object_value, "SQLite");
    }

    #[test]
    fn does_not_treat_four_space_indented_code_as_a_fence() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "indented-fence-test".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::User,
                kind: MemoryKind::Note,
                status: MemoryStatus::Active,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: "# Example\n    ```\n@fact User | USES | SQLite".to_string(),
            path: None,
        };

        let extraction = extract(&memory).unwrap();
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].relation, "USES");
    }

    #[test]
    fn ignores_fact_directives_inside_html_comments() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "comment-test".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::User,
                kind: MemoryKind::Note,
                status: MemoryStatus::Active,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: Vec::new(),
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body:
                "# Example\n<!--\n@fact User | LOVES | CommentOnly\n-->\n@fact User | USES | SQLite"
                    .to_string(),
            path: None,
        };

        let extraction = extract(&memory).unwrap();
        assert_eq!(extraction.facts.len(), 1);
        assert_eq!(extraction.facts[0].relation, "USES");
    }
}
