use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

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

    fn rem_with_env(&self, args: &[&str], set: &[(&str, &str)], remove: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rem"));
        command
            .env("REM_HOME", &self.rem_home)
            .current_dir(&self.root)
            .args(args);
        for (key, value) in set {
            command.env(key, value);
        }
        for key in remove {
            command.env_remove(key);
        }
        command.output().unwrap()
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
fn color_policy_styles_human_output_without_breaking_pipes() {
    let project = TempProject::new("color-output");
    project.init_rem("git");
    let added = project.rem_ok(&[
        "add",
        "--long",
        "--scope",
        "project",
        "--kind",
        "decision",
        "# Color Contract\nKeep machine output stable.",
    ]);
    let id = added.split_whitespace().last().unwrap();

    let piped = project.rem_with_env(
        &["list"],
        &[],
        &["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "FORCE_COLOR"],
    );
    assert!(piped.status.success());
    let piped = String::from_utf8(piped.stdout).unwrap();
    assert!(piped.contains(&format!("{id}\tlong\tproject\tdecision\tColor Contract")));
    assert!(!piped.contains("\u{1b}["));

    let always = project.rem_ok(&["--color", "always", "list"]);
    assert!(always.contains(id));
    assert!(always.contains("\u{1b}["));

    let forced = project.rem_with_env(
        &["list"],
        &[("CLICOLOR_FORCE", "1")],
        &["NO_COLOR", "CLICOLOR", "FORCE_COLOR"],
    );
    assert!(forced.status.success());
    assert!(
        String::from_utf8(forced.stdout)
            .unwrap()
            .contains("\u{1b}[")
    );

    let no_color = project.rem_with_env(
        &["list"],
        &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")],
        &["CLICOLOR", "FORCE_COLOR"],
    );
    assert!(no_color.status.success());
    assert!(
        !String::from_utf8(no_color.stdout)
            .unwrap()
            .contains("\u{1b}[")
    );

    let never = project.rem_with_env(
        &["--color", "never", "list"],
        &[("CLICOLOR_FORCE", "1")],
        &["NO_COLOR", "CLICOLOR", "FORCE_COLOR"],
    );
    assert!(never.status.success());
    assert!(!String::from_utf8(never.stdout).unwrap().contains("\u{1b}["));

    let shown = project.rem_ok(&["--color", "always", "show", id]);
    assert!(shown.contains("\u{1b}["));
    assert!(shown.contains("# Color Contract"));

    let error = project.rem_err(&["--color", "always", "search", "--vector", "color"]);
    assert!(error.contains("\u{1b}["));
    assert!(error.contains("vector search is not configured"));
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
fn tui_alias_is_not_a_supported_command() {
    let project = TempProject::new("no-tui-alias");
    let help = project.rem_ok(&["--help"]);
    assert!(help.contains("configure"));
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("tui"))
    );

    let error = project.rem_err(&["tui"]);
    assert!(error.contains("unrecognized subcommand"));
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
    let output = project.rem_ok_with_stdin(&["--color", "always", "commit", "--review"], "d\nc\n");

    assert!(output.contains("diff -- preview.md"));
    assert!(output.contains("+++ preview.md"));
    assert!(output.contains("+# Preview"));
    assert!(output.contains("\u{1b}[32m+# Preview\u{1b}[0m"));
    assert!(output.contains("committed"));
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

#[cfg(unix)]
#[test]
fn successful_hook_worktree_mutation_rolls_back_commit_markdown_and_index() {
    let project = TempProject::new("git-hook-dirty-rollback");
    project.init_rem("local");
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let hook = project.vault.join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf 'hook mutation\\n' > hook-output.md\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();

    let error = project.rem_err(&["add", "--short", "# Hook mutation\nrollback all state"]);
    assert!(error.contains("Git hooks changed vault files after commit"));
    assert_eq!(project.head(), head);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    assert_eq!(memory_file_count(&project.vault), 0);
    assert!(!project.vault.join("hook-output.md").exists());
    assert_git_clean(&project);
}

#[cfg(unix)]
#[test]
fn successful_hook_mutation_during_initial_commit_restores_unborn_head() {
    let project = TempProject::new("git-hook-unborn-rollback");
    project.init_git_vault();
    let hook = project.vault.join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf 'hook mutation\\n' > hook-output.md\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();

    let error = project.rem_err(&[
        "init",
        "--root",
        path_str(&project.vault),
        "--storage",
        "local",
    ]);
    assert!(error.contains("Git hooks changed vault files after commit"));
    assert!(
        !TempProject::git_in(&["rev-parse", "--verify", "HEAD"], &project.vault)
            .status
            .success()
    );
    assert!(!project.vault.join("hook-output.md").exists());
    assert!(!project.vault.join(".gitignore").exists());
    assert_git_clean(&project);
    assert!(
        project
            .rem_ok(&["doctor"])
            .contains("no profiles configured")
    );
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

#[test]
fn semantic_fact_directives_support_current_historical_and_source_queries() {
    let project = TempProject::new("semantic-facts");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--long",
        "--kind",
        "preference",
        "# Brand Preference\nUser changed running shoe preference after Adidas broke.\n@fact User | PREFERS | Adidas | valid_from=2025-01-10 | valid_to=2025-04-02 | confidence=0.8\n@fact User | PREFERS | Puma | valid_from=2025-04-02 | confidence=0.9",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();

    let current = project.rem_ok(&["facts", "--entity", "User"]);
    assert!(current.contains("PREFERS"));
    assert!(current.contains("Puma"));
    assert!(!current.contains("Adidas"));
    assert!(current.contains(&id));

    let historical = project.rem_ok(&["facts", "--entity", "User", "--at", "2025-02-01"]);
    assert!(historical.contains("Adidas"));
    assert!(!historical.contains("Puma"));

    let all = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(all.contains("Adidas"));
    assert!(all.contains("Puma"));

    let source = project.rem_ok(&["facts", "--entity", "Puma", "--source"]);
    assert!(source.contains("episode-"));
    assert!(source.contains("memories/long/"));
    assert!(source.contains("User changed running shoe preference"));

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("semantic_entities=3"));
    assert!(rebuild.contains("semantic_episodes=1"));
    assert!(rebuild.contains("semantic_facts=2"));

    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("semantic cache ready entities=3 episodes=1 facts=2"));
    assert_git_clean(&project);
}

#[test]
fn unsupported_semantic_relation_rolls_back_transaction() {
    let project = TempProject::new("semantic-relation-rollback");
    project.init_rem("local");
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let error = project.rem_err(&[
        "add",
        "--long",
        "# Invalid Relation\n@fact User | LOVES | Puma | valid_from=2025-04-02",
    ]);

    assert!(error.contains("reindex produced 1 diagnostics"));
    assert_eq!(project.head(), head);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    assert!(!memory_files_contain(&project.vault, "Invalid Relation"));
    assert_git_clean(&project);
}

#[test]
fn semantic_rebuild_reports_unique_entities_across_memories() {
    let project = TempProject::new("semantic-unique-entities");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--short",
        "# Tool One\n@fact User | USES | SQLite | valid_from=2026-01-01",
    ]);
    project.rem_ok(&[
        "add",
        "--short",
        "# Tool Two\n@fact User | USES | Rust | valid_from=2026-01-02",
    ]);

    let rebuild = project.rem_ok(&["rebuild"]);
    assert!(rebuild.contains("semantic_entities=3"));
    assert!(rebuild.contains("semantic_episodes=2"));
    assert!(rebuild.contains("semantic_facts=2"));

    let uses = project.rem_ok(&["facts", "--relation", "USES"]);
    assert!(uses.contains("SQLite"));
    assert!(uses.contains("Rust"));
    assert_git_clean(&project);
}

