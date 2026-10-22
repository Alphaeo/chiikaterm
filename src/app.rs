use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::fx::fuzzy_match;
use crate::keys::{encode_key, encode_paste};
use crate::session::{Session, Status};
use crate::theme::{THEMES, Theme};
use crate::ui;

/// Key that leaves Terminal mode; never forwarded to the PTY.
pub const LEAVE_TERMINAL: (KeyCode, KeyModifiers) = (KeyCode::Char('q'), KeyModifiers::CONTROL);

/// Lines scrolled per mouse wheel notch.
const WHEEL_LINES: usize = 3;

/// Longest text accepted by rename and the palette (fits the sidebar).
const MAX_INPUT_LEN: usize = 20;

/// Duration of the panel wipe when switching sessions.
pub const SWITCH_ANIM: Duration = Duration::from_millis(200);
pub const TOAST_LIFE: Duration = Duration::from_secs(4);
pub const TOAST_SLIDE: Duration = Duration::from_millis(150);
pub const TOAST_FADE: Duration = Duration::from_millis(500);
const MAX_TOASTS: usize = 3;

pub enum AppEvent {
    Input(Event),
    /// Session id and number of bytes read.
    PtyOutput(u32, usize),
    PtyClosed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Workspace,
    Terminal,
    Rename,
    Palette,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Focus,
    Leave,
    New,
    /// Open the palette with this query already typed.
    Palette(&'static str),
    Rename,
    Close,
    Next,
    Prev,
    /// Scroll the history one page; `true` = towards older output.
    ScrollPage(bool),
    InputChar(char),
    InputBackspace,
    Confirm,
    Cancel,
    Send(Vec<u8>),
}

/// What a palette entry does.
#[derive(Clone, Copy)]
pub enum Command {
    New(&'static ShellSpec),
    GoTo(usize),
    Rename,
    Close,
    Theme(usize),
    Quit,
}

pub struct PaletteItem {
    pub label: String,
    pub command: Command,
    /// Char indices of `label` matched by the query.
    pub hits: Vec<usize>,
}

pub struct Toast {
    pub text: String,
    pub ok: bool,
    pub born: Instant,
}

/// A shell that can be started in a session.
pub struct ShellSpec {
    pub label: &'static str,
    /// `None` = the user's default shell (`$SHELL`).
    prog: Option<&'static str>,
    args: &'static [&'static str],
}

/// Known shells; `n` starts the first one that spawns, the palette offers each of them.
#[cfg(windows)]
const SHELLS: &[ShellSpec] = &[
    ShellSpec {
        label: "pwsh",
        prog: Some("pwsh.exe"),
        args: &["-NoLogo"],
    },
    ShellSpec {
        label: "powershell",
        prog: Some("powershell.exe"),
        args: &["-NoLogo"],
    },
    ShellSpec {
        label: "cmd",
        prog: Some("cmd.exe"),
        args: &[],
    },
    ShellSpec {
        label: "wsl",
        prog: Some("wsl.exe"),
        args: &[],
    },
];
#[cfg(not(windows))]
const SHELLS: &[ShellSpec] = &[
    ShellSpec {
        label: "shell",
        prog: None,
        args: &[],
    },
    ShellSpec {
        label: "bash",
        prog: Some("bash"),
        args: &[],
    },
    ShellSpec {
        label: "zsh",
        prog: Some("zsh"),
        args: &[],
    },
    ShellSpec {
        label: "fish",
        prog: Some("fish"),
        args: &[],
    },
];

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub mode: Mode,
    pub next_id: u32,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Text being typed in Rename and Palette modes.
    pub input: String,
    /// Highlighted palette entry.
    pub picked: usize,
    pub toasts: Vec<Toast>,
    /// When the selected session last changed (drives the panel wipe).
    pub switched_at: Option<Instant>,
    /// Reference for animations that loop (spinners).
    pub epoch: Instant,
    theme: usize,
    area: Rect,
    tx: Sender<AppEvent>,
}

