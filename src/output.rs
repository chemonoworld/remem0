use std::{
    env, fmt,
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use clap::ValueEnum;
use owo_colors::Style;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl fmt::Display for ColorChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Success,
    Warning,
    Error,
    Info,
    Id,
    Short,
    Long,
    Scope,
    Kind,
    Title,
    Path,
    Number,
    Source,
    Key,
    Value,
    Prompt,
    Muted,
    Added,
    Removed,
    DiffHeader,
}

#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

static STDOUT_COLOR: AtomicBool = AtomicBool::new(false);
static STDERR_COLOR: AtomicBool = AtomicBool::new(false);

pub fn configure(choice: ColorChoice) {
    let (stdout, stderr) = resolve_color(
        choice,
        color_disabled_by_environment(),
        color_forced_by_environment(),
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
    );
    STDOUT_COLOR.store(stdout, Ordering::Relaxed);
    STDERR_COLOR.store(stderr, Ordering::Relaxed);
}

pub fn line(value: impl fmt::Display) {
    println!("{value}");
}

pub fn error(value: impl fmt::Display) {
    eprintln!(
        "{}: {}",
        paint_stream("error", Tone::Error, OutputStream::Stderr),
        paint_stream(value, Tone::Error, OutputStream::Stderr)
    );
}

pub fn warning(value: impl fmt::Display) {
    eprintln!(
        "{}\t{}",
        paint_stream("warn", Tone::Warning, OutputStream::Stderr),
        value
    );
}

pub fn prompt(value: impl fmt::Display) -> io::Result<()> {
    print!("{}", paint(value, Tone::Prompt));
    io::stdout().flush()
}

pub fn paint(value: impl fmt::Display, tone: Tone) -> String {
    paint_stream(value, tone, OutputStream::Stdout)
}

pub fn row<const N: usize>(cells: [(String, Tone); N]) -> String {
    cells
        .into_iter()
        .map(|(value, tone)| paint(value, tone))
        .collect::<Vec<_>>()
        .join("\t")
}

pub fn key_value(key: impl fmt::Display, value: impl fmt::Display, tone: Tone) -> String {
    format!("{}={}", paint(key, Tone::Key), paint(value, tone))
}

pub fn colon_value(key: impl fmt::Display, value: impl fmt::Display, tone: Tone) -> String {
    format!("{}: {}", paint(key, Tone::Key), paint(value, tone))
}

pub fn action(verb: impl fmt::Display, detail: impl fmt::Display, tone: Tone) -> String {
    format!("{} {}", paint(verb, tone), detail)
}

pub fn memory_type_tone(memory_type: &str) -> Tone {
    match memory_type {
        "short" => Tone::Short,
        "long" => Tone::Long,
        _ => Tone::Info,
    }
}

pub fn change_tone(kind: &str) -> Tone {
    match kind {
        "added" => Tone::Success,
        "deleted" => Tone::Error,
        "modified" => Tone::Warning,
        "renamed" | "copied" => Tone::Info,
        "untracked" => Tone::Short,
        _ => Tone::Value,
    }
}

pub fn action_tone(action: &str) -> Tone {
    match action {
        "add" | "append" | "update" | "supersede" | "include" | "commit" => Tone::Success,
        "no-op" | "restore" => Tone::Warning,
        "abort" | "delete" => Tone::Error,
        _ => Tone::Value,
    }
}

pub fn markdown(raw: &str) {
    document(raw, highlight_markdown_line);
}

pub fn toml(raw: &str) {
    document(raw, highlight_toml_line);
}

pub fn diff(raw: &str) {
    document(raw, highlight_diff_line);
}

fn document(raw: &str, highlight: fn(&str) -> String) {
    if !STDOUT_COLOR.load(Ordering::Relaxed) {
        print!("{raw}");
        return;
    }

    for segment in raw.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        print!("{}{newline}", highlight(line));
    }
}

fn highlight_markdown_line(line: &str) -> String {
    if line == "---" {
        return paint(line, Tone::Muted);
    }
    if line.starts_with('#') {
        return paint(line, Tone::Info);
    }
    if line.trim_start().starts_with("@fact ") {
        return paint(line, Tone::Short);
    }
    if line.trim_start().starts_with("```") {
        return paint(line, Tone::Warning);
    }
    if let Some((key, value)) = line.split_once(':')
        && !key.contains(char::is_whitespace)
    {
        return format!("{}:{}", paint(key, Tone::Key), value);
    }
    if let Some(rest) = line.strip_prefix("- ") {
        return format!("{} {rest}", paint("-", Tone::Info));
    }
    if line.starts_with('>') {
        return paint(line, Tone::Muted);
    }
    line.to_string()
}

fn highlight_toml_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return paint(line, Tone::Muted);
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return paint(line, Tone::Short);
    }
    if let Some((key, value)) = line.split_once('=') {
        return format!("{}={}", paint(key.trim_end(), Tone::Key), value);
    }
    line.to_string()
}

