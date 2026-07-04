use std::{
    collections::BTreeSet,
    fs,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{Result, WrapErr, eyre};

use crate::{index, workspace::Workspace};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalChangePolicy {
    Prompt,
    Accept,
    Restore,
    Abort,
}

#[derive(Clone, Copy, Debug)]
pub struct TransactionOptions {
    pub external_policy: ExternalChangePolicy,
    pub non_interactive: bool,
}

impl Default for TransactionOptions {
    fn default() -> Self {
        Self {
            external_policy: ExternalChangePolicy::Prompt,
            non_interactive: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransactionOutcome {
    pub commit_id: Option<String>,
    pub changed_paths: Vec<String>,
    pub indexed: usize,
}

#[derive(Clone, Debug)]
pub struct DryRunReport {
    pub changed_paths: Vec<String>,
    pub indexed: usize,
    pub diagnostics: usize,
}

#[derive(Clone, Debug)]
pub struct GitStatusEntry {
    pub code: String,
    pub path: String,
}

impl GitStatusEntry {
    fn is_untracked(&self) -> bool {
        self.code == "??"
    }
}

pub fn run_mutation<T>(
    workspace: &Workspace,
    message: &str,
    options: TransactionOptions,
    apply: impl FnOnce() -> Result<T>,
) -> Result<(T, TransactionOutcome)> {
    run_mutation_with_message(workspace, options, || {
        let result = apply()?;
        Ok((result, message.to_string()))
    })
}

pub fn run_mutation_with_message<T>(
    workspace: &Workspace,
    options: TransactionOptions,
    apply: impl FnOnce() -> Result<(T, String)>,
) -> Result<(T, TransactionOutcome)> {
    let mut tx = Transaction::begin(workspace, options)?;

    let (result, message) = match ensure_gitignore(workspace).and_then(|_| apply()) {
        Ok(result) => result,
        Err(err) => {
            tx.rollback()?;
            return Err(err);
        }
    };

    let report = match tx.reindex_strict() {
        Ok(report) => report,
        Err(err) => {
            tx.rollback()?;
            return Err(err);
        }
    };

    let outcome = match tx.git_commit(&message, report.indexed) {
        Ok(outcome) => outcome,
        Err(err) => {
            tx.rollback()?;
            return Err(err);
        }
    };

    tx.finish()?;
    Ok((result, outcome))
}

pub fn dry_run(workspace: &Workspace) -> Result<DryRunReport> {
    validate_git_vault(workspace.root())?;
    fail_if_pending_transactions(workspace)?;

    let changed_paths = git_status(workspace.root())?
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    let temp_path = index::temp_index_path(workspace, "dry-run");
    let report = index::rebuild_to_path(workspace, &temp_path);
    let _ = fs::remove_file(&temp_path);
    let report = report?;

    Ok(DryRunReport {
        changed_paths,
        indexed: report.indexed,
        diagnostics: report.diagnostics,
    })
}

pub fn validate_git_vault(root: &Path) -> Result<GitInfo> {
    if !root.is_dir() {
        return Err(eyre!(
            "vault root {} must be an existing Git working tree",
            root.display()
        ));
    }

    let root = root
        .canonicalize()
        .wrap_err_with(|| format!("failed to resolve {}", root.display()))?;
    let top_level = git_stdout(&root, &["rev-parse", "--show-toplevel"])
        .wrap_err("vault root is not a Git repository")?;
    let top_level = PathBuf::from(top_level.trim());
    let top_level = top_level
        .canonicalize()
        .wrap_err_with(|| format!("failed to resolve Git root {}", top_level.display()))?;
    if top_level != root {
        return Err(eyre!(
            "vault root {} must be the Git repository root {}",
            root.display(),
            top_level.display()
        ));
    }

    let remote = git_stdout(&root, &["config", "--get", "remote.origin.url"])
        .wrap_err("vault Git repository must define remote origin")?;
    let remote = remote.trim().to_string();
    if !is_github_or_gitlab_remote(&remote) {
        return Err(eyre!(
            "vault Git remote origin must point to GitHub or GitLab, got {remote:?}"
        ));
    }

    Ok(GitInfo {
        root,
        origin_url: remote,
    })
}

pub fn pending_transactions(workspace: &Workspace) -> Result<Vec<PathBuf>> {
    let tx_dir = workspace.tx_dir();
    if !tx_dir.exists() {
        return Ok(Vec::new());
    }

    let mut pending = Vec::new();
    for entry in
        fs::read_dir(&tx_dir).wrap_err_with(|| format!("failed to read {}", tx_dir.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            pending.push(path);
        }
    }
    pending.sort();
    Ok(pending)
}

pub fn ensure_gitignore(workspace: &Workspace) -> Result<bool> {
    let path = workspace.root().join(".gitignore");
    let mut raw = fs::read_to_string(&path).unwrap_or_default();
    let mut changed = false;

    for entry in [".rem/cache/", ".rem/tx/"] {
        if !gitignore_has_entry(&raw, entry) {
            if !raw.is_empty() && !raw.ends_with('\n') {
                raw.push('\n');
            }
            raw.push_str(entry);
            raw.push('\n');
            changed = true;
        }
    }

    if changed {
        fs::write(&path, raw).wrap_err_with(|| format!("failed to write {}", path.display()))?;
    }

    Ok(changed)
}

pub fn git_status(root: &Path) -> Result<Vec<GitStatusEntry>> {
    let output = git_output(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !output.status.success() {
        return Err(git_error("git status", &output));
    }

    Ok(parse_status_z(&output.stdout)
        .into_iter()
        .filter(|entry| !is_excluded_relative_str(&entry.path))
        .collect())
}

pub fn is_github_or_gitlab_remote(remote: &str) -> bool {
    let remote = remote.to_ascii_lowercase();
    remote.contains("github.com") || remote.contains("gitlab.com")
}

pub fn is_excluded_relative(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == ".git" => true,
        Some(Component::Normal(first)) if first == ".rem" => matches!(
            components.next(),
            Some(Component::Normal(second)) if second == "cache" || second == "tx"
        ),
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct GitInfo {
    pub root: PathBuf,
    pub origin_url: String,
}

#[derive(Debug)]
struct Transaction {
    workspace: Workspace,
    tx_dir: PathBuf,
    snapshot_dir: PathBuf,
    snapshot_files: BTreeSet<String>,
    index_snapshot: Option<PathBuf>,
    temp_index: Option<PathBuf>,
    finished: bool,
}

impl Transaction {
    fn begin(workspace: &Workspace, options: TransactionOptions) -> Result<Self> {
        validate_git_vault(workspace.root())?;
        fail_if_pending_transactions(workspace)?;

        let external = git_status(workspace.root())?;
        resolve_external_changes(workspace.root(), &external, options)?;

        let tx_id = tx_id();
        let tx_dir = workspace.tx_dir().join(&tx_id);
        let snapshot_dir = tx_dir.join("snapshot");
        fs::create_dir_all(&snapshot_dir)
            .wrap_err_with(|| format!("failed to create {}", snapshot_dir.display()))?;

        let snapshot_files = snapshot_vault(workspace.root(), &snapshot_dir)?;
        let index_snapshot = snapshot_index(workspace, &tx_dir)?;

        Ok(Self {
            workspace: workspace.clone(),
            tx_dir,
            snapshot_dir,
            snapshot_files,
            index_snapshot,
            temp_index: None,
            finished: false,
        })
    }

    fn reindex_strict(&mut self) -> Result<index::RebuildReport> {
        let temp_path = self
            .workspace
            .cache_dir()
            .join(format!("index.sqlite.tmp-{}", tx_id()));
        self.temp_index = Some(temp_path.clone());
        let report = index::rebuild_to_path(&self.workspace, &temp_path)?;
        if report.diagnostics > 0 {
            let _ = fs::remove_file(&temp_path);
            self.temp_index = None;
            return Err(eyre!(
                "reindex produced {} diagnostics; fix malformed or duplicate memory files before committing",
                report.diagnostics
            ));
        }

        fs::rename(&temp_path, self.workspace.index_path()).wrap_err_with(|| {
            format!(
                "failed to replace {} with {}",
                self.workspace.index_path().display(),
                temp_path.display()
            )
        })?;
        self.temp_index = None;
        Ok(index::RebuildReport {
            index_path: self.workspace.index_path().display().to_string(),
            ..report
        })
    }

    fn git_commit(&mut self, message: &str, indexed: usize) -> Result<TransactionOutcome> {
        let status = git_status(self.workspace.root())?;
        if status.is_empty() {
            return Ok(TransactionOutcome {
                commit_id: None,
                changed_paths: Vec::new(),
                indexed,
            });
        }

        let mut args = vec!["add", "--all", "--"];
        args.extend(status.iter().map(|entry| entry.path.as_str()));
        git_checked(self.workspace.root(), &args, "git add")?;
        unstage_excluded(self.workspace.root())?;

        let changed_paths = staged_paths(self.workspace.root())?;
        if changed_paths.is_empty() {
            return Ok(TransactionOutcome {
                commit_id: None,
                changed_paths,
                indexed,
            });
        }

        let output = git_output(
            self.workspace.root(),
            &[
                "-c",
                "user.name=rem",
                "-c",
                "user.email=rem@example.invalid",
                "commit",
                "-m",
                message,
            ],
        )?;
        if !output.status.success() {
            return Err(git_error("git commit", &output));
        }

        let commit_id = git_stdout(self.workspace.root(), &["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string();

        Ok(TransactionOutcome {
            commit_id: Some(commit_id),
            changed_paths,
            indexed,
        })
    }

    fn rollback(&mut self) -> Result<()> {
        let mut first_error = None;

        if let Err(err) = unstage_all(self.workspace.root()) {
            first_error.get_or_insert(err);
        }
        if let Err(err) = restore_snapshot(
            self.workspace.root(),
            &self.snapshot_dir,
            &self.snapshot_files,
        ) {
            first_error.get_or_insert(err);
        }
        if let Err(err) = self.restore_index() {
            first_error.get_or_insert(err);
        }
        if let Some(temp_index) = &self.temp_index {
            let _ = fs::remove_file(temp_index);
        }
        if let Err(err) = unstage_all(self.workspace.root()) {
            first_error.get_or_insert(err);
        }
        if let Err(err) = self.finish() {
            first_error.get_or_insert(err);
        }

        if let Some(err) = first_error {
            Err(err)
        } else {
            Ok(())
        }
    }

    fn restore_index(&self) -> Result<()> {
        let index_path = self.workspace.index_path();
        if index_path.exists() {
            fs::remove_file(&index_path)
                .wrap_err_with(|| format!("failed to remove {}", index_path.display()))?;
        }
        if let Some(snapshot) = &self.index_snapshot {
            if let Some(parent) = index_path.parent() {
                fs::create_dir_all(parent)
                    .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(snapshot, &index_path).wrap_err_with(|| {
                format!(
                    "failed to restore {} from {}",
                    index_path.display(),
                    snapshot.display()
                )
            })?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.tx_dir.exists() {
            fs::remove_dir_all(&self.tx_dir)
                .wrap_err_with(|| format!("failed to remove {}", self.tx_dir.display()))?;
        }
        self.finished = true;
        Ok(())
    }
}

fn resolve_external_changes(
    root: &Path,
    entries: &[GitStatusEntry],
    options: TransactionOptions,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let action = match options.external_policy {
        ExternalChangePolicy::Accept => ExternalChangePolicy::Accept,
        ExternalChangePolicy::Restore => ExternalChangePolicy::Restore,
        ExternalChangePolicy::Abort => ExternalChangePolicy::Abort,
        ExternalChangePolicy::Prompt => {
            if options.non_interactive || !io::stdin().is_terminal() {
                return Err(eyre!(
                    "external vault changes detected; rerun with --accept-external or --restore-external"
                ));
            }
            prompt_external_changes(entries)?
        }
    };

    match action {
        ExternalChangePolicy::Accept => Ok(()),
        ExternalChangePolicy::Restore => restore_external_changes(root, entries),
        ExternalChangePolicy::Abort | ExternalChangePolicy::Prompt => {
            Err(eyre!("aborted due to external vault changes"))
        }
    }
}

fn prompt_external_changes(entries: &[GitStatusEntry]) -> Result<ExternalChangePolicy> {
    println!("external vault changes detected:");
    for entry in entries {
        println!("{}\t{}", entry.code, entry.path);
    }
    print!("choose commit / restore / abort [commit]: ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim() {
        "" | "commit" | "c" => Ok(ExternalChangePolicy::Accept),
        "restore" | "r" => Ok(ExternalChangePolicy::Restore),
        "abort" | "a" => Ok(ExternalChangePolicy::Abort),
        other => Err(eyre!("unknown external-change choice {other:?}")),
    }
}

fn restore_external_changes(root: &Path, entries: &[GitStatusEntry]) -> Result<()> {
    for entry in entries {
        let path = root.join(&entry.path);
        if entry.is_untracked() {
            remove_path_if_exists(&path)?;
        } else {
            git_checked(
                root,
                &["restore", "--staged", "--worktree", "--", &entry.path],
                "git restore",
            )?;
        }
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
            .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
    } else if path.exists() {
        fs::remove_file(path).wrap_err_with(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn snapshot_vault(root: &Path, snapshot_dir: &Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    for file in collect_files(root)? {
        let relative = file
            .strip_prefix(root)
            .wrap_err_with(|| format!("failed to relativize {}", file.display()))?;
        if is_excluded_relative(relative) {
            continue;
        }

        let label = path_label(relative);
        let target = snapshot_dir.join(&label);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(&file, &target).wrap_err_with(|| {
            format!(
                "failed to snapshot {} to {}",
                file.display(),
                target.display()
            )
        })?;
        files.insert(label);
    }
    Ok(files)
}

fn snapshot_index(workspace: &Workspace, tx_dir: &Path) -> Result<Option<PathBuf>> {
    let index_path = workspace.index_path();
    if !index_path.exists() {
        return Ok(None);
    }

    let target = tx_dir.join("index.sqlite.snapshot");
    fs::copy(&index_path, &target).wrap_err_with(|| {
        format!(
            "failed to snapshot {} to {}",
            index_path.display(),
            target.display()
        )
    })?;
    Ok(Some(target))
}

fn restore_snapshot(
    root: &Path,
    snapshot_dir: &Path,
    snapshot_files: &BTreeSet<String>,
) -> Result<()> {
    for file in collect_files(root)? {
        let relative = file
            .strip_prefix(root)
            .wrap_err_with(|| format!("failed to relativize {}", file.display()))?;
        if is_excluded_relative(relative) {
            continue;
        }
        let label = path_label(relative);
        if !snapshot_files.contains(&label) {
            fs::remove_file(&file)
                .wrap_err_with(|| format!("failed to remove {}", file.display()))?;
        }
    }

    for label in snapshot_files {
        let source = snapshot_dir.join(label);
        let target = root.join(label);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(&source, &target).wrap_err_with(|| {
            format!(
                "failed to restore {} from {}",
                target.display(),
                source.display()
            )
        })?;
    }

    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files_into(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_into(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(dir).wrap_err_with(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        let relative = path
            .strip_prefix(root)
            .wrap_err_with(|| format!("failed to relativize {}", path.display()))?;
        if is_excluded_relative(relative) {
            continue;
        }
        if path.is_dir() {
            collect_files_into(root, &path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn staged_paths(root: &Path) -> Result<Vec<String>> {
    let output = git_output(root, &["diff", "--cached", "--name-only", "-z"])?;
    if !output.status.success() {
        return Err(git_error("git diff --cached", &output));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .filter(|path| !is_excluded_relative_str(path))
        .collect())
}

fn unstage_excluded(root: &Path) -> Result<()> {
    let output = git_output(root, &["reset", "-q", "--", ".rem/cache", ".rem/tx"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error("git reset excluded paths", &output))
    }
}

fn unstage_all(root: &Path) -> Result<()> {
    let output = git_output(root, &["reset", "-q", "--", "."])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error("git reset", &output))
    }
}

fn fail_if_pending_transactions(workspace: &Workspace) -> Result<()> {
    let pending = pending_transactions(workspace)?;
    if pending.is_empty() {
        return Ok(());
    }

    let paths = pending
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(eyre!(
        "transaction recovery pending in {}; inspect or remove stale journals before committing",
        paths
    ))
}

fn gitignore_has_entry(raw: &str, entry: &str) -> bool {
    let normalized = entry.trim_end_matches('/');
    raw.lines().any(|line| {
        let line = line.trim().trim_end_matches('/');
        line == normalized
    })
}

fn parse_status_z(raw: &[u8]) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());

    while let Some(part) = parts.next() {
        let text = String::from_utf8_lossy(part);
        if text.len() < 4 {
            continue;
        }
        let code = text[0..2].to_string();
        let mut path = text[3..].to_string();
        if matches!(code.as_bytes().first(), Some(b'R' | b'C')) {
            if let Some(next) = parts.next() {
                path = String::from_utf8_lossy(next).to_string();
            }
        }
        if !path.is_empty() {
            entries.push(GitStatusEntry { code, path });
        }
    }

    entries
}

fn is_excluded_relative_str(path: &str) -> bool {
    is_excluded_relative(Path::new(path))
}

fn path_label(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        return Err(git_error(&format!("git {}", args.join(" ")), &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_checked(root: &Path, args: &[&str], label: &str) -> Result<()> {
    let output = git_output(root, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(label, &output))
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .wrap_err_with(|| format!("failed to run git {}", args.join(" ")))
}

fn git_error(label: &str, output: &Output) -> color_eyre::Report {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stderr.is_empty() && stdout.is_empty() {
        eyre!("{label} failed with status {}", output.status)
    } else if stderr.is_empty() {
        eyre!("{label} failed: {stdout}")
    } else {
        eyre!("{label} failed: {stderr}")
    }
}

fn tx_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("tx-{nanos}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_github_and_gitlab_remotes() {
        assert!(is_github_or_gitlab_remote(
            "https://github.com/acme/rem.git"
        ));
        assert!(is_github_or_gitlab_remote("git@gitlab.com:acme/rem.git"));
        assert!(!is_github_or_gitlab_remote(
            "https://example.com/acme/rem.git"
        ));
    }

    #[test]
    fn excludes_cache_tx_and_git_paths() {
        assert!(is_excluded_relative(Path::new(".rem/cache/index.sqlite")));
        assert!(is_excluded_relative(Path::new(".rem/tx/tx-1/snapshot")));
        assert!(is_excluded_relative(Path::new(".git/config")));
        assert!(!is_excluded_relative(Path::new(".rem/config.toml")));
        assert!(!is_excluded_relative(Path::new("memories/short/a.md")));
    }

    #[test]
    fn parses_porcelain_z_status() {
        let parsed = parse_status_z(b" M memories/short/a.md\0?? notes.md\0");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].code, " M");
        assert_eq!(parsed[0].path, "memories/short/a.md");
        assert_eq!(parsed[1].code, "??");
        assert_eq!(parsed[1].path, "notes.md");
    }
}
