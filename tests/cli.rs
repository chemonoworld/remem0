use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct TempProject {
    root: PathBuf,
    rem_home: PathBuf,
    vault: PathBuf,
}

impl TempProject {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rem-cli-{name}-{nonce}"));
        let rem_home = root.join("home");
        let vault = root.join("vault");
        fs::create_dir_all(&root).unwrap();

        Self {
            root,
            rem_home,
            vault,
        }
    }

    fn rem(&self, args: &[&str]) -> Output {
        self.rem_in(args, &self.root)
    }

    fn rem_in(&self, args: &[&str], cwd: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rem"))
            .env("REM_HOME", &self.rem_home)
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap()
    }

    fn rem_ok(&self, args: &[&str]) -> String {
        self.rem_ok_in(args, &self.root)
    }

    fn rem_ok_in(&self, args: &[&str], cwd: &Path) -> String {
        let output = self.rem_in(args, cwd);
        if !output.status.success() {
            panic!(
                "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout).unwrap()
    }

    fn rem_err(&self, args: &[&str]) -> String {
        let output = self.rem(args);
        assert!(
            !output.status.success(),
            "command unexpectedly succeeded: {:?}",
            args
        );
        String::from_utf8_lossy(&output.stderr).to_string()
    }

    fn rem_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rem"))
            .env("REM_HOME", &self.rem_home)
            .current_dir(&self.root)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn rem_ok_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        let output = self.rem_with_stdin(args, stdin);
        if !output.status.success() {
            panic!(
                "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout).unwrap()
    }

    fn git_in(args: &[&str], cwd: &Path) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap()
    }

    fn git_ok(&self, args: &[&str]) -> String {
        Self::git_ok_in(args, &self.vault)
    }

    fn git_ok_in(args: &[&str], cwd: &Path) -> String {
        let output = Self::git_in(args, cwd);
        if !output.status.success() {
            panic!(
                "git failed: {:?}\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout).unwrap()
    }

    fn init_git_vault(&self) {
        Self::init_git_repo(&self.vault);
    }

    fn init_git_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        Self::git_ok_in(&["init"], path);
        Self::git_ok_in(
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/rem-test.git",
            ],
            path,
        );
    }

    fn init_rem(&self, storage: &str) {
        self.init_git_vault();
        self.rem_ok(&[
            "init",
            "--root",
            path_str(&self.vault),
            "--storage",
            storage,
        ]);
        assert_git_clean(self);
    }

    fn head(&self) -> String {
        self.git_ok(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn last_commit_subject(&self) -> String {
        self.git_ok(&["log", "-1", "--pretty=%s"])
            .trim()
            .to_string()
    }

    fn tracked_files(&self) -> String {
        self.git_ok(&["ls-tree", "-r", "--name-only", "HEAD"])
    }

    fn status_short(&self) -> String {
        self.git_ok(&["status", "--short"])
    }

    fn git_commit_all(&self, message: &str) {
        self.git_ok(&["add", "--all"]);
        self.git_ok(&[
            "-c",
            "user.name=rem-test",
            "-c",
            "user.email=rem-test@example.invalid",
            "commit",
            "-m",
            message,
        ]);
    }

    fn create_unmerged_conflict(&self) {
        let conflict = self.vault.join("conflict.md");
        fs::write(&conflict, "base\n").unwrap();
        self.git_commit_all("base conflict fixture");

        let base_branch = self.git_ok(&["branch", "--show-current"]);
        let base_branch = base_branch.trim();
        self.git_ok(&["switch", "-c", "other"]);
        fs::write(&conflict, "other\n").unwrap();
        self.git_commit_all("other conflict fixture");

        self.git_ok(&["switch", base_branch]);
        fs::write(&conflict, "main\n").unwrap();
        self.git_commit_all("main conflict fixture");

        let merge = Self::git_in(&["merge", "other"], &self.vault);
        assert!(
            !merge.status.success(),
            "merge unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn init_requires_git_repository_and_supported_origin() {
    let project = TempProject::new("init-git-required");
    let error = project.rem_err(&["init", "--root", path_str(&project.vault)]);
    assert!(error.contains("Git working tree"));

    fs::create_dir_all(&project.vault).unwrap();
    TempProject::git_ok_in(&["init"], &project.vault);
    let error = project.rem_err(&["init", "--root", path_str(&project.vault)]);
    assert!(error.contains("remote origin"));

    TempProject::git_ok_in(
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/acme/rem.git",
        ],
        &project.vault,
    );
    let error = project.rem_err(&["init", "--root", path_str(&project.vault)]);
    assert!(error.contains("GitHub or GitLab"));

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("no profiles configured"));
}

#[test]
fn init_add_list_show_update_delete_flow() {
    let project = TempProject::new("crud");
    project.init_rem("local");

    assert!(project.vault.join("memories/short").is_dir());
    assert!(project.vault.join("memories/long").is_dir());
    assert!(project.vault.join("policies/short.md").is_file());
    assert!(project.tracked_files().contains(".gitignore"));
    assert!(project.tracked_files().contains("policies/short.md"));

    let added = project.rem_ok(&[
        "add",
        "--short",
        "--tag",
        "rust",
        "--scope",
        "project",
        "--kind",
        "decision",
        "# Rust Decision\nUse Markdown as canonical memory.",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: add short memory {id}")
    );

    let listed = project.rem_ok(&["list", "--short"]);
    assert!(listed.contains(&id));
    assert!(listed.contains("Rust Decision"));

    let shown = project.rem_ok(&["show", &id]);
    assert!(shown.contains("type: short"));
    assert!(shown.contains("Use Markdown as canonical memory."));

    project.rem_ok(&["update", &id, "# Updated\nUse SQLite only as cache."]);
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: update memory {id}")
    );
    let updated = project.rem_ok(&["show", &id]);
    assert!(updated.contains("Use SQLite only as cache."));

    project.rem_ok(&["delete", &id]);
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: archive memory {id}")
    );
    assert!(
        project
            .vault
            .join("archive")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    assert_git_clean(&project);
}

#[test]
fn init_can_accept_preexisting_external_files() {
    let project = TempProject::new("init-accept-external");
    project.init_git_vault();
    fs::write(project.vault.join("README.md"), "# Existing vault\n").unwrap();

    project.rem_ok(&[
        "init",
        "--root",
        path_str(&project.vault),
        "--accept-external",
    ]);

    assert!(project.tracked_files().contains("README.md"));
    assert_git_clean(&project);
}

#[test]
fn init_can_restore_preexisting_external_files() {
    let project = TempProject::new("init-restore-external");
    project.init_git_vault();
    fs::write(project.vault.join("scratch.md"), "# Scratch\n").unwrap();

    project.rem_ok(&[
        "init",
        "--root",
        path_str(&project.vault),
        "--restore-external",
    ]);

    assert!(!project.vault.join("scratch.md").exists());
    assert!(!project.tracked_files().contains("scratch.md"));
    assert_git_clean(&project);
}

#[test]
fn promote_rebuild_search_and_doctor_flow() {
    let project = TempProject::new("search");
    project.init_rem("git");
    let added = project.rem_ok(&[
        "add",
        "--short",
        "--tag",
        "sqlite",
        "# Search Design\nSQLite FTS5 provides BM25 ranking.",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();

    let grep = project.rem_ok(&["search", "--grep", "BM25"]);
    assert!(grep.contains(&id));

    let promoted = project.rem_ok(&["promote", &id]);
    let long_id = promoted.split_whitespace().last().unwrap().to_string();
    assert_ne!(id, long_id);
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: promote memory {id} to {long_id}")
    );

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("indexed=2"));
    assert!(project.vault.join(".rem/cache/index.sqlite").is_file());

    let bm25 = project.rem_ok(&["search", "--bm25", "SQLite"]);
    assert!(bm25.contains("Search Design"));
    assert!(!bm25.contains("-0.000"));

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("active_profile: default"));
    assert!(doctor.contains("index diagnostics are clean"));
    assert!(doctor.contains(".gitignore excludes .rem/cache/"));
    assert!(doctor.contains(".gitignore excludes .rem/tx/"));
    assert!(doctor.contains("Git remote origin is supported"));

    let vector = project.rem_err(&["search", "--vector", "SQLite"]);
    assert!(vector.contains("vector search is not configured"));
    assert!(!vector.contains("Location:"));
}

#[test]
fn profile_commands_manage_multiple_vaults() {
    let project = TempProject::new("profiles");
    let second = project.root.join("second-vault");
    project.init_git_vault();
    TempProject::init_git_repo(&second);

    project.rem_ok(&[
        "profile",
        "add",
        "alpha",
        path_str(&project.vault),
        "--storage",
        "obsidian",
    ]);
    project.rem_ok(&[
        "profile",
        "add",
        "beta",
        path_str(&second),
        "--storage",
        "local",
    ]);
    project.rem_ok(&["profile", "use", "beta"]);
    let shown = project.rem_ok(&["profile", "show"]);

    assert!(shown.contains("name = beta"));
    assert!(shown.contains(path_str(&second)));
}

#[test]
fn doctor_without_profile_reports_actionable_warning() {
    let project = TempProject::new("doctor-empty");
    let output = project.rem_ok(&["doctor"]);

    assert!(output.contains("no profiles configured"));
    assert!(output.contains("rem init --root <path>"));
}

#[test]
fn expected_user_errors_are_concise() {
    let project = TempProject::new("concise-errors");
    project.init_rem("local");
    let error = project.rem_err(&["add", "--short"]);

    assert!(error.contains("memory body cannot be empty"));
    assert!(!error.contains("Location:"));
    assert!(!error.contains("Backtrace"));
}

#[test]
fn malformed_memory_does_not_block_valid_memory_commands() {
    let project = TempProject::new("malformed-memory");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Healthy\nalpha survives bad neighbor"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    fs::write(
        project.vault.join("memories/short/bad.md"),
        "this is not a rem memory",
    )
    .unwrap();

    let listed = project.rem_ok(&["list"]);
    assert!(listed.contains(&id));

    let shown = project.rem_ok(&["show", &id]);
    assert!(shown.contains("alpha survives bad neighbor"));

    let grep = project.rem_ok(&["search", "--grep", "alpha"]);
    assert!(grep.contains(&id));

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("indexed=1"));
    assert!(rebuild.contains("diagnostics=1"));
}

