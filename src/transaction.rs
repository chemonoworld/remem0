use std::{
    collections::BTreeSet,
    fs,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{Result, WrapErr, eyre};

use crate::{
    index,
    output::{self, Tone},
    workspace::{self, Workspace},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalChangePolicy {
    Prompt,
    Accept,
    Restore,
    Review,
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
    pub original_path: Option<String>,
}

impl GitStatusEntry {
    fn is_untracked(&self) -> bool {
        self.code == "??"
    }

    fn is_unmerged(&self) -> bool {
        matches!(
            self.code.as_str(),
            "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU"
        )
    }

    fn kind(&self) -> GitStatusKind {
        if self.is_unmerged() {
            return GitStatusKind::Unmerged;
        }
        if self.is_untracked() {
            return GitStatusKind::Untracked;
        }

        let mut chars = self.code.chars();
        let index = chars.next().unwrap_or(' ');
        let worktree = chars.next().unwrap_or(' ');
        match (index, worktree) {
            ('R', _) | (_, 'R') => GitStatusKind::Renamed,
            ('C', _) | (_, 'C') => GitStatusKind::Copied,
            ('A', _) | (_, 'A') => GitStatusKind::Added,
            ('D', _) | (_, 'D') => GitStatusKind::Deleted,
            _ => GitStatusKind::Modified,
        }
    }

    fn path_args(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        if let Some(original_path) = &self.original_path {
            paths.push(original_path.as_str());
        }
        paths.push(self.path.as_str());
        paths
    }

    fn display_path(&self) -> String {
        if let Some(original_path) = &self.original_path {
            format!("{original_path} -> {}", self.path)
        } else {
            self.path.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatusKind {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    Unmerged,
    Untracked,
}

impl GitStatusKind {
    fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Copied => "copied",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Unmerged => "unmerged",
            Self::Untracked => "untracked",
        }
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
    let _lock = TransactionLock::acquire(workspace)?;
    fail_if_pending_transaction_journals(workspace)?;
    workspace.ensure_layout()?;
    validate_gitignore_path(workspace)?;

    let changed_paths = git_status(workspace.root())?
        .into_iter()
        .collect::<Vec<_>>();
    fail_if_unmerged(&changed_paths)?;
    let changed_paths = changed_paths
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

pub fn rebuild_index(workspace: &Workspace) -> Result<index::RebuildReport> {
    validate_git_vault(workspace.root())?;
    let _lock = TransactionLock::acquire(workspace)?;
    fail_if_pending_transaction_journals(workspace)?;
    workspace.ensure_layout()?;
    index::rebuild(workspace)
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
        if path.is_dir() || path.file_name().is_some_and(|name| name == "active.lock") {
            pending.push(path);
        }
    }
    pending.sort();
    Ok(pending)
}

pub fn ensure_gitignore(workspace: &Workspace) -> Result<bool> {
    let path = workspace.root().join(".gitignore");
    validate_gitignore_path(workspace)?;
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

fn validate_gitignore_path(workspace: &Workspace) -> Result<()> {
    let path = workspace.root().join(".gitignore");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(eyre!(
                "refusing to update symlinked .gitignore {}; replace it with a regular vault file first",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("failed to inspect {}", path.display()));
        }
    }
    Ok(())
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
        .filter(|entry| {
            !is_excluded_relative_str(&entry.path)
                && entry
                    .original_path
                    .as_deref()
                    .is_none_or(|path| !is_excluded_relative_str(path))
        })
        .collect())
}

pub fn is_github_or_gitlab_remote(remote: &str) -> bool {
    git_remote_host(remote)
        .as_deref()
        .is_some_and(|host| matches!(host, "github.com" | "gitlab.com"))
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
struct TransactionLock {
    path: PathBuf,
    owner: String,
    released: bool,
}

impl TransactionLock {
    fn acquire(workspace: &Workspace) -> Result<Self> {
        ensure_transaction_root(workspace)?;
        let path = workspace.tx_dir().join("active.lock");
        let owner = tx_id();
        let mut file = match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                return Err(eyre!(
                    "another rem transaction is active or a stale lock remains at {}; run `rem doctor` and remove the lock only after confirming no rem process is running",
                    path.display()
                ));
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("failed to acquire transaction lock {}", path.display())
                });
            }
        };

        let mut lock = Self {
            path,
            owner,
            released: false,
        };
        if let Err(err) = writeln!(file, "{}", lock.owner).and_then(|_| file.sync_all()) {
            let _ = lock.release();
            return Err(err).wrap_err("failed to persist transaction lock ownership");
        }
        Ok(lock)
    }

    fn release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        match fs::read_to_string(&self.path) {
            Ok(owner) if owner.trim() == self.owner => {
                fs::remove_file(&self.path).wrap_err_with(|| {
                    format!("failed to release transaction lock {}", self.path.display())
                })?;
            }
            Ok(_) => {
                return Err(eyre!(
                    "transaction lock ownership changed at {}; refusing to remove another process's lock",
                    self.path.display()
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("failed to inspect transaction lock {}", self.path.display())
                });
            }
        }
        self.released = true;
        Ok(())
    }
}