#[test]
fn semantic_current_query_respects_future_and_bounded_validity() {
    let project = TempProject::new("semantic-current-validity");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "--kind",
        "preference",
        "# Temporal Current\n@fact User | PREFERS | CurrentBounded | valid_from=2020-01-01 | valid_to=9999-01-01\n@fact User | PREFERS | FutureBrand | valid_from=9999-01-01\n@fact User | DISLIKES | ExpiredBrand | valid_from=2020-01-01 | valid_to=2021-01-01",
    ]);

    let current = project.rem_ok(&["facts", "--entity", "User"]);
    assert!(current.contains("CurrentBounded"));
    assert!(!current.contains("FutureBrand"));
    assert!(!current.contains("ExpiredBrand"));

    let all = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(all.contains("CurrentBounded"));
    assert!(all.contains("FutureBrand"));
    assert!(all.contains("ExpiredBrand"));

    let historical = project.rem_ok(&["facts", "--entity", "User", "--at", "2020-06-01"]);
    assert!(historical.contains("CurrentBounded"));
    assert!(historical.contains("ExpiredBrand"));
    assert!(!historical.contains("FutureBrand"));
    assert_git_clean(&project);
}

#[test]
fn semantic_unix_time_queries_compare_numerically() {
    let project = TempProject::new("semantic-unix-time");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "# Unix Time\n@fact User | USES | AncientUnixTool | valid_from=999 | valid_to=9999999999\n@fact User | USES | FutureUnixTool | valid_from=9999999999",
    ]);

    let current = project.rem_ok(&["facts", "--entity", "User"]);
    assert!(current.contains("AncientUnixTool"));
    assert!(!current.contains("FutureUnixTool"));

    let historical = project.rem_ok(&["facts", "--entity", "User", "--at", "1000"]);
    assert!(historical.contains("AncientUnixTool"));
    assert!(!historical.contains("FutureUnixTool"));
    assert_git_clean(&project);
}

#[test]
fn semantic_historical_queries_normalize_iso_and_unix_time_formats() {
    let project = TempProject::new("semantic-mixed-time-query");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "# Mixed Time\n@fact User | USES | UnixTool | valid_from=1735689600 | valid_to=1735776000\n@fact User | USES | IsoTool | valid_from=2025-01-01T00:00:00Z | valid_to=2025-01-02T00:00:00Z",
    ]);

    let iso_query = project.rem_ok(&["facts", "--entity", "User", "--at", "2025-01-01T12:00:00Z"]);
    assert!(iso_query.contains("UnixTool"));
    assert!(iso_query.contains("IsoTool"));

    let unix_query = project.rem_ok(&["facts", "--entity", "User", "--at", "1735732800"]);
    assert!(unix_query.contains("UnixTool"));
    assert!(unix_query.contains("IsoTool"));
    assert_git_clean(&project);
}

