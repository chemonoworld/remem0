use std::{
    collections::VecDeque,
    env, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::Duration,
};

use color_eyre::eyre::{Result, eyre};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    config::{AppConfig, ConfigStore, ProfileConfig, StorageMode, normalize_root},
    search::SearchMode,
};

const FIELDS: [Field; 5] = [
    Field::ActiveProfile,
    Field::ProfileRoot,
    Field::StorageMode,
    Field::DefaultSearch,
    Field::Editor,
];
const ROOT_FINDER_BATCH_SIZE: usize = 256;
const ROOT_FINDER_CHANNEL_CAPACITY: usize = 256;
const ROOT_FINDER_FILTER_BATCH_SIZE: usize = 1024;
const ROOT_FINDER_RENDER_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug)]
struct PickerOption {
    value: &'static str,
    help: &'static str,
    enabled: bool,
}

const STORAGE_OPTIONS: &[PickerOption] = &[
    PickerOption {
        value: "local",
        help: "Local Markdown vault",
        enabled: true,
    },
    PickerOption {
        value: "obsidian",
        help: "Obsidian-compatible Markdown vault",
        enabled: true,
    },
    PickerOption {
        value: "git",
        help: "Git-backed Markdown vault",
        enabled: true,
    },
];

const SEARCH_OPTIONS: &[PickerOption] = &[
    PickerOption {
        value: "auto",
        help: "Use grep and BM25 when an index exists",
        enabled: true,
    },
    PickerOption {
        value: "grep",
        help: "Search Markdown directly",
        enabled: true,
    },
    PickerOption {
        value: "bm25",
        help: "Search the local SQLite index",
        enabled: true,
    },
    PickerOption {
        value: "vector",
        help: "Unavailable in v1",
        enabled: false,
    },
    PickerOption {
        value: "all",
        help: "Use every configured local search source",
        enabled: true,
    },
];