#[test]
fn duplicate_memory_ids_are_index_diagnostics_not_rebuild_failures() {
    let project = TempProject::new("duplicate-ids");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Duplicate\nsame id should diagnose"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    fs::copy(
        project.vault.join(format!("memories/short/{id}.md")),
        project.vault.join("memories/long/duplicate.md"),
    )
    .unwrap();

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("indexed=1"));
    assert!(rebuild.contains("diagnostics=1"));

    let bm25 = project.rem_ok(&["search", "--bm25", "Duplicate"]);
    assert!(bm25.contains(&id));
}

#[test]
fn bm25_punctuation_only_query_returns_empty_success() {
    let project = TempProject::new("bm25-punctuation");
    project.init_rem("local");
    project.rem_ok(&["add", "--short", "# Punctuation\nSearch works"]);
    project.rem_ok(&["rebuild"]);

    let output = project.rem_ok(&["search", "--bm25", "!!!"]);
    assert!(output.trim().is_empty());
}

#[test]
fn bm25_multi_term_query_requires_all_terms() {
    let project = TempProject::new("bm25-all-terms");
    project.init_rem("local");
    project.rem_ok(&["add", "--short", "# Alpha Beta\nalpha beta"]);
    project.rem_ok(&["add", "--short", "# Alpha Gamma\nalpha gamma"]);
    project.rem_ok(&["rebuild"]);

    let output = project.rem_ok(&["search", "--bm25", "alpha beta"]);
    assert!(output.contains("Alpha Beta"));
    assert!(!output.contains("Alpha Gamma"));
}