#[test]
fn semantic_temporal_queries_honor_precise_and_pre_epoch_boundaries() {
    let project = TempProject::new("semantic-temporal-boundaries");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "# Boundaries\n@fact User | USES | PreEpochTool | valid_from=-1 | valid_to=0\n@fact User | USES | DateTool | valid_from=2025-01-01 | valid_to=2025-01-02",
    ]);

    let pre_epoch = project.rem_ok(&["facts", "--entity", "User", "--at", "-1"]);
    assert!(pre_epoch.contains("PreEpochTool"));
    let epoch = project.rem_ok(&["facts", "--entity", "User", "--at", "0"]);
    assert!(!epoch.contains("PreEpochTool"));

    let before_end = project.rem_ok(&["facts", "--entity", "User", "--at", "2025-01-01T23:59:59Z"]);
    assert!(before_end.contains("DateTool"));
    let at_end = project.rem_ok(&["facts", "--entity", "User", "--at", "2025-01-02T00:00:00Z"]);
    assert!(!at_end.contains("DateTool"));
    assert_git_clean(&project);
}

#[test]
fn semantic_rejects_out_of_range_unix_times_and_rolls_back() {
    let project = TempProject::new("semantic-unix-overflow");
    project.init_rem("local");
    let head = project.head();

    let error = project.rem_err(&[
        "add",
        "--long",
        "# Invalid Unix\n@fact User | USES | OverflowTool | valid_from=9223372036854775808",
    ]);

    assert!(error.contains("signed 64-bit unix seconds"));
    assert_eq!(project.head(), head);
    assert!(!memory_files_contain(&project.vault, "OverflowTool"));
    assert_git_clean(&project);
}

#[test]
fn semantic_facts_reject_ambiguous_at_and_all_combination() {
    let project = TempProject::new("semantic-at-all");
    project.init_rem("local");

    let error = project.rem_err(&["facts", "--at", "2025-01-01", "--all"]);
    assert!(error.contains("cannot be used with"));
    assert_git_clean(&project);
}

#[test]
fn semantic_facts_explain_when_the_local_cache_is_missing() {
    let project = TempProject::new("semantic-missing-cache");
    project.init_rem("local");
    fs::remove_file(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let error = project.rem_err(&["facts"]);
    assert!(error.contains("semantic index does not exist"));
    assert_git_clean(&project);
}

#[test]
fn invalid_semantic_time_values_roll_back_transaction() {
    let project = TempProject::new("semantic-time-rollback");
    project.init_rem("local");
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let error = project.rem_err(&[
        "add",
        "--long",
        "# Bad Time\n@fact User | PREFERS | Puma | valid_from=2025-04-02 | valid_to=2025-01-10",
    ]);

    assert!(error.contains("reindex produced 1 diagnostics"));
    assert_eq!(project.head(), head);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    assert!(!memory_files_contain(&project.vault, "Bad Time"));

    let error = project.rem_err(&[
        "add",
        "--long",
        "# Bad Time Format\n@fact User | PREFERS | Puma | valid_from=2025-4-2",
    ]);
    assert!(error.contains("reindex produced 1 diagnostics"));
    assert!(!memory_files_contain(&project.vault, "Bad Time Format"));

    let error = project.rem_err(&[
        "add",
        "--long",
        "# Zero Interval\n@fact User | PREFERS | Puma | valid_from=2025-04-02 | valid_to=2025-04-02T00:00:00Z",
    ]);
    assert!(error.contains("valid_to must be later"));
    assert!(!memory_files_contain(&project.vault, "Zero Interval"));
    assert_git_clean(&project);
}

#[test]
fn semantic_extraction_ignores_fact_like_text_inside_code_fences() {
    let project = TempProject::new("semantic-code-fence");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--short",
        "# Fact Example\n```text\n@fact User | LOVES | This should stay inert\n```\n    @fact User | LOVES | Indented code should stay inert\n@fact User | USES | SQLite | valid_from=2020-01-01",
    ]);

    let facts = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(facts.contains("USES"));
    assert!(facts.contains("SQLite"));
    assert!(!facts.contains("LOVES"));
    assert!(!facts.contains("This should stay inert"));
    assert!(!facts.contains("Indented code should stay inert"));
    assert_git_clean(&project);
}

#[test]
fn semantic_facts_support_value_objects_relation_normalization_and_at_validation() {
    let project = TempProject::new("semantic-value-object");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "# Value Object\n@fact User | works-at | OpenAI | valid_from=2020-01-01\n@fact User | MENTIONS | 1440 | object_kind=Value | valid_from=2020-01-01",
    ]);

    let relation = project.rem_ok(&["facts", "--relation", "works-at"]);
    assert!(relation.contains("WORKS_AT"));
    assert!(relation.contains("OpenAI"));

    let value_object = project.rem_ok(&["facts", "--entity", "1440"]);
    assert!(value_object.contains("MENTIONS"));
    assert!(value_object.contains("1440"));

    let error = project.rem_err(&["facts", "--at", "2020-1-1"]);
    assert!(error.contains("zero-padded YYYY-MM-DD"));
    assert_git_clean(&project);
}

#[test]
fn semantic_fact_output_exposes_expiration_timestamp() {
    let project = TempProject::new("semantic-expiration-output");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--long",
        "# Expiration\n@fact User | USES | RetiredTool | valid_from=2020-01-01 | expired_at=2021-02-03",
    ]);

    let facts = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(facts.contains("RetiredTool"));
    assert!(facts.contains("2021-02-03"));
    let sourced = project.rem_ok(&["facts", "--entity", "User", "--all", "--source"]);
    assert!(sourced.contains("2021-02-03"));
    assert_git_clean(&project);
}