pub fn run(store: ConfigStore) -> Result<()> {
    let config = store.load_or_default()?;
    let mut terminal = TerminalSession::new()?;
    let mut app = ConfigApp::new(config, store.path().to_path_buf());

    loop {
        app.poll_root_finder();
        terminal.draw(|frame| render(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key, &store),
                Event::Paste(value) => app.handle_paste(&value),
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, draw: F) -> Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(draw)?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

struct ConfigApp {
    config: AppConfig,
    config_path: PathBuf,
    editing: Option<Field>,
    input: String,
    picker: Option<Picker>,
    root_finder: Option<RootFinder>,
    finder_root: Option<PathBuf>,
    message: String,
    selected: usize,
    should_quit: bool,
    dirty: bool,
}

#[derive(Debug)]
struct Picker {
    field: Field,
    options: &'static [PickerOption],
    selected: usize,
}

impl Picker {
    fn new(field: Field, options: &'static [PickerOption], current_value: &str) -> Self {
        let selected = options
            .iter()
            .position(|option| option.value == current_value)
            .unwrap_or_else(|| {
                options
                    .iter()
                    .position(|option| option.enabled)
                    .unwrap_or(0)
            });
        Self {
            field,
            options,
            selected,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.options.len() as isize;
        if len == 0 {
            return;
        }

        for _ in 0..len {
            self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
            if self.options[self.selected].enabled {
                return;
            }
        }
    }

    fn selected_option(&self) -> PickerOption {
        self.options[self.selected]
    }
}

struct RootFinder {
    search_root: PathBuf,
    query: String,
    candidates: Vec<PathBuf>,
    matches: Vec<usize>,
    selected: usize,
    focus: RootFinderFocus,
    receiver: Receiver<PathBuf>,
    scanning: bool,
    cancellation: Arc<AtomicBool>,
    refilter_cursor: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootFinderFocus {
    Filter,
    Paths,
}

impl RootFinder {
    fn new(search_root: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel(ROOT_FINDER_CHANNEL_CAPACITY);
        let cancellation = Arc::new(AtomicBool::new(false));
        let scanner_cancellation = Arc::clone(&cancellation);
        let scanner_root = search_root.clone();
        let query = search_root.display().to_string();
        std::thread::spawn(move || scan_directories(scanner_root, sender, scanner_cancellation));

        Self {
            candidates: vec![search_root.clone()],
            matches: vec![0],
            search_root,
            query,
            selected: 0,
            focus: RootFinderFocus::Filter,
            receiver,
            scanning: true,
            cancellation,
            refilter_cursor: None,
        }
    }

    fn poll(&mut self) {
        for _ in 0..ROOT_FINDER_BATCH_SIZE {
            match self.receiver.try_recv() {
                Ok(path) => {
                    let index = self.candidates.len();
                    self.candidates.push(path);
                    if self.refilter_cursor.is_none()
                        && fuzzy_path_matches(&self.candidates[index], &self.query)
                    {
                        self.matches.push(index);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.scanning = false;
                    break;
                }
            }
        }

        if let Some(mut cursor) = self.refilter_cursor {
            let end = (cursor + ROOT_FINDER_FILTER_BATCH_SIZE).min(self.candidates.len());
            while cursor < end {
                if fuzzy_path_matches(&self.candidates[cursor], &self.query) {
                    self.matches.push(cursor);
                }
                cursor += 1;
            }
            self.refilter_cursor = (cursor < self.candidates.len()).then_some(cursor);
        }
    }

    fn visible_matching_paths(&self) -> (usize, Vec<&PathBuf>) {
        let start = self
            .selected
            .saturating_sub(ROOT_FINDER_RENDER_LIMIT / 2)
            .min(self.matches.len().saturating_sub(ROOT_FINDER_RENDER_LIMIT));
        let end = (start + ROOT_FINDER_RENDER_LIMIT).min(self.matches.len());
        let paths = self.matches[start..end]
            .iter()
            .filter_map(|index| self.candidates.get(*index))
            .collect();

        (start, paths)
    }

    fn selected_path(&self) -> Option<&PathBuf> {
        self.matches
            .get(self.selected)
            .and_then(|index| self.candidates.get(*index))
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.matches.len() as isize;
        if len > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    fn toggle_focus(&mut self) -> RootFinderFocus {
        self.focus = match self.focus {
            RootFinderFocus::Filter => RootFinderFocus::Paths,
            RootFinderFocus::Paths => RootFinderFocus::Filter,
        };
        self.focus
    }

    fn push_query(&mut self, value: &str) {
        self.focus = RootFinderFocus::Filter;
        self.query.push_str(value);
        self.reset_filter();
    }

    fn pop_query(&mut self) {
        self.focus = RootFinderFocus::Filter;
        self.query.pop();
        self.reset_filter();
    }

    fn clear_query(&mut self) {
        self.focus = RootFinderFocus::Filter;
        self.query.clear();
        self.reset_filter();
    }

    fn reset_filter(&mut self) {
        self.matches.clear();
        self.selected = 0;
        self.refilter_cursor = Some(0);
    }
}

impl Drop for RootFinder {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Relaxed);
    }
}

impl ConfigApp {
    fn new(config: AppConfig, config_path: PathBuf) -> Self {
        Self::with_finder_root(config, config_path, home_directory())
    }

    fn with_finder_root(
        mut config: AppConfig,
        config_path: PathBuf,
        finder_root: Option<PathBuf>,
    ) -> Self {
        ensure_active_profile(&mut config);
        Self {
            config,
            config_path,
            editing: None,
            input: String::new(),
            picker: None,
            root_finder: None,
            finder_root,
            message:
                "Arrows move. Enter applies a field. S saves config. Ctrl-F opens the HOME path finder."
                    .to_string(),
            selected: 0,
            should_quit: false,
            dirty: false,
        }
    }

    fn poll_root_finder(&mut self) {
        if let Some(finder) = self.root_finder.as_mut() {
            finder.poll();
        }
    }

    fn selected_field(&self) -> Field {
        FIELDS[self.selected]
    }

    fn handle_key(&mut self, key: KeyEvent, store: &ConfigStore) {
        if self.root_finder.is_some() {
            self.handle_root_finder_key(key);
            return;
        }

        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }

        if self.editing.is_some() {
            self.handle_text_edit_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('f')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.selected_field() == Field::ProfileRoot =>
            {
                self.open_root_finder();
            }
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => self.begin_edit(),
            KeyCode::Char('r' | 'R') => {
                self.config = AppConfig::default();
                ensure_active_profile(&mut self.config);
                self.dirty = true;
                self.message = "Reset to defaults. Press S to write config changes.".to_string();
            }
            KeyCode::Char('i' | 'I') => self.initialize_vault(),
            KeyCode::Char('s' | 'S') => self.save(store),
            _ => {}
        }
    }

    fn handle_paste(&mut self, value: &str) {
        if self.editing.is_some() {
            self.input.push_str(value);
        } else if let Some(finder) = self.root_finder.as_mut() {
            finder.push_query(value);
        }
    }

    fn initialize_vault(&mut self) {
        let profile = active_profile_mut(&mut self.config).clone();
        let workspace = crate::workspace::Workspace::new(&profile);
        match crate::transaction::run_mutation(
            &workspace,
            "rem: initialize vault",
            crate::transaction::TransactionOptions::default(),
            || workspace.init(),
        ) {
            Ok(_) => {
                self.message = format!(
                    "Initialized {}. Press S to save the profile configuration.",
                    profile.root.display()
                );
            }
            Err(err) => {
                self.message = format!("Could not initialize vault: {err}");
            }
        }
    }

    fn save(&mut self, store: &ConfigStore) {
        match store.save(&self.config) {
            Ok(()) => {
                self.dirty = false;
                self.message = format!("Saved {}", self.config_path.display());
            }
            Err(err) => {
                self.message = format!("Save failed: {err}");
            }
        }
    }

    fn handle_text_edit_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('a' | 'A' | 'u' | 'U'))
        {
            self.input.clear();
            return;
        }

        match key.code {
            KeyCode::Enter => self.commit_text_edit(),
            KeyCode::Esc => {
                self.editing = None;
                self.input.clear();
                self.message = "Edit cancelled.".to_string();
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(value);
            }
            _ => {}
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.picker
                    .as_mut()
                    .expect("picker checked")
                    .move_selection(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker
                    .as_mut()
                    .expect("picker checked")
                    .move_selection(-1);
            }
            KeyCode::Enter => self.commit_picker(),
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
                self.picker = None;
                self.message = "Selection cancelled.".to_string();
            }
            _ => {}
        }
    }

    fn handle_root_finder_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('a' | 'A' | 'u' | 'U'))
        {
            self.root_finder
                .as_mut()
                .expect("root finder checked")
                .clear_query();
            return;
        }

        let paths_focused = self
            .root_finder
            .as_ref()
            .is_some_and(|finder| finder.focus == RootFinderFocus::Paths);

        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                let focus = self
                    .root_finder
                    .as_mut()
                    .expect("root finder checked")
                    .toggle_focus();
                self.message = match focus {
                    RootFinderFocus::Filter => {
                        "Directory finder path filter focused. Edit the HOME path; Tab focuses paths."
                            .to_string()
                    }
                    RootFinderFocus::Paths => {
                        "Directory finder paths focused. Use arrows or j/k; Tab returns to filter."
                            .to_string()
                    }
                };
            }
            KeyCode::Down | KeyCode::Char('j') if paths_focused => self
                .root_finder
                .as_mut()
                .expect("root finder checked")
                .move_selection(1),
            KeyCode::Up | KeyCode::Char('k') if paths_focused => self
                .root_finder
                .as_mut()
                .expect("root finder checked")
                .move_selection(-1),
            KeyCode::Enter => self.commit_root_finder(),
            KeyCode::Esc => {
                self.root_finder = None;
                self.message = "Directory finder cancelled.".to_string();
            }
            KeyCode::Backspace => self
                .root_finder
                .as_mut()
                .expect("root finder checked")
                .pop_query(),
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => self
                .root_finder
                .as_mut()
                .expect("root finder checked")
                .push_query(&value.to_string()),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = FIELDS.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    fn begin_edit(&mut self) {
        let field = self.selected_field();
        if let Some(options) = field.picker_options() {
            let current_value = field.value(&self.config);
            self.picker = Some(Picker::new(field, options, &current_value));
            self.message = format!("Choose {} with arrows, then press Enter.", field.label());
            return;
        }

        self.input = field.value(&self.config);
        self.editing = Some(field);
        self.message = format!(
            "Editing {}. Enter applies, Ctrl-A clears, Esc cancels.",
            field.label()
        );
    }

    fn commit_text_edit(&mut self) {
        let Some(field) = self.editing else {
            return;
        };

        match field.set_value(&mut self.config, self.input.trim().to_string()) {
            Ok(()) => {
                self.editing = None;
                self.input.clear();
                self.dirty = true;
                self.message = format!(
                    "Updated {}. Press S to write config changes.",
                    field.label()
                );
            }
            Err(err) => {
                self.message = format!("Invalid {}: {err}", field.label());
            }
        }
    }

    fn commit_picker(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let option = picker.selected_option();
        if !option.enabled {
            self.message = format!("{} is unavailable: {}", option.value, option.help);
            self.picker = Some(picker);
            return;
        }

        match picker
            .field
            .set_value(&mut self.config, option.value.to_string())
        {
            Ok(()) => {
                self.dirty = true;
                self.message = format!(
                    "Updated {}. Press S to write config changes.",
                    picker.field.label()
                );
            }
            Err(err) => {
                self.message = format!("Invalid {}: {err}", picker.field.label());
                self.picker = Some(picker);
            }
        }
    }

    fn open_root_finder(&mut self) {
        let Some(search_root) = self.finder_root.clone() else {
            self.message =
                "Directory finder needs HOME to point to an existing directory.".to_string();
            return;
        };
        self.root_finder = Some(RootFinder::new(search_root));
        self.message =
            "Directory finder: HOME path is prefilled; edit it to narrow results, Tab focuses paths."
                .to_string();
    }

    fn commit_root_finder(&mut self) {
        let selected_path = self
            .root_finder
            .as_ref()
            .and_then(RootFinder::selected_path)
            .cloned();

        let Some(path) = selected_path else {
            self.message = if self
                .root_finder
                .as_ref()
                .is_some_and(|finder| finder.refilter_cursor.is_some())
            {
                "Still filtering HOME directories; try Enter again in a moment.".to_string()
            } else {
                "No matching directories. Type a different query or press Esc.".to_string()
            };
            return;
        };

        active_profile_mut(&mut self.config).root = normalize_root(path);
        self.root_finder = None;
        self.dirty = true;
        self.message = "Updated profile-root. Press S to write config changes.".to_string();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    ActiveProfile,
    ProfileRoot,
    StorageMode,
    DefaultSearch,
    Editor,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::ActiveProfile => "active-profile",
            Self::ProfileRoot => "profile-root",
            Self::StorageMode => "storage-mode",
            Self::DefaultSearch => "default-search",
            Self::Editor => "editor",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::ActiveProfile => "Profile name; a new name copies the active profile",
            Self::ProfileRoot => "Type a path, or Ctrl-F to search directories under HOME",
            Self::StorageMode => "Choose local, obsidian, or git",
            Self::DefaultSearch => "Choose the default local search mode",
            Self::Editor => "External editor command",
        }
    }

    fn picker_options(self) -> Option<&'static [PickerOption]> {
        match self {
            Self::StorageMode => Some(STORAGE_OPTIONS),
            Self::DefaultSearch => Some(SEARCH_OPTIONS),
            Self::ActiveProfile | Self::ProfileRoot | Self::Editor => None,
        }
    }

    fn value(self, config: &AppConfig) -> String {
        let profile = active_profile(config);
        match self {
            Self::ActiveProfile => config.active_profile.clone(),
            Self::ProfileRoot => profile.root.display().to_string(),
            Self::StorageMode => profile.storage.to_string(),
            Self::DefaultSearch => config.default_search.clone(),
            Self::Editor => config.editor.clone().unwrap_or_default(),
        }
    }

    fn set_value(self, config: &mut AppConfig, value: String) -> Result<()> {
        ensure_active_profile(config);
        match self {
            Self::ActiveProfile => {
                let old_profile = active_profile(config).clone();
                config.active_profile = if value.is_empty() {
                    "default".to_string()
                } else {
                    value
                };
                if config.profile(&config.active_profile).is_err() {
                    config.upsert_profile(ProfileConfig {
                        name: config.active_profile.clone(),
                        root: old_profile.root,
                        storage: old_profile.storage,
                    });
                }
            }
            Self::ProfileRoot => {
                if value.trim().is_empty() {
                    return Err(eyre!("profile root cannot be empty"));
                }
                active_profile_mut(config).root = normalize_root(PathBuf::from(value));
            }
            Self::StorageMode => {
                active_profile_mut(config).storage = StorageMode::parse_config_value(&value)?;
            }
            Self::DefaultSearch => {
                config.default_search = SearchMode::parse_config_value(&value)?.to_string();
            }
            Self::Editor => config.editor = (!value.is_empty()).then_some(value),
        }
        Ok(())
    }
}

