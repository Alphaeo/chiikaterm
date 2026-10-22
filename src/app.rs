use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::fx::fuzzy_match;
use crate::keys::{encode_key, encode_paste};
use crate::layout::{Dir, Layout, Split};
use crate::session::{Session, Status};
use crate::theme::{THEMES, Theme};
use crate::ui;

/// Key that leaves Terminal mode; never forwarded to the PTY.
pub const LEAVE_TERMINAL: (KeyCode, KeyModifiers) = (KeyCode::Char('q'), KeyModifiers::CONTROL);

/// Lines scrolled per mouse wheel notch.
const WHEEL_LINES: usize = 3;

/// Longest text accepted by rename and the palette (fits the sidebar).
const MAX_INPUT_LEN: usize = 20;

/// Two clicks on the same card within this delay = double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

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
    Menu,
    /// The list of key bindings.
    Help,
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
    Move,
    Split(Split),
    ClosePane,
    FocusPane(Dir),
    ToggleSidebar,
    Help,
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

/// One line of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Folder(u32),
    Session(u32),
}

/// Something that can be renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Folder(u32),
    Session(u32),
}

pub struct Folder {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
}

/// What a palette entry does.
#[derive(Clone, Copy)]
pub enum Command {
    New(&'static ShellSpec),
    GoTo(u32),
    Rename,
    Close,
    NewFolder,
    Split(Split),
    ClosePane,
    ToggleSidebar,
    Theme(usize),
    Quit,
}

pub struct PaletteItem {
    pub label: String,
    pub command: Command,
    /// Char indices of `label` matched by the query.
    pub hits: Vec<usize>,
}

/// What a context-menu entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Rename(Target),
    CloseSession(u32),
    DeleteFolder(u32),
    /// Opens the "move to folder" menu for this session.
    MoveMenu(u32),
    MoveTo(u32, Option<u32>),
    MoveToNewFolder(u32),
    Split(u32, Split),
    ClosePane(u32),
    /// Opens the settings menu.
    Settings,
    Bindings,
    Detach,
    SetTheme(usize),
    ToggleSidebar,
}

impl MenuAction {
    pub fn destructive(self) -> bool {
        matches!(
            self,
            MenuAction::CloseSession(_) | MenuAction::DeleteFolder(_)
        )
    }

    /// Greyed-out entries announce a feature that is not built yet.
    pub fn enabled(self) -> bool {
        self != MenuAction::Detach
    }
}

/// Shown when the greyed-out "detach" entry is picked.
const DETACH_SOON: &str =
    "detach needs the background daemon (planned): quitting chiika still closes its shells";

pub struct Menu {
    pub items: Vec<(String, MenuAction)>,
    pub picked: usize,
    /// Top-left corner requested (the menu is moved to fit on screen).
    pub at: Position,
}

pub struct Toast {
    pub text: String,
    pub ok: bool,
    pub born: Instant,
}

/// Where a new session is shown.
enum Place {
    /// In the focused pane, instead of what it showed.
    Replace,
    /// In a new pane cut from the focused one.
    Split(Split),
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
    pub folders: Vec<Folder>,
    /// Highlighted sidebar row.
    pub cursor: usize,
    /// Panes of the main area; `None` when there is no session.
    pub layout: Option<Layout>,
    /// Session shown in the focused pane: it receives the keyboard.
    pub focused: Option<u32>,
    pub mode: Mode,
    pub next_id: u32,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Text being typed in Rename and Palette modes.
    pub input: String,
    /// Highlighted palette entry.
    pub picked: usize,
    pub menu: Option<Menu>,
    /// Sidebar reduced to a rail of icons.
    pub sidebar_collapsed: bool,
    pub toasts: Vec<Toast>,
    /// When the focused pane last changed content (drives the panel wipe).
    pub switched_at: Option<Instant>,
    /// Reference for animations that loop (spinners).
    pub epoch: Instant,
    rename: Option<Target>,
    next_folder: u32,
    last_click: Option<(Instant, usize)>,
    theme: usize,
    area: Rect,
    tx: Sender<AppEvent>,
}