#[test]
fn semantic_cache_tracks_update_and_archive_lifecycle() {
    let project = TempProject::new("semantic-crud-lifecycle");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--long",
        "# Lifecycle\n@fact User | USES | OldTool | valid_from=2020-01-01",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let before_update = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(before_update.contains("OldTool"));

    project.rem_ok(&[
        "update",
        &id,
        "# Lifecycle Updated\n@fact User | USES | NewTool | valid_from=2020-01-01",
    ]);
    let after_update = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(after_update.contains("NewTool"));
    assert!(!after_update.contains("OldTool"));

    project.rem_ok(&["delete", &id]);
    let after_archive = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(after_archive.trim().is_empty());
    assert_git_clean(&project);
}

#[test]
fn semantic_rebuild_keeps_fact_identity_and_provenance_deterministic() {
    let project = TempProject::new("semantic-deterministic-rebuild");
    project.init_rem("local");

    project.rem_ok(&[
        "add",
        "--short",
        "# Deterministic\nsource text\n@fact User | USES | SQLite | valid_from=2020-01-01",
    ]);
    let before = project.rem_ok(&["facts", "--entity", "User", "--all", "--source"]);
    project.rem_ok(&["rebuild"]);
    let after = project.rem_ok(&["facts", "--entity", "User", "--all", "--source"]);
    assert_eq!(after, before);
    assert_git_clean(&project);
}

#[test]
fn rem_commit_rebuilds_semantic_cache_from_external_markdown_edits() {
    let project = TempProject::new("semantic-external-commit");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--short",
        "# External Edit\n@fact User | USES | OldTool | valid_from=2020-01-01",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let path = project.vault.join(format!("memories/short/{id}.md"));
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("OldTool", "ExternalTool");
    fs::write(&path, edited).unwrap();

    project.rem_ok(&["commit", "--non-interactive", "--accept-external"]);
    let facts = project.rem_ok(&["facts", "--entity", "User", "--all"]);
    assert!(facts.contains("ExternalTool"));
    assert!(!facts.contains("OldTool"));
    assert_eq!(project.last_commit_subject(), "rem: commit vault changes");
    assert_git_clean(&project);
}

#[test]
fn invalid_external_semantic_edit_preserves_markdown_and_restores_cache() {
    let project = TempProject::new("semantic-external-rollback");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--short",
        "# External Rollback\n@fact User | USES | StableTool | valid_from=2020-01-01",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let path = project.vault.join(format!("memories/short/{id}.md"));
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace("USES | StableTool", "LOVES | BrokenTool");
    fs::write(&path, invalid).unwrap();

    let error = project.rem_err(&["commit", "--non-interactive", "--accept-external"]);
    assert!(error.contains("unsupported semantic relation"));
    assert_eq!(project.head(), head);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("LOVES | BrokenTool")
    );
    assert!(project.status_short().contains("memories/short/"));
}

#[cfg(unix)]
#[test]
fn rollback_preserves_unrelated_symlink_entries() {
    let project = TempProject::new("rollback-symlink");
    project.init_rem("local");

    let target = project.root.join("linked-asset.md");
    fs::write(&target, "external asset\n").unwrap();
    let link = project.vault.join("attachments/linked-asset.md");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&target, &link).unwrap();
    project.git_commit_all("add linked asset");

    let invalid = project.vault.join("memories/long/invalid-semantic.md");
    fs::write(
        &invalid,
        "---\nid: invalid-semantic\ntype: long\nscope: user\nkind: note\nstatus: active\ncreated_at: 1\nupdated_at: 1\ntags: []\ntitle: null\nsource: fixture\nagent: null\nsession: null\nconfidence: 1.0\npromoted_from: null\nsupersedes: []\n---\n# Invalid Semantic\n@fact User | LOVES | Broken\n",
    )
    .unwrap();
    project.git_commit_all("add invalid semantic fixture");

    let error = project.rem_err(&["add", "--short", "# Fails\nrollback"]);
    assert!(error.contains("reindex produced 1 diagnostics"));
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), target);
    assert_eq!(fs::read_to_string(&link).unwrap(), "external asset\n");
    assert_git_clean(&project);
}

