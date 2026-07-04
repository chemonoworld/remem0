use std::{fs, path::Path};

use color_eyre::eyre::Result;

use crate::{config::StorageMode, index, transaction, workspace::Workspace};

#[derive(Clone, Debug)]
pub struct DoctorFinding {
    pub level: &'static str,
    pub message: String,
}

pub fn run(workspace: &Workspace, _storage: StorageMode) -> Result<Vec<DoctorFinding>> {
    let mut findings = Vec::new();
    check_dir(&mut findings, workspace.root(), "vault root");
    check_dir(&mut findings, &workspace.short_dir(), "memories/short");
    check_dir(&mut findings, &workspace.long_dir(), "memories/long");
    check_dir(&mut findings, &workspace.policies_dir(), "policies");
    check_dir(&mut findings, &workspace.archive_dir(), "archive");
    check_dir(&mut findings, &workspace.cache_dir(), ".rem/cache");

    for policy in ["short.md", "long.md", "promote.md"] {
        let path = workspace.policies_dir().join(policy);
        if path.exists() {
            findings.push(ok(format!("policy exists: {}", path.display())));
        } else {
            findings.push(warn(format!("missing policy: {}", path.display())));
        }
    }

    if workspace.index_path().exists() {
        findings.push(ok(format!(
            "index exists: {}",
            workspace.index_path().display()
        )));
        match index::diagnostics_count(workspace) {
            Ok(0) => findings.push(ok("index diagnostics are clean".to_string())),
            Ok(diagnostics) => {
                findings.push(warn(format!("index has {diagnostics} diagnostics")));
            }
            Err(err) => {
                findings.push(warn(format!(
                    "index is unreadable; run `rem rebuild`: {err}"
                )));
            }
        }
    } else {
        findings.push(warn("index missing; run `rem rebuild`".to_string()));
    }

    match transaction::validate_git_vault(workspace.root()) {
        Ok(info) => findings.push(ok(format!(
            "Git remote origin is supported for {}: {}",
            info.root.display(),
            info.origin_url
        ))),
        Err(err) => findings.push(warn(format!("Git vault validation failed: {err}"))),
    }

    check_gitignore(&mut findings, workspace.root());

    match transaction::pending_transactions(workspace) {
        Ok(pending) if pending.is_empty() => {
            findings.push(ok("no pending transaction journals".to_string()));
        }
        Ok(pending) => {
            for path in pending {
                findings.push(warn(format!(
                    "pending transaction journal: {}; run `rem commit` after recovery",
                    path.display()
                )));
            }
        }
        Err(err) => findings.push(warn(format!(
            "could not inspect transaction journals: {err}"
        ))),
    }

    Ok(findings)
}

fn check_dir(findings: &mut Vec<DoctorFinding>, path: &Path, label: &str) {
    if path.is_dir() {
        findings.push(ok(format!("{label} exists: {}", path.display())));
    } else {
        findings.push(warn(format!("{label} missing: {}", path.display())));
    }
}

fn check_gitignore(findings: &mut Vec<DoctorFinding>, root: &Path) {
    let path = root.join(".gitignore");
    let raw = fs::read_to_string(&path).unwrap_or_default();
    if raw
        .lines()
        .any(|line| line.trim() == ".rem/cache/" || line.trim() == ".rem/cache")
    {
        findings.push(ok(".gitignore excludes .rem/cache/".to_string()));
    } else {
        findings.push(warn(
            "Git-backed vault should ignore .rem/cache/".to_string(),
        ));
    }

    if raw
        .lines()
        .any(|line| line.trim() == ".rem/tx/" || line.trim() == ".rem/tx")
    {
        findings.push(ok(".gitignore excludes .rem/tx/".to_string()));
    } else {
        findings.push(warn("Git-backed vault should ignore .rem/tx/".to_string()));
    }
}

fn ok(message: String) -> DoctorFinding {
    DoctorFinding {
        level: "ok",
        message,
    }
}

fn warn(message: String) -> DoctorFinding {
    DoctorFinding {
        level: "warn",
        message,
    }
}