impl App {
    /// `area` is the full terminal area; the first shell is started at the panel size.
    pub fn new(area: Rect, tx: Sender<AppEvent>) -> App {
        let mut app = App {
            sessions: Vec::new(),
            folders: Vec::new(),
            cursor: 0,
            layout: None,
            focused: None,
            mode: Mode::Workspace,
            next_id: 1,
            should_quit: false,
            status_msg: None,
            input: String::new(),
            picked: 0,
            menu: None,
            sidebar_collapsed: false,
            toasts: Vec::new(),
            switched_at: None,
            epoch: Instant::now(),
            rename: None,
            next_folder: 1,
            last_click: None,
            theme: 0,
            area,
            tx,
        };
        app.new_session(&available_shells(), Place::Replace);
        // Start typing straight into the first shell, like tmux.
        if !app.sessions.is_empty() {
            app.mode = Mode::Terminal;
        }
        app
    }

    pub fn theme(&self) -> &'static Theme {
        &THEMES[self.theme]
    }

    pub fn session(&self, id: u32) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: u32) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn folder(&self, id: u32) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Sidebar lines, top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        let sessions: Vec<(u32, Option<u32>)> =
            self.sessions.iter().map(|s| (s.id, s.folder)).collect();
        build_rows(&self.folders, &sessions)
    }

    fn cursor_row(&self) -> Option<Row> {
        self.rows().get(self.cursor).copied()
    }

    pub fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key),
            AppEvent::Input(Event::Mouse(mouse)) => self.handle_mouse(mouse),
            AppEvent::Input(Event::Paste(text)) if self.mode == Mode::Terminal => {
                let bracketed = self
                    .focused_screen(|screen| screen.bracketed_paste())
                    .unwrap_or(false);
                self.send(&encode_paste(&text, bracketed));
            }
            AppEvent::Input(Event::Resize(cols, rows)) => {
                self.area = Rect::new(0, 0, cols, rows);
                self.relayout();
            }
            AppEvent::Input(_) => {}
            AppEvent::PtyOutput(id, bytes) => {
                if let Some(session) = self.session_mut(id) {
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
                .map(|s| (format!("go to {}", s.name), Command::GoTo(s.id))),
        );
        entries.push(("new folder".into(), Command::NewFolder));
        if self.focused.is_some() {
            entries.push(("split vertical │".into(), Command::Split(Split::Right)));
            entries.push(("split horizontal ─".into(), Command::Split(Split::Down)));
            entries.push(("unsplit (close pane)".into(), Command::ClosePane));
        }
        if self.cursor_row().is_some() {
            entries.push(("rename".into(), Command::Rename));
            entries.push(("close".into(), Command::Close));
        }
        entries.extend(
            THEMES
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("theme: {}", t.name), Command::Theme(i))),
        );
        entries.push(("toggle sidebar".into(), Command::ToggleSidebar));
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

    /// Runs `f` on the screen of the focused session.
    fn focused_screen<T>(&self, f: impl FnOnce(&vt100::Screen) -> T) -> Option<T> {
        let session = self.session(self.focused?)?;
        let parser = session.parser.lock().unwrap();
        Some(f(parser.screen()))
    }

    pub fn sidebar_width(&self) -> u16 {
        if self.sidebar_collapsed {
            ui::RAIL_WIDTH
        } else {
            ui::SIDEBAR_WIDTH
        }
    }

    /// (sidebar, main area, status line) for the current terminal size.
    fn screen(&self) -> (Rect, Rect, Rect) {
        ui::layout(self.area, self.sidebar_width())
    }

    fn main_area(&self) -> Rect {
        self.screen().1
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.relayout();
    }

    /// Gives the keyboard to the pane of session `id` and moves the sidebar cursor to it.
    fn focus(&mut self, id: u32) {
        self.focused = Some(id);
        if let Some(i) = self.rows().iter().position(|r| *r == Row::Session(id)) {
            self.cursor = i;
        }
    }

    /// Shows session `id`: focuses its pane if visible, else puts it in the focused pane.
    fn show_session(&mut self, id: u32) {
        if self.focused == Some(id) {
            return;
        }
        match (&mut self.layout, self.focused) {
            (Some(layout), _) if layout.contains(id) => {}
            (Some(layout), Some(focused)) => {
                layout.replace(focused, id);
                self.switched_at = Some(Instant::now());
            }
            _ => {
                self.layout = Some(Layout::Pane(id));
                self.switched_at = Some(Instant::now());
            }
        }
        self.focus(id);
        self.relayout();
    }

    /// Resizes every visible session to its pane.
    fn relayout(&mut self) {
        let main = self.main_area();
        let Some(layout) = &self.layout else {
            return;
        };
        for (id, rect) in layout.rects(main) {
            let size = ui::inner_size(rect);
            let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) else {
                continue;
            };
            if session.size() != size
                && let Err(e) = session.resize(size.0, size.1)
            {
                tracing::warn!(id, "resize failed: {e:#}");
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let app_cursor = self
            .focused_screen(|screen| screen.application_cursor())
            .unwrap_or(false);
        let action = key_action(self.mode, key, !self.sessions.is_empty(), app_cursor);
        if action != Action::None && self.mode != Mode::Terminal {
            self.status_msg = None;
        }
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::Focus => match self.cursor_row() {
                Some(Row::Folder(id)) => self.toggle_folder(id),
                Some(Row::Session(id)) => {
                    self.show_session(id);
                    self.mode = Mode::Terminal;
                }
                None => {}
            },
            Action::Leave | Action::Cancel => {
                self.mode = Mode::Workspace;
                self.menu = None;
                self.rename = None;
            }
            Action::New => self.new_session(&available_shells(), Place::Replace),
            Action::Palette(query) => {
                self.input = query.to_string();
                self.picked = 0;
                self.mode = Mode::Palette;
            }
            Action::Rename => self.run(Command::Rename),
            Action::Close => self.run(Command::Close),
            Action::Move => {
                if let Some(Row::Session(id)) = self.cursor_row() {
                    let at = self.cursor_anchor();
                    self.open_menu(self.move_menu(id), at);
                }
            }
            Action::Split(dir) => self.run(Command::Split(dir)),
            Action::ClosePane => self.run(Command::ClosePane),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::Help => self.mode = Mode::Help,
            Action::FocusPane(dir) => {
                let target = self
                    .layout
                    .as_ref()
                    .zip(self.focused)
                    .and_then(|(layout, from)| layout.neighbor(self.main_area(), from, dir));
                if let Some(id) = target {
                    self.focus(id);
                }
            }
            Action::Next | Action::Prev => {
                let down = action == Action::Next;
                match self.mode {
                    Mode::Palette => {
                        self.picked = step(self.picked, self.palette_items().len(), down)
                    }
                    Mode::Menu => {
                        if let Some(menu) = &mut self.menu {
                            menu.picked = step(menu.picked, menu.items.len(), down);
                        }
                    }
                    _ => self.move_cursor(step(self.cursor, self.rows().len(), down)),
                }
            }
            Action::ScrollPage(up) => {
                let rows = self.focused_pane_rows();
                if let Some(id) = self.focused {
                    self.scroll(id, usize::from(rows.saturating_sub(1).max(1)), up);
                }
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

    /// Moves the sidebar cursor; landing on a session shows it.
    fn move_cursor(&mut self, index: usize) {
        self.cursor = index;
        if let Some(Row::Session(id)) = self.cursor_row() {
            self.show_session(id);
        }
    }

    fn focused_pane_rows(&self) -> u16 {
        let main = self.main_area();
        self.layout
            .as_ref()
            .zip(self.focused)
            .and_then(|(layout, id)| layout.rects(main).into_iter().find(|(i, _)| *i == id))
            .map_or(1, |(_, rect)| ui::inner_size(rect).0)
    }

    fn confirm(&mut self) {
        match std::mem::replace(&mut self.mode, Mode::Workspace) {
            Mode::Rename => {
                let name = self.input.trim().to_string();
                match self.rename.take() {
                    _ if name.is_empty() => {}
                    Some(Target::Session(id)) => {
                        if let Some(session) = self.session_mut(id) {
                            session.name = name;
                        }
                    }
                    Some(Target::Folder(id)) => {
                        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
                            folder.name = name;
                        }
                    }
                    None => {}
                }
            }
            Mode::Palette => {
                if let Some(item) = self.palette_items().get(self.picked) {
                    self.run(item.command);
                }
            }
            Mode::Menu => {
                if let Some(menu) = self.menu.take()
                    && let Some(&(_, action)) = menu.items.get(menu.picked)
                {
                    self.run_menu(action, menu.at);
                }
            }
            Mode::Workspace | Mode::Terminal | Mode::Help => {}
        }
    }

    fn run(&mut self, command: Command) {
        match command {
            Command::New(shell) => self.new_session(&[shell], Place::Replace),
            Command::GoTo(id) => self.show_session(id),
            Command::Rename => match self.cursor_row() {
                Some(Row::Folder(id)) => self.start_rename(Target::Folder(id)),
                Some(Row::Session(id)) => self.start_rename(Target::Session(id)),
                None => {}
            },
            Command::Close => match self.cursor_row() {
                Some(Row::Folder(id)) => self.delete_folder(id),
                Some(Row::Session(id)) => self.close_session(id),
                None => {}
            },
            Command::NewFolder => {
                let id = self.new_folder();
                self.start_rename(Target::Folder(id));
            }
            Command::Split(dir) => self.new_session(&available_shells(), Place::Split(dir)),
            Command::ClosePane => {
                if let Some(id) = self.focused {
                    self.close_pane(id);
                }
            }
            Command::ToggleSidebar => self.toggle_sidebar(),
            Command::Theme(index) => self.theme = index,
            Command::Quit => self.should_quit = true,
        }
    }

    fn run_menu(&mut self, action: MenuAction, at: Position) {
        match action {
            MenuAction::Rename(target) => self.start_rename(target),
            MenuAction::CloseSession(id) => self.close_session(id),
            MenuAction::DeleteFolder(id) => self.delete_folder(id),
            MenuAction::MoveMenu(id) => self.open_menu(self.move_menu(id), at),
            MenuAction::MoveTo(id, folder) => self.move_to(id, folder),
            MenuAction::MoveToNewFolder(id) => {
                let folder = self.new_folder();
                self.move_to(id, Some(folder));
                self.start_rename(Target::Folder(folder));
            }
            MenuAction::Split(id, dir) => {
                self.focus(id);
                self.new_session(&available_shells(), Place::Split(dir));
            }
            MenuAction::ClosePane(id) => self.close_pane(id),
            MenuAction::Settings => self.open_menu(self.settings_menu(), at),
            MenuAction::Bindings => self.mode = Mode::Help,
            MenuAction::Detach => self.status_msg = Some(DETACH_SOON.into()),
            MenuAction::SetTheme(index) => self.theme = index,
            MenuAction::ToggleSidebar => self.toggle_sidebar(),
        }
    }

    fn start_rename(&mut self, target: Target) {
        let name = match target {
            Target::Session(id) => self.session(id).map(|s| s.name.clone()),
            Target::Folder(id) => self.folder(id).map(|f| f.name.clone()),
        };
        if let Some(name) = name {
            self.input = name;
            self.rename = Some(target);
            self.mode = Mode::Rename;
        }
    }

    fn open_menu(&mut self, items: Vec<(String, MenuAction)>, at: Position) {
        self.menu = Some(Menu {
            items,
            picked: 0,
            at,
        });
        self.mode = Mode::Menu;
    }

    fn session_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        vec![
            ("rename".into(), MenuAction::Rename(Target::Session(id))),
            ("move to folder…".into(), MenuAction::MoveMenu(id)),
            ("close session".into(), MenuAction::CloseSession(id)),
        ]
    }

    fn folder_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        vec![
            ("rename".into(), MenuAction::Rename(Target::Folder(id))),
            ("delete folder".into(), MenuAction::DeleteFolder(id)),
        ]
    }

    fn pane_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        let mut items = vec![
            (
                "split vertical │".into(),
                MenuAction::Split(id, Split::Right),
            ),
            (
                "split horizontal ─".into(),
                MenuAction::Split(id, Split::Down),
            ),
        ];
        if self.layout.as_ref().is_some_and(|l| l.panes().len() > 1) {
            items.push(("unsplit (close pane)".into(), MenuAction::ClosePane(id)));
        }
        items.push((
            "rename session".into(),
            MenuAction::Rename(Target::Session(id)),
        ));
        items.push(("close session".into(), MenuAction::CloseSession(id)));
        items
    }

    /// Menu of the button at the bottom of the sidebar.
    fn app_menu() -> Vec<(String, MenuAction)> {
        vec![
            ("settings…".into(), MenuAction::Settings),
            ("bindings".into(), MenuAction::Bindings),
            ("detach (soon)".into(), MenuAction::Detach),
        ]
    }

    fn settings_menu(&self) -> Vec<(String, MenuAction)> {
        let mut items: Vec<(String, MenuAction)> = THEMES
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let current = if i == self.theme { " ✓" } else { "" };
                (
                    format!("theme: {}{current}", t.name),
                    MenuAction::SetTheme(i),
                )
            })
            .collect();
        let sidebar = if self.sidebar_collapsed {
            "expand sidebar"
        } else {
            "collapse sidebar"
        };
        items.push((sidebar.into(), MenuAction::ToggleSidebar));
        items
    }

    /// Opens the app menu just above its button.
    fn open_app_menu(&mut self) {
        let items = Self::app_menu();
        let button = ui::menu_button(self.screen().0);
        let at = Position::new(button.x, button.y.saturating_sub(items.len() as u16 + 2));
        self.open_menu(items, at);
    }

    fn move_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        let mut items: Vec<(String, MenuAction)> = self
            .folders
            .iter()
            .map(|f| (format!("→ {}", f.name), MenuAction::MoveTo(id, Some(f.id))))
            .collect();
        items.push(("→ no folder".into(), MenuAction::MoveTo(id, None)));
        items.push(("+ new folder".into(), MenuAction::MoveToNewFolder(id)));
        items
    }

    /// Screen position just right of the cursor's sidebar row (for keyboard-opened menus).
    fn cursor_anchor(&self) -> Position {
        let (sidebar, _, _) = self.screen();
        let cards = ui::cards_area(sidebar);
        ui::visible_rows(&self.rows(), self.cursor, cards, self.sidebar_collapsed)
            .into_iter()
            .find(|(i, _)| *i == self.cursor)
            .map_or(Position::new(sidebar.right(), sidebar.y), |(_, r)| {
                Position::new(sidebar.right(), r.y)
            })
    }

    fn new_folder(&mut self) -> u32 {
        let id = self.next_folder;
        self.next_folder += 1;
        self.folders.push(Folder {
            id,
            name: format!("folder {id}"),
            collapsed: false,
        });
        id
    }

    fn toggle_folder(&mut self, id: u32) {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
            folder.collapsed = !folder.collapsed;
        }
    }

    fn delete_folder(&mut self, id: u32) {
        for session in &mut self.sessions {
            if session.folder == Some(id) {
                session.folder = None;
            }
        }
        self.folders.retain(|f| f.id != id);
        self.cursor = clamp_index(self.cursor, self.rows().len());
    }

    fn move_to(&mut self, id: u32, folder: Option<u32>) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        session.folder = folder;
        if let Some(f) = self.folders.iter_mut().find(|f| Some(f.id) == folder) {
            f.collapsed = false;
        }
        if let Some(i) = self.rows().iter().position(|r| *r == Row::Session(id)) {
            self.cursor = i;
        }
    }

    /// Folder a new session goes to: the one of the sidebar cursor.
    fn context_folder(&self) -> Option<u32> {
        match self.cursor_row()? {
            Row::Folder(id) => Some(id),
            Row::Session(id) => self.session(id)?.folder,
        }
    }

    /// Kills session `id` and removes its pane; a lone pane shows another session instead.
    fn close_session(&mut self, id: u32) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else {
            return;
        };
        self.sessions.remove(index).kill();
        let only_pane = self.layout.as_ref().is_some_and(|l| l.panes() == [id]);
        if only_pane {
            self.layout = self.sessions.first().map(|s| Layout::Pane(s.id));
        } else if let Some(layout) = &mut self.layout {
            layout.remove(id);
        }
        self.cursor = clamp_index(self.cursor, self.rows().len());
        if self.focused == Some(id) || only_pane {
            self.focused = self.layout.as_ref().map(|l| l.panes()[0]);
            self.switched_at = Some(Instant::now());
        }
        if let Some(focused) = self.focused {
            self.focus(focused);
        } else {
            self.mode = Mode::Workspace;
        }
        self.relayout();
    }

    fn close_pane(&mut self, id: u32) {
        let removed = self.layout.as_mut().is_some_and(|l| l.remove(id));
        if !removed {
            self.status_msg = Some("the last pane cannot be closed".into());
            return;
        }
        if self.focused == Some(id) {
            let first = self.layout.as_ref().map(|l| l.panes()[0]);
            if let Some(first) = first {
                self.focus(first);
            }
        }
        self.relayout();
    }

    /// Writes to the focused session and jumps back to its live screen.
    fn send(&mut self, bytes: &[u8]) {
        if let Some(id) = self.focused {
            self.send_to(id, bytes);
        }
    }

    fn send_to(&mut self, id: u32, bytes: &[u8]) {
        let Some(session) = self.session_mut(id) else {
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

    /// Moves the view of session `id` `lines` into (`up`) or out of its history.
    fn scroll(&mut self, id: u32, lines: usize, up: bool) {
        let Some(session) = self.session(id) else {
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

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let pos = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.left_click(pos),
            MouseEventKind::Down(MouseButton::Right) => self.right_click(pos),
            MouseEventKind::Moved => {
                if let Some(index) = self.menu_item_at(pos)
                    && let Some(menu) = &mut self.menu
                {
                    menu.picked = index;
                }
            }
            MouseEventKind::ScrollUp => self.wheel(pos, true),
            MouseEventKind::ScrollDown => self.wheel(pos, false),
            _ => {}
        }
    }

    fn menu_item_at(&self, pos: Position) -> Option<usize> {
        let menu = self.menu.as_ref()?;
        let rect = ui::menu_rect(menu, self.area);
        let inside = pos.x > rect.x && pos.x + 1 < rect.right();
        let index = usize::from(pos.y.checked_sub(rect.y + 1)?);
        (inside && index < menu.items.len()).then_some(index)
    }

    /// Sidebar row under `pos`, by index.
    fn row_at(&self, pos: Position) -> Option<usize> {
        let (sidebar, _, _) = self.screen();
        let cards = ui::cards_area(sidebar);
        ui::visible_rows(&self.rows(), self.cursor, cards, self.sidebar_collapsed)
            .into_iter()
            .find(|(_, rect)| rect.contains(pos))
            .map(|(i, _)| i)
    }

    fn left_click(&mut self, pos: Position) {
        if self.mode == Mode::Help {
            self.mode = Mode::Workspace;
            return;
        }
        if self.mode == Mode::Menu {
            match self.menu_item_at(pos) {
                Some(index) => {
                    if let Some(menu) = &mut self.menu {
                        menu.picked = index;
                    }
                    self.confirm();
                }
                None => {
                    self.menu = None;
                    self.mode = Mode::Workspace;
                }
            }
            return;
        }
        let (sidebar, _, _) = self.screen();
        if pos.y == sidebar.y && sidebar.contains(pos) {
            self.toggle_sidebar();
            return;
        }
        if ui::menu_button(sidebar).contains(pos) {
            self.open_app_menu();
            return;
        }
        if let Some(index) = self.row_at(pos) {
            let now = Instant::now();
            let double = self.last_click.is_some_and(|(t, i)| {
                i == index && now.saturating_duration_since(t) < DOUBLE_CLICK
            });
            self.last_click = Some((now, index));
            self.cursor = index;
            match self.cursor_row() {
                Some(Row::Folder(id)) => self.toggle_folder(id),
                Some(Row::Session(id)) if double => self.start_rename(Target::Session(id)),
                Some(Row::Session(id)) => self.show_session(id),
                None => {}
            }
            return;
        }
        let pane = self
            .layout
            .as_ref()
            .and_then(|l| l.pane_at(self.main_area(), pos));
        if let Some((id, _)) = pane {
            self.focus(id);
            if self.mode == Mode::Workspace {
                self.mode = Mode::Terminal;
            }
        }
    }

    fn right_click(&mut self, pos: Position) {
        if let Some(index) = self.row_at(pos) {
            self.cursor = index;
            let items = match self.cursor_row() {
                Some(Row::Folder(id)) => self.folder_menu(id),
                Some(Row::Session(id)) => self.session_menu(id),
                None => return,
            };
            self.open_menu(items, pos);
            return;
        }
        let pane = self
            .layout
            .as_ref()
            .and_then(|l| l.pane_at(self.main_area(), pos));
        if let Some((id, _)) = pane {
            self.focus(id);
            self.open_menu(self.pane_menu(id), pos);
        }
    }

    fn wheel(&mut self, pos: Position, up: bool) {
        let pane = self
            .layout
            .as_ref()
            .and_then(|l| l.pane_at(self.main_area(), pos));
        let Some((id, _)) = pane else {
            return;
        };
        let Some(session) = self.session(id) else {
            return;
        };
        let (alternate, app_cursor) = {
            let parser = session.parser.lock().unwrap();
            let screen = parser.screen();
            (screen.alternate_screen(), screen.application_cursor())
        };
        // Full-screen programs (alternate screen) have no history: send arrows like xterm.
        if alternate {
            let code = if up { KeyCode::Up } else { KeyCode::Down };
            let arrow =
                encode_key(KeyEvent::new(code, KeyModifiers::NONE), app_cursor).unwrap_or_default();
            self.send_to(id, &arrow.repeat(WHEEL_LINES));
        } else {
            self.scroll(id, WHEEL_LINES, up);
        }
    }

    /// Starts a session with the first of `shells` that spawns, in the cursor's folder.
    fn new_session(&mut self, shells: &[&ShellSpec], place: Place) {
        let id = self.next_id;
        let (rows, cols) = ui::inner_size(self.main_area());
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
                Ok(mut session) => {
                    tracing::info!(id, shell = shell.label, "session started");
                    session.folder = self.context_folder();
                    self.sessions.push(session);
                    self.next_id += 1;
                    self.place(id, place);
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

    fn place(&mut self, id: u32, place: Place) {
        match (&mut self.layout, self.focused, place) {
            (Some(layout), Some(focused), Place::Split(dir)) => {
                layout.split(focused, id, dir);
            }
            (Some(layout), Some(focused), Place::Replace) => {
                layout.replace(focused, id);
            }
            _ => self.layout = Some(Layout::Pane(id)),
        }
        self.switched_at = Some(Instant::now());
        self.focus(id);
        self.relayout();
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

/// Sidebar order: each folder followed by its sessions (unless collapsed), then loose sessions.
pub fn build_rows(folders: &[Folder], sessions: &[(u32, Option<u32>)]) -> Vec<Row> {
    let in_folder = |folder: Option<u32>| {
        sessions
            .iter()
            .filter(move |(_, f)| *f == folder)
            .map(|(id, _)| Row::Session(*id))
    };
    let mut rows = Vec::new();
    for folder in folders {
        rows.push(Row::Folder(folder.id));
        if !folder.collapsed {
            rows.extend(in_folder(Some(folder.id)));
        }
    }
    rows.extend(in_folder(None));
    rows
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
    // Ctrl+Alt is AltGr on Windows ('@', '{', '|'…).
    let altgr = ctrl && alt;
    let pane_dir = (key.modifiers == KeyModifiers::ALT)
        .then(|| arrow_dir(key.code))
        .flatten();
    match mode {
        Mode::Terminal if is_leave(key) => Action::Leave,
        Mode::Workspace | Mode::Terminal if pane_dir.is_some() => {
            Action::FocusPane(pane_dir.expect("checked"))
        }
        Mode::Terminal if shift && key.code == KeyCode::PageUp => Action::ScrollPage(true),
        Mode::Terminal if shift && key.code == KeyCode::PageDown => Action::ScrollPage(false),
        Mode::Terminal => encode_key(key, app_cursor).map_or(Action::None, Action::Send),
        Mode::Workspace => match key.code {
            KeyCode::Enter if has_session => Action::Focus,
            KeyCode::Down => Action::Next,
            KeyCode::Up => Action::Prev,
            KeyCode::PageUp => Action::ScrollPage(true),
            KeyCode::PageDown => Action::ScrollPage(false),
            KeyCode::F(2) => Action::Rename,
            KeyCode::Char('p') if ctrl && !alt => Action::Palette(""),
            KeyCode::Char(_) if !plain && !altgr => Action::None,
            KeyCode::Char(':') => Action::Palette(""),
            KeyCode::Char('|') => Action::Split(Split::Right),
            KeyCode::Char('-') => Action::Split(Split::Down),
            KeyCode::Char(_) if altgr => Action::None,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('n') => Action::New,
            KeyCode::Char('N') => Action::Palette("new "),
            KeyCode::Char('r') => Action::Rename,
            KeyCode::Char('x') => Action::Close,
            KeyCode::Char('m') if has_session => Action::Move,
            KeyCode::Char('w') => Action::ClosePane,
            KeyCode::Char('b') => Action::ToggleSidebar,
            KeyCode::Char('?') => Action::Help,
            KeyCode::Char('j') => Action::Next,
            KeyCode::Char('k') => Action::Prev,
            _ => Action::None,
        },
        Mode::Help => match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                Action::Cancel
            }
            _ => Action::None,
        },
        Mode::Menu => match key.code {
            KeyCode::Enter => Action::Confirm,
            KeyCode::Esc | KeyCode::Char('q') => Action::Cancel,
            KeyCode::Down | KeyCode::Char('j') => Action::Next,
            KeyCode::Up | KeyCode::Char('k') => Action::Prev,
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
            KeyCode::Char(c) if plain || altgr => Action::InputChar(c),
            _ => Action::None,
        },
    }
}

fn arrow_dir(code: KeyCode) -> Option<Dir> {
    match code {
        KeyCode::Left => Some(Dir::Left),
        KeyCode::Right => Some(Dir::Right),
        KeyCode::Up => Some(Dir::Up),
        KeyCode::Down => Some(Dir::Down),
        _ => None,
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

fn clamp_index(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventState;

    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn folder(id: u32, collapsed: bool) -> Folder {
        Folder {
            id,
            name: format!("f{id}"),
            collapsed,
        }
    }

    #[test]
    fn selection_is_bounded() {
        assert_eq!(step(0, 3, false), 0);
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 2);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn clamp_keeps_index_in_range() {
        assert_eq!(clamp_index(2, 2), 1);
        assert_eq!(clamp_index(0, 2), 0);
        assert_eq!(clamp_index(0, 0), 0);
    }

    #[test]
    fn rows_list_folders_then_loose_sessions() {
        let folders = [folder(1, false), folder(2, true), folder(3, false)];
        let sessions = [(10, None), (11, Some(1)), (12, Some(2)), (13, Some(1))];
        assert_eq!(
            build_rows(&folders, &sessions),
            [
                Row::Folder(1),
                Row::Session(11),
                Row::Session(13),
                Row::Folder(2),
                Row::Folder(3),
                Row::Session(10),
            ]
        );
        assert_eq!(build_rows(&[], &[]), []);
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
    fn rename_keys() {
        let r = key(KeyCode::Char('r'), KeyModifiers::NONE);
        let f2 = key(KeyCode::F(2), KeyModifiers::NONE);
        assert_eq!(key_action(Mode::Workspace, r, true, false), Action::Rename);
        assert_eq!(key_action(Mode::Workspace, f2, true, false), Action::Rename);
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
    fn split_keys_work_with_altgr() {
        let pipe = key(KeyCode::Char('|'), KeyModifiers::SHIFT);
        let pipe_altgr = key(
            KeyCode::Char('|'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let dash = key(KeyCode::Char('-'), KeyModifiers::NONE);
        let w = key(KeyCode::Char('w'), KeyModifiers::NONE);
        let right = Action::Split(Split::Right);
        assert_eq!(key_action(Mode::Workspace, pipe, true, false), right);
        assert_eq!(key_action(Mode::Workspace, pipe_altgr, true, false), right);
        assert_eq!(
            key_action(Mode::Workspace, dash, true, false),
            Action::Split(Split::Down)
        );
        assert_eq!(
            key_action(Mode::Workspace, w, true, false),
            Action::ClosePane
        );
    }

    #[test]
    fn altgr_letter_does_not_trigger_commands() {
        let altgr_q = key(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(
            key_action(Mode::Workspace, altgr_q, true, false),
            Action::None
        );
    }

    #[test]
    fn alt_arrows_move_between_panes_in_both_modes() {
        let alt_left = key(KeyCode::Left, KeyModifiers::ALT);
        let ctrl_left = key(KeyCode::Left, KeyModifiers::CONTROL);
        let focus_left = Action::FocusPane(Dir::Left);
        assert_eq!(
            key_action(Mode::Terminal, alt_left, true, false),
            focus_left
        );
        assert_eq!(
            key_action(Mode::Workspace, alt_left, true, false),
            focus_left
        );
        assert_eq!(
            key_action(Mode::Terminal, ctrl_left, true, false),
            Action::Send(b"\x1b[1;5D".to_vec())
        );
    }

    #[test]
    fn move_needs_a_session() {
        let m = key(KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(key_action(Mode::Workspace, m, true, false), Action::Move);
        assert_eq!(key_action(Mode::Workspace, m, false, false), Action::None);
    }

    #[test]
    fn b_toggles_the_sidebar_only_from_workspace() {
        let b = key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Workspace, b, true, false),
            Action::ToggleSidebar
        );
        assert_eq!(
            key_action(Mode::Terminal, b, true, false),
            Action::Send(b"b".to_vec())
        );
    }

    #[test]
    fn question_mark_shows_bindings_and_esc_closes_them() {
        let question = key(KeyCode::Char('?'), KeyModifiers::SHIFT);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Workspace, question, true, false),
            Action::Help
        );
        assert_eq!(key_action(Mode::Help, esc, true, false), Action::Cancel);
    }

    #[test]
    fn detach_is_greyed_out_until_the_daemon_exists() {
        assert!(!MenuAction::Detach.enabled());
        assert!(MenuAction::Bindings.enabled());
    }

    #[test]
    fn menu_keys() {
        let j = key(KeyCode::Char('j'), KeyModifiers::NONE);
        let up = key(KeyCode::Up, KeyModifiers::NONE);
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(key_action(Mode::Menu, j, true, false), Action::Next);
        assert_eq!(key_action(Mode::Menu, up, true, false), Action::Prev);
        assert_eq!(key_action(Mode::Menu, enter, true, false), Action::Confirm);
        assert_eq!(key_action(Mode::Menu, esc, true, false), Action::Cancel);
    }

    #[test]
    fn default_shell_is_available() {
        assert!(!available_shells().is_empty());
    }
}