impl Drop for TransactionLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[derive(Debug)]
struct Transaction {
    lock: TransactionLock,
    workspace: Workspace,
    tx_dir: PathBuf,
    snapshot_dir: PathBuf,
    snapshot_files: BTreeSet<String>,
    index_snapshot: Option<PathBuf>,
    staging_snapshot: Option<PathBuf>,
    temp_index: Option<PathBuf>,
    head_before: Option<String>,
    symbolic_head_before: Option<String>,
    commit_created: bool,
    finished: bool,
}

impl Transaction {
    fn begin(workspace: &Workspace, options: TransactionOptions) -> Result<Self> {
        validate_git_vault(workspace.root())?;
        let lock = TransactionLock::acquire(workspace)?;
        fail_if_pending_transaction_journals(workspace)?;
        workspace.ensure_layout()?;

        let external = git_status(workspace.root())?;
        resolve_external_changes(workspace.root(), &external, options)?;

        let tx_id = tx_id();
        let tx_dir = workspace.tx_dir().join(&tx_id);
        let snapshot_dir = tx_dir.join("snapshot");
        fs::create_dir_all(&snapshot_dir)
            .wrap_err_with(|| format!("failed to create {}", snapshot_dir.display()))?;

        let snapshot_files = snapshot_vault(workspace.root(), &snapshot_dir)?;
        let index_snapshot = snapshot_index(workspace, &tx_dir)?;
        let staging_snapshot = snapshot_staging(workspace.root(), &tx_dir)?;
        let head_before = current_head(workspace.root())?;
        let symbolic_head_before = symbolic_head(workspace.root())?;

        Ok(Self {
            lock,
            workspace: workspace.clone(),
            tx_dir,
            snapshot_dir,
            snapshot_files,
            index_snapshot,
            staging_snapshot,
            temp_index: None,
            head_before,
            symbolic_head_before,
            commit_created: false,
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
            let diagnostics = index::diagnostic_messages(&temp_path)?;
            let _ = fs::remove_file(&temp_path);
            self.temp_index = None;
            let details = diagnostics.join("\n");
            return Err(eyre!(
                "reindex produced {} diagnostics; fix malformed or duplicate memory files before committing:\n{}",
                report.diagnostics,
                details
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
        for entry in &status {
            args.extend(entry.path_args());
        }
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
        let head_after = current_head(self.workspace.root())?;
        self.commit_created = head_after != self.head_before;
        if !output.status.success() {
            return Err(git_error("git commit", &output));
        }

        let remaining = git_status(self.workspace.root())?;
        if !remaining.is_empty() {
            let paths = remaining
                .iter()
                .map(GitStatusEntry::display_path)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(eyre!(
                "Git hooks changed vault files after commit ({paths}); transaction will roll back"
            ));
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

        if let Err(err) = self.restore_head() {
            first_error.get_or_insert(err);
        }
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
        if let Err(err) = self.restore_staging() {
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

    fn restore_head(&mut self) -> Result<()> {
        if !self.commit_created {
            return Ok(());
        }

        if let Some(head) = &self.head_before {
            git_checked(
                self.workspace.root(),
                &["reset", "--mixed", head],
                "git reset transaction commit",
            )?;
        } else {
            let reference = self
                .symbolic_head_before
                .as_deref()
                .ok_or_else(|| eyre!("cannot restore unborn Git HEAD without a symbolic branch"))?;
            git_checked(
                self.workspace.root(),
                &["update-ref", "-d", reference],
                "git delete transaction commit",
            )?;
            git_checked(
                self.workspace.root(),
                &["read-tree", "--empty"],
                "git restore unborn index",
            )?;
        }
        self.commit_created = false;
        Ok(())
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

    fn restore_staging(&self) -> Result<()> {
        let Some(snapshot) = &self.staging_snapshot else {
            return Ok(());
        };
        let snapshot = snapshot
            .to_str()
            .ok_or_else(|| eyre!("staging snapshot path is not valid UTF-8"))?;
        git_checked(
            self.workspace.root(),
            &["apply", "--cached", "--whitespace=nowarn", snapshot],
            "git apply staged snapshot",
        )
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.tx_dir.exists() {
            fs::remove_dir_all(&self.tx_dir)
                .wrap_err_with(|| format!("failed to remove {}", self.tx_dir.display()))?;
        }
        self.lock.release()?;
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
    fail_if_unmerged(entries)?;

    let action = match options.external_policy {
        ExternalChangePolicy::Accept => ExternalChangePolicy::Accept,
        ExternalChangePolicy::Restore => ExternalChangePolicy::Restore,
        ExternalChangePolicy::Review => ExternalChangePolicy::Review,
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
        ExternalChangePolicy::Review => review_external_changes(root, entries),
        ExternalChangePolicy::Abort | ExternalChangePolicy::Prompt => {
            Err(eyre!("aborted due to external vault changes"))
        }
    }
}

fn fail_if_unmerged(entries: &[GitStatusEntry]) -> Result<()> {
    let unmerged = entries
        .iter()
        .filter(|entry| entry.is_unmerged())
        .collect::<Vec<_>>();
    if unmerged.is_empty() {
        return Ok(());
    }

    let paths = unmerged
        .iter()
        .map(|entry| format!("{}\t{}", entry.code, entry.display_path()))
        .collect::<Vec<_>>()
        .join("\n");
    Err(eyre!(
        "unmerged Git conflict detected; resolve with Git/editor tooling, then rerun `rem commit --review`:\n{paths}"
    ))
}

fn prompt_external_changes(entries: &[GitStatusEntry]) -> Result<ExternalChangePolicy> {
    output::line(output::paint(
        "external vault changes detected:",
        Tone::Warning,
    ));
    for entry in entries {
        let kind = entry.kind().label();
        output::line(output::row([
            (kind.to_string(), output::change_tone(kind)),
            (entry.code.clone(), Tone::Muted),
            (entry.display_path(), Tone::Path),
        ]));
    }
    output::prompt("choose commit / restore / abort [commit]: ")?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim() {
        "" | "commit" | "c" => Ok(ExternalChangePolicy::Accept),
        "restore" | "r" => Ok(ExternalChangePolicy::Restore),
        "abort" | "a" => Ok(ExternalChangePolicy::Abort),
        other => Err(eyre!("unknown external-change choice {other:?}")),
    }
}

fn review_external_changes(root: &Path, entries: &[GitStatusEntry]) -> Result<()> {
    loop {
        print_review_summary(entries);
        output::prompt("choose commit-all / pick / diff / restore-all / abort [commit-all]: ")?;

        let answer = read_choice()?;
        match answer.as_str() {
            "" | "commit-all" | "commit" | "c" => return Ok(()),
            "restore-all" | "restore" | "r" => return restore_external_changes(root, entries),
            "pick" | "p" => return review_each_file(root, entries),
            "diff" | "d" => {
                for entry in entries {
                    print_entry_diff(root, entry)?;
                }
            }
            "abort" | "a" => return Err(eyre!("aborted due to external vault changes")),
            other => output::line(output::paint(
                format!("unknown choice {other:?}"),
                Tone::Warning,
            )),
        }
    }
}

fn print_review_summary(entries: &[GitStatusEntry]) {
    output::line(output::paint(
        "external Git changes detected:",
        Tone::Warning,
    ));
    for kind in [
        GitStatusKind::Modified,
        GitStatusKind::Added,
        GitStatusKind::Deleted,
        GitStatusKind::Renamed,
        GitStatusKind::Copied,
        GitStatusKind::Untracked,
    ] {
        let matching = entries
            .iter()
            .filter(|entry| entry.kind() == kind)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }

        output::line(format!(
            "{}:",
            output::paint(kind.label(), output::change_tone(kind.label()))
        ));
        for entry in matching {
            output::line(format!(
                "  {}\t{}",
                output::paint(&entry.code, Tone::Muted),
                output::paint(entry.display_path(), Tone::Path)
            ));
        }
    }
}

fn review_each_file(root: &Path, entries: &[GitStatusEntry]) -> Result<()> {
    for entry in entries {
        loop {
            let kind = entry.kind().label();
            output::line(output::row([
                (kind.to_string(), output::change_tone(kind)),
                (entry.code.clone(), Tone::Muted),
                (entry.display_path(), Tone::Path),
            ]));
            output::prompt("choose include / restore / diff / abort [include]: ")?;

            let answer = read_choice()?;
            match answer.as_str() {
                "" | "include" | "i" => break,
                "restore" | "r" => {
                    restore_external_entry(root, entry)?;
                    break;
                }
                "diff" | "d" => print_entry_diff(root, entry)?,
                "abort" | "a" => return Err(eyre!("aborted due to external vault changes")),
                other => output::line(output::paint(
                    format!("unknown choice {other:?}"),
                    Tone::Warning,
                )),
            }
        }
    }

    Ok(())
}

fn print_entry_diff(root: &Path, entry: &GitStatusEntry) -> Result<()> {
    output::diff(&format!("diff -- {}\n", entry.path));
    if entry.is_untracked() {
        let path = root.join(&entry.path);
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("<binary or unreadable file: {}>", path.display()));
        let mut diff = format!("--- /dev/null\n+++ {}\n", entry.path);
        for line in raw.lines() {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
        output::diff(&diff);
        return Ok(());
    }

    let cached = git_output(root, &["diff", "--cached", "--", &entry.path])?;
    if !cached.status.success() {
        return Err(git_error("git diff --cached", &cached));
    }
    if !cached.stdout.is_empty() {
        output::diff(&String::from_utf8_lossy(&cached.stdout));
    }

    let worktree = git_output(root, &["diff", "--", &entry.path])?;
    if !worktree.status.success() {
        return Err(git_error("git diff", &worktree));
    }
    if !worktree.stdout.is_empty() {
        output::diff(&String::from_utf8_lossy(&worktree.stdout));
    }
    Ok(())
}

fn read_choice() -> Result<String> {
    let mut answer = String::new();
    let bytes = io::stdin().read_line(&mut answer)?;
    if bytes == 0 {
        return Err(eyre!(
            "interactive review input ended before a choice was made"
        ));
    }
    Ok(answer.trim().to_ascii_lowercase())
}

fn restore_external_changes(root: &Path, entries: &[GitStatusEntry]) -> Result<()> {
    for entry in entries {
        restore_external_entry(root, entry)?;
    }
    Ok(())
}

fn restore_external_entry(root: &Path, entry: &GitStatusEntry) -> Result<()> {
    let path = root.join(&entry.path);
    if entry.is_untracked() {
        remove_path_if_exists(&path)?;
    } else {
        let mut args = vec!["restore", "--staged", "--worktree", "--"];
        args.extend(entry.path_args());
        git_checked(root, &args, "git restore")?;
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
                .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
        }
        Ok(_) => {
            fs::remove_file(path)
                .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("failed to inspect {}", path.display()));
        }
    }
    Ok(())
}

fn copy_snapshot_entry(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .wrap_err_with(|| format!("failed to inspect {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        let link_target = fs::read_link(source)
            .wrap_err_with(|| format!("failed to read symlink {}", source.display()))?;
        create_symlink(source, &link_target, target)?;
    } else {
        fs::copy(source, target).wrap_err_with(|| {
            format!(
                "failed to copy snapshot entry {} to {}",
                source.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

fn restore_snapshot_entry(source: &Path, target: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source)
        .wrap_err_with(|| format!("failed to inspect snapshot {}", source.display()))?;
    let source_is_symlink = source_metadata.file_type().is_symlink();
    if source_is_symlink || path_is_symlink(target)? {
        remove_path_if_exists(target)?;
    }
    copy_snapshot_entry(source, target)
}

fn path_is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).wrap_err_with(|| format!("failed to inspect {}", path.display())),
    }
}

#[cfg(unix)]
fn create_symlink(source: &Path, link_target: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link_target, target).wrap_err_with(|| {
        format!(
            "failed to snapshot symlink {} to {}",
            source.display(),
            target.display()
        )
    })
}

#[cfg(windows)]
fn create_symlink(source: &Path, link_target: &Path, target: &Path) -> Result<()> {
    let result = if source.is_dir() {
        std::os::windows::fs::symlink_dir(link_target, target)
    } else {
        std::os::windows::fs::symlink_file(link_target, target)
    };
    result.wrap_err_with(|| {
        format!(
            "failed to snapshot symlink {} to {}",
            source.display(),
            target.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(source: &Path, _link_target: &Path, target: &Path) -> Result<()> {
    Err(eyre!(
        "cannot snapshot symlink {} to {} on this platform",
        source.display(),
        target.display()
    ))
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
        copy_snapshot_entry(&file, &target)?;
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

fn snapshot_staging(root: &Path, tx_dir: &Path) -> Result<Option<PathBuf>> {
    let output = git_output(root, &["diff", "--cached", "--binary", "--full-index"])?;
    if !output.status.success() {
        return Err(git_error("git diff --cached", &output));
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }

    let target = tx_dir.join("staged.patch");
    fs::write(&target, output.stdout)
        .wrap_err_with(|| format!("failed to write {}", target.display()))?;
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
            remove_path_if_exists(&file)?;
        }
    }

    for label in snapshot_files {
        let source = snapshot_dir.join(label);
        let target = root.join(label);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        restore_snapshot_entry(&source, &target)?;
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
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            files.push(path);
        } else if metadata.is_dir() {
            collect_files_into(root, &path, files)?;
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

fn fail_if_pending_transaction_journals(workspace: &Workspace) -> Result<()> {
    let pending = pending_transaction_journals(workspace)?;
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

fn pending_transaction_journals(workspace: &Workspace) -> Result<Vec<PathBuf>> {
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

fn ensure_transaction_root(workspace: &Workspace) -> Result<()> {
    workspace::ensure_regular_directory(&workspace.rem_dir(), "rem metadata")?;
    workspace::ensure_regular_directory(&workspace.tx_dir(), "transaction")
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
        let path = text[3..].to_string();
        let mut original_path = None;
        if matches!(code.as_bytes().first(), Some(b'R' | b'C'))
            && let Some(next) = parts.next()
        {
            original_path = Some(String::from_utf8_lossy(next).to_string());
        }
        if !path.is_empty() {
            entries.push(GitStatusEntry {
                code,
                path,
                original_path,
            });
        }
    }

    entries
}

fn git_remote_host(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }

    let host = if let Some((_, rest)) = remote.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        authority.rsplit('@').next().unwrap_or_default()
    } else if let Some((left, _)) = remote.split_once(':') {
        if left.contains('/') {
            return None;
        }
        left.rsplit('@').next().unwrap_or_default()
    } else {
        return None;
    };

    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
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

fn current_head(root: &Path) -> Result<Option<String>> {
    let output = git_output(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(git_error("git rev-parse HEAD", &output))
    }
}

fn symbolic_head(root: &Path) -> Result<Option<String>> {
    let output = git_output(root, &["symbolic-ref", "--quiet", "HEAD"])?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(git_error("git symbolic-ref HEAD", &output))
    }
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
        assert!(is_github_or_gitlab_remote(
            "ssh://git@github.com:22/acme/rem.git"
        ));
        assert!(!is_github_or_gitlab_remote(
            "https://example.com/acme/rem.git"
        ));
        assert!(!is_github_or_gitlab_remote(
            "https://github.com.evil/acme/rem.git"
        ));
        assert!(!is_github_or_gitlab_remote(
            "git@evilgithub.com:acme/rem.git"
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
        assert_eq!(parsed[0].original_path, None);
        assert_eq!(parsed[1].code, "??");
        assert_eq!(parsed[1].path, "notes.md");
        assert_eq!(parsed[1].original_path, None);
    }

    #[test]
    fn parses_rename_porcelain_z_status() {
        let parsed = parse_status_z(b"R  new.md\0old.md\0");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].code, "R ");
        assert_eq!(parsed[0].path, "new.md");
        assert_eq!(parsed[0].original_path, Some("old.md".to_string()));
        assert_eq!(parsed[0].display_path(), "old.md -> new.md");
    }

    #[test]
    fn parses_copy_porcelain_z_status() {
        let parsed = parse_status_z(b"C  copy.md\0source.md\0");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].code, "C ");
        assert_eq!(parsed[0].path, "copy.md");
        assert_eq!(parsed[0].original_path, Some("source.md".to_string()));
        assert_eq!(parsed[0].display_path(), "source.md -> copy.md");
    }

    #[test]
    fn classifies_git_status_entries() {
        assert_eq!(
            GitStatusEntry {
                code: " M".to_string(),
                path: "a.md".to_string(),
                original_path: None,
            }
            .kind(),
            GitStatusKind::Modified
        );
        assert_eq!(
            GitStatusEntry {
                code: "??".to_string(),
                path: "a.md".to_string(),
                original_path: None,
            }
            .kind(),
            GitStatusKind::Untracked
        );
        assert_eq!(
            GitStatusEntry {
                code: "UU".to_string(),
                path: "a.md".to_string(),
                original_path: None,
            }
            .kind(),
            GitStatusKind::Unmerged
        );
    }
}
