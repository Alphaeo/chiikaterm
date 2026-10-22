use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use portable_pty::PtySize;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::agent::{self, AgentInfo, AgentState};
use crate::canvas::{self, CanvasState};
use crate::channel::{self, HookEvent, Reply, Request};
use crate::config::{self, AgentChoice, CanvasFile, Config, TeamEntry};
use crate::coord;
use crate::fx::{format_duration, fuzzy_match};
use crate::keys::{encode_key, encode_paste};
use crate::layout::{Dir, Layout, Split};
use crate::link::{self, Link};
use crate::protocol::{ClientMsg, Launch, MarksSnapshot, SessionInfo};
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

/// An agent exiting this soon after its start failed to launch.
const AGENT_EARLY_EXIT: Duration = Duration::from_secs(15);

/// How often team instructions are re-read from disk.
const CONSIGNE_RELOAD: Duration = Duration::from_secs(2);

/// Duration of the panel wipe when switching sessions.
pub const SWITCH_ANIM: Duration = Duration::from_millis(200);
pub const TOAST_LIFE: Duration = Duration::from_secs(4);
pub const TOAST_SLIDE: Duration = Duration::from_millis(150);
pub const TOAST_FADE: Duration = Duration::from_millis(500);
const MAX_TOASTS: usize = 3;

/// Commands at least this long get a notification (when not focused) and a mascot reaction.
const NOTABLE_COMMAND: Duration = Duration::from_secs(2);

/// How long the mascot keeps reacting to a command result (for the frame clock).
const REACTION: Duration = Duration::from_secs(5);

pub enum AppEvent {
    Input(Event),
    /// Session id and number of bytes read.
    PtyOutput(u32, usize),
    PtyClosed(u32),
    /// A Claude Code hook of the agent in this session, received by the chiika channel.
    Hook(u32, crate::channel::HookEvent),
    /// The daemon reports that a session exited.
    RemoteExited(u32, Option<u32>),
    /// The daemon could not start a session.
    SpawnFailed(u32, String),
    /// The connection to the daemon ended, with the reason.
    DaemonGone(String),
    /// A hook or a tool call on chiika's own channel (no daemon), with its reply when the
    /// sender waits for one.
    Channel(u32, Request, Option<Reply>),
    /// Agent `from` sent a message to agent `to`.
    Pulse(u32, u32),
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
    /// First launch: offer to connect Claude Code.
    Onboarding,
}

/// What the main area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Terminals,
    Canvas,
}

/// Where chiika keeps its files; `None` keeps nothing (tests).
#[derive(Debug, Clone, Default)]
pub struct Paths {
    /// The user config file.
    pub config: Option<PathBuf>,
    /// The project's `.chiika` directory.
    pub project: Option<PathBuf>,
    /// The chiika executable, run by the agents' hooks.
    pub exe: Option<PathBuf>,
}

impl Paths {
    fn canvas_file(&self) -> Option<PathBuf> {
        Some(self.project.as_ref()?.join("canvas.toml"))
    }

    fn team_file(&self, id: u32) -> Option<PathBuf> {
        let teams = self.project.as_ref()?.join("teams");
        Some(teams.join(format!("team-{id}")).join("TEAM.md"))
    }
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
    ToggleView,
    NewTeam,
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

/// A sidebar folder, which is also a team box of the canvas.
pub struct Folder {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
    /// Canvas position of the team box.
    pub pos: (i32, i32),
    /// First meaningful line of the team's `TEAM.md`.
    pub consigne: Option<String>,
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
    ToggleView,
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
    /// Leave, keeping the daemon's sessions running.
    Detach,
    /// Leave and close every session.
    QuitAll,
    SetTheme(usize),
    ToggleSidebar,
    EditConsigne(u32),
    OpenBoard(u32),
    /// New team at this canvas position.
    NewTeamAt(i32, i32),
    ToggleClaude,
    /// Opens the role menu to add an agent to this team.
    AgentRoles(u32),
    /// Team, index in `agent::ROLES`.
    AddAgent(u32, usize),
}

impl MenuAction {
    pub fn destructive(self) -> bool {
        matches!(
            self,
            MenuAction::CloseSession(_) | MenuAction::DeleteFolder(_) | MenuAction::QuitAll
        )
    }
}

/// Shown when "detach" is picked while chiika runs its sessions itself.
const NO_DAEMON: &str = "no daemon is running: quitting chiika closes its shells";

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
#[derive(Clone, Copy)]
enum Place {
    /// In the focused pane, instead of what it showed.
    Replace,
    /// In a new pane cut from the focused one.
    Split(Split),
    /// Nowhere yet (an agent lives in its team box until opened).
    Hidden,
}

/// A shell that can be started in a session.
pub struct ShellSpec {
    pub label: &'static str,
    /// `None` = the user's default shell (`$SHELL`).
    prog: Option<&'static str>,
    args: &'static [&'static str],
    /// PowerShell: load chiika's shell integration (see `shell.rs`).
    integrated: bool,
}

