use std::{fs, io};

use color_eyre::eyre::{Result, WrapErr, eyre};

use crate::workspace::Workspace;

pub fn write_default_policies(workspace: &Workspace) -> Result<()> {
    write_if_missing(
        workspace,
        "short.md",
        r#"---
policy_id: default-short
memory_type: short
auto_apply: true
requires_review: false
allow_agent_write: true
redact_secrets: true
max_age_days: 14
review_operations: []
---

# Short-Term Memory Policy

Capture temporary project context, active tasks, recent decisions, and unresolved questions.
Do not store credentials, raw logs, or unverified claims as durable facts.
"#,
    )?;
    write_if_missing(
        workspace,
        "long.md",
        r#"---
policy_id: default-long
memory_type: long
auto_apply: false
requires_review: true
allow_agent_write: false
redact_secrets: true
max_age_days: null
review_operations: [create, update, delete]
---

# Long-Term Memory Policy

Store durable facts, preferences, decisions, procedures, and stable project knowledge.
Long-term mutations should be preview-first by default.
"#,
    )?;
    write_if_missing(
        workspace,
        "promote.md",
        r#"---
policy_id: default-promote
memory_type: promote
auto_apply: false
requires_review: true
allow_agent_write: false
redact_secrets: true
max_age_days: null
review_operations: [promote]
---

# Promotion Policy

Promote short-term memory when it captures a stable preference, durable decision,
reusable procedure, or fact referenced by multiple sessions.
"#,
    )?;
    Ok(())
}

fn write_if_missing(workspace: &Workspace, name: &str, content: &str) -> Result<()> {
    let path = workspace.policies_dir().join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(eyre!(
                "refusing to use symlinked policy file {}",
                path.display()
            ));
        }
        Ok(metadata) if metadata.is_file() => return Ok(()),
        Ok(_) => {
            return Err(eyre!("policy path {} must be a file", path.display()));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("failed to inspect {}", path.display()));
        }
    }
    fs::write(&path, content.trim_start())
        .wrap_err_with(|| format!("failed to write {}", path.display()))
}