#[cfg(unix)]
#[test]
fn mutations_refuse_symlinked_memory_files() {
    let project = TempProject::new("symlinked-memory-mutation");
    project.init_rem("local");

    let added = project.rem_ok(&["add", "--short", "# Linked Memory\nvalid memory"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let memory_path = project.vault.join(format!("memories/short/{id}.md"));
    let target = project.root.join("linked-memory.md");
    let original = fs::read_to_string(&memory_path).unwrap();
    fs::rename(&memory_path, &target).unwrap();
    symlink(&target, &memory_path).unwrap();
    project.git_commit_all("store memory through symlink");
    let head = project.head();

    let update = project.rem_err(&["update", &id, "# Updated\nshould not write"]);
    assert!(update.contains("refusing to mutate symlinked memory"));
    let delete = project.rem_err(&["delete", "--hard", &id]);
    assert!(delete.contains("refusing to mutate symlinked memory"));
    let edit = project.rem_err(&["edit", &id]);
    assert!(edit.contains("refusing to mutate symlinked memory"));
    let commit = project.rem_err(&["commit", "--non-interactive"]);
    assert!(commit.contains("memory file must be a regular vault file"));
    assert_eq!(project.head(), head);
    assert!(
        fs::symlink_metadata(&memory_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert_git_clean(&project);
}

#[cfg(unix)]
#[test]
fn mutations_refuse_symlinked_vault_directories() {
    let project = TempProject::new("symlinked-vault-directory");
    project.init_rem("local");
    let short_dir = project.vault.join("memories/short");
    let outside = project.root.join("outside-short");
    fs::remove_dir(&short_dir).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &short_dir).unwrap();
    let head = project.head();

    let error = project.rem_err(&["add", "# Blocked\nno writes through directory links"]);
    assert!(error.contains("refusing to use symlinked vault directory"));
    assert_eq!(project.head(), head);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    fs::remove_file(&short_dir).unwrap();
    fs::create_dir(&short_dir).unwrap();
    assert_git_clean(&project);
}

#[cfg(unix)]
#[test]
fn mutations_refuse_symlinked_gitignore() {
    let project = TempProject::new("symlinked-gitignore");
    project.init_rem("local");

    let gitignore = project.vault.join(".gitignore");
    let target = project.root.join("linked-gitignore");
    let original = fs::read_to_string(&gitignore).unwrap();
    fs::rename(&gitignore, &target).unwrap();
    symlink(&target, &gitignore).unwrap();
    project.git_commit_all("store gitignore through symlink");
    let head = project.head();

    let error = project.rem_err(&["add", "--short", "# Blocked\nno external writes"]);
    assert!(error.contains("refusing to update symlinked .gitignore"));
    let dry_run = project.rem_err(&["commit", "--dry-run"]);
    assert!(dry_run.contains("refusing to update symlinked .gitignore"));
    assert_eq!(project.head(), head);
    assert!(
        fs::symlink_metadata(&gitignore)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert_git_clean(&project);
}

#[test]
fn add_source_identity_is_idempotent_and_requires_explicit_update() {
    let project = TempProject::new("source-identity");
    project.init_rem("local");

    let first = project.rem_ok(&[
        "add",
        "--short",
        "--source",
        "codex",
        "--source-id",
        "conversation-42",
        "# Original\nsame event",
    ]);
    let id = first.split_whitespace().last().unwrap().to_string();
    let first_head = project.head();

    let no_op = project.rem_ok(&[
        "add",
        "--short",
        "--source",
        "codex",
        "--source-id",
        "conversation-42",
        "# Original\nsame event\n",
    ]);
    assert!(no_op.contains(&format!("no-op {id} reason=source-identity")));
    assert_eq!(project.head(), first_head);
    assert_git_clean(&project);

    let conflicting = project.rem_err(&[
        "add",
        "--short",
        "--source",
        "codex",
        "--source-id",
        "conversation-42",
        "# Changed\nsame event was edited",
    ]);
    assert!(conflicting.contains(&format!("rem update {id}")));
    assert_eq!(project.head(), first_head);
    assert_git_clean(&project);

    let updated = project.rem_ok(&["update", &id, "# Changed\nsame event was edited"]);
    assert!(updated.contains(&format!("updated {id}")));
    let updated_head = project.head();
    assert_ne!(updated_head, first_head);
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: update memory {id}")
    );

    let shown = project.rem_ok(&["show", &id]);
    assert!(shown.contains("source_id: conversation-42"));
    assert!(shown.contains("# Changed\nsame event was edited"));

    let unchanged = project.rem_ok(&["update", &id, "# Changed\nsame event was edited\n"]);
    assert!(unchanged.contains(&format!("no-op {id} reason=unchanged-body")));
    assert_eq!(project.head(), updated_head);
    assert_git_clean(&project);
}

#[test]
fn review_is_read_only_and_returns_an_explicit_action_choice() {
    let project = TempProject::new("review");
    project.init_rem("local");

    let added = project.rem_ok(&["add", "--short", "# Original\nkeep this body"]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let head = project.head();

    let review = project.rem_ok_with_stdin(
        &[
            "review",
            "--id",
            &id,
            "# Replacement\nchoose but do not write",
        ],
        "update\n",
    );
    assert!(review.contains(&format!("candidate id={id}")));
    assert!(review.contains(&format!(
        "review action=update target={id} reason=explicit-target"
    )));
    assert_eq!(project.head(), head);
    assert_git_clean(&project);
    let shown = project.rem_ok(&["show", &id]);
    assert!(shown.contains("# Original\nkeep this body"));

    let no_op = project.rem_ok(&["review", "--id", &id, "# Original\nkeep this body"]);
    assert!(no_op.contains(&format!(
        "review action=no-op target={id} reason=unchanged-body"
    )));
    assert_eq!(project.head(), head);

    let add = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# Unrelated\nproposed memory",
    ]);
    assert!(add.contains("review action=add target=- reason=no-explicit-target"));
    assert_eq!(project.head(), head);
    assert_git_clean(&project);
}

#[test]
fn semantic_review_recommends_explicit_actions_without_writing() {
    let project = TempProject::new("semantic-review");
    project.init_rem("local");

    let preferred = project.rem_ok(&[
        "add",
        "--long",
        "--kind",
        "preference",
        "# Preferred editor\n@fact User | PREFERS | Vim | valid_from=2020-01-01",
    ]);
    let preferred_id = preferred.split_whitespace().last().unwrap().to_string();
    let initial_head = project.head();

    let exact_body = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# Preferred editor\n@fact User | PREFERS | Vim | valid_from=2020-01-01",
    ]);
    assert!(exact_body.contains(&format!(
        "review action=no-op target={preferred_id} reason=matching-body"
    )));
    assert_eq!(project.head(), initial_head);

    let exclusive = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# Changed editor\n@fact user | prefers | Helix | valid_from=2020-01-01",
    ]);
    assert!(exclusive.contains(&format!("candidate id={preferred_id}")));
    assert!(exclusive.contains(&format!(
        "candidate semantic id={preferred_id} suggested=supersede reason=semantic-exclusive-conflict"
    )));
    assert!(exclusive.contains(&format!(
        "review action=supersede target={preferred_id} reason=semantic-exclusive-conflict"
    )));
    assert_eq!(project.head(), initial_head);
    assert_git_clean(&project);

    let same_fact = project.rem_ok_with_stdin(
        &[
            "review",
            "# More editor context\n@fact User | PREFERS | vim | valid_from=2020-01-01",
        ],
        "append\n",
    );
    assert!(same_fact.contains(&format!(
        "review action=append target={preferred_id} reason=semantic-same-fact"
    )));
    assert_eq!(project.head(), initial_head);
    assert_git_clean(&project);

    let tool = project.rem_ok(&[
        "add",
        "--long",
        "# Existing tool\n@fact User | USES | Git | valid_from=2020-01-01",
    ]);
    let tool_id = tool.split_whitespace().last().unwrap().to_string();
    let compatible_head = project.head();
    let compatible = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# Another tool\n@fact User | USES | SQLite | valid_from=2020-01-01",
    ]);
    assert!(compatible.contains(&format!(
        "candidate semantic id={tool_id} suggested=add reason=semantic-compatible-fact"
    )));
    assert!(compatible.contains(&format!(
        "review action=add target={tool_id} reason=semantic-compatible-fact"
    )));
    assert_eq!(project.head(), compatible_head);
    assert_git_clean(&project);

    project.rem_ok(&[
        "add",
        "--long",
        "# Other preference\n@fact User | PREFERS | Emacs | valid_from=2020-01-01",
    ]);
    let ambiguous_head = project.head();
    let ambiguous = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# New preference\n@fact User | PREFERS | Neovim | valid_from=2020-01-01",
    ]);
    assert!(ambiguous.contains("candidate none"));
    assert!(ambiguous.contains("candidate semantic"));
    assert!(ambiguous.contains("review action=add target=- reason=ambiguous-semantic-candidates"));
    assert_eq!(project.head(), ambiguous_head);
    assert_git_clean(&project);

    project.rem_ok(&[
        "add",
        "--long",
        "--scope",
        "project",
        "# Former employer\n@fact User | WORKS_AT | OldCo | valid_from=2000-01-01 | valid_to=2001-01-01",
    ]);
    let temporal_head = project.head();
    let expired = project.rem_ok(&[
        "review",
        "--scope",
        "project",
        "--non-interactive",
        "# Current employer\n@fact User | WORKS_AT | NewCo | valid_from=2020-01-01",
    ]);
    assert!(expired.contains("candidate none"));
    assert!(expired.contains("review action=add target=- reason=no-explicit-target"));
    assert_eq!(project.head(), temporal_head);
    assert_git_clean(&project);
}

