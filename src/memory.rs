use std::{
    fmt, fs,
    path::{Path, PathBuf},
    process,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::ValueEnum;
use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};

use crate::{
    frontmatter::{self, FieldValue},
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryType {
    Short,
    Long,
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => f.write_str("short"),
            Self::Long => f.write_str("long"),
        }
    }
}

impl FromStr for MemoryType {
    type Err = color_eyre::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "short" => Ok(Self::Short),
            "long" => Ok(Self::Long),
            other => Err(eyre!("unknown memory type {other:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryScope {
    User,
    Project,
    Agent,
    Session,
}

impl fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Project => f.write_str("project"),
            Self::Agent => f.write_str("agent"),
            Self::Session => f.write_str("session"),
        }
    }
}

impl FromStr for MemoryScope {
    type Err = color_eyre::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "agent" => Ok(Self::Agent),
            "session" => Ok(Self::Session),
            other => Err(eyre!("unknown memory scope {other:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryKind {
    Fact,
    Preference,
    Decision,
    Task,
    Procedure,
    Note,
    Question,
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fact => f.write_str("fact"),
            Self::Preference => f.write_str("preference"),
            Self::Decision => f.write_str("decision"),
            Self::Task => f.write_str("task"),
            Self::Procedure => f.write_str("procedure"),
            Self::Note => f.write_str("note"),
            Self::Question => f.write_str("question"),
        }
    }
}

impl FromStr for MemoryKind {
    type Err = color_eyre::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "fact" => Ok(Self::Fact),
            "preference" => Ok(Self::Preference),
            "decision" => Ok(Self::Decision),
            "task" => Ok(Self::Task),
            "procedure" => Ok(Self::Procedure),
            "note" => Ok(Self::Note),
            "question" => Ok(Self::Question),
            other => Err(eyre!("unknown memory kind {other:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryStatus {
    Active,
    Archived,
    Superseded,
}

impl fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Archived => f.write_str("archived"),
            Self::Superseded => f.write_str("superseded"),
        }
    }
}