fn highlight_diff_line(line: &str) -> String {
    if line.starts_with("diff ") || line.starts_with("index ") {
        paint(line, Tone::Muted)
    } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        paint(line, Tone::DiffHeader)
    } else if line.starts_with('+') {
        paint(line, Tone::Added)
    } else if line.starts_with('-') {
        paint(line, Tone::Removed)
    } else {
        line.to_string()
    }
}

fn paint_stream(value: impl fmt::Display, tone: Tone, stream: OutputStream) -> String {
    let value = value.to_string();
    let enabled = match stream {
        OutputStream::Stdout => STDOUT_COLOR.load(Ordering::Relaxed),
        OutputStream::Stderr => STDERR_COLOR.load(Ordering::Relaxed),
    };
    paint_with(value, tone, enabled)
}

fn paint_with(value: String, tone: Tone, enabled: bool) -> String {
    if !enabled {
        return value;
    }
    style(tone).style(value).to_string()
}

fn style(tone: Tone) -> Style {
    match tone {
        Tone::Success => Style::new().bright_green().bold(),
        Tone::Warning => Style::new().bright_yellow().bold(),
        Tone::Error => Style::new().bright_red().bold(),
        Tone::Info => Style::new().bright_blue().bold(),
        Tone::Id => Style::new().bright_cyan().bold(),
        Tone::Short => Style::new().bright_magenta().bold(),
        Tone::Long => Style::new().bright_blue().bold(),
        Tone::Scope => Style::new().yellow(),
        Tone::Kind => Style::new().green(),
        Tone::Title => Style::new().bold(),
        Tone::Path => Style::new().cyan().dimmed(),
        Tone::Number => Style::new().bright_yellow(),
        Tone::Source => Style::new().magenta(),
        Tone::Key => Style::new().cyan(),
        Tone::Value => Style::new(),
        Tone::Prompt => Style::new().bright_cyan().bold(),
        Tone::Muted => Style::new().bright_black(),
        Tone::Added => Style::new().green(),
        Tone::Removed => Style::new().red(),
        Tone::DiffHeader => Style::new().cyan().bold(),
    }
}

fn color_disabled_by_environment() -> bool {
    env_nonempty("NO_COLOR") || env::var_os("CLICOLOR").is_some_and(|value| value == "0")
}

fn color_forced_by_environment() -> bool {
    env_nonzero("CLICOLOR_FORCE") || env_nonzero("FORCE_COLOR")
}

fn env_nonempty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn env_nonzero(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
}

fn resolve_color(
    choice: ColorChoice,
    disabled: bool,
    forced: bool,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
) -> (bool, bool) {
    match choice {
        ColorChoice::Always => (true, true),
        ColorChoice::Never => (false, false),
        ColorChoice::Auto if disabled => (false, false),
        ColorChoice::Auto if forced => (true, true),
        ColorChoice::Auto => (stdout_is_terminal, stderr_is_terminal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_style_preserves_machine_output() {
        assert_eq!(
            paint_with("memory-id".to_string(), Tone::Id, false),
            "memory-id"
        );
    }

    #[test]
    fn enabled_style_wraps_and_resets_text() {
        let rendered = paint_with("ok".to_string(), Tone::Success, true);
        assert!(rendered.starts_with("\u{1b}["));
        assert!(rendered.ends_with("\u{1b}[0m"));
        assert!(rendered.contains("ok"));
    }

    #[test]
    fn color_choice_has_stable_cli_values() {
        assert_eq!(ColorChoice::Auto.to_string(), "auto");
        assert_eq!(ColorChoice::Always.to_string(), "always");
        assert_eq!(ColorChoice::Never.to_string(), "never");
    }

    #[test]
    fn automatic_color_respects_streams_and_environment_precedence() {
        assert_eq!(
            resolve_color(ColorChoice::Auto, false, false, true, false),
            (true, false)
        );
        assert_eq!(
            resolve_color(ColorChoice::Auto, false, true, false, false),
            (true, true)
        );
        assert_eq!(
            resolve_color(ColorChoice::Auto, true, true, true, true),
            (false, false)
        );
        assert_eq!(
            resolve_color(ColorChoice::Always, true, false, false, false),
            (true, true)
        );
        assert_eq!(
            resolve_color(ColorChoice::Never, false, true, true, true),
            (false, false)
        );
    }

    #[test]
    fn tone_mapping_is_semantic() {
        assert_eq!(memory_type_tone("short"), Tone::Short);
        assert_eq!(memory_type_tone("long"), Tone::Long);
        assert_eq!(change_tone("deleted"), Tone::Error);
        assert_eq!(change_tone("modified"), Tone::Warning);
        assert_eq!(action_tone("no-op"), Tone::Warning);
        assert_eq!(action_tone("abort"), Tone::Error);
    }
}