#[test]
fn relative_profile_roots_are_stored_as_absolute_paths() {
    let project = TempProject::new("relative-root");
    let work = project.root.join("work");
    let elsewhere = project.root.join("elsewhere");
    let relative_vault = work.join("rel-vault");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    TempProject::init_git_repo(&relative_vault);

    project.rem_ok_in(
        &["init", "--root", "rel-vault", "--storage", "local"],
        &work,
    );
    project.rem_ok_in(
        &["add", "--short", "# Relative\nroot stays stable"],
        &elsewhere,
    );

    let shown = project.rem_ok_in(&["profile", "show"], &elsewhere);
    assert!(shown.contains(path_str(&relative_vault)));

    let listed = project.rem_ok_in(&["list"], &elsewhere);
    assert!(listed.contains("Relative"));
}

#[test]
fn default_search_config_is_honored() {
    let project = TempProject::new("default-search");
    project.init_rem("local");
    project.rem_ok(&["add", "--short", "# Config Mode\nneedle-config-mode"]);
    project.rem_ok(&["rebuild"]);
    project.rem_ok(&["config", "set", "default-search", "bm25"]);

    let output = project.rem_ok(&["search", "needle-config-mode"]);
    assert!(output.contains("\tbm25\t"));
    assert!(!output.contains("\tgrep,bm25\t"));

    let error = project.rem_err(&["config", "set", "default-search", "nonsense"]);
    assert!(error.contains("invalid search mode"));
}