impl FromStr for MemoryStatus {
    type Err = color_eyre::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            "superseded" => Ok(Self::Superseded),
            other => Err(eyre!("unknown memory status {other:?}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryMetadata {
    pub id: String,
    pub memory_type: MemoryType,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub status: MemoryStatus,
    pub created_at: String,
    pub updated_at: String,
    pub tags: Vec<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub confidence: Option<String>,
    pub promoted_from: Option<String>,
    pub supersedes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Memory {
    pub metadata: MemoryMetadata,
    pub body: String,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryFilter {
    pub memory_type: Option<MemoryType>,
    pub scope: Option<MemoryScope>,
    pub kind: Option<MemoryKind>,
    pub tag: Option<String>,
    pub include_archived: bool,
}

#[derive(Clone, Debug)]
pub struct CreateMemoryInput {
    memory_type: MemoryType,
    scope: MemoryScope,
    kind: MemoryKind,
    tags: Vec<String>,
    body: String,
    source: Option<String>,
    agent: Option<String>,
    session: Option<String>,
}

impl CreateMemoryInput {
    pub fn new(
        memory_type: MemoryType,
        scope: MemoryScope,
        kind: MemoryKind,
        body: String,
    ) -> Self {
        Self {
            memory_type,
            scope,
            kind,
            tags: Vec::new(),
            body,
            source: None,
            agent: None,
            session: None,
        }
    }

    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    pub fn agent(mut self, agent: Option<String>) -> Self {
        self.agent = agent;
        self
    }

    pub fn session(mut self, session: Option<String>) -> Self {
        self.session = session;
        self
    }
}

pub fn create_memory(workspace: &Workspace, input: CreateMemoryInput) -> Result<Memory> {
    if input.body.trim().is_empty() {
        return Err(eyre!("memory body cannot be empty"));
    }

    let now = now_string();
    let id = generate_id();
    let memory = Memory {
        metadata: MemoryMetadata {
            id,
            memory_type: input.memory_type,
            scope: input.scope,
            kind: input.kind,
            status: MemoryStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            tags: normalize_tags(input.tags),
            title: None,
            source: input.source,
            agent: input.agent,
            session: input.session,
            confidence: Some("1.0".to_string()),
            promoted_from: None,
            supersedes: Vec::new(),
        },
        body: input.body,
        path: None,
    };

    write_memory(workspace, &memory)
}

pub fn write_memory(workspace: &Workspace, memory: &Memory) -> Result<Memory> {
    let dir = match memory.metadata.status {
        MemoryStatus::Archived => workspace.archive_dir(),
        MemoryStatus::Active | MemoryStatus::Superseded => match memory.metadata.memory_type {
            MemoryType::Short => workspace.short_dir(),
            MemoryType::Long => workspace.long_dir(),
        },
    };
    fs::create_dir_all(&dir).wrap_err_with(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("{}.md", memory.metadata.id));
    fs::write(&path, memory.to_markdown())
        .wrap_err_with(|| format!("failed to write {}", path.display()))?;

    let mut written = memory.clone();
    written.path = Some(path);
    Ok(written)
}

pub fn read_memory(path: &Path) -> Result<Memory> {
    let raw = fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read memory {}", path.display()))?;
    let (frontmatter, body) = frontmatter::split_document(&raw)
        .wrap_err_with(|| format!("failed to parse memory {}", path.display()))?;

    let memory_type = required_scalar(&frontmatter, "type")?.parse()?;
    let scope = required_scalar(&frontmatter, "scope")?.parse()?;
    let kind = required_scalar(&frontmatter, "kind")?.parse()?;
    let status = required_scalar(&frontmatter, "status")?.parse()?;

    let memory = Memory {
        metadata: MemoryMetadata {
            id: required_scalar(&frontmatter, "id")?,
            memory_type,
            scope,
            kind,
            status,
            created_at: required_scalar(&frontmatter, "created_at")?,
            updated_at: required_scalar(&frontmatter, "updated_at")?,
            tags: frontmatter::get_list(&frontmatter, "tags"),
            title: frontmatter::get_scalar(&frontmatter, "title"),
            source: frontmatter::get_scalar(&frontmatter, "source"),
            agent: frontmatter::get_scalar(&frontmatter, "agent"),
            session: frontmatter::get_scalar(&frontmatter, "session"),
            confidence: frontmatter::get_scalar(&frontmatter, "confidence"),
            promoted_from: frontmatter::get_scalar(&frontmatter, "promoted_from"),
            supersedes: frontmatter::get_list(&frontmatter, "supersedes"),
        },
        body,
        path: Some(path.to_path_buf()),
    };
    memory.validate()?;
    Ok(memory)
}

pub fn list_memories(workspace: &Workspace, filter: &MemoryFilter) -> Result<Vec<Memory>> {
    let mut memories = Vec::new();
    for path in memory_paths(workspace, filter.include_archived)? {
        match read_memory(&path) {
            Ok(memory) if matches_filter(&memory, filter) => memories.push(memory),
            Ok(_) => {}
            Err(err) => {
                eprintln!("warn\t{}: {err}", path.display());
            }
        }
    }
    memories.sort_by(|left, right| left.metadata.id.cmp(&right.metadata.id));
    Ok(memories)
}

pub fn memory_paths(workspace: &Workspace, include_archived: bool) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![workspace.short_dir(), workspace.long_dir()];
    if include_archived {
        dirs.push(workspace.archive_dir());
    }

    let mut paths = Vec::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let mut dir_paths = Vec::new();
        for entry in
            fs::read_dir(&dir).wrap_err_with(|| format!("failed to read {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                dir_paths.push(path);
            }
        }
        dir_paths.sort();
        paths.extend(dir_paths);
    }
    Ok(paths)
}

pub fn find_memory(
    workspace: &Workspace,
    id_or_prefix: &str,
    include_archived: bool,
) -> Result<Memory> {
    let mut exact_matches = Vec::new();
    let mut prefix_matches = Vec::new();
    for path in memory_paths(workspace, include_archived)? {
        let memory = match read_memory(&path) {
            Ok(memory) => memory,
            Err(err) => {
                eprintln!("warn\t{}: {err}", path.display());
                continue;
            }
        };
        if memory.metadata.id == id_or_prefix {
            exact_matches.push(memory);
        } else if memory.metadata.id.starts_with(id_or_prefix) {
            prefix_matches.push(memory);
        }
    }

    if !exact_matches.is_empty() {
        return select_exact_match(id_or_prefix, exact_matches);
    }

    match prefix_matches.len() {
        0 => Err(eyre!("no memory found for {id_or_prefix:?}")),
        1 => Ok(prefix_matches.remove(0)),
        _ => Err(eyre!("memory prefix {id_or_prefix:?} is ambiguous")),
    }
}

pub fn effective_priority(memory: &Memory) -> (u8, u8) {
    exact_match_priority(memory, &memory.metadata.id)
}

pub fn update_memory(
    workspace: &Workspace,
    id: &str,
    body: String,
    append: bool,
) -> Result<Memory> {
    if body.trim().is_empty() {
        return Err(eyre!("updated body cannot be empty"));
    }
    let mut memory = find_memory(workspace, id, false)?;
    let original_path = memory.path.clone();
    memory.body = if append {
        format!("{}\n\n{}", memory.body.trim(), body.trim())
    } else {
        body
    };
    memory.metadata.updated_at = now_string();
    let written = write_memory(workspace, &memory)?;
    if original_path.as_ref() != written.path.as_ref()
        && let Some(path) = original_path
        && path.exists()
    {
        fs::remove_file(path)?;
    }
    Ok(written)
}

pub fn delete_memory(workspace: &Workspace, id: &str, hard: bool) -> Result<Memory> {
    let mut memory = find_memory(workspace, id, true)?;
    let original_path = memory.path.clone();
    if hard {
        if let Some(path) = &memory.path {
            fs::remove_file(path)?;
        }
        return Ok(memory);
    }

    memory.metadata.status = MemoryStatus::Archived;
    memory.metadata.updated_at = now_string();
    let written = write_memory(workspace, &memory)?;
    if original_path.as_ref() != written.path.as_ref()
        && let Some(path) = original_path
        && path.exists()
    {
        fs::remove_file(path)?;
    }
    Ok(written)
}

pub fn promote_memory(
    workspace: &Workspace,
    id: &str,
    body_override: Option<String>,
) -> Result<Memory> {
    let source = find_memory(workspace, id, false)?;
    if source.metadata.memory_type != MemoryType::Short {
        return Err(eyre!("only short-term memories can be promoted"));
    }

    let now = now_string();
    let body = body_override.unwrap_or_else(|| source.body.clone());
    let promoted = Memory {
        metadata: MemoryMetadata {
            id: generate_id(),
            memory_type: MemoryType::Long,
            scope: source.metadata.scope,
            kind: source.metadata.kind,
            status: MemoryStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            tags: source.metadata.tags.clone(),
            title: source.metadata.title.clone(),
            source: Some("promote".to_string()),
            agent: source.metadata.agent.clone(),
            session: source.metadata.session.clone(),
            confidence: source.metadata.confidence.clone(),
            promoted_from: Some(source.metadata.id),
            supersedes: Vec::new(),
        },
        body,
        path: None,
    };

    write_memory(workspace, &promoted)
}

impl Memory {
    pub fn title(&self) -> String {
        self.metadata
            .title
            .clone()
            .or_else(|| {
                self.body
                    .lines()
                    .find_map(|line| line.strip_prefix("# ").map(str::trim).map(str::to_string))
            })
            .unwrap_or_else(|| first_words(&self.body, 8))
    }

    pub fn to_markdown(&self) -> String {
        use FieldValue::{List, Scalar};

        let mut fields = vec![
            ("id", Scalar(self.metadata.id.clone())),
            ("type", Scalar(self.metadata.memory_type.to_string())),
            ("scope", Scalar(self.metadata.scope.to_string())),
            ("kind", Scalar(self.metadata.kind.to_string())),
            ("status", Scalar(self.metadata.status.to_string())),
            ("created_at", Scalar(self.metadata.created_at.clone())),
            ("updated_at", Scalar(self.metadata.updated_at.clone())),
            ("tags", List(self.metadata.tags.clone())),
            (
                "title",
                Scalar(
                    self.metadata
                        .title
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            (
                "source",
                Scalar(
                    self.metadata
                        .source
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            (
                "agent",
                Scalar(
                    self.metadata
                        .agent
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            (
                "session",
                Scalar(
                    self.metadata
                        .session
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            (
                "confidence",
                Scalar(
                    self.metadata
                        .confidence
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            (
                "promoted_from",
                Scalar(
                    self.metadata
                        .promoted_from
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                ),
            ),
            ("supersedes", List(self.metadata.supersedes.clone())),
        ];

        let mut output = frontmatter::render_frontmatter(&fields);
        output.push('\n');
        output.push_str(self.body.trim());
        output.push('\n');
        fields.clear();
        output
    }

    pub fn validate(&self) -> Result<()> {
        if self.metadata.id.trim().is_empty() {
            return Err(eyre!("memory id is required"));
        }
        if self.body.trim().is_empty() {
            return Err(eyre!("memory body is required"));
        }
        Ok(())
    }
}

fn matches_filter(memory: &Memory, filter: &MemoryFilter) -> bool {
    if !filter.include_archived && memory.metadata.status != MemoryStatus::Active {
        return false;
    }
    if let Some(memory_type) = filter.memory_type
        && memory.metadata.memory_type != memory_type
    {
        return false;
    }
    if let Some(scope) = filter.scope
        && memory.metadata.scope != scope
    {
        return false;
    }
    if let Some(kind) = filter.kind
        && memory.metadata.kind != kind
    {
        return false;
    }
    if let Some(tag) = &filter.tag
        && !memory
            .metadata
            .tags
            .iter()
            .any(|candidate| candidate == tag)
    {
        return false;
    }
    true
}

fn select_exact_match(id: &str, mut matches: Vec<Memory>) -> Result<Memory> {
    matches.sort_by_key(|memory| exact_match_priority(memory, id));
    let best_priority = exact_match_priority(&matches[0], id);
    if matches
        .get(1)
        .is_some_and(|memory| exact_match_priority(memory, id) == best_priority)
    {
        return Err(eyre!("memory id {id:?} has duplicate canonical matches"));
    }
    Ok(matches.remove(0))
}

fn exact_match_priority(memory: &Memory, id: &str) -> (u8, u8) {
    let status_rank = if memory.metadata.status == MemoryStatus::Active {
        0
    } else {
        1
    };
    let path_rank = if is_canonical_path(memory, id) { 0 } else { 1 };
    (status_rank, path_rank)
}

fn is_canonical_path(memory: &Memory, id: &str) -> bool {
    memory
        .path
        .as_ref()
        .and_then(|path| path.file_stem())
        .is_some_and(|stem| stem == id)
}

fn required_scalar(map: &frontmatter::Frontmatter, key: &str) -> Result<String> {
    frontmatter::get_scalar(map, key)
        .ok_or_else(|| eyre!("missing required frontmatter field {key:?}"))
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut tags = tags
        .into_iter()
        .flat_map(|tag| {
            tag.split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn generate_id() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "m{}{:09}-{}",
        duration.as_secs(),
        duration.subsec_nanos(),
        process::id()
    )
}

fn now_string() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
}

fn first_words(body: &str, count: usize) -> String {
    let title = body
        .split_whitespace()
        .take(count)
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "Untitled memory".to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_parses_memory() {
        let memory = Memory {
            metadata: MemoryMetadata {
                id: "m1".to_string(),
                memory_type: MemoryType::Short,
                scope: MemoryScope::Project,
                kind: MemoryKind::Decision,
                status: MemoryStatus::Active,
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
                tags: vec!["rust".to_string()],
                title: None,
                source: Some("test".to_string()),
                agent: None,
                session: None,
                confidence: Some("1.0".to_string()),
                promoted_from: None,
                supersedes: Vec::new(),
            },
            body: "# Use Markdown\nBody".to_string(),
            path: None,
        };

        let raw = memory.to_markdown();
        let temp_path = std::env::temp_dir().join(format!("rem-test-{}.md", process::id()));
        fs::write(&temp_path, raw).unwrap();
        let parsed = read_memory(&temp_path).unwrap();
        let _ = fs::remove_file(&temp_path);

        assert_eq!(parsed.metadata.id, "m1");
        assert_eq!(parsed.title(), "Use Markdown");
    }
}
