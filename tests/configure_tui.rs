#![cfg(unix)]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct TempTuiProject {
    root: PathBuf,
    rem_home: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

impl TempTuiProject {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rem-configure-tui-{name}-{nonce}"));
        let rem_home = root.join("rem-home");
        let home = root.join("finder-home");
        let work = home.join("work");
        fs::create_dir_all(&work).unwrap();

        Self {
            root,
            rem_home,
            home,
            work,
        }
    }

    fn configure_with_expect(&self) -> Output {
        let script = r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) configure
after 400
send -- "\033\[B"
after 100
send -- "\006"
after 800
send -- "\001"
after 100
send -- "finder-jk-target"
after 500
send -- "\t"
after 100
send -- "\r"
after 100
send -- "\033\[B"
after 100
send -- "\r"
after 100
send -- "\033\[B"
after 100
send -- "\r"
after 100
send -- "\033\[B"
after 100
send -- "\r"
after 100
send -- "\033\[B\033\[B"
after 100
send -- "\r"
after 100
send -- "S"
after 200
send -- "q"
expect eof
"#;

        self.run_expect(script)
    }

    fn run_expect(&self, script: &str) -> Output {
        Command::new("expect")
            .arg("-c")
            .arg(script)
            .env("REM_HOME", &self.rem_home)
            .env("HOME", &self.home)
            .env("REM_TUI_BIN", env!("CARGO_BIN_EXE_rem"))
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("FORCE_COLOR")
            .current_dir(&self.work)
            .output()
            .unwrap()
    }

    fn create_list_fixture(&self) {
        let vault = self.root.join("vault");
        fs::create_dir_all(vault.join("memories/short")).unwrap();
        fs::create_dir_all(&self.rem_home).unwrap();
        fs::write(
            self.rem_home.join("config.toml"),
            format!(
                "active_profile = \"test\"\ndefault_search = \"auto\"\n\n[[profiles]]\nname = \"test\"\nroot = {:?}\nstorage = \"local\"\n",
                vault.display().to_string()
            ),
        )
        .unwrap();
        fs::write(
            vault.join("memories/short/list-header-id.md"),
            "---\nid: list-header-id\ntype: short\nscope: project\nkind: decision\nstatus: active\ncreated_at: 1\nupdated_at: 1\ntags: []\n---\n# Header Contract\nA readable list row.\n",
        )
        .unwrap();
    }

    fn create_conflict_list_fixture(&self) {
        let vault = self.root.join("conflict-vault");
        fs::create_dir_all(vault.join(".rem/cache")).unwrap();
        fs::create_dir_all(&self.rem_home).unwrap();
        fs::write(
            self.rem_home.join("config.toml"),
            format!(
                "active_profile = \"test\"\ndefault_search = \"auto\"\n\n[[profiles]]\nname = \"test\"\nroot = {:?}\nstorage = \"local\"\n",
                vault.display().to_string()
            ),
        )
        .unwrap();
        let conn = rusqlite::Connection::open(vault.join(".rem/cache/index.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE semantic_conflicts (
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               status TEXT NOT NULL,
               evidence_hash TEXT NOT NULL,
               decision TEXT,
               accepted_evidence_hash TEXT,
               accepted_at TEXT,
               reason TEXT,
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
               PRIMARY KEY (conflict_id, ordinal)
             );
             INSERT INTO semantic_conflicts
               (id, kind, status, evidence_hash, decision, accepted_evidence_hash,
                accepted_at, reason, scope, subject_id, subject, relation, member_count)
             VALUES
               ('conflict-header-id', 'exact-active-duplicate', 'open',
                'evidence-0000000000000000', NULL, NULL, NULL, NULL,
                'user', NULL, NULL, NULL, 2);
             INSERT INTO semantic_conflict_members
               (conflict_id, ordinal, memory_id, memory_path, memory_title, excerpt)
             VALUES
               ('conflict-header-id', 0, 'memory-a', '/vault/a.md', 'A', 'same'),
               ('conflict-header-id', 1, 'memory-b', '/vault/b.md', 'B', 'same');",
        )
        .unwrap();
    }
}