impl App {
    /// `area` is the full terminal area; the first shell is started at the panel size.
    pub fn new(area: Rect, tx: Sender<AppEvent>) -> App {
        let mut app = App {
            sessions: Vec::new(),
            selected: 0,
            mode: Mode::Workspace,
            next_id: 1,
            should_quit: false,
            status_msg: None,
            input: String::new(),
            picked: 0,
            toasts: Vec::new(),
            switched_at: None,
            epoch: Instant::now(),
            theme: 0,
            area,
            tx,
        };
        app.new_session(&available_shells());
        // Start typing straight into the first shell, like tmux.
        if !app.sessions.is_empty() {
            app.mode = Mode::Terminal;
        }
        app
    }

    pub fn theme(&self) -> &'static Theme {
        &THEMES[self.theme]
    }

    pub fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key),
            AppEvent::Input(Event::Mouse(mouse)) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => self.click(mouse.column, mouse.row),
                MouseEventKind::ScrollUp => self.wheel(mouse.column, mouse.row, true),
                MouseEventKind::ScrollDown => self.wheel(mouse.column, mouse.row, false),
                _ => {}
            },
            AppEvent::Input(Event::Paste(text)) if self.mode == Mode::Terminal => {
                let bracketed = self
                    .current_screen(|screen| screen.bracketed_paste())
                    .unwrap_or(false);
                self.send(&encode_paste(&text, bracketed));
            }
            AppEvent::Input(Event::Resize(cols, rows)) => {
                self.resize_all(Rect::new(0, 0, cols, rows))
            }
            AppEvent::Input(_) => {}
            AppEvent::PtyOutput(id, bytes) => {
                if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                    session.record_output(Instant::now(), bytes);
                }
            }
            AppEvent::PtyClosed(id) => tracing::debug!(id, "pty reader closed"),
        }
    }

    /// Per-frame housekeeping: exits (with a toast), activity windows, expired toasts.
    pub fn tick(&mut self, now: Instant) {
        for session in &mut self.sessions {
            session.tick(now);
            if session.poll_exit() {
                let (text, ok) = match session.status {
                    Status::Exited(Some(0)) => (format!("{} exited", session.name), true),
                    Status::Exited(Some(code)) => {
                        (format!("{} exited ({code})", session.name), false)
                    }
                    _ => (format!("{} killed", session.name), false),
                };
                self.toasts.push(Toast {
                    text,
                    ok,
                    born: now,
                });
            }
        }
        self.toasts
            .retain(|t| now.saturating_duration_since(t.born) < TOAST_LIFE);
        let excess = self.toasts.len().saturating_sub(MAX_TOASTS);
        self.toasts.drain(..excess);
    }

    /// How long the main loop may sleep before the next frame is due.
    pub fn frame_interval(&self, now: Instant) -> Duration {
        let switching = self
            .switched_at
            .is_some_and(|t| now.saturating_duration_since(t) < SWITCH_ANIM);
        let toast_moving = self.toasts.iter().any(|t| {
            let age = now.saturating_duration_since(t.born);
            age < TOAST_SLIDE || age > TOAST_LIFE - TOAST_FADE
        });
        if switching || toast_moving {
            Duration::from_millis(16)
        } else if self.sessions.iter().any(|s| s.is_busy(now)) {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(250)
        }
    }

    pub fn kill_all(&mut self) {
        self.sessions.iter_mut().for_each(Session::kill);
    }

    /// Palette entries matching the current query, in display order.
    pub fn palette_items(&self) -> Vec<PaletteItem> {
        let mut entries: Vec<(String, Command)> = available_shells()
            .into_iter()
            .map(|shell| (format!("new {}", shell.label), Command::New(shell)))
            .collect();
        entries.extend(
            self.sessions
                .iter()
                .enumerate()
                .map(|(i, s)| (format!("go to {}", s.name), Command::GoTo(i))),
        );
        if !self.sessions.is_empty() {
            entries.push(("rename session".into(), Command::Rename));
            entries.push(("close session".into(), Command::Close));
        }
        entries.extend(
            THEMES
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("theme: {}", t.name), Command::Theme(i))),
        );
        entries.push(("quit chiika".into(), Command::Quit));
        entries
            .into_iter()
            .filter_map(|(label, command)| {
                let hits = fuzzy_match(&self.input, &label)?;
                Some(PaletteItem {
                    label,
                    command,
                    hits,
                })
            })
            .collect()
    }

    /// Runs `f` on the screen of the selected session.
    fn current_screen<T>(&self, f: impl FnOnce(&vt100::Screen) -> T) -> Option<T> {
        let session = self.sessions.get(self.selected)?;
        let parser = session.parser.lock().unwrap();
        Some(f(parser.screen()))
    }

    /// Selects session `index` and plays the switch animation if it changed.
    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.selected = index;
            self.switched_at = Some(Instant::now());
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let app_cursor = self
            .current_screen(|screen| screen.application_cursor())
            .unwrap_or(false);
        let action = key_action(self.mode, key, !self.sessions.is_empty(), app_cursor);
        if action != Action::None && self.mode != Mode::Terminal {
            self.status_msg = None;
        }
        let len = self.sessions.len();
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::Focus => self.mode = Mode::Terminal,
            Action::Leave | Action::Cancel => self.mode = Mode::Workspace,
            Action::New => self.new_session(&available_shells()),
            Action::Palette(query) => {
                self.input = query.to_string();
                self.picked = 0;
                self.mode = Mode::Palette;
            }
            Action::Rename => self.run(Command::Rename),
            Action::Close => self.run(Command::Close),
            Action::Next if self.mode == Mode::Palette => {
                self.picked = step(self.picked, self.palette_items().len(), true)
            }
            Action::Prev if self.mode == Mode::Palette => {
                self.picked = step(self.picked, self.palette_items().len(), false)
            }
            Action::Next => self.select(step(self.selected, len, true)),
            Action::Prev => self.select(step(self.selected, len, false)),
            Action::ScrollPage(up) => {
                let (rows, _) = ui::pty_size(self.area);
                self.scroll(usize::from(rows.saturating_sub(1).max(1)), up);
            }
            Action::InputChar(c) => {
                if self.input.chars().count() < MAX_INPUT_LEN {
                    self.input.push(c);
                }
                self.picked = 0;
            }
            Action::InputBackspace => {
                self.input.pop();
                self.picked = 0;
            }
            Action::Confirm => self.confirm(),
            Action::Send(bytes) => self.send(&bytes),
        }
    }

    fn confirm(&mut self) {
        match std::mem::replace(&mut self.mode, Mode::Workspace) {
            Mode::Rename => {
                let name = self.input.trim();
                if let (false, Some(session)) =
                    (name.is_empty(), self.sessions.get_mut(self.selected))
                {
                    session.name = name.to_string();
                }
            }
            Mode::Palette => {
                if let Some(item) = self.palette_items().get(self.picked) {
                    self.run(item.command);
                }
            }
            Mode::Workspace | Mode::Terminal => {}
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::New(shell) => self.new_session(&[shell]),
            Command::GoTo(index) => self.select(index),
            Command::Rename => {
                if let Some(session) = self.sessions.get(self.selected) {
                    self.input = session.name.clone();
                    self.mode = Mode::Rename;
                }
            }
            Command::Close => {
                if self.selected < self.sessions.len() {
                    let mut session = self.sessions.remove(self.selected);
                    session.kill();
                    self.selected = clamp_selected(self.selected, self.sessions.len());
                    self.switched_at = Some(Instant::now());
                }
            }
            Command::Theme(index) => self.theme = index,
            Command::Quit => self.should_quit = true,
        }
    }

    /// Writes to the selected session and jumps back to the live screen.
    fn send(&mut self, bytes: &[u8]) {
        let Some(session) = self.sessions.get_mut(self.selected) else {
            return;
        };
        if session.status != Status::Running {
            return;
        }
        session
            .parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_scrollback(0);
        if let Err(e) = session.write(bytes) {
            self.status_msg = Some(format!("write to {} failed: {e}", session.name));
        }
    }

    /// Moves the view of the selected session `lines` into (`up`) or out of the history.
    fn scroll(&mut self, lines: usize, up: bool) {
        let Some(session) = self.sessions.get(self.selected) else {
            return;
        };
        let mut parser = session.parser.lock().unwrap();
        let offset = parser.screen().scrollback();
        let offset = if up {
            offset.saturating_add(lines)
        } else {
            offset.saturating_sub(lines)
        };
        parser.screen_mut().set_scrollback(offset);
    }

    fn wheel(&mut self, column: u16, row: u16, up: bool) {
        let (_, main, _) = ui::layout(self.area);
        if !main.contains(Position::new(column, row)) {
            return;
        }
        // Full-screen programs (alternate screen) have no history: send arrows like xterm.
        let Some((alternate, app_cursor)) =
            self.current_screen(|s| (s.alternate_screen(), s.application_cursor()))
        else {
            return;
        };
        if alternate {
            let code = if up { KeyCode::Up } else { KeyCode::Down };
            let arrow =
                encode_key(KeyEvent::new(code, KeyModifiers::NONE), app_cursor).unwrap_or_default();
            self.send(&arrow.repeat(WHEEL_LINES));
        } else {
            self.scroll(WHEEL_LINES, up);
        }
    }

    fn click(&mut self, column: u16, row: u16) {
        let (sidebar, main, _) = ui::layout(self.area);
        let cards = ui::cards_area(sidebar);
        let at = Position::new(column, row);
        if main.contains(at) && !self.sessions.is_empty() && self.mode == Mode::Workspace {
            self.mode = Mode::Terminal;
            return;
        }
        if !cards.contains(at) {
            return;
        }
        let first = ui::first_visible(self.selected, self.sessions.len(), cards.height);
        let index = first + usize::from((row - cards.y) / ui::CARD_HEIGHT);
        if index < self.sessions.len() {
            self.select(index);
        }
    }

    fn resize_all(&mut self, area: Rect) {
        let old = ui::pty_size(self.area);
        self.area = area;
        let (rows, cols) = ui::pty_size(area);
        if (rows, cols) == old {
            return;
        }
        for session in &mut self.sessions {
            if let Err(e) = session.resize(rows, cols) {
                tracing::warn!(id = session.id, "resize failed: {e:#}");
            }
        }
    }

    /// Starts a session with the first of `shells` that spawns.
    fn new_session(&mut self, shells: &[&ShellSpec]) {
        let id = self.next_id;
        let (rows, cols) = ui::pty_size(self.area);
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                self.status_msg = Some(format!("cannot start a shell: {e}"));
                return;
            }
        };
        let mut last_err = anyhow::anyhow!("no shell found");
        for shell in shells {
            let name = format!("{} {id}", shell.label);
            match Session::spawn(id, name, shell.command(&cwd), size, self.tx.clone()) {
                Ok(session) => {
                    tracing::info!(id, shell = shell.label, "session started");
                    self.sessions.push(session);
                    self.selected = self.sessions.len() - 1;
                    self.switched_at = Some(Instant::now());
                    self.next_id += 1;
                    return;
                }
                Err(e) => {
                    tracing::warn!(id, shell = shell.label, "shell spawn failed: {e:#}");
                    last_err = e;
                }
            }
        }
        self.status_msg = Some(format!("cannot start a shell: {last_err:#}"));
    }
}