/// Known shells; `n` starts the first one that spawns, the palette offers each of them.
#[cfg(windows)]
const SHELLS: &[ShellSpec] = &[
    ShellSpec {
        label: "pwsh",
        prog: Some("pwsh.exe"),
        args: &["-NoLogo"],
        integrated: true,
    },
    ShellSpec {
        label: "powershell",
        prog: Some("powershell.exe"),
        args: &["-NoLogo"],
        integrated: true,
    },
    ShellSpec {
        label: "cmd",
        prog: Some("cmd.exe"),
        args: &[],
        integrated: false,
    },
    ShellSpec {
        label: "wsl",
        prog: Some("wsl.exe"),
        args: &[],
        integrated: false,
    },
];
#[cfg(not(windows))]
const SHELLS: &[ShellSpec] = &[
    ShellSpec {
        label: "shell",
        prog: None,
        args: &[],
        integrated: false,
    },
    ShellSpec {
        label: "bash",
        prog: Some("bash"),
        args: &[],
        integrated: false,
    },
    ShellSpec {
        label: "zsh",
        prog: Some("zsh"),
        args: &[],
        integrated: false,
    },
    ShellSpec {
        label: "fish",
        prog: Some("fish"),
        args: &[],
        integrated: false,
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
    /// When the last notable command ended and whether it succeeded (drives the mascot).
    pub last_result: Option<(Instant, bool)>,
    pub view: View,
    pub canvas: CanvasState,
    pub config: Config,
    paths: Paths,
    consigne_checked: Instant,
    /// The daemon hosting the sessions; `None` when chiika runs them itself (they end with it).
    link: Option<Link>,
    /// Port and token where the agents' hooks report: the daemon's channel, or chiika's own.
    hooks: Option<(u16, String)>,
    /// On exit, keep the daemon's sessions running.
    pub detach: bool,
    /// Printed once the terminal is restored.
    exit_note: Option<String>,
    /// Without a daemon: permission requests held for the user's answer, by agent session.
    pending: HashMap<u32, (Reply, Instant)>,
    /// Messages between agents being drawn: from, to, when.
    pub pulses: Vec<(u32, u32, Instant)>,
    rename: Option<Target>,
    next_folder: u32,
    last_click: Option<(Instant, usize)>,
    theme: usize,
    area: Rect,
    tx: Sender<AppEvent>,
}

impl App {
    /// `area` is the full terminal area; the first shell is started at the panel size.
    /// Teams and settings are read from (and saved to) `paths`.
    pub fn new(area: Rect, tx: Sender<AppEvent>, paths: Paths) -> App {
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
            last_result: None,
            view: View::Terminals,
            canvas: CanvasState::default(),
            config: paths
                .config
                .as_deref()
                .map(config::load)
                .unwrap_or_default(),
            paths,
            consigne_checked: Instant::now(),
            link: None,
            hooks: None,
            detach: false,
            exit_note: None,
            pending: HashMap::new(),
            pulses: Vec::new(),
            rename: None,
            next_folder: 1,
            last_click: None,
            theme: 0,
            area,
            tx,
        };
        app.connect_daemon();
        if app.sessions.is_empty() {
            app.new_session(&available_shells(), Place::Replace);
        }
        // Start typing straight into the first shell, like tmux.
        if !app.sessions.is_empty() {
            app.mode = Mode::Terminal;
        }
        // Teams are loaded after the first shell so that it does not land in one.
        app.load_teams();
        if let Some(id) = app.focused {
            app.focus(id);
        }
        if app.link.is_none() {
            app.hooks = channel::start(app.tx.clone())
                .inspect_err(|e| tracing::warn!("chiika channel unavailable: {e}"))
                .ok()
                .map(|c| (c.port, c.token));
        }
        let ask_claude = app.config.agents.claude.is_none() && claude_installed();
        if app.paths.config.is_some() && ask_claude {
            app.mode = Mode::Onboarding;
            app.picked = 0;
        }
        app
    }

    /// Attaches to the project's daemon (starting it if needed) and shows its sessions. Without
    /// a daemon, sessions run inside chiika and end with it.
    fn connect_daemon(&mut self) {
        let (Some(data), Some(exe)) = (self.paths.project.clone(), self.paths.exe.clone()) else {
            return;
        };
        let attached = match link::attach(&data, &exe) {
            Ok(attached) => attached,
            Err(e) => {
                tracing::warn!("no daemon: {e:#}");
                self.status_msg = Some(format!("no daemon ({e:#}): shells close with chiika"));
                return;
            }
        };
        let now = Instant::now();
        for (info, screen) in &attached.sessions {
            let session = Session::remote(info, screen, attached.link.clone(), now);
            self.sessions.push(session);
        }
        self.next_id = self.sessions.iter().map(|s| s.id + 1).max().unwrap_or(1);
        attached.link.listen(attached.reader, self.tx.clone());
        self.hooks = attached.hooks;
        self.link = Some(attached.link);
        // One pane, on a shell rather than an agent (agents live in their team boxes).
        let first = self
            .sessions
            .iter()
            .find(|s| s.agent.is_none() && !s.ephemeral)
            .or(self.sessions.first())
            .map(|s| s.id);
        if let Some(first) = first {
            self.layout = Some(Layout::Pane(first));
            self.focus(first);
            self.relayout();
        }
    }

    /// On exit: sessions run by chiika end with it; the daemon's keep running unless the user
    /// chose to close everything.
    pub fn shutdown(&mut self) {
        match &self.link {
            Some(link) if !self.detach => {
                if let Err(e) = link.send(&ClientMsg::Shutdown) {
                    tracing::warn!("cannot stop the daemon: {e}");
                }
            }
            Some(link) => link.close(),
            None => self.kill_all(),
        }
    }

    /// What to print after leaving.
    pub fn exit_note(&self) -> Option<String> {
        if self.exit_note.is_some() {
            return self.exit_note.clone();
        }
        let running = self
            .sessions
            .iter()
            .filter(|s| s.status == Status::Running)
            .count();
        (self.link.is_some() && self.detach && running > 0).then(|| {
            format!(
                "chiika: detached, {running} session(s) keep running; run chiika here to come back"
            )
        })
    }

    /// Leaving: with a daemon, ask whether to keep the sessions running.
    fn ask_quit(&mut self) {
        if self.link.is_none() {
            self.should_quit = true;
            return;
        }
        let items = vec![
            ("detach: keep everything running".into(), MenuAction::Detach),
            ("quit and close all shells".into(), MenuAction::QuitAll),
        ];
        let at = Position::new(
            (self.area.width / 2).saturating_sub(18),
            (self.area.height / 2).saturating_sub(2),
        );
        self.open_menu(items, at);
    }

    /// Tells the daemon what to keep about session `id` (name, team, agent…).
    fn sync_session(&self, id: u32) {
        if let Some(session) = self.session(id) {
            session.sync(Instant::now());
        }
    }

    fn load_teams(&mut self) {
        let file: CanvasFile = self
            .paths
            .canvas_file()
            .as_deref()
            .map(config::load)
            .unwrap_or_default();
        self.folders = file
            .teams
            .into_iter()
            .map(|t| Folder {
                id: t.id,
                name: t.name,
                collapsed: t.collapsed,
                pos: (t.x, t.y),
                consigne: None,
            })
            .collect();
        self.next_folder = self.folders.iter().map(|f| f.id + 1).max().unwrap_or(1);
        self.reload_consignes();
    }

    pub(crate) fn save_canvas(&mut self) {
        let Some(path) = self.paths.canvas_file() else {
            return;
        };
        let file = CanvasFile {
            teams: self
                .folders
                .iter()
                .map(|f| TeamEntry {
                    id: f.id,
                    name: f.name.clone(),
                    x: f.pos.0,
                    y: f.pos.1,
                    collapsed: f.collapsed,
                })
                .collect(),
        };
        if let Err(e) = config::save(&path, &file) {
            self.status_msg = Some(format!("cannot save the teams: {e:#}"));
        }
    }

    fn reload_consignes(&mut self) {
        for folder in &mut self.folders {
            folder.consigne = self
                .paths
                .team_file(folder.id)
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| consigne_excerpt(&text));
        }
    }

    fn set_claude(&mut self, choice: AgentChoice) {
        self.config.agents.claude = Some(choice);
        if let Some(path) = &self.paths.config
            && let Err(e) = config::save(path, &self.config)
        {
            self.status_msg = Some(format!("cannot save the settings: {e:#}"));
        }
    }

    pub(crate) fn set_view(&mut self, view: View) {
        self.view = view;
        self.canvas.grab = None;
        if view == View::Canvas && self.mode == Mode::Terminal {
            self.mode = Mode::Workspace;
        }
    }

    /// Opens the team's `TEAM.md` (created from a template) in an editor pane.
    fn edit_consigne(&mut self, id: u32) {
        let template = "Instructions for this team: its goal, its rules, its priorities.";
        self.open_team_file(id, self.paths.team_file(id), template);
    }

    /// Opens the team's shared board, where its agents `post`.
    fn open_board(&mut self, id: u32) {
        let board = self
            .paths
            .project
            .as_deref()
            .map(|data| coord::board_file(data, id));
        self.open_team_file(id, board, "Notes posted by the team's agents.");
    }

    /// Opens a team file (created with a title and `template`) in an editor pane that closes
    /// with the editor.
    fn open_team_file(&mut self, id: u32, path: Option<PathBuf>, template: &str) {
        let Some(path) = path else {
            self.status_msg = Some("no project folder to keep team files in".into());
            return;
        };
        let team = self
            .folder(id)
            .map_or_else(|| "team".to_string(), |f| f.name.clone());
        if !path.exists() {
            let template = format!("# {team}\n\n{template}\n");
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, template));
            if let Err(e) = written {
                self.status_msg = Some(format!("cannot create {}: {e}", path.display()));
                return;
            }
        }
        match self.spawn_session(format!("edit {team}"), editor_launch(&path), Place::Replace) {
            Ok(session) => {
                if let Some(s) = self.session_mut(session) {
                    s.ephemeral = true;
                    s.folder = None;
                }
                self.sync_session(session);
                self.view = View::Terminals;
                self.mode = Mode::Terminal;
            }
            Err(e) => self.status_msg = Some(format!("cannot open the editor: {e:#}")),
        }
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
            AppEvent::Hook(id, event) => self.on_hook(id, &event),
            AppEvent::Channel(id, request, reply) => self.on_channel(id, request, reply),
            AppEvent::Pulse(from, to) => self.pulses.push((from, to, Instant::now())),
            AppEvent::RemoteExited(id, code) => {
                if let Some(session) = self.session_mut(id) {
                    session.set_exited(code);
                }
            }
            AppEvent::SpawnFailed(id, error) => {
                self.close_session(id);
                self.status_msg = Some(format!("cannot start a session: {error}"));
            }
            AppEvent::DaemonGone(reason) => {
                self.exit_note = Some(format!("chiika: {reason}"));
                self.detach = true;
                self.should_quit = true;
            }
        }
    }

    /// Per-frame housekeeping: exits (with a toast), activity windows, expired toasts.
    pub fn tick(&mut self, now: Instant) {
        let mut closed_editors = Vec::new();
        for session in &mut self.sessions {
            session.tick(now);
            for done in session.take_finished() {
                if done.duration < NOTABLE_COMMAND {
                    continue;
                }
                let ok = done.code == 0;
                self.last_result = Some((now, ok));
                if self.focused != Some(session.id) {
                    let code = if ok {
                        String::new()
                    } else {
                        format!(" ({})", done.code)
                    };
                    let text = format!(
                        "{}{code} · {} — {}",
                        truncate(&done.text, 30),
                        format_duration(done.duration),
                        session.name
                    );
                    self.toasts.push(Toast {
                        text,
                        ok,
                        born: now,
                    });
                }
            }
            if session.poll_exit() {
                if session.ephemeral {
                    closed_editors.push(session.id);
                    continue;
                }
                let (text, ok) = match session.status {
                    Status::Exited(Some(0)) => (format!("{} exited", session.name), true),
                    Status::Exited(Some(code)) => {
                        (format!("{} exited ({code})", session.name), false)
                    }
                    _ => (format!("{} killed", session.name), false),
                };
                // An agent that stops right away printed why (e.g. a project Claude does not
                // trust yet): show it.
                let early = now.saturating_duration_since(session.started) < AGENT_EARLY_EXIT;
                let reason = session
                    .agent
                    .as_ref()
                    .filter(|_| early)
                    .and_then(|_| first_screen_line(session));
                let text = match reason {
                    Some(reason) => format!("{text}: {}", truncate(&reason, 70)),
                    None => text,
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
        // An instructions editor was closed: back to the canvas, with the new text.
        for id in closed_editors {
            self.close_session(id);
            self.reload_consignes();
            self.set_view(View::Canvas);
        }
        self.pending
            .retain(|_, (_, since)| now.saturating_duration_since(*since) < agent::PERMISSION_WAIT);
        self.pulses
            .retain(|&(_, _, at)| now.saturating_duration_since(at) < canvas::PULSE);
        if now.saturating_duration_since(self.consigne_checked) > CONSIGNE_RELOAD {
            self.reload_consignes();
            self.consigne_checked = now;
        }
        self.canvas_tick(now);
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
        if switching || toast_moving || self.canvas.animating(now) || !self.pulses.is_empty() {
            Duration::from_millis(16)
        } else if self
            .sessions
            .iter()
            .any(|s| s.is_busy(now) || s.needs_user(now))
            || self
                .last_result
                .is_some_and(|(t, _)| now.saturating_duration_since(t) < REACTION)
        {
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
        entries.push(("new team".into(), Command::NewFolder));
        let view = match self.view {
            View::Terminals => "show canvas",
            View::Canvas => "show terminals",
        };
        entries.push((view.into(), Command::ToggleView));
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

    pub(crate) fn main_area(&self) -> Rect {
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
    pub(crate) fn show_session(&mut self, id: u32) {
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
            Action::Quit => self.ask_quit(),
            Action::Focus => match self.cursor_row() {
                Some(Row::Folder(id)) => self.toggle_folder(id),
                Some(Row::Session(id)) => {
                    self.view = View::Terminals;
                    self.show_session(id);
                    self.mode = Mode::Terminal;
                }
                None => {}
            },
            Action::Cancel if self.mode == Mode::Onboarding => {
                self.picked = 1;
                self.confirm();
            }
            Action::Cancel if self.canvas_cancel() => {}
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
            Action::ToggleView => self.run(Command::ToggleView),
            Action::NewTeam => self.run(Command::NewFolder),
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
                    Mode::Onboarding => self.picked = step(self.picked, 2, down),
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
                        self.sync_session(id);
                    }
                    Some(Target::Folder(id)) => {
                        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
                            folder.name = name;
                        }
                        self.save_canvas();
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
            Mode::Onboarding => {
                let choice = if self.picked == 0 {
                    AgentChoice::Connected
                } else {
                    AgentChoice::Declined
                };
                self.set_claude(choice);
                if !self.sessions.is_empty() {
                    self.mode = Mode::Terminal;
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
            Command::ToggleView => self.set_view(match self.view {
                View::Terminals => View::Canvas,
                View::Canvas => View::Terminals,
            }),
            Command::Theme(index) => self.theme = index,
            Command::Quit => self.ask_quit(),
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
            MenuAction::Detach if self.link.is_some() => {
                self.detach = true;
                self.should_quit = true;
            }
            MenuAction::Detach => self.status_msg = Some(NO_DAEMON.into()),
            MenuAction::QuitAll => {
                self.detach = false;
                self.should_quit = true;
            }
            MenuAction::SetTheme(index) => self.theme = index,
            MenuAction::ToggleSidebar => self.toggle_sidebar(),
            MenuAction::EditConsigne(id) => self.edit_consigne(id),
            MenuAction::OpenBoard(id) => self.open_board(id),
            MenuAction::NewTeamAt(x, y) => {
                let id = self.new_folder();
                if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
                    folder.pos = (x, y);
                }
                self.save_canvas();
                self.start_rename(Target::Folder(id));
            }
            MenuAction::ToggleClaude => {
                let connected = self.config.agents.claude == Some(AgentChoice::Connected);
                self.set_claude(if connected {
                    AgentChoice::Declined
                } else {
                    AgentChoice::Connected
                });
            }
            MenuAction::AgentRoles(team) => self.open_menu(self.role_menu(team), at),
            MenuAction::AddAgent(team, role) => self.add_agent(team, role),
        }
    }

    pub(crate) fn role_menu(&self, team: u32) -> Vec<(String, MenuAction)> {
        (0..)
            .zip(agent::ROLES)
            .map(|(i, (role, _))| (format!("+ {role}"), MenuAction::AddAgent(team, i)))
            .collect()
    }

    /// Starts a Claude Code agent with the `role`-th role in `team`, in its own worktree when
    /// the project is a git repository.
    fn add_agent(&mut self, team: u32, role: usize) {
        if self.config.agents.claude != Some(AgentChoice::Connected) {
            self.mode = Mode::Onboarding;
            self.picked = 0;
            return;
        }
        let Some(&(role, _)) = agent::ROLES.get(role) else {
            return;
        };
        let (Some(data), Some(exe), Some((port, token))) = (
            self.paths.project.clone(),
            self.paths.exe.clone(),
            self.hooks.clone(),
        ) else {
            self.status_msg = Some("agents need a project folder and the chiika channel".into());
            return;
        };
        let Some(claude) = agent::claude_exe() else {
            self.status_msg = Some("claude code not found in PATH".into());
            return;
        };
        let root = data
            .parent()
            .map_or_else(|| data.clone(), Path::to_path_buf);
        let team_name = self
            .folder(team)
            .map_or_else(|| "team".to_string(), |f| f.name.clone());
        let id = self.next_id;
        if let Err(e) = agent::ensure_gitignore(&data) {
            tracing::warn!("cannot write .chiika/.gitignore: {e}");
        }
        // Claude creates the worktree itself (`--worktree`, from the project folder): it then
        // trusts it like the project, with no question at start.
        // ponytail: git runs on the UI thread (a few ms, once per agent).
        let worktree = if agent::has_commit(&root) {
            let base = format!("{}-{role}", agent::slug(&team_name));
            Some(agent::free_worktree_name(&root, &base, id))
        } else {
            self.toasts.push(Toast {
                text: format!("{role} works in the project folder (no git commit to branch from)"),
                ok: false,
                born: Instant::now(),
            });
            None
        };
        let workdir = root.clone();
        let instructions = self
            .paths
            .team_file(team)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let prompt = agent::system_prompt(
            role,
            &team_name,
            &instructions,
            &workdir,
            worktree.as_deref(),
        );
        let settings = data
            .join("agents")
            .join(format!("agent-{id}"))
            .join("settings.json");
        let mcp = settings.with_file_name("mcp.json");
        let written = settings
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&settings, agent::hook_settings(&exe)))
            .and_then(|()| std::fs::write(&mcp, agent::mcp_config(&exe, port, &token, id)));
        if let Err(e) = written {
            self.status_msg = Some(format!("cannot write {}: {e}", settings.display()));
            return;
        }
        let mut argv = vec![
            claude.display().to_string(),
            "--settings".into(),
            settings.display().to_string(),
            "--mcp-config".into(),
            mcp.display().to_string(),
            "--append-system-prompt".into(),
            prompt,
        ];
        if let Some(name) = &worktree {
            argv.extend(["--worktree".into(), name.clone()]);
        }
        let mut launch = Launch::new(argv, workdir.clone());
        launch.env.extend([
            ("CHIIKA_PORT".into(), port.to_string()),
            ("CHIIKA_TOKEN".into(), token),
            ("CHIIKA_SESSION".into(), id.to_string()),
        ]);
        // Set when chiika itself runs inside Claude Code: it would turn off the agent's transcripts.
        launch.env_remove.push("CLAUDE_CODE_CHILD_SESSION".into());
        match self.spawn_session(role.to_string(), launch, Place::Hidden) {
            Ok(session) => {
                if let Some(s) = self.session_mut(session) {
                    s.folder = Some(team);
                    s.agent = Some(AgentInfo::new(team, workdir, worktree, Instant::now()));
                }
                self.sync_session(session);
                self.select_row(Row::Session(session));
            }
            Err(e) => self.status_msg = Some(format!("cannot start {role}: {e:#}")),
        }
    }

    /// A hook of an agent: new state, and a notification when it needs the user or is done.
    fn on_hook(&mut self, id: u32, event: &HookEvent) {
        let now = Instant::now();
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        let Some(agent) = session.agent.as_mut() else {
            return;
        };
        let Some(state) = agent.apply(event, now) else {
            return;
        };
        let team = self
            .folders
            .iter()
            .find(|f| f.id == agent.team)
            .map_or("no team", |f| f.name.as_str());
        let note = match state {
            AgentState::Waiting => Some((format!("{} needs you — {team}", session.name), false)),
            AgentState::Done if self.focused != Some(id) => {
                Some((format!("{} is done — {team}", session.name), true))
            }
            _ => None,
        };
        if state == AgentState::Done {
            self.last_result = Some((now, true));
        }
        if let Some((text, ok)) = note {
            self.toasts.push(Toast {
                text,
                ok,
                born: now,
            });
        }
    }

    /// Inspector buttons.
    pub(crate) fn agent_open(&mut self, id: u32) {
        self.view = View::Terminals;
        self.show_session(id);
        self.mode = Mode::Terminal;
    }

    /// Answers the permission request of agent `id`: allow, deny, or `None` to let it ask in its
    /// terminal.
    pub(crate) fn decide(&mut self, id: u32, allow: Option<bool>) {
        if let Some(agent) = self.session_mut(id).and_then(|s| s.agent.as_mut()) {
            agent.permission = None;
        }
        match &self.link {
            Some(link) => {
                if let Err(e) = link.send(&ClientMsg::Decide { id, allow }) {
                    self.status_msg = Some(format!("cannot answer the agent: {e}"));
                }
            }
            None => {
                if let Some((reply, _)) = self.pending.remove(&id)
                    && let Some(allow) = allow
                {
                    reply.send(&agent::decision(allow));
                }
            }
        }
    }

    /// Without a daemon: hooks and tool calls arrive on chiika's own channel.
    fn on_channel(&mut self, id: u32, request: Request, reply: Option<Reply>) {
        match request {
            Request::Hook(event) => {
                self.on_hook(id, &event);
                if let Some(reply) = reply {
                    self.pending.insert(id, (reply, Instant::now()));
                }
            }
            Request::Tool { name, arguments } => {
                let data = self.paths.project.clone();
                let outcome =
                    coord::run(&mut self.sessions, data.as_deref(), id, &name, &arguments);
                if let Some(reply) = reply {
                    reply.send(&outcome.to_mcp());
                }
                if let Some((from, to)) = outcome.link {
                    self.pulses.push((from, to, Instant::now()));
                }
            }
        }
    }

    pub(crate) fn agent_interrupt(&mut self, id: u32) {
        self.send_to(id, b"\x1b");
    }

    /// Sends the team instructions again, as a pasted message.
    pub(crate) fn agent_resend(&mut self, id: u32) {
        let Some(team) = self
            .session(id)
            .and_then(|s| s.agent.as_ref())
            .map(|a| a.team)
        else {
            return;
        };
        let instructions = self
            .paths
            .team_file(team)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let text = format!(
            "Reminder of the team instructions:\n{}",
            instructions.trim()
        );
        let bracketed = self
            .session(id)
            .is_some_and(|s| s.parser.lock().unwrap().screen().bracketed_paste());
        let mut bytes = encode_paste(&text, bracketed);
        bytes.push(b'\r');
        self.send_to(id, &bytes);
    }

    pub(crate) fn start_rename(&mut self, target: Target) {
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

    pub(crate) fn open_menu(&mut self, items: Vec<(String, MenuAction)>, at: Position) {
        self.menu = Some(Menu {
            items,
            picked: 0,
            at,
        });
        self.mode = Mode::Menu;
    }

    pub(crate) fn session_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        vec![
            ("rename".into(), MenuAction::Rename(Target::Session(id))),
            ("move to team…".into(), MenuAction::MoveMenu(id)),
            ("close session".into(), MenuAction::CloseSession(id)),
        ]
    }

    pub(crate) fn folder_menu(&self, id: u32) -> Vec<(String, MenuAction)> {
        vec![
            ("add agent…".into(), MenuAction::AgentRoles(id)),
            ("rename".into(), MenuAction::Rename(Target::Folder(id))),
            ("edit instructions".into(), MenuAction::EditConsigne(id)),
            ("open board".into(), MenuAction::OpenBoard(id)),
            ("delete team".into(), MenuAction::DeleteFolder(id)),
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
            ("detach".into(), MenuAction::Detach),
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
        let claude = match self.config.agents.claude {
            Some(AgentChoice::Connected) => "claude code: connected ✓",
            _ => "connect claude code",
        };
        items.push((claude.into(), MenuAction::ToggleClaude));
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
        items.push(("→ no team".into(), MenuAction::MoveTo(id, None)));
        items.push(("+ new team".into(), MenuAction::MoveToNewFolder(id)));
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

    /// A new team, placed on the canvas after the others.
    pub(crate) fn new_folder(&mut self) -> u32 {
        let id = self.next_folder;
        self.next_folder += 1;
        self.folders.push(Folder {
            id,
            name: format!("team {id}"),
            collapsed: false,
            pos: canvas::team_position(self.folders.len()),
            consigne: None,
        });
        self.save_canvas();
        id
    }

    fn toggle_folder(&mut self, id: u32) {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
            folder.collapsed = !folder.collapsed;
        }
        self.save_canvas();
    }

    /// Moves the sidebar cursor to `row` when it is visible.
    pub(crate) fn select_row(&mut self, row: Row) {
        if let Some(i) = self.rows().iter().position(|r| *r == row) {
            self.cursor = i;
        }
    }

    fn delete_folder(&mut self, id: u32) {
        let now = Instant::now();
        for session in &mut self.sessions {
            if session.folder == Some(id) {
                session.folder = None;
                session.sync(now);
            }
        }
        self.folders.retain(|f| f.id != id);
        self.cursor = clamp_index(self.cursor, self.rows().len());
        self.save_canvas();
    }

    pub(crate) fn move_to(&mut self, id: u32, folder: Option<u32>) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        session.folder = folder;
        session.sync(Instant::now());
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
            MouseEventKind::Drag(MouseButton::Left) if self.canvas.grab.is_some() => {
                self.canvas_drag(pos)
            }
            MouseEventKind::Up(MouseButton::Left) if self.canvas.grab.is_some() => {
                self.canvas_release(pos)
            }
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
        if self.mode == Mode::Onboarding {
            let (_, connect, later) = ui::onboarding_layout(self.area);
            if connect.contains(pos) || later.contains(pos) {
                self.picked = usize::from(later.contains(pos));
                self.confirm();
            }
            return;
        }
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
        if let Some(view) = ui::tab_at(sidebar, self.sidebar_collapsed, pos) {
            self.set_view(view);
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
        if self.view == View::Canvas {
            if self.main_area().contains(pos) {
                self.canvas_press(pos);
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
        if self.view == View::Canvas {
            if self.main_area().contains(pos) {
                self.canvas_right_click(pos);
            }
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
        if self.view == View::Canvas {
            if self.main_area().contains(pos) {
                self.canvas_wheel(up);
            }
            return;
        }
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
        // Like tmux: start where the focused shell is, when its integration told us.
        let focused_cwd = self
            .focused
            .and_then(|id| self.session(id))
            .and_then(|s| s.marks(|m| m.cwd.clone()))
            .map(std::path::PathBuf::from)
            .filter(|dir| dir.is_dir());
        let cwd = match focused_cwd.map_or_else(std::env::current_dir, Ok) {
            Ok(cwd) => cwd,
            Err(e) => {
                self.status_msg = Some(format!("cannot start a shell: {e}"));
                return;
            }
        };
        let mut last_err = anyhow::anyhow!("no shell found");
        for shell in shells {
            let name = format!("{} {}", shell.label, self.next_id);
            match self.spawn_session(name, shell.launch(&cwd), place) {
                Ok(id) => {
                    tracing::info!(id, shell = shell.label, "session started");
                    return;
                }
                Err(e) => {
                    tracing::warn!(shell = shell.label, "shell spawn failed: {e:#}");
                    last_err = e;
                }
            }
        }
        self.status_msg = Some(format!("cannot start a shell: {last_err:#}"));
    }

    /// Starts `launch` in a new session of the cursor's team (in the daemon when there is one)
    /// and shows it at `place`.
    fn spawn_session(&mut self, name: String, launch: Launch, place: Place) -> anyhow::Result<u32> {
        let id = self.next_id;
        let (rows, cols) = ui::inner_size(self.main_area());
        let mut session = match &self.link {
            Some(link) => {
                let info = SessionInfo {
                    id,
                    name: name.clone(),
                    folder: None,
                    ephemeral: false,
                    rows,
                    cols,
                    age_ms: 0,
                    exited: None,
                    marks: MarksSnapshot::default(),
                    agent: None,
                };
                // Registered before the spawn: its very first output has a screen to go to.
                let session = Session::remote(&info, &[], link.clone(), Instant::now());
                let launch_msg = ClientMsg::Spawn {
                    id,
                    name,
                    launch,
                    rows,
                    cols,
                };
                link.send(&launch_msg)?;
                session
            }
            None => {
                let size = PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                };
                Session::spawn(id, name, launch.to_command(), size, self.tx.clone())?
            }
        };
        session.folder = self.context_folder();
        session.sync(Instant::now());
        self.sessions.push(session);
        self.next_id += 1;
        self.place(id, place);
        Ok(id)
    }

    fn place(&mut self, id: u32, place: Place) {
        if let Place::Hidden = place {
            return;
        }
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
    fn launch(&self, cwd: &Path) -> Launch {
        let mut argv: Vec<String> = self
            .prog
            .into_iter()
            .chain(self.args.iter().copied())
            .map(String::from)
            .collect();
        if self.integrated {
            // After the user's profile: wraps their prompt, survives their settings.
            argv.extend([
                "-NoExit".into(),
                "-EncodedCommand".into(),
                crate::shell::encoded_command(),
            ]);
        }
        Launch::new(argv, cwd.to_path_buf())
    }
}

/// Whether an executable named `prog` is in one of the `PATH` directories.
fn in_path(prog: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    // symlink_metadata: Store aliases (pwsh in WindowsApps) are reparse points that `exists()`
    // can fail to follow.
    std::env::split_paths(&path).any(|dir| dir.join(prog).symlink_metadata().is_ok())
}

/// Shells whose executable is found in `PATH` (the default shell always is).
fn available_shells() -> Vec<&'static ShellSpec> {
    SHELLS
        .iter()
        .filter(|shell| shell.prog.is_none_or(in_path))
        .collect()
}

/// Claude Code's command line (npm installs `claude.cmd` on Windows).
fn claude_installed() -> bool {
    ["claude.exe", "claude.cmd", "claude"]
        .into_iter()
        .any(in_path)
}

/// A terminal editor for `path`: `edit` on Windows (else Notepad), `$EDITOR` or `nano` elsewhere.
fn editor_launch(path: &Path) -> Launch {
    let editor = if cfg!(windows) {
        if in_path("edit.exe") {
            "edit.exe".to_string()
        } else {
            "notepad.exe".to_string()
        }
    } else {
        std::env::var("EDITOR").unwrap_or_else(|_| "nano".into())
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    Launch::new(vec![editor, path.display().to_string()], dir)
}

/// The line shown in a team box: the first one that is not empty nor a Markdown title.
pub fn consigne_excerpt(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
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
            KeyCode::Tab => Action::ToggleView,
            KeyCode::Esc => Action::Cancel,
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
            KeyCode::Char('t') => Action::NewTeam,
            KeyCode::Char('?') => Action::Help,
            KeyCode::Char('j') => Action::Next,
            KeyCode::Char('k') => Action::Prev,
            _ => Action::None,
        },
        Mode::Onboarding => match key.code {
            KeyCode::Enter => Action::Confirm,
            KeyCode::Esc => Action::Cancel,
            KeyCode::Left | KeyCode::BackTab => Action::Prev,
            KeyCode::Right | KeyCode::Tab => Action::Next,
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

/// First line with text on a session's screen.
fn first_screen_line(session: &Session) -> Option<String> {
    let parser = session.parser.lock().unwrap();
    let contents = parser.screen().contents();
    contents
        .lines()
        .map(str::trim)
        .find(|line| line.chars().any(char::is_alphanumeric))
        .map(str::to_string)
}

/// Cuts `text` to `max` chars, ending with "…" when cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
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
            pos: (0, 0),
            consigne: None,
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
    fn closing_everything_is_marked_destructive() {
        assert!(MenuAction::QuitAll.destructive());
        assert!(!MenuAction::Detach.destructive());
    }

    #[test]
    fn truncate_keeps_short_text_and_marks_cuts() {
        assert_eq!(truncate("cargo build", 30), "cargo build");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("", 4), "");
    }

    #[test]
    fn tab_switches_view_and_t_creates_a_team() {
        let tab = key(KeyCode::Tab, KeyModifiers::NONE);
        let t = key(KeyCode::Char('t'), KeyModifiers::NONE);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Workspace, tab, true, false),
            Action::ToggleView
        );
        assert_eq!(key_action(Mode::Workspace, t, true, false), Action::NewTeam);
        assert_eq!(
            key_action(Mode::Workspace, esc, true, false),
            Action::Cancel
        );
        assert_eq!(
            key_action(Mode::Terminal, tab, true, false),
            Action::Send(b"\t".to_vec())
        );
    }

    #[test]
    fn onboarding_keys() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        let right = key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            key_action(Mode::Onboarding, enter, true, false),
            Action::Confirm
        );
        assert_eq!(
            key_action(Mode::Onboarding, esc, true, false),
            Action::Cancel
        );
        assert_eq!(
            key_action(Mode::Onboarding, right, true, false),
            Action::Next
        );
    }

    #[test]
    fn consigne_excerpt_skips_titles_and_blank_lines() {
        assert_eq!(
            consigne_excerpt("# backend\n\n  Rust API, tests first.\nmore"),
            Some("Rust API, tests first.".into())
        );
        assert_eq!(consigne_excerpt("# only a title\n"), None);
        assert_eq!(consigne_excerpt(""), None);
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