#[test]
fn semantic_review_uses_only_the_active_side_of_a_supersede_chain() {
    let project = TempProject::new("semantic-review-supersede-chain");
    project.init_rem("local");

    let old = project.rem_ok(&[
        "add",
        "--long",
        "--kind",
        "preference",
        "# Old editor\n@fact User | PREFERS | Vim | valid_from=2020-01-01",
    ]);
    let old_id = old.split_whitespace().last().unwrap().to_string();
    let replacement_body = "# Current editor\n@fact User | PREFERS | Helix | valid_from=2020-01-01";
    let superseded = project.rem_ok(&["supersede", &old_id, replacement_body]);
    let replacement_id = superseded
        .trim()
        .strip_prefix(&format!("superseded {old_id} with "))
        .unwrap()
        .to_string();
    let head = project.head();

    let proposal = project.rem_ok(&[
        "review",
        "--non-interactive",
        "# Return to Vim\n@fact User | PREFERS | Vim | valid_from=2020-01-01",
    ]);
    assert!(proposal.contains(&format!("candidate id={replacement_id}")));
    assert!(!proposal.contains(&format!("candidate id={old_id}")));
    assert!(proposal.contains(&format!(
        "review action=supersede target={replacement_id} reason=semantic-exclusive-conflict"
    )));

    let facts = project.rem_ok(&["facts", "--relation", "PREFERS"]);
    assert!(facts.contains(&replacement_id));
    assert!(!facts.contains(&old_id));
    assert_eq!(project.head(), head);
    assert_git_clean(&project);
}