#[test]
fn mutations_rebuild_index_cache_transactionally() {
    let project = TempProject::new("transactional-index");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Cache\nneedle-transactional-cache"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    assert!(project.vault.join(".rem/cache/index.sqlite").is_file());

    project.rem_ok(&["delete", &id, "--hard"]);
    assert!(project.vault.join(".rem/cache/index.sqlite").is_file());

    let bm25 = project.rem_ok(&["search", "--bm25", "needle-transactional-cache"]);
    assert!(bm25.trim().is_empty());

    let grep = project.rem_ok(&["search", "--grep", "needle-transactional-cache"]);
    assert!(grep.trim().is_empty());
    assert_git_clean(&project);
}

#[test]
fn auto_search_and_doctor_degrade_when_index_is_corrupt() {
    let project = TempProject::new("corrupt-index");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Corrupt Index\nneedle-corrupt-index"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    fs::write(
        project.vault.join(".rem/cache/index.sqlite"),
        "not a sqlite db",
    )
    .unwrap();

    let output = project.rem_ok(&["search", "needle-corrupt-index"]);
    assert!(output.contains(&id));
    assert!(output.contains("\tgrep\t"));

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("index is unreadable"));
}

#[test]
fn failed_init_does_not_persist_broken_profile() {
    let project = TempProject::new("failed-init");
    let file_root = project.root.join("not-a-directory");
    fs::write(&file_root, "plain file").unwrap();

    let error = project.rem_err(&["init", "--root", path_str(&file_root)]);
    assert!(error.contains("Git working tree"));

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("no profiles configured"));
}

#[test]
fn edit_supports_editor_commands_with_arguments() {
    let project = TempProject::new("editor-args");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Editor\nargument splitting works"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    project.rem_ok(&["config", "set", "editor", "true --ignored"]);

    let edited = project.rem_ok(&["edit", &id]);
    assert!(edited.contains("edited "));
}

