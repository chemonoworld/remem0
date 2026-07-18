use std::{collections::BTreeMap, io};

use color_eyre::eyre::{Result, eyre};

use crate::{
    memory::{self, Memory, MemoryKind, MemoryMetadata, MemoryScope, MemoryStatus, MemoryType},
    output::{self, Tone},
    semantic,
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewAction {
    Add,
    Append,
    Update,
    Supersede,
    NoOp,
    Abort,
}

impl ReviewAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Append => "append",
            Self::Update => "update",
            Self::Supersede => "supersede",
            Self::NoOp => "no-op",
            Self::Abort => "abort",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReviewPlan {
    pub suggested_action: ReviewAction,
    pub target: Option<Memory>,
    pub candidates: Vec<ReviewCandidate>,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct ReviewCandidate {
    pub memory: Memory,
    pub suggested_action: ReviewAction,
    pub reason: &'static str,
    pub semantic_match: Option<SemanticMatch>,
}

#[derive(Clone, Debug)]
pub struct SemanticMatch {
    pub subject: String,
    pub relation: String,
    pub existing_object: String,
    pub proposed_object: String,
}

impl ReviewPlan {
    pub fn target_id(&self) -> Option<&str> {
        self.target
            .as_ref()
            .map(|memory| memory.metadata.id.as_str())
    }
}

pub fn plan(
    workspace: &Workspace,
    id: Option<&str>,
    body: &str,
    scope: MemoryScope,
) -> Result<ReviewPlan> {
    if body.trim().is_empty() {
        return Err(eyre!("review body cannot be empty"));
    }

    let Some(id) = id else {
        return semantic_plan(workspace, body, scope);
    };
    let target = memory::find_memory(workspace, id, true)?;
    if target.metadata.status != MemoryStatus::Active {
        return Err(eyre!(
            "memory {} is {}; choose an explicit add or supersede action instead",
            target.metadata.id,
            target.metadata.status
        ));
    }

    let suggested_action = if memory::bodies_match(&target.body, body) {
        ReviewAction::NoOp
    } else {
        ReviewAction::Update
    };
    let reason = if suggested_action == ReviewAction::NoOp {
        "unchanged-body"
    } else {
        "explicit-target"
    };

    Ok(ReviewPlan {
        suggested_action,
        target: Some(target),
        candidates: Vec::new(),
        reason,
    })
}

pub fn semantic_plan(workspace: &Workspace, body: &str, scope: MemoryScope) -> Result<ReviewPlan> {
    let active_memories = memory::list_memories(
        workspace,
        &memory::MemoryFilter {
            scope: Some(scope),
            ..memory::MemoryFilter::default()
        },
    )?;

    let exact_matches = active_memories
        .iter()
        .filter(|memory| memory::bodies_match(&memory.body, body))
        .cloned()
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        let target = exact_matches.into_iter().next().unwrap();
        return Ok(ReviewPlan {
            suggested_action: ReviewAction::NoOp,
            target: Some(target.clone()),
            candidates: vec![ReviewCandidate {
                memory: target,
                suggested_action: ReviewAction::NoOp,
                reason: "matching-body",
                semantic_match: None,
            }],
            reason: "matching-body",
        });
    }
    if exact_matches.len() > 1 {
        return Ok(ReviewPlan {
            suggested_action: ReviewAction::Add,
            target: None,
            candidates: exact_matches
                .into_iter()
                .map(|memory| ReviewCandidate {
                    memory,
                    suggested_action: ReviewAction::NoOp,
                    reason: "matching-body",
                    semantic_match: None,
                })
                .collect(),
            reason: "ambiguous-body-candidates",
        });
    }

    let proposal = proposal_memory(body, scope);
    let proposed = semantic::extract(&proposal)?;
    let proposed_names = entity_names(&proposed);
    let mut candidates = BTreeMap::<String, ReviewCandidate>::new();

    for memory in active_memories {
        let extracted = semantic::extract(&memory).map_err(|err| {
            eyre!(
                "failed to analyze active memory {} for semantic review: {err}",
                memory.metadata.id
            )
        })?;
        for proposed_fact in &proposed.facts {
            if !semantic::fact_is_current(proposed_fact)? {
                continue;
            }
            for existing_fact in &extracted.facts {
                if !semantic::fact_is_current(existing_fact)?
                    || proposed_fact.subject_id != existing_fact.subject_id
                    || proposed_fact.relation != existing_fact.relation
                {
                    continue;
                }

                let same_object = same_semantic_object(proposed_fact, existing_fact);
                let (suggested_action, reason) =
                    semantic_action(&proposed_fact.relation, same_object);
                let candidate = ReviewCandidate {
                    memory: memory.clone(),
                    suggested_action,
                    reason,
                    semantic_match: Some(SemanticMatch {
                        subject: proposed_names
                            .get(&proposed_fact.subject_id)
                            .cloned()
                            .unwrap_or_else(|| proposed_fact.subject_id.clone()),
                        relation: proposed_fact.relation.clone(),
                        existing_object: existing_fact.object_value.clone(),
                        proposed_object: proposed_fact.object_value.clone(),
                    }),
                };
                match candidates.get(&memory.metadata.id) {
                    Some(existing)
                        if action_priority(existing.suggested_action)
                            >= action_priority(suggested_action) => {}
                    _ => {
                        candidates.insert(memory.metadata.id.clone(), candidate);
                    }
                }
            }
        }
    }

    let candidates = candidates.into_values().collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(ReviewPlan {
            suggested_action: ReviewAction::Add,
            target: None,
            candidates,
            reason: "no-explicit-target",
        });
    }
    if candidates.len() == 1 {
        let candidate = &candidates[0];
        let suggested_action = candidate.suggested_action;
        let target = candidate.memory.clone();
        let reason = candidate.reason;
        return Ok(ReviewPlan {
            suggested_action,
            target: Some(target),
            candidates,
            reason,
        });
    }
    Ok(ReviewPlan {
        suggested_action: ReviewAction::Add,
        target: None,
        candidates,
        reason: "ambiguous-semantic-candidates",
    })
}