#[test]
fn append_and_supersede_are_explicit_and_preserve_provenance() {
    let project = TempProject::new("append-supersede");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--long",
        "--kind",
        "preference",
        "--source",
        "codex",
        "--source-id",
        "event-1",
        "--agent",
        "codex",
        "--session",
        "run-1",
        "# Preferred editor\nUse Vim for terminal work.",
    ]);
    let old_id = added.split_whitespace().last().unwrap().to_string();

    let appended = project.rem_ok(&[
        "append",
        &old_id,
        "Keep the configuration portable across machines.",
    ]);
    assert!(appended.contains(&format!("appended {old_id}")));
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: append memory {old_id}")
    );
    let after_append = project.head();
    let appended_memory = project.rem_ok(&["show", &old_id]);
    assert!(appended_memory.contains("Use Vim for terminal work.\n\nKeep the configuration"));

    let same_source = project.rem_err(&[
        "supersede",
        &old_id,
        "--source",
        "codex",
        "--source-id",
        "event-1",
        "# Preferred editor\nUse Helix for terminal work.",
    ]);
    assert!(same_source.contains("source identity"));
    assert_eq!(project.head(), after_append);
    assert_git_clean(&project);

    let replacement_body = "# Preferred editor\nUse Helix for terminal work.";
    let superseded = project.rem_ok(&[
        "supersede",
        &old_id,
        "--source",
        "codex",
        "--source-id",
        "event-2",
        replacement_body,
    ]);
    let replacement_id = superseded
        .trim()
        .strip_prefix(&format!("superseded {old_id} with "))
        .unwrap()
        .to_string();
    assert_ne!(replacement_id, old_id);
    assert_eq!(
        project.last_commit_subject(),
        format!("rem: supersede memory {old_id} with {replacement_id}")
    );

    let old = project.rem_ok(&["show", &old_id]);
    assert!(old.contains("status: superseded"));
    assert!(old.contains("source_id: event-1"));
    let replacement = project.rem_ok(&["show", &replacement_id]);
    assert!(replacement.contains("status: active"));
    assert!(replacement.contains("source_id: event-2"));
    assert!(replacement.contains("agent: codex"));
    assert!(replacement.contains("session: run-1"));
    assert!(replacement.contains(&format!("supersedes: [{old_id}]")));

    let listed = project.rem_ok(&["list", "--long"]);
    assert!(listed.contains(&replacement_id));
    assert!(!listed.contains(&old_id));

    let update = project.rem_err(&["update", &old_id, "# Mutated\nnot allowed"]);
    assert!(update.contains("only active memories can be updated"));
    let append = project.rem_err(&["append", &old_id, "not allowed"]);
    assert!(append.contains("only active memories can be appended"));
    project.rem_ok(&["config", "set", "editor", "false"]);
    let edit = project.rem_err(&["edit", &old_id]);
    assert!(edit.contains("only active memories can be edited"));
    let promote = project.rem_err(&["promote", &old_id]);
    assert!(promote.contains("only active memories can be promoted"));
    let delete = project.rem_err(&["delete", "--hard", &old_id]);
    assert!(delete.contains("superseded memory"));

    let replacement_head = project.head();
    let no_op = project.rem_ok(&["supersede", &replacement_id, replacement_body]);
    assert!(no_op.contains(&format!("no-op {replacement_id} reason=unchanged-body")));
    assert_eq!(project.head(), replacement_head);
    assert_git_clean(&project);
}

#[test]
fn supersede_rolls_back_if_reindex_rejects_the_replacement() {
    let project = TempProject::new("supersede-rollback");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--long",
        "# Stable preference\n@fact User | PREFERS | Vim | valid_from=2025-01-01",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    let head = project.head();
    let index_before = fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap();

    let error = project.rem_err(&[
        "supersede",
        &id,
        "# Invalid replacement\n@fact User | LOVES | Broken",
    ]);
    assert!(error.contains("unsupported semantic relation"));
    assert_eq!(project.head(), head);
    assert_eq!(memory_file_count(&project.vault), 1);
    assert_eq!(
        fs::read(project.vault.join(".rem/cache/index.sqlite")).unwrap(),
        index_before
    );
    let original = project.rem_ok(&["show", &id]);
    assert!(original.contains("status: active"));
    assert!(original.contains("PREFERS | Vim"));
    assert_git_clean(&project);
}

#[test]
fn no_op_mutations_still_require_a_valid_git_vault() {
    let project = TempProject::new("no-op-git-validation");
    project.init_rem("local");

    let added = project.rem_ok(&[
        "add",
        "--source",
        "codex",
        "--source-id",
        "event-1",
        "# Stable\nunchanged body",
    ]);
    let id = added.split_whitespace().last().unwrap().to_string();
    fs::rename(
        project.vault.join(".git"),
        project.vault.join(".git-disabled"),
    )
    .unwrap();

    let add = project.rem_err(&[
        "add",
        "--source",
        "codex",
        "--source-id",
        "event-1",
        "# Stable\nunchanged body",
    ]);
    assert!(add.contains("not a Git repository"));
    let update = project.rem_err(&["update", &id, "# Stable\nunchanged body"]);
    assert!(update.contains("not a Git repository"));
    let supersede = project.rem_err(&["supersede", &id, "# Stable\nunchanged body"]);
    assert!(supersede.contains("not a Git repository"));
}

#[test]
fn metadata_scalars_reject_multiline_frontmatter_injection() {
    let project = TempProject::new("metadata-scalar-validation");
    project.init_rem("local");
    let head = project.head();

    let source_id = project.rem_err(&[
        "add",
        "--source-id",
        "event-1\nstatus: archived",
        "# Blocked\nno frontmatter injection",
    ]);
    assert!(source_id.contains("source_id must be a single line"));
    let tag = project.rem_err(&[
        "add",
        "--tag",
        "safe\nstatus: archived",
        "# Blocked\nno frontmatter injection",
    ]);
    assert!(tag.contains("memory tags must be single-line values"));
    assert_eq!(project.head(), head);
    assert_eq!(memory_file_count(&project.vault), 0);
    assert_git_clean(&project);
}

#[test]
fn source_identity_scalars_round_trip_without_aliasing_special_values() {
    let project = TempProject::new("source-identity-round-trip");
    project.init_rem("local");

    for (index, source_id) in ["null", "[event,1]", "\"quoted\"", " event "]
        .into_iter()
        .enumerate()
    {
        let body = format!("# Event {index}\nopaque source identity");
        let added = project.rem_ok(&["add", "--source", "codex", "--source-id", source_id, &body]);
        let id = added.split_whitespace().last().unwrap().to_string();
        let shown = project.rem_ok(&["show", &id]);
        let rendered_source_id = match source_id {
            "null" => "source_id: \"null\"",
            "[event,1]" => "source_id: \"[event,1]\"",
            "\"quoted\"" => "source_id: \"\\\"quoted\\\"\"",
            " event " => "source_id: \" event \"",
            _ => unreachable!(),
        };
        assert!(shown.contains(rendered_source_id));

        let no_op = project.rem_ok(&["add", "--source", "codex", "--source-id", source_id, &body]);
        assert!(no_op.contains(&format!("no-op {id} reason=source-identity")));
    }

    assert_eq!(memory_file_count(&project.vault), 4);
    assert_git_clean(&project);
}

