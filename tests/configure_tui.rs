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
            .current_dir(&self.work)
            .output()
            .unwrap()
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