impl Drop for TempTuiProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn expect_is_available() -> bool {
    Command::new("expect")
        .arg("-v")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn configure_tui_first_run_persists_finder_and_picker_values() {
    if !expect_is_available() {
        eprintln!("skipping PTY test because expect is not installed");
        return;
    }

    let project = TempTuiProject::new("first-run");
    let target = project.home.join("outside-work/alpha/finder-jk-target");
    fs::create_dir_all(&target).unwrap();

    let output = project.configure_with_expect();
    assert!(
        output.status.success(),
        "configure TUI failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = fs::read_to_string(project.rem_home.join("config.toml")).unwrap();
    assert!(config.contains("default_search = \"bm25\""));
    assert!(config.contains("storage = \"obsidian\""));
    assert!(
        config.contains(&format!("root = {:?}", target.display().to_string())),
        "expected finder root {} in config:\n{config}",
        target.display()
    );

    let reloaded = Command::new(env!("CARGO_BIN_EXE_rem"))
        .env("REM_HOME", &project.rem_home)
        .env("HOME", &project.home)
        .current_dir(&project.work)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(reloaded.status.success());
    let reloaded = String::from_utf8(reloaded.stdout).unwrap();
    assert!(reloaded.contains("default_search = \"bm25\""));
    assert!(reloaded.contains("storage = \"obsidian\""));
}

#[test]
fn configure_tui_saves_a_fresh_uninitialized_root() {
    if !expect_is_available() {
        eprintln!("skipping PTY test because expect is not installed");
        return;
    }

    let project = TempTuiProject::new("fresh-save");
    let output = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) configure
after 400
send -- "S"
after 200
send -- "q"
expect eof
"#,
    );
    assert!(
        output.status.success(),
        "configure TUI failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = fs::read_to_string(project.rem_home.join("config.toml")).unwrap();
    let default_root = fs::canonicalize(&project.work).unwrap().join("rem");
    assert!(config.contains(&format!("root = {:?}", default_root.display().to_string())));

    let reloaded = Command::new(env!("CARGO_BIN_EXE_rem"))
        .env("REM_HOME", &project.rem_home)
        .env("HOME", &project.home)
        .current_dir(&project.work)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(reloaded.status.success());
}

#[test]
fn automatic_color_is_enabled_for_a_real_tty() {
    if !expect_is_available() {
        eprintln!("skipping PTY test because expect is not installed");
        return;
    }

    let project = TempTuiProject::new("color-auto");
    let colored = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) profile list
expect eof
"#,
    );
    assert!(colored.status.success());
    let colored = String::from_utf8_lossy(&colored.stdout);
    assert!(colored.contains("no profiles configured"));
    assert!(
        colored.contains("\u{1b}["),
        "expected ANSI output: {colored:?}"
    );

    let plain = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) --color never profile list
expect eof
"#,
    );
    assert!(plain.status.success());
    let plain = String::from_utf8_lossy(&plain.stdout);
    assert!(plain.contains("no profiles configured"));
    assert!(
        !plain.contains("\u{1b}["),
        "unexpected ANSI output: {plain:?}"
    );
}

#[test]
fn list_shows_aligned_column_headers_only_for_a_terminal() {
    if !expect_is_available() {
        eprintln!("skipping PTY test because expect is not installed");
        return;
    }

    let project = TempTuiProject::new("list-headers");
    project.create_list_fixture();
    let output = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) --color never list
expect eof
"#,
    );
    assert!(output.status.success());

    let output = String::from_utf8_lossy(&output.stdout);
    let header = output
        .lines()
        .find(|line| line.trim_start().starts_with("ID"))
        .unwrap_or_else(|| panic!("missing list header in PTY output: {output:?}"));
    let positions =
        ["ID", "TYPE", "SCOPE", "KIND", "TITLE"].map(|label| header.find(label).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(output.contains("list-header-id"));
    assert!(output.contains("Header Contract"));
    assert!(!output.contains('\t'));

    let empty = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) --color never list --long
expect eof
"#,
    );
    assert!(empty.status.success());
    let empty = String::from_utf8_lossy(&empty.stdout);
    assert!(empty.lines().any(|line| {
        ["ID", "TYPE", "SCOPE", "KIND", "TITLE"]
            .iter()
            .all(|label| line.contains(label))
    }));
    assert!(!empty.contains("list-header-id"));
}

#[test]
fn conflict_list_shows_aligned_headers_for_a_terminal() {
    if !expect_is_available() {
        eprintln!("skipping PTY test because expect is not installed");
        return;
    }

    let project = TempTuiProject::new("conflict-list-headers");
    project.create_conflict_list_fixture();
    let output = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) --color never conflict list
expect eof
"#,
    );
    assert!(output.status.success());
    let output = String::from_utf8_lossy(&output.stdout);
    let header = output
        .lines()
        .find(|line| line.trim_start().starts_with("ID"))
        .unwrap_or_else(|| panic!("missing conflict header in PTY output: {output:?}"));
    let positions = [
        "ID", "STATUS", "KIND", "SCOPE", "SUBJECT", "RELATION", "MEMBERS",
    ]
    .map(|label| header.find(label).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(output.contains("conflict-header-id"));
    assert!(output.contains("exact-active-duplicate"));
    assert!(!output.contains('\t'));

    let empty = project.run_expect(
        r#"
set timeout 10
spawn -noecho $env(REM_TUI_BIN) --color never conflict list --scope project
expect eof
"#,
    );
    assert!(empty.status.success());
    let empty = String::from_utf8_lossy(&empty.stdout);
    assert!(empty.lines().any(|line| {
        [
            "ID", "STATUS", "KIND", "SCOPE", "SUBJECT", "RELATION", "MEMBERS",
        ]
        .iter()
        .all(|label| line.contains(label))
    }));
    assert!(!empty.contains("conflict-header-id"));
}