pub fn print_plan(plan: &ReviewPlan) {
    if let Some(target) = &plan.target {
        let memory_type = target.metadata.memory_type.to_string();
        output::line(format!(
            "{} {} {} {} {} {}",
            output::paint("candidate", Tone::Info),
            output::key_value("id", &target.metadata.id, Tone::Id),
            output::key_value("type", &memory_type, output::memory_type_tone(&memory_type)),
            output::key_value("scope", target.metadata.scope, Tone::Scope),
            output::key_value("kind", target.metadata.kind, Tone::Kind),
            output::key_value("title", target.title(), Tone::Title)
        ));
    } else {
        output::line(format!(
            "{} {}",
            output::paint("candidate", Tone::Info),
            output::paint("none", Tone::Muted)
        ));
    }
    for candidate in &plan.candidates {
        if let Some(semantic_match) = &candidate.semantic_match {
            output::line(format!(
                "{} {} {} {} {} {} {} {} {}",
                output::paint("candidate", Tone::Info),
                output::paint("semantic", Tone::Short),
                output::key_value("id", &candidate.memory.metadata.id, Tone::Id),
                output::key_value(
                    "suggested",
                    candidate.suggested_action.label(),
                    output::action_tone(candidate.suggested_action.label())
                ),
                output::key_value("reason", candidate.reason, Tone::Muted),
                output::key_value("subject", &semantic_match.subject, Tone::Title),
                output::key_value("relation", &semantic_match.relation, Tone::Kind),
                output::key_value("existing", &semantic_match.existing_object, Tone::Warning),
                output::key_value("proposed", &semantic_match.proposed_object, Tone::Success)
            ));
        }
    }
    output::line(format!(
        "{} {} {} {}",
        output::paint("review", Tone::Info),
        output::key_value(
            "suggested",
            plan.suggested_action.label(),
            output::action_tone(plan.suggested_action.label())
        ),
        output::key_value("target", plan.target_id().unwrap_or("-"), Tone::Id),
        output::key_value("reason", plan.reason, Tone::Muted)
    ));
}