#[test]
fn active_memory_wins_duplicate_id_conflict_with_archive() {
    let project = TempProject::new("active-archive-duplicate");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Active Wins\nneedle-active-wins"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let active_path = project.vault.join(format!("memories/short/{id}.md"));
    let archive_path = project.vault.join(format!("archive/{id}.md"));
    let archived = fs::read_to_string(&active_path)
        .unwrap()
        .replace("status: active", "status: archived");
    fs::write(archive_path, archived).unwrap();

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("indexed=1"));
    assert!(rebuild.contains("diagnostics=1"));

    let bm25 = project.rem_ok(&["search", "--bm25", "needle-active-wins"]);
    assert!(bm25.contains(&id));
    assert!(bm25.contains("\tbm25\t"));
}

#[test]
fn exact_id_show_prefers_canonical_file_over_sync_conflict_copy() {
    let project = TempProject::new("exact-id-conflict");
    project.init_rem("local");
    let added = project.rem_ok(&["add", "--short", "# Canonical\nfreshcanonicaltoken"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let active_path = project.vault.join(format!("memories/short/{id}.md"));
    let conflict_path = project
        .vault
        .join(format!("memories/short/{id}-sync-conflict.md"));
    let conflict = fs::read_to_string(&active_path)
        .unwrap()
        .replace("freshcanonicaltoken", "staleconflicttoken");
    fs::write(conflict_path, conflict).unwrap();

    let shown = project.rem_ok(&["show", &id]);
    assert!(shown.contains("freshcanonicaltoken"));

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("indexed=1"));
    assert!(rebuild.contains("diagnostics=1"));

    let canonical = project.rem_ok(&["search", "--bm25", "freshcanonicaltoken"]);
    assert!(canonical.contains(&id));

    let conflict = project.rem_ok(&["search", "--bm25", "staleconflicttoken"]);
    assert!(conflict.trim().is_empty());
}

#[test]
fn rem_commit_accepts_or_restores_external_changes_non_interactive() {
    let project = TempProject::new("commit-external");
    project.init_rem("local");

    fs::write(project.vault.join("notes.md"), "# External\ninclude me").unwrap();
    let error = project.rem_err(&["commit", "--non-interactive"]);
    assert!(error.contains("external vault changes detected"));

    let output = project.rem_ok(&["commit", "--non-interactive", "--accept-external"]);
    assert!(output.contains("committed "));
    assert!(project.tracked_files().contains("notes.md"));

    fs::write(project.vault.join("scratch.md"), "# Scratch\nremove me").unwrap();
    let head = project.head();
    let output = project.rem_ok(&["commit", "--non-interactive", "--restore-external"]);
    assert!(output.contains("nothing to commit"));
    assert_eq!(project.head(), head);
    assert!(!project.vault.join("scratch.md").exists());
}

#[test]
fn rem_commit_review_can_include_all_external_changes() {
    let project = TempProject::new("commit-review-include");
    project.init_rem("local");

    fs::write(project.vault.join("notes.md"), "# External\ninclude me").unwrap();
    let output = project.rem_ok_with_stdin(&["commit", "--review"], "c\n");

    assert!(output.contains("external Git changes detected"));
    assert!(output.contains("committed "));
    assert!(project.tracked_files().contains("notes.md"));
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_can_restore_all_external_changes() {
    let project = TempProject::new("commit-review-restore");
    project.init_rem("local");
    let head = project.head();

    fs::write(project.vault.join("scratch.md"), "# Scratch\nremove me").unwrap();
    let output = project.rem_ok_with_stdin(&["commit", "--review"], "r\n");

    assert!(output.contains("external Git changes detected"));
    assert!(output.contains("nothing to commit"));
    assert_eq!(project.head(), head);
    assert!(!project.vault.join("scratch.md").exists());
    assert_git_clean(&project);
}

#[cfg(unix)]
#[test]
fn restore_external_untracked_symlink_removes_link_not_target() {
    let project = TempProject::new("restore-symlink");
    project.init_rem("local");
    let target_dir = project.root.join("outside-target");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("keep.txt"), "do not delete\n").unwrap();
    std::os::unix::fs::symlink(&target_dir, project.vault.join("linked-dir")).unwrap();

    let output = project.rem_ok(&["commit", "--non-interactive", "--restore-external"]);

    assert!(output.contains("nothing to commit"));
    assert!(fs::symlink_metadata(project.vault.join("linked-dir")).is_err());
    assert_eq!(
        fs::read_to_string(target_dir.join("keep.txt")).unwrap(),
        "do not delete\n"
    );
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_pick_can_include_or_restore_each_file() {
    let project = TempProject::new("commit-review-pick");
    project.init_rem("local");

    fs::write(project.vault.join("a-include.md"), "# Include\nkeep me").unwrap();
    fs::write(project.vault.join("b-restore.md"), "# Restore\ndrop me").unwrap();
    let output = project.rem_ok_with_stdin(&["commit", "--review"], "p\ni\nr\n");

    assert!(output.contains("a-include.md"));
    assert!(output.contains("b-restore.md"));
    assert!(project.tracked_files().contains("a-include.md"));
    assert!(!project.vault.join("b-restore.md").exists());
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_restore_all_resets_tracked_modified_deleted_and_staged_added() {
    let project = TempProject::new("commit-review-restore-tracked");
    project.init_rem("local");
    let head = project.head();

    let tracked = project.vault.join("tracked.md");
    fs::write(&tracked, "original\n").unwrap();
    project.git_commit_all("add tracked fixture");
    let deleted = project.vault.join("delete-me.md");
    fs::write(&deleted, "delete me\n").unwrap();
    project.git_commit_all("add delete fixture");
    let head_after_fixture = project.head();

    fs::write(&tracked, "modified\n").unwrap();
    fs::remove_file(&deleted).unwrap();
    let staged_added = project.vault.join("staged-added.md");
    fs::write(&staged_added, "staged\n").unwrap();
    project.git_ok(&["add", "staged-added.md"]);

    let output = project.rem_ok_with_stdin(&["commit", "--review"], "r\n");

    assert!(output.contains("external Git changes detected"));
    assert!(output.contains("nothing to commit"));
    assert_ne!(head, head_after_fixture);
    assert_eq!(project.head(), head_after_fixture);
    assert_eq!(fs::read_to_string(&tracked).unwrap(), "original\n");
    assert!(!staged_added.exists());
    assert_eq!(fs::read_to_string(&deleted).unwrap(), "delete me\n");
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_restore_all_resets_staged_rename() {
    let project = TempProject::new("commit-review-restore-rename");
    project.init_rem("local");

    let old_path = project.vault.join("old-name.md");
    let new_path = project.vault.join("new-name.md");
    fs::write(&old_path, "# Old\nrename fixture").unwrap();
    project.git_commit_all("add rename fixture");
    let head = project.head();
    project.git_ok(&["mv", "old-name.md", "new-name.md"]);

    let output = project.rem_ok_with_stdin(&["commit", "--review"], "r\n");

    assert!(output.contains("renamed:"));
    assert!(output.contains("old-name.md -> new-name.md"));
    assert!(output.contains("nothing to commit"));
    assert_eq!(project.head(), head);
    assert!(old_path.exists());
    assert!(!new_path.exists());
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_abort_leaves_external_changes_untouched() {
    let project = TempProject::new("commit-review-abort");
    project.init_rem("local");
    let head = project.head();

    let scratch = project.vault.join("scratch.md");
    fs::write(&scratch, "# Scratch\nkeep pending").unwrap();
    let output = project.rem_with_stdin(&["commit", "--review"], "a\n");

    assert!(
        !output.status.success(),
        "review abort unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("external Git changes detected"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("aborted due to external vault changes")
    );
    assert_eq!(project.head(), head);
    assert!(scratch.exists());
    assert!(project.status_short().contains("scratch.md"));
}

#[test]
fn rem_commit_review_diff_then_include_shows_untracked_preview() {
    let project = TempProject::new("commit-review-diff");
    project.init_rem("local");

    fs::write(project.vault.join("preview.md"), "# Preview\nshow me").unwrap();
    let output = project.rem_ok_with_stdin(&["commit", "--review"], "d\nc\n");

    assert!(output.contains("diff -- preview.md"));
    assert!(output.contains("+++ preview.md"));
    assert!(output.contains("+# Preview"));
    assert!(output.contains("committed "));
    assert!(project.tracked_files().contains("preview.md"));
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_diff_shows_staged_and_unstaged_hunks() {
    let project = TempProject::new("commit-review-diff-mixed");
    project.init_rem("local");

    let tracked = project.vault.join("tracked.md");
    fs::write(&tracked, "base\n").unwrap();
    project.git_commit_all("add mixed diff fixture");
    fs::write(&tracked, "staged change\n").unwrap();
    project.git_ok(&["add", "tracked.md"]);
    fs::write(&tracked, "unstaged change\n").unwrap();

    let output = project.rem_ok_with_stdin(&["commit", "--review"], "d\nr\n");

    assert!(output.contains("diff -- tracked.md"));
    assert!(output.contains("+staged change"));
    assert!(output.contains("+unstaged change"));
    assert_git_clean(&project);
}

#[test]
fn rem_commit_review_requires_choice_input() {
    let project = TempProject::new("commit-review-eof");
    project.init_rem("local");

    fs::write(project.vault.join("pending.md"), "# Pending\nneeds choice").unwrap();
    let output = project.rem_with_stdin(&["commit", "--review"], "");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("interactive review input ended before a choice was made")
    );
    assert!(project.vault.join("pending.md").exists());
}

#[test]
fn unmerged_git_conflicts_block_rem_commit_even_with_accept_external() {
    let project = TempProject::new("unmerged-block");
    project.init_rem("local");
    project.create_unmerged_conflict();

    let error = project.rem_err(&["commit", "--non-interactive", "--accept-external"]);
    assert!(error.contains("unmerged Git conflict detected"));
    assert!(error.contains("conflict.md"));
}

#[test]
fn unmerged_git_conflicts_block_dry_run_before_reindex() {
    let project = TempProject::new("unmerged-dry-run");
    project.init_rem("local");
    project.create_unmerged_conflict();

    let error = project.rem_err(&["commit", "--dry-run"]);
    assert!(error.contains("unmerged Git conflict detected"));
    assert!(error.contains("conflict.md"));
}

#[test]
fn unmerged_git_conflicts_block_mutations_before_writing_memory() {
    let project = TempProject::new("unmerged-mutation");
    project.init_rem("local");
    project.create_unmerged_conflict();

    let error = project.rem_err(&[
        "add",
        "--short",
        "--accept-external",
        "# Should Not Write\nblocked by merge conflict",
    ]);
    assert!(error.contains("unmerged Git conflict detected"));
    assert!(error.contains("conflict.md"));
    assert!(!memory_files_contain(&project.vault, "Should Not Write"));
}

#[test]
fn mutation_external_changes_require_explicit_policy_non_interactive() {
    let project = TempProject::new("mutation-external");
    project.init_rem("local");

    fs::create_dir_all(project.vault.join("attachments")).unwrap();
    fs::write(
        project.vault.join("attachments/image.txt"),
        "external asset",
    )
    .unwrap();
    let error = project.rem_err(&["add", "--short", "# Blocked\nno policy"]);
    assert!(error.contains("external vault changes detected"));

    let added = project.rem_ok(&[
        "add",
        "--short",
        "--accept-external",
        "# Accepted\nexternal asset included",
    ]);
    let id = added.split_whitespace().last().unwrap();
    assert!(project.tracked_files().contains("attachments/image.txt"));
    assert!(
        project
            .tracked_files()
            .contains(&format!("memories/short/{id}.md"))
    );
}

#[test]
fn reindex_failure_rolls_back_markdown_and_commit() {
    let project = TempProject::new("reindex-rollback");
    project.init_rem("local");
    let head = project.head();
    fs::write(
        project.vault.join("memories/short/bad.md"),
        "this is not a rem memory",
    )
    .unwrap();

    let error = project.rem_err(&[
        "add",
        "--short",
        "--accept-external",
        "# Should Roll Back\nthis must not survive",
    ]);
    assert!(error.contains("reindex produced 1 diagnostics"));
    assert_eq!(project.head(), head);
    assert_eq!(memory_file_count(&project.vault), 1);
    assert!(!memory_files_contain(&project.vault, "Should Roll Back"));
    assert!(project.vault.join("memories/short/bad.md").exists());
}

#[test]
fn git_commit_failure_rolls_back_markdown_and_index() {
    let project = TempProject::new("git-rollback");
    project.init_rem("local");
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let hook = project.vault.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();
    }

    let error = project.rem_err(&["add", "--short", "# Fails Commit\nrollback me"]);
    assert!(error.contains("git commit failed"));
    assert_eq!(project.head(), head);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    assert_eq!(memory_file_count(&project.vault), 0);
    assert_git_clean(&project);
}

#[test]
fn git_commit_failure_restores_preexisting_staged_changes() {
    let project = TempProject::new("git-rollback-staging");
    project.init_rem("local");

    let tracked = project.vault.join("external.md");
    fs::write(&tracked, "original\n").unwrap();
    project.git_commit_all("add external fixture");
    let head = project.head();

    fs::write(&tracked, "staged external\n").unwrap();
    project.git_ok(&["add", "external.md"]);
    fs::write(&tracked, "unstaged external\n").unwrap();

    let hook = project.vault.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();
    }

    let error = project.rem_err(&[
        "add",
        "--short",
        "--accept-external",
        "# Fails Commit\nrollback memory only",
    ]);
    assert!(error.contains("git commit failed"));
    assert_eq!(project.head(), head);
    assert_eq!(memory_file_count(&project.vault), 0);

    let staged_names = project.git_ok(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged_names.trim(), "external.md");
    let staged_diff = project.git_ok(&["diff", "--cached", "--", "external.md"]);
    assert!(staged_diff.contains("+staged external"));
    let worktree_diff = project.git_ok(&["diff", "--", "external.md"]);
    assert!(worktree_diff.contains("+unstaged external"));
    assert!(project.status_short().contains("MM external.md"));
}

#[test]
fn cache_and_tx_never_enter_git_commit() {
    let project = TempProject::new("excluded-paths");
    project.init_rem("local");
    project.rem_ok(&["add", "--short", "# Excluded\ncache and tx stay local"]);
    fs::create_dir_all(project.vault.join(".rem/tx/manual")).unwrap();
    fs::write(project.vault.join(".rem/tx/manual/journal"), "leftover").unwrap();

    let tracked = project.tracked_files();
    assert!(!tracked.contains(".rem/cache"));
    assert!(!tracked.contains(".rem/tx"));
}

#[test]
fn stale_transaction_journal_reported_by_doctor_and_commit() {
    let project = TempProject::new("stale-tx");
    project.init_rem("local");
    fs::create_dir_all(project.vault.join(".rem/tx/stale")).unwrap();
    fs::write(project.vault.join(".rem/tx/stale/journal"), "stale").unwrap();

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("pending transaction journal"));

    let error = project.rem_err(&["commit", "--non-interactive", "--accept-external"]);
    assert!(error.contains("transaction recovery pending"));
}

fn assert_git_clean(project: &TempProject) {
    assert_eq!(project.status_short(), "");
}

fn memory_file_count(vault: &Path) -> usize {
    memory_files(vault).len()
}

fn memory_files_contain(vault: &Path, needle: &str) -> bool {
    memory_files(vault).iter().any(|path| {
        fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
    })
}

fn memory_files(vault: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in [vault.join("memories/short"), vault.join("memories/long")] {
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "md") {
                files.push(path);
            }
        }
    }
    files
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}