#[test]
fn external_duplicate_source_identity_blocks_commit_and_preserves_state() {
    let project = TempProject::new("duplicate-source-identity");
    project.init_rem("local");
    project.rem_ok(&[
        "add",
        "--source",
        "codex",
        "--source-id",
        "event-a",
        "# First\nsource event",
    ]);
    project.rem_ok(&[
        "add",
        "--source",
        "codex",
        "--source-id",
        "event-b",
        "# Second\nsource event",
    ]);
    let head = project.head();
    let index_path = project.vault.join(".rem/cache/index.sqlite");
    let index_before = fs::read(&index_path).unwrap();
    let second = memory_files(&project.vault)
        .into_iter()
        .find(|path| {
            fs::read_to_string(path)
                .unwrap()
                .contains("source_id: event-b")
        })
        .unwrap();
    let raw = fs::read_to_string(&second)
        .unwrap()
        .replace("source_id: event-b", "source_id: event-a");
    fs::write(&second, raw).unwrap();

    let error = project.rem_err(&["commit", "--non-interactive", "--accept-external"]);
    assert!(error.contains("duplicate source identity"));
    assert_eq!(project.head(), head);
    assert_eq!(fs::read(&index_path).unwrap(), index_before);
    assert!(
        fs::read_to_string(&second)
            .unwrap()
            .contains("source_id: event-a")
    );
    assert!(project.status_short().contains(" M memories/"));
}

#[test]
fn supersede_same_body_honors_explicit_metadata_overrides() {
    let project = TempProject::new("supersede-metadata-override");
    project.init_rem("local");
    let body = "# Stable body\nmetadata changed explicitly";
    let added = project.rem_ok(&["add", "--kind", "note", body]);
    let old_id = added.split_whitespace().last().unwrap().to_string();

    let output = project.rem_ok(&["supersede", "--kind", "decision", &old_id, body]);
    let replacement_id = output
        .trim()
        .strip_prefix(&format!("superseded {old_id} with "))
        .unwrap()
        .to_string();
    assert_ne!(replacement_id, old_id);
    assert!(
        project
            .rem_ok(&["show", &old_id])
            .contains("status: superseded")
    );
    let replacement = project.rem_ok(&["show", &replacement_id]);
    assert!(replacement.contains("kind: decision"));
    assert!(replacement.contains(body));
    assert_eq!(memory_file_count(&project.vault), 2);
    assert_git_clean(&project);
}

#[test]
fn stale_transaction_lock_blocks_mutation_and_is_reported_by_doctor() {
    let project = TempProject::new("stale-transaction-lock");
    project.init_rem("local");
    let lock = project.vault.join(".rem/tx/active.lock");
    fs::write(&lock, "stale-owner\n").unwrap();
    let head = project.head();

    let error = project.rem_err(&["add", "# Blocked\ntransaction is locked"]);
    assert!(error.contains("another rem transaction is active or a stale lock remains"));
    let dry_run = project.rem_err(&["commit", "--dry-run"]);
    assert!(dry_run.contains("another rem transaction is active or a stale lock remains"));
    let rebuild = project.rem_err(&["rebuild"]);
    assert!(rebuild.contains("another rem transaction is active or a stale lock remains"));
    assert_eq!(project.head(), head);
    assert_eq!(memory_file_count(&project.vault), 0);
    let doctor = project.rem_ok(&["doctor"]);
    assert!(doctor.contains("transaction lock present"));
    assert!(doctor.contains("active.lock"));

    fs::remove_file(lock).unwrap();
    project.rem_ok(&["add", "# Recovered\nlock was inspected and removed"]);
    assert_eq!(memory_file_count(&project.vault), 1);
    assert_git_clean(&project);
}

#[test]
fn concurrent_mutations_never_clobber_a_successful_transaction() {
    let project = TempProject::new("concurrent-mutations");
    project.init_rem("local");
    let barrier = Arc::new(Barrier::new(3));

    let spawn_add = |body: &'static str| {
        let barrier = Arc::clone(&barrier);
        let rem_home = project.rem_home.clone();
        let cwd = project.root.clone();
        thread::spawn(move || {
            barrier.wait();
            Command::new(env!("CARGO_BIN_EXE_rem"))
                .env("REM_HOME", rem_home)
                .current_dir(cwd)
                .args([
                    "add",
                    "--source",
                    "codex",
                    "--source-id",
                    "concurrent-event",
                    body,
                ])
                .output()
                .unwrap()
        })
    };

    let first = spawn_add("# Concurrent A\nfirst process");
    let second = spawn_add("# Concurrent B\nsecond process");
    barrier.wait();
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert!(
        first.status.success() || second.status.success(),
        "both concurrent commands failed\nfirst: {}\nsecond: {}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(memory_file_count(&project.vault), 1);
    assert_eq!(project.git_ok(&["rev-list", "--count", "HEAD"]).trim(), "2");
    assert_git_clean(&project);
    assert_eq!(
        fs::read_dir(project.vault.join(".rem/tx")).unwrap().count(),
        0
    );
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