impl ShellSpec {
    fn command(&self, cwd: &std::path::Path) -> CommandBuilder {
        let mut cmd = match self.prog {
            Some(prog) => {
                let mut cmd = CommandBuilder::new(prog);
                cmd.args(self.args);
                cmd
            }
            None => CommandBuilder::new_default_prog(),
        };
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd
    }
}

/// Shells whose executable is found in `PATH` (the default shell always is).
fn available_shells() -> Vec<&'static ShellSpec> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    SHELLS
        .iter()
        .filter(|shell| {
            shell.prog.is_none_or(|prog| {
                // symlink_metadata: Store aliases (pwsh in WindowsApps) are reparse points that
                // `exists()` can fail to follow.
                std::env::split_paths(&path).any(|dir| dir.join(prog).symlink_metadata().is_ok())
            })
        })
        .collect()
}

/// Maps a key event to an action, without side effects.
pub fn key_action(mode: Mode, key: KeyEvent, has_session: bool, app_cursor: bool) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Single-letter commands only fire without Ctrl/Alt (Ctrl+Q must not quit chiika).
    let plain = !ctrl && !alt;
    // Ctrl+Alt is AltGr on Windows ('@', '{'…).
    let typed = |c: char| (plain || (ctrl && alt)).then_some(Action::InputChar(c));
    match mode {
        Mode::Terminal if is_leave(key) => Action::Leave,
        Mode::Terminal if shift && key.code == KeyCode::PageUp => Action::ScrollPage(true),
        Mode::Terminal if shift && key.code == KeyCode::PageDown => Action::ScrollPage(false),
        Mode::Terminal => encode_key(key, app_cursor).map_or(Action::None, Action::Send),
        Mode::Workspace => match key.code {
            KeyCode::Enter if has_session => Action::Focus,
            KeyCode::Down => Action::Next,
            KeyCode::Up => Action::Prev,
            KeyCode::PageUp => Action::ScrollPage(true),
            KeyCode::PageDown => Action::ScrollPage(false),
            KeyCode::Char('p') if ctrl && !alt => Action::Palette(""),
            KeyCode::Char(_) if !plain => Action::None,
            KeyCode::Char(':') => Action::Palette(""),
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('n') => Action::New,
            KeyCode::Char('N') => Action::Palette("new "),
            KeyCode::Char('r') if has_session => Action::Rename,
            KeyCode::Char('x') if has_session => Action::Close,
            KeyCode::Char('j') => Action::Next,
            KeyCode::Char('k') => Action::Prev,
            _ => Action::None,
        },
        Mode::Rename | Mode::Palette => match key.code {
            KeyCode::Enter => Action::Confirm,
            KeyCode::Esc => Action::Cancel,
            KeyCode::Backspace => Action::InputBackspace,
            KeyCode::Down if mode == Mode::Palette => Action::Next,
            KeyCode::Up if mode == Mode::Palette => Action::Prev,
            KeyCode::Char('n') if ctrl && mode == Mode::Palette => Action::Next,
            KeyCode::Char('p') if ctrl && mode == Mode::Palette => Action::Prev,
            KeyCode::Char(c) => typed(c).unwrap_or(Action::None),
            _ => Action::None,
        },
    }
}