pub fn prompt_for_action(plan: &ReviewPlan) -> Result<ReviewAction> {
    if plan.suggested_action == ReviewAction::NoOp {
        return Ok(ReviewAction::NoOp);
    }

    let options = if plan.target.is_some() {
        "add / append / update / supersede / no-op / abort"
    } else {
        "add / no-op / abort"
    };
    output::prompt(format!(
        "choose {options} [{}]: ",
        plan.suggested_action.label()
    ))?;

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Err(eyre!("memory review input ended before a choice was made"));
    }
    let answer = answer.trim().to_ascii_lowercase();
    match answer.as_str() {
        "" => Ok(plan.suggested_action),
        "add" | "a" => Ok(ReviewAction::Add),
        "append" | "p" if plan.target.is_some() => Ok(ReviewAction::Append),
        "update" | "u" if plan.target.is_some() => Ok(ReviewAction::Update),
        "supersede" | "s" if plan.target.is_some() => Ok(ReviewAction::Supersede),
        "no-op" | "noop" | "n" => Ok(ReviewAction::NoOp),
        "abort" | "q" => Ok(ReviewAction::Abort),
        "append" | "p" | "update" | "u" | "supersede" | "s" => Err(eyre!(
            "append, update, and supersede require one review target"
        )),
        other => Err(eyre!("unknown memory review choice {other:?}")),
    }
}

fn proposal_memory(body: &str, scope: MemoryScope) -> Memory {
    Memory {
        metadata: MemoryMetadata {
            id: "review-proposal".to_string(),
            memory_type: MemoryType::Short,
            scope,
            kind: MemoryKind::Note,
            status: MemoryStatus::Active,
            created_at: "0".to_string(),
            updated_at: "0".to_string(),
            tags: Vec::new(),
            title: None,
            source: None,
            source_id: None,
            agent: None,
            session: None,
            confidence: Some("1.0".to_string()),
            promoted_from: None,
            supersedes: Vec::new(),
        },
        body: body.to_string(),
        path: None,
    }
}

fn entity_names(extraction: &semantic::SemanticExtraction) -> BTreeMap<String, String> {
    extraction
        .entities
        .iter()
        .map(|entity| (entity.id.clone(), entity.canonical_name.clone()))
        .collect()
}

fn same_semantic_object(left: &semantic::SemanticFact, right: &semantic::SemanticFact) -> bool {
    match (&left.object_id, &right.object_id) {
        (Some(left), Some(right)) => left == right,
        (None, None) => {
            canonical_semantic_value(&left.object_value)
                == canonical_semantic_value(&right.object_value)
        }
        _ => false,
    }
}

fn canonical_semantic_value(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn semantic_action(relation: &str, same_object: bool) -> (ReviewAction, &'static str) {
    if same_object {
        return (ReviewAction::Append, "semantic-same-fact");
    }
    let exclusive = semantic::relation_specs()
        .iter()
        .find(|spec| spec.name == relation)
        .is_some_and(|spec| spec.invalidation_policy == "review-close-previous");
    if exclusive {
        (ReviewAction::Supersede, "semantic-exclusive-conflict")
    } else {
        (ReviewAction::Add, "semantic-compatible-fact")
    }
}

fn action_priority(action: ReviewAction) -> u8 {
    match action {
        ReviewAction::Supersede => 3,
        ReviewAction::Append => 2,
        ReviewAction::Add => 1,
        ReviewAction::Update | ReviewAction::NoOp | ReviewAction::Abort => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::ReviewAction;

    #[test]
    fn review_action_labels_are_stable() {
        assert_eq!(ReviewAction::Add.label(), "add");
        assert_eq!(ReviewAction::Append.label(), "append");
        assert_eq!(ReviewAction::Update.label(), "update");
        assert_eq!(ReviewAction::Supersede.label(), "supersede");
        assert_eq!(ReviewAction::NoOp.label(), "no-op");
    }

    #[test]
    fn semantic_action_respects_relation_cardinality() {
        assert_eq!(
            super::semantic_action("PREFERS", false),
            (ReviewAction::Supersede, "semantic-exclusive-conflict")
        );
        assert_eq!(
            super::semantic_action("PREFERS", true),
            (ReviewAction::Append, "semantic-same-fact")
        );
        assert_eq!(
            super::semantic_action("USES", false),
            (ReviewAction::Add, "semantic-compatible-fact")
        );
    }
}
