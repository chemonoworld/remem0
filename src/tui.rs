use std::{io, path::PathBuf, time::Duration};

use color_eyre::eyre::Result;
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

use crate::config::{AppConfig, ConfigStore};

const FIELDS: [Field; 4] = [
    Field::ProfileName,
    Field::DataDir,
    Field::EnableSync,
    Field::Editor,
];

pub fn run(store: ConfigStore) -> Result<()> {
    let config = store.load_or_default()?;
    let mut terminal = TerminalSession::new()?;
    let mut app = ConfigApp::new(config, store.path().to_path_buf());

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            app.handle_key(key, &store)?;
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

#[derive(Debug)]
struct ConfigApp {
    config: AppConfig,
    config_path: PathBuf,
    editing: Option<Field>,
    input: String,
    message: String,
    selected: usize,
    should_quit: bool,
}

impl ConfigApp {
    fn new(config: AppConfig, config_path: PathBuf) -> Self {
        Self {
            config,
            config_path,
            editing: None,
            input: String::new(),
            message: "Use arrow keys, Enter to edit, Space to toggle, S to save, Q to quit."
                .to_string(),
            selected: 0,
            should_quit: false,
        }
    }

    fn selected_field(&self) -> Field {
        FIELDS[self.selected]
    }

    fn handle_key(&mut self, key: KeyEvent, store: &ConfigStore) -> Result<()> {
        if self.editing.is_some() {
            self.handle_edit_key(key);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => self.begin_edit(),
            KeyCode::Char(' ') => self.toggle_selected(),
            KeyCode::Char('r') => {
                self.config = AppConfig::default();
                self.message = "Reset to defaults. Press S to write changes.".to_string();
            }
            KeyCode::Char('s') => {
                store.save(&self.config)?;
                self.message = format!("Saved {}", self.config_path.display());
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Esc => {
                self.editing = None;
                self.input.clear();
                self.message = "Edit cancelled.".to_string();
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(value) => self.input.push(value),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = FIELDS.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    fn begin_edit(&mut self) {
        let field = self.selected_field();

        if field == Field::EnableSync {
            self.toggle_selected();
            return;
        }

        self.input = field.value(&self.config);
        self.editing = Some(field);
        self.message = format!("Editing {}. Enter saves, Esc cancels.", field.label());
    }

    fn toggle_selected(&mut self) {
        if self.selected_field() == Field::EnableSync {
            self.config.enable_sync = !self.config.enable_sync;
            self.message = "Toggled enable-sync. Press S to write changes.".to_string();
        }
    }

    fn commit_edit(&mut self) {
        if let Some(field) = self.editing {
            field.set_value(&mut self.config, self.input.trim().to_string());
            self.message = format!("Updated {}. Press S to write changes.", field.label());
        }

        self.editing = None;
        self.input.clear();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    ProfileName,
    DataDir,
    EnableSync,
    Editor,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::ProfileName => "profile-name",
            Self::DataDir => "data-dir",
            Self::EnableSync => "enable-sync",
            Self::Editor => "editor",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::ProfileName => "Active profile label",
            Self::DataDir => "Directory used for app data",
            Self::EnableSync => "Whether sync features should be enabled",
            Self::Editor => "External editor command",
        }
    }

    fn value(self, config: &AppConfig) -> String {
        match self {
            Self::ProfileName => config.profile_name.clone(),
            Self::DataDir => config.data_dir.display().to_string(),
            Self::EnableSync => config.enable_sync.to_string(),
            Self::Editor => config.editor.clone().unwrap_or_default(),
        }
    }

    fn set_value(self, config: &mut AppConfig, value: String) {
        match self {
            Self::ProfileName => config.profile_name = value,
            Self::DataDir => config.data_dir = PathBuf::from(value),
            Self::EnableSync => {
                config.enable_sync = matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "t" | "yes" | "y" | "on"
                )
            }
            Self::Editor => config.editor = (!value.is_empty()).then_some(value),
        }
    }
}

fn render(frame: &mut Frame<'_>, app: &mut ConfigApp) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);

    render_header(frame, app, chunks[0]);
    render_fields(frame, app, chunks[1]);
    render_footer(frame, app, chunks[2]);

    if let Some(field) = app.editing {
        render_editor(frame, area, field, &app.input);
    }
}

fn render_header(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let title = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "remem0",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" configure"),
        ]),
        Line::from(format!("{}", app.config_path.display())),
    ])
    .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(title, area);
}

fn render_fields(frame: &mut Frame<'_>, app: &mut ConfigApp, area: Rect) {
    let items = FIELDS
        .iter()
        .map(|field| {
            let value = field.value(&app.config);
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<14}", field.label()),
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
        Line::from("Enter edit  Space toggle  S save  R reset  Q quit"),
    ])
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(footer, area);
}

fn render_editor(frame: &mut Frame<'_>, area: Rect, field: Field, input: &str) {
    let width = area.width.saturating_sub(8).min(72);
    let modal = centered_rect(width, 7, area);
    let editor = Paragraph::new(input.to_string())
        .block(Block::default().title(field.label()).borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, modal);
    frame.render_widget(editor, modal);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    Rect {
        x,
        y,
        width,
        height,
    }
}