fn render(frame: &mut Frame<'_>, app: &ConfigApp) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(5),
        ])
        .split(area);

    render_header(frame, app, chunks[0]);
    render_fields(frame, app, chunks[1]);
    render_footer(frame, app, chunks[2]);

    if let Some(picker) = app.picker.as_ref() {
        render_picker(frame, area, picker);
    } else if let Some(finder) = app.root_finder.as_ref() {
        render_root_finder(frame, area, finder);
    } else if let Some(field) = app.editing {
        render_text_editor(frame, area, field, &app.input);
    }
}

fn render_header(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let state = if app.dirty { " [unsaved]" } else { "" };
    let title = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "rem",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" configure"),
            Span::styled(state, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(format!("{}", app.config_path.display())),
    ])
    .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(title, area);
}

fn render_fields(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let items = FIELDS
        .iter()
        .map(|field| {
            let value = field.value(&app.config);
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<16}", field.label()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(value),
                Span::styled(
                    format!("  {}", field.help()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .block(Block::default().title(" Settings ").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_footer(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let footer = Paragraph::new(vec![
        Line::from(app.message.clone()),
        Line::from("Enter apply/select  S save config  I init vault  R reset  Q quit"),
        Line::from("profile-root: Ctrl-F opens an editable HOME path filter; Tab focuses paths"),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(footer, area);
}

fn render_text_editor(frame: &mut Frame<'_>, area: Rect, field: Field, input: &str) {
    let width = area.width.saturating_sub(8).min(88);
    let modal = centered_rect(width, 7, area);
    let editor = Paragraph::new(input.to_string())
        .block(
            Block::default()
                .title(format!(
                    " {} (Enter apply, Ctrl-A clear, Esc cancel) ",
                    field.label()
                ))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, modal);
    frame.render_widget(editor, modal);
}

fn render_picker(frame: &mut Frame<'_>, area: Rect, picker: &Picker) {
    let height = (picker.options.len() as u16 + 4).min(area.height.saturating_sub(2));
    let modal = centered_rect(area.width.saturating_sub(8).min(88), height, area);
    let items = picker
        .options
        .iter()
        .map(|option| {
            let style = if option.enabled {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<10}", option.value), style),
                Span::styled(option.help, style),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(" Choose {} ", picker.field.label()))
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">");
    let mut state = ListState::default();
    state.select(Some(picker.selected));

    frame.render_widget(Clear, modal);
    frame.render_stateful_widget(list, modal, &mut state);
}

fn render_root_finder(frame: &mut Frame<'_>, area: Rect, finder: &RootFinder) {
    let (matching_offset, matching) = finder.visible_matching_paths();
    let matching_count = finder.matches.len();
    let height = 16.min(area.height.saturating_sub(2));
    let modal = centered_rect(area.width.saturating_sub(6).min(96), height, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(4)])
        .split(modal);
    let query = Paragraph::new(vec![
        Line::from(format!(
            "path filter{}: {}",
            if finder.focus == RootFinderFocus::Filter {
                " [focused]"
            } else {
                ""
            },
            finder.query
        )),
        Line::from(format!(
            "{} · {} matches under {}",
            if finder.refilter_cursor.is_some() {
                "filtering results"
            } else if finder.scanning {
                "searching HOME"
            } else {
                "search complete"
            },
            matching_count,
            finder.search_root.display()
        )),
        Line::from(match finder.focus {
            RootFinderFocus::Filter => {
                "Edit the HOME path or type freely (including j/k) · Ctrl-A clears · Tab focuses paths · Enter selects · Esc cancels"
            }
            RootFinderFocus::Paths => {
                "Paths [focused]: arrows/jk move · Tab returns to filter · Enter selects · Esc cancels"
            }
        }),
    ])
    .block(
        Block::default()
            .title(format!(" Find profile root · {} ", finder.search_root.display()))
            .borders(Borders::ALL),
    )
    .wrap(Wrap { trim: true });
    let items = matching
        .iter()
        .map(|path| ListItem::new(path.display().to_string()))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title(if finder.focus == RootFinderFocus::Paths {
                    " Paths [focused] "
                } else {
                    " Paths (Tab to focus) "
                })
                .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">");
    let mut state = ListState::default();
    state.select(
        (finder.focus == RootFinderFocus::Paths && !matching.is_empty())
            .then_some(finder.selected - matching_offset),
    );

    frame.render_widget(Clear, modal);
    frame.render_widget(query, chunks[0]);
    frame.render_stateful_widget(list, chunks[1], &mut state);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    Rect {
        x,
        y,
        width,
        height,
    }
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn scan_directories(
    search_root: PathBuf,
    sender: SyncSender<PathBuf>,
    cancellation: Arc<AtomicBool>,
) {
    let mut queue = VecDeque::from([search_root]);

    while let Some(directory) = queue.pop_front() {
        if cancellation.load(Ordering::Relaxed) {
            return;
        }

        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut children = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                (file_type.is_dir() && !file_type.is_symlink()).then_some(entry.path())
            })
            .collect::<Vec<_>>();
        children.sort();

        for child in children {
            if cancellation.load(Ordering::Relaxed) || sender.send(child.clone()).is_err() {
                return;
            }
            queue.push_back(child);
        }
    }
}

fn fuzzy_path_matches(path: &Path, query: &str) -> bool {
    let haystack = path.display().to_string().to_lowercase();
    query.split_whitespace().all(|term| {
        let term = term.to_lowercase();
        haystack.contains(&term) || is_subsequence(&haystack, &term)
    })
}

fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut remaining = needle.chars();
    let mut expected = remaining.next();
    for character in haystack.chars() {
        if Some(character) == expected {
            expected = remaining.next();
            if expected.is_none() {
                return true;
            }
        }
    }
    expected.is_none()
}

fn ensure_active_profile(config: &mut AppConfig) {
    if config.profile(&config.active_profile).is_ok() {
        return;
    }

    let root = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("rem");
    config.upsert_profile(ProfileConfig {
        name: config.active_profile.clone(),
        root,
        storage: StorageMode::Local,
    });
}

fn active_profile(config: &AppConfig) -> &ProfileConfig {
    config
        .profile(&config.active_profile)
        .or_else(|_| config.profiles.first().ok_or_else(|| eyre!("no profile")))
        .expect("ensure_active_profile should create a profile")
}

fn active_profile_mut(config: &mut AppConfig) -> &mut ProfileConfig {
    ensure_active_profile(config);
    let index = config
        .profiles
        .iter()
        .position(|profile| profile.name == config.active_profile)
        .unwrap_or(0);
    &mut config.profiles[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn temp_store(name: &str) -> (ConfigStore, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("rem-tui-{name}-{nonce}"));
        (ConfigStore::for_test(root.clone()), root)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    fn select_field(app: &mut ConfigApp, field: Field) {
        app.selected = FIELDS
            .iter()
            .position(|candidate| *candidate == field)
            .unwrap();
    }

    fn wait_for_finder_candidate(app: &mut ConfigApp, expected: &Path) {
        for _ in 0..100 {
            app.poll_root_finder();
            if app
                .root_finder
                .as_ref()
                .is_some_and(|finder| finder.candidates.iter().any(|path| path == expected))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        panic!("finder did not discover {}", expected.display());
    }

    fn wait_for_finder_match(app: &mut ConfigApp, expected: &Path) {
        for _ in 0..100 {
            app.poll_root_finder();
            if app
                .root_finder
                .as_ref()
                .and_then(RootFinder::selected_path)
                .is_some_and(|path| path == expected)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        panic!("finder did not match {}", expected.display());
    }

    #[test]
    fn finder_requires_a_valid_home_directory() {
        let (store, root) = temp_store("missing-home");
        let mut app =
            ConfigApp::with_finder_root(AppConfig::default(), store.path().to_path_buf(), None);
        select_field(&mut app, Field::ProfileRoot);

        app.handle_key(ctrl('f'), &store);

        assert!(app.root_finder.is_none());
        assert!(app.message.contains("HOME"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn closing_finder_requests_background_scan_cancellation() {
        let (_store, root) = temp_store("cancel-finder");
        fs::create_dir_all(&root).unwrap();
        let finder = RootFinder::new(root.clone());
        let cancellation = Arc::clone(&finder.cancellation);

        drop(finder);

        assert!(cancellation.load(Ordering::Relaxed));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fresh_configure_saves_without_a_git_vault_and_uppercase_s_works() {
        let (store, root) = temp_store("fresh-save");
        let mut app = ConfigApp::new(AppConfig::default(), store.path().to_path_buf());

        app.handle_key(key(KeyCode::Char('S')), &store);

        assert!(store.path().is_file());
        assert!(!app.should_quit);
        assert_eq!(store.load_or_default().unwrap().default_search, "auto");
        assert!(app.message.starts_with("Saved "));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn option_pickers_persist_explicit_storage_and_search_values() {
        let (store, root) = temp_store("pickers");
        let mut app = ConfigApp::new(AppConfig::default(), store.path().to_path_buf());

        select_field(&mut app, Field::StorageMode);
        app.handle_key(key(KeyCode::Enter), &store);
        app.handle_key(key(KeyCode::Down), &store);
        app.handle_key(key(KeyCode::Enter), &store);
        assert_eq!(active_profile(&app.config).storage, StorageMode::Obsidian);

        select_field(&mut app, Field::DefaultSearch);
        app.handle_key(key(KeyCode::Enter), &store);
        app.handle_key(key(KeyCode::Down), &store);
        app.handle_key(key(KeyCode::Down), &store);
        app.handle_key(key(KeyCode::Enter), &store);
        assert_eq!(app.config.default_search, "bm25");

        app.handle_key(key(KeyCode::Char('s')), &store);
        let saved = store.load_or_default().unwrap();
        assert_eq!(saved.default_search, "bm25");
        assert_eq!(
            saved.active_profile().unwrap().storage,
            StorageMode::Obsidian
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_root_finder_searches_the_full_injected_home_directory() {
        let (store, root) = temp_store("root-finder");
        let finder_root = root.join("home");
        let profile_root = finder_root.join("current-project/missing-child");
        let direct_root = root.join("typed-root");
        let picked_root = finder_root.join("outside-project/one/two/three/four/picked-directory");
        fs::create_dir_all(&picked_root).unwrap();
        let config = AppConfig {
            active_profile: "default".to_string(),
            default_search: "auto".to_string(),
            editor: None,
            profiles: vec![ProfileConfig {
                name: "default".to_string(),
                root: profile_root.clone(),
                storage: StorageMode::Local,
            }],
        };
        let mut app = ConfigApp::with_finder_root(
            config,
            store.path().to_path_buf(),
            Some(finder_root.clone()),
        );
        select_field(&mut app, Field::ProfileRoot);

        app.handle_key(key(KeyCode::Enter), &store);
        app.handle_key(ctrl('a'), &store);
        for character in direct_root.display().to_string().chars() {
            app.handle_key(key(KeyCode::Char(character)), &store);
        }
        app.handle_key(key(KeyCode::Enter), &store);
        assert_eq!(active_profile(&app.config).root, direct_root);

        active_profile_mut(&mut app.config).root = profile_root;
        app.handle_key(key(KeyCode::Char(' ')), &store);
        app.handle_key(key(KeyCode::Char('f')), &store);
        app.handle_key(key(KeyCode::Char('f')), &store);
        assert!(app.root_finder.is_none());
        app.handle_key(ctrl('f'), &store);
        assert!(app.root_finder.is_some());
        wait_for_finder_candidate(&mut app, &picked_root);
        app.handle_key(ctrl('a'), &store);
        for character in "picked-directory".chars() {
            app.handle_key(key(KeyCode::Char(character)), &store);
        }
        wait_for_finder_match(&mut app, &picked_root);
        app.handle_key(key(KeyCode::Enter), &store);

        assert_eq!(active_profile(&app.config).root, picked_root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_finder_prefills_home_path_and_uses_tab_for_path_focus() {
        let (store, root) = temp_store("finder-focus");
        let finder_root = root.join("home");
        let first = finder_root.join("j-k-first");
        let second = finder_root.join("j-k-second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let mut app = ConfigApp::with_finder_root(
            AppConfig::default(),
            store.path().to_path_buf(),
            Some(finder_root.clone()),
        );
        select_field(&mut app, Field::ProfileRoot);
        app.handle_key(ctrl('f'), &store);
        wait_for_finder_candidate(&mut app, &first);
        wait_for_finder_candidate(&mut app, &second);

        assert_eq!(
            app.root_finder.as_ref().expect("finder is open").query,
            finder_root.display().to_string()
        );
        app.handle_key(ctrl('a'), &store);
        app.handle_key(key(KeyCode::Char('j')), &store);
        app.handle_key(key(KeyCode::Char('k')), &store);
        for _ in 0..100 {
            app.poll_root_finder();
            if app
                .root_finder
                .as_ref()
                .is_some_and(|finder| finder.refilter_cursor.is_none() && finder.matches.len() >= 2)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let finder = app.root_finder.as_ref().expect("finder is open");
        assert_eq!(finder.query, "jk");
        assert_eq!(finder.focus, RootFinderFocus::Filter);
        assert!(finder.matches.len() >= 2);

        app.handle_key(key(KeyCode::Tab), &store);
        let selected = app.root_finder.as_ref().expect("finder is open").selected;
        assert_eq!(
            app.root_finder.as_ref().expect("finder is open").focus,
            RootFinderFocus::Paths
        );
        app.handle_key(key(KeyCode::Char('j')), &store);
        let finder = app.root_finder.as_ref().expect("finder is open");
        assert_eq!(finder.query, "jk");
        assert_ne!(finder.selected, selected);

        app.handle_key(key(KeyCode::BackTab), &store);
        app.handle_key(key(KeyCode::Char('k')), &store);
        let finder = app.root_finder.as_ref().expect("finder is open");
        assert_eq!(finder.focus, RootFinderFocus::Filter);
        assert_eq!(finder.query, "jkk");
        app.root_finder = None;
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_failure_stays_in_the_tui_and_keeps_changes_unsaved() {
        let (store, root) = temp_store("save-error");
        fs::write(&root, "not a directory").unwrap();
        let mut app = ConfigApp::new(AppConfig::default(), store.path().to_path_buf());
        app.dirty = true;

        app.handle_key(key(KeyCode::Char('S')), &store);

        assert!(!app.should_quit);
        assert!(app.dirty);
        assert!(app.message.starts_with("Save failed:"));
        fs::remove_file(root).unwrap();
    }

    #[test]
    fn default_search_picker_skips_unavailable_vector_option() {
        let (store, root) = temp_store("vector-option");
        let mut app = ConfigApp::new(AppConfig::default(), store.path().to_path_buf());
        select_field(&mut app, Field::DefaultSearch);
        app.handle_key(key(KeyCode::Enter), &store);
        app.handle_key(key(KeyCode::Down), &store);
        app.handle_key(key(KeyCode::Down), &store);
        app.handle_key(key(KeyCode::Down), &store);

        assert_eq!(app.picker.as_ref().unwrap().selected_option().value, "all");
        assert_eq!(app.config.default_search, "auto");
        let _ = fs::remove_dir_all(root);
    }
}