fn is_leave(key: KeyEvent) -> bool {
    let code = match key.code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
        code => code,
    };
    (code, key.modifiers - KeyModifiers::SHIFT) == LEAVE_TERMINAL
}

fn step(selected: usize, len: usize, down: bool) -> usize {
    if down {
        (selected + 1).min(len.saturating_sub(1))
    } else {
        selected.saturating_sub(1)
    }
}

fn clamp_selected(selected: usize, len: usize) -> usize {
    selected.min(len.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventState;

    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn selection_is_bounded() {
        assert_eq!(step(0, 3, false), 0);
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 2);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn closing_last_selected_moves_back() {
        assert_eq!(clamp_selected(2, 2), 1);
        assert_eq!(clamp_selected(0, 2), 0);
        assert_eq!(clamp_selected(0, 0), 0);
    }

    #[test]
    fn ctrl_q_leaves_terminal() {
        let ctrl_q = key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let ctrl_shift_q = key(
            KeyCode::Char('Q'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            key_action(Mode::Terminal, ctrl_q, true, false),
            Action::Leave
        );
        assert_eq!(
            key_action(Mode::Terminal, ctrl_shift_q, true, false),
            Action::Leave
        );
    }

    #[test]
    fn ctrl_q_in_workspace_does_not_quit() {
        let ctrl_q = key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(
            key_action(Mode::Workspace, ctrl_q, true, false),
            Action::None
        );
    }

    #[test]
    fn ctrl_c_is_sent_to_terminal() {
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            key_action(Mode::Terminal, ctrl_c, true, false),
            Action::Send(vec![0x03])
        );
    }

    #[test]
    fn q_in_terminal_is_typed() {
        let q = key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Terminal, q, true, false),
            Action::Send(b"q".to_vec())
        );
    }

    #[test]
    fn enter_without_session_stays_in_workspace() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Workspace, enter, false, false),
            Action::None
        );
        assert_eq!(
            key_action(Mode::Workspace, enter, true, false),
            Action::Focus
        );
    }

    #[test]
    fn release_is_ignored() {
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(
            key_action(Mode::Workspace, release, true, false),
            Action::None
        );
    }

    #[test]
    fn shift_page_scrolls_instead_of_typing() {
        let up = key(KeyCode::PageUp, KeyModifiers::SHIFT);
        let down = key(KeyCode::PageDown, KeyModifiers::SHIFT);
        let plain = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Terminal, up, true, false),
            Action::ScrollPage(true)
        );
        assert_eq!(
            key_action(Mode::Terminal, down, true, false),
            Action::ScrollPage(false)
        );
        assert_eq!(
            key_action(Mode::Terminal, plain, true, false),
            Action::Send(b"\x1b[5~".to_vec())
        );
    }

    #[test]
    fn rename_mode_edits_text() {
        let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        let at = key(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let ctrl_a = key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Rename, a, true, false),
            Action::InputChar('a')
        );
        assert_eq!(
            key_action(Mode::Rename, at, true, false),
            Action::InputChar('@')
        );
        assert_eq!(key_action(Mode::Rename, ctrl_a, true, false), Action::None);
        assert_eq!(
            key_action(Mode::Rename, enter, true, false),
            Action::Confirm
        );
        assert_eq!(key_action(Mode::Rename, esc, true, false), Action::Cancel);
    }

    #[test]
    fn rename_and_close_need_a_session() {
        let r = key(KeyCode::Char('r'), KeyModifiers::NONE);
        let x = key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(key_action(Mode::Workspace, r, false, false), Action::None);
        assert_eq!(key_action(Mode::Workspace, x, false, false), Action::None);
        assert_eq!(key_action(Mode::Workspace, r, true, false), Action::Rename);
    }

    #[test]
    fn palette_opens_from_workspace() {
        let colon = key(KeyCode::Char(':'), KeyModifiers::NONE);
        let ctrl_p = key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        let shift_n = key(KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(
            key_action(Mode::Workspace, colon, true, false),
            Action::Palette("")
        );
        assert_eq!(
            key_action(Mode::Workspace, ctrl_p, true, false),
            Action::Palette("")
        );
        assert_eq!(
            key_action(Mode::Workspace, shift_n, true, false),
            Action::Palette("new ")
        );
    }

    #[test]
    fn ctrl_p_in_terminal_goes_to_the_shell() {
        let ctrl_p = key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(
            key_action(Mode::Terminal, ctrl_p, true, false),
            Action::Send(vec![0x10])
        );
    }

    #[test]
    fn palette_types_letters_and_navigates_with_arrows() {
        let j = key(KeyCode::Char('j'), KeyModifiers::NONE);
        let down = key(KeyCode::Down, KeyModifiers::NONE);
        let ctrl_p = key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Palette, j, true, false),
            Action::InputChar('j')
        );
        assert_eq!(key_action(Mode::Palette, down, true, false), Action::Next);
        assert_eq!(key_action(Mode::Palette, ctrl_p, true, false), Action::Prev);
        assert_eq!(
            key_action(Mode::Palette, enter, true, false),
            Action::Confirm
        );
        assert_eq!(key_action(Mode::Palette, esc, true, false), Action::Cancel);
    }

    #[test]
    fn default_shell_is_available() {
        assert!(!available_shells().is_empty());
    }
}
