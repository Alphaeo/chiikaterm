use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::agent::{AgentInfo, AgentState};
use crate::app::AppEvent;
use crate::link::Link;
use crate::protocol::{self, ClientMsg, SessionInfo};
use crate::shell::{Finished, ShellMarks};

/// Seconds of output history kept for the activity sparkline.
pub const ACTIVITY_SECS: usize = 8;

/// A session counts as busy for this long after its last output.
const BUSY_FOR: Duration = Duration::from_millis(600);

/// The daemon's current client, to which every session forwards its output.
pub type Tap = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Exited(Option<u32>),
}

/// Where the program runs.
enum Backend {
    /// In this process (the daemon, or chiika without a daemon).
    Local {
        master: Option<Box<dyn MasterPty + Send>>,
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        child: Box<dyn Child + Send + Sync>,
    },
    /// In the daemon: input and resizes go through the link, output comes from it.
    Remote {
        link: Link,
        /// Exit reported by the daemon, not yet returned by `poll_exit`.
        exit_pending: bool,
    },
}

pub struct Session {
    pub id: u32,
    pub name: String,
    pub parser: Arc<Mutex<vt100::Parser<ShellMarks>>>,
    pub status: Status,
    backend: Backend,
    pub started: Instant,
    pub activity: Activity,
    /// Sidebar folder (= canvas team) holding this session.
    pub folder: Option<u32>,
    /// A helper (the instructions editor): closed as soon as its program exits.
    pub ephemeral: bool,
    /// Set when this session runs a Claude Code agent.
    pub agent: Option<AgentInfo>,
    last_output: Option<Instant>,
}

/// Bytes of output per second over the last `ACTIVITY_SECS` seconds, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    buckets: VecDeque<u32>,
    /// Second (since session start) of the last bucket.
    sec: u64,
}

impl Default for Activity {
    fn default() -> Self {
        Activity {
            buckets: VecDeque::from([0; ACTIVITY_SECS]),
            sec: 0,
        }
    }
}

impl Activity {
    /// Shifts the window so its last bucket is second `sec`.
    pub fn advance(&mut self, sec: u64) {
        if sec <= self.sec {
            return;
        }
        let gap = (sec - self.sec).min(ACTIVITY_SECS as u64);
        for _ in 0..gap {
            self.buckets.pop_front();
            self.buckets.push_back(0);
        }
        self.sec = sec;
    }

    pub fn record(&mut self, sec: u64, bytes: usize) {
        self.advance(sec);
        let last = self.buckets.back_mut().expect("window is never empty");
        *last = last.saturating_add(u32::try_from(bytes).unwrap_or(u32::MAX));
    }

    pub fn values(&self) -> Vec<u32> {
        self.buckets.iter().copied().collect()
    }
}

impl Session {
    pub fn spawn(
        id: u32,
        name: String,
        cmd: CommandBuilder,
        size: PtySize,
        tx: Sender<AppEvent>,
    ) -> anyhow::Result<Session> {
        Session::spawn_tapped(id, name, cmd, size, tx, None)
    }

    /// Starts `cmd` in a new PTY. With a `tap` (in the daemon), the output is also sent to the
    /// client, while the screen is locked so that a client attaching meanwhile gets it once.
    pub fn spawn_tapped(
        id: u32,
        name: String,
        cmd: CommandBuilder,
        size: PtySize,
        tx: Sender<AppEvent>,
        tap: Option<Tap>,
    ) -> anyhow::Result<Session> {
        let pair = native_pty_system().openpty(size)?;
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser =
            vt100::Parser::new_with_callbacks(size.rows, size.cols, 1000, ShellMarks::default());
        let parser = Arc::new(Mutex::new(parser));

        let thread_parser = Arc::clone(&parser);
        let thread_writer = Arc::clone(&writer);
        thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = tx.send(AppEvent::PtyClosed(id));
                            return;
                        }
                        Ok(n) => {
                            tracing::trace!(id, bytes = ?&buf[..n], "pty output");
                            let mut parser = thread_parser.lock().unwrap();
                            parser.process(&buf[..n]);
                            // ConPTY asks for the cursor position at startup and blocks until
                            // it gets an answer (DSR). ponytail: a query split across two reads
                            // is missed.
                            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                                let (row, col) = parser.screen().cursor_position();
                                let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                                let mut writer = thread_writer.lock().unwrap();
                                let _ = writer
                                    .write_all(reply.as_bytes())
                                    .and_then(|()| writer.flush());
                            }
                            if let Some(tap) = &tap {
                                let mut client = tap.lock().unwrap();
                                if let Some(stream) = client.as_mut()
                                    && protocol::write_data(stream, id, &buf[..n]).is_err()
                                {
                                    *client = None;
                                }
                            }
                            drop(parser);
                            let _ = tx.send(AppEvent::PtyOutput(id, n));
                        }
                    }
                }
            })
            .context("spawning pty reader thread")?;

        Ok(Session {
            id,
            name,
            parser,
            status: Status::Running,
            backend: Backend::Local {
                master: Some(pair.master),
                writer,
                child,
            },
            started: Instant::now(),
            activity: Activity::default(),
            folder: None,
            ephemeral: false,
            agent: None,
            last_output: None,
        })
    }

    /// A session running in the daemon, shown from `screen` (as sent when attaching).
    pub fn remote(info: &SessionInfo, screen: &[u8], link: Link, now: Instant) -> Session {
        let marks = ShellMarks::from_snapshot(&info.marks, now);
        let mut parser = vt100::Parser::new_with_callbacks(info.rows, info.cols, 1000, marks);
        parser.process(screen);
        let parser = Arc::new(Mutex::new(parser));
        link.register(info.id, Arc::clone(&parser));
        let age = Duration::from_millis(info.age_ms);
        Session {
            id: info.id,
            name: info.name.clone(),
            parser,
            status: info.exited.map_or(Status::Running, Status::Exited),
            backend: Backend::Remote {
                link,
                exit_pending: false,
            },
            started: now.checked_sub(age).unwrap_or(now),
            activity: Activity::default(),
            folder: info.folder,
            ephemeral: info.ephemeral,
            agent: info
                .agent
                .as_ref()
                .map(|agent| AgentInfo::from_snapshot(agent, now)),
            last_output: None,
        }
    }

    /// What the daemon sends of `parser` to an attaching client: its history, then its screen.
    /// Replayed in a parser of the same size, the history lands in that parser's own.
    pub fn screen_with_history(parser: &mut vt100::Parser<ShellMarks>) -> Vec<u8> {
        let screen = parser.screen_mut();
        let (rows, cols) = screen.size();
        let mut out = Vec::new();
        // ponytail: one offset per history line (1000 at most), each reading its top row.
        screen.set_scrollback(usize::MAX);
        for offset in (1..=screen.scrollback()).rev() {
            screen.set_scrollback(offset);
            out.extend(screen.rows_formatted(0, cols).next().unwrap_or_default());
            out.extend(b"\x1b[m\r\n");
        }
        screen.set_scrollback(0);
        // Pushes the last history lines off the screen, which the screen's state then repaints.
        if !out.is_empty() {
            out.extend(b"\r\n".repeat(usize::from(rows.saturating_sub(1))));
        }
        out.extend(screen.state_formatted());
        out
    }

    /// What the daemon tells an attaching client about this session; `screen` is its locked
    /// parser.
    pub fn info(&self, screen: &vt100::Parser<ShellMarks>, now: Instant) -> SessionInfo {
        let (rows, cols) = screen.screen().size();
        SessionInfo {
            id: self.id,
            name: self.name.clone(),
            folder: self.folder,
            ephemeral: self.ephemeral,
            rows,
            cols,
            age_ms: now.saturating_duration_since(self.started).as_millis() as u64,
            exited: match self.status {
                Status::Running => None,
                Status::Exited(code) => Some(code),
            },
            marks: screen.callbacks().snapshot(now),
            agent: self.agent.as_ref().map(|agent| agent.snapshot(now)),
        }
    }

    /// The daemon reported that this remote session exited.
    pub fn set_exited(&mut self, code: Option<u32>) {
        if let Backend::Remote { exit_pending, .. } = &mut self.backend
            && self.status == Status::Running
        {
            self.status = Status::Exited(code);
            *exit_pending = true;
        }
    }

    /// Tells the daemon what to keep about this session (name, team, agent…).
    pub fn sync(&self, now: Instant) {
        if let Backend::Remote { link, .. } = &self.backend {
            let meta = ClientMsg::Meta {
                id: self.id,
                name: self.name.clone(),
                folder: self.folder,
                ephemeral: self.ephemeral,
                agent: self
                    .agent
                    .as_ref()
                    .map(|agent| Box::new(agent.snapshot(now))),
            };
            if let Err(e) = link.send(&meta) {
                tracing::warn!(id = self.id, "cannot update the daemon: {e}");
            }
        }
    }

    pub fn record_output(&mut self, now: Instant, bytes: usize) {
        self.activity.record(self.second(now), bytes);
        self.last_output = Some(now);
    }

    /// Moves the activity window forward so idle seconds show up as zeros.
    pub fn tick(&mut self, now: Instant) {
        self.activity.advance(self.second(now));
    }

    /// A command is running (shell integration), else output arrived just now.
    pub fn is_busy(&self, now: Instant) -> bool {
        if self.status != Status::Running {
            return false;
        }
        if let Some(agent) = &self.agent {
            return agent.state == AgentState::Working;
        }
        let parser = self.parser.lock().unwrap();
        let marks = parser.callbacks();
        if marks.integrated {
            return marks.running.is_some();
        }
        self.last_output
            .is_some_and(|t| now.saturating_duration_since(t) < BUSY_FOR)
    }

    /// An agent waiting for an answer of the user.
    pub fn needs_user(&self, now: Instant) -> bool {
        self.status == Status::Running
            && self
                .agent
                .as_ref()
                .is_some_and(|a| a.shown_state(now) == AgentState::Waiting)
    }

    pub fn last_output(&self) -> Option<Instant> {
        self.last_output
    }

    /// Runs `f` on what the shell integration knows.
    pub fn marks<T>(&self, f: impl FnOnce(&ShellMarks) -> T) -> T {
        f(self.parser.lock().unwrap().callbacks())
    }

    /// Commands finished since the last call.
    pub fn take_finished(&self) -> Vec<Finished> {
        self.parser.lock().unwrap().callbacks_mut().take_finished()
    }

    fn second(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs()
    }

    // ponytail: écriture synchrone, passer à un thread d'écriture par session si on observe des gels.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match &self.backend {
            Backend::Local { writer, .. } => {
                let mut writer = writer.lock().unwrap();
                writer.write_all(bytes)?;
                writer.flush()
            }
            Backend::Remote { link, .. } => link.input(self.id, bytes),
        }
    }

    /// Current screen size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        self.parser.lock().unwrap().screen().size()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Local {
                master: Some(master),
                ..
            } => master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?,
            Backend::Local { master: None, .. } => {}
            Backend::Remote { link, .. } => link.send(&ClientMsg::Resize {
                id: self.id,
                rows,
                cols,
            })?,
        }
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
        Ok(())
    }

    /// Updates `status` once the child has exited. Under ConPTY the reader does not always get
    /// an EOF when the process ends: dropping the master unblocks the reader thread.
    /// Returns true when the session has just exited.
    pub fn poll_exit(&mut self) -> bool {
        match &mut self.backend {
            Backend::Remote { exit_pending, .. } => std::mem::take(exit_pending),
            Backend::Local { .. } if self.status != Status::Running => false,
            Backend::Local { master, child, .. } => {
                let Ok(Some(exit)) = child.try_wait() else {
                    return false;
                };
                let code = exit.signal().is_none().then(|| exit.exit_code());
                tracing::info!(id = self.id, ?code, "session exited");
                self.status = Status::Exited(code);
                *master = None;
                true
            }
        }
    }

    pub fn kill(&mut self) {
        match &mut self.backend {
            Backend::Local { child, .. } => {
                let _ = child.kill();
            }
            Backend::Remote { link, .. } => {
                let _ = link.send(&ClientMsg::Kill { id: self.id });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn spawn(cmd: CommandBuilder) -> (Session, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        (Session::spawn(1, "test".into(), cmd, size, tx).unwrap(), rx)
    }

    fn assert_prints_hello_and_exits(cmd: CommandBuilder) {
        let (mut session, _rx) = spawn(cmd);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            session.poll_exit();
            let printed = session
                .parser
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("hello");
            if printed && session.status == Status::Exited(Some(0)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timeout, status {:?}",
                session.status
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(windows)]
    #[test]
    fn runs_command_and_reports_exit() {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "echo hello"]);
        assert_prints_hello_and_exits(cmd);
    }

    #[cfg(unix)]
    #[test]
    fn runs_command_and_reports_exit() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf hello"]);
        assert_prints_hello_and_exits(cmd);
    }

    #[test]
    fn resize_updates_screen_size() {
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let (mut session, _rx) = spawn(CommandBuilder::new(shell));
        session.resize(10, 40).unwrap();
        assert_eq!(session.parser.lock().unwrap().screen().size(), (10, 40));
        session.kill();
    }

    #[test]
    fn history_and_screen_are_replayed() {
        let mut daemon = vt100::Parser::new_with_callbacks(5, 20, 1000, ShellMarks::default());
        for line in 1..=30 {
            daemon.process(format!("\x1b[1mline {line}\x1b[m\r\n").as_bytes());
        }
        daemon.process(b"$ ls");
        let bytes = Session::screen_with_history(&mut daemon);
        let mut client = vt100::Parser::new_with_callbacks(5, 20, 1000, ShellMarks::default());
        client.process(&bytes);

        assert_eq!(client.screen().contents(), daemon.screen().contents());
        assert_eq!(client.screen().cursor_position(), (4, 4));
        let history = |p: &mut vt100::Parser<ShellMarks>| {
            p.screen_mut().set_scrollback(usize::MAX);
            let lines = p.screen().scrollback();
            let top = p.screen().contents_formatted();
            p.screen_mut().set_scrollback(0);
            (lines, top)
        };
        let (lines, top) = history(&mut client);
        assert_eq!((lines, top.clone()), history(&mut daemon));
        assert!(
            String::from_utf8_lossy(&top).contains("line 1"),
            "bold kept"
        );

        let mut empty = vt100::Parser::new_with_callbacks(5, 20, 1000, ShellMarks::default());
        empty.process(b"$ ");
        let mut client = vt100::Parser::new_with_callbacks(5, 20, 1000, ShellMarks::default());
        client.process(&Session::screen_with_history(&mut empty));
        assert_eq!(history(&mut client).0, 0, "no history, no blank lines");
    }

    #[test]
    fn activity_accumulates_within_a_second() {
        let mut activity = Activity::default();
        activity.record(0, 10);
        activity.record(0, 5);
        assert_eq!(activity.values(), [0, 0, 0, 0, 0, 0, 0, 15]);
    }

    #[test]
    fn activity_window_slides_and_stays_bounded() {
        let mut activity = Activity::default();
        activity.record(0, 10);
        activity.record(2, 7);
        assert_eq!(activity.values(), [0, 0, 0, 0, 0, 10, 0, 7]);
        activity.advance(100);
        assert_eq!(activity.values(), [0; ACTIVITY_SECS]);
    }

    #[test]
    fn activity_ignores_going_back_in_time() {
        let mut activity = Activity::default();
        activity.advance(5);
        activity.record(3, 4);
        assert_eq!(activity.values(), [0, 0, 0, 0, 0, 0, 0, 4]);
    }

    /// Runs `line` (which exits with 3) in an integrated shell and checks chiika saw it;
    /// returns the shell's directory as reported.
    fn assert_integration_reports(cmd: CommandBuilder, line: &str) -> String {
        let (mut session, _rx) = spawn(cmd);
        let wait = |session: &Session, what: &str, done: &dyn Fn(&ShellMarks) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !session.marks(done) {
                let screen = session.parser.lock().unwrap().screen().contents();
                assert!(
                    Instant::now() < deadline,
                    "timeout waiting for {what}, screen:\n{screen}"
                );
                thread::sleep(Duration::from_millis(50));
            }
        };
        wait(&session, "first prompt", &|m| {
            m.integrated && m.cwd.is_some()
        });
        session.write(format!("{line}\r").as_bytes()).unwrap();
        wait(&session, "command end", &|m| m.last.is_some());
        let last = session.marks(|m| m.last.clone()).unwrap();
        assert_eq!(last.text, line);
        assert_eq!(last.code, 3);
        assert_eq!(session.take_finished().len(), 1);
        assert!(!session.is_busy(Instant::now()));
        session.kill();
        session.marks(|m| m.cwd.clone()).unwrap()
    }

    /// The bash or zsh `prog` with chiika's integration, its scripts in a fresh folder.
    fn integrated(prog: &str, integration: crate::shell::Integration) -> CommandBuilder {
        let dir = std::env::temp_dir().join(format!("chiika-{prog}-{}", std::process::id()));
        let mut launch = crate::protocol::Launch::new(vec![prog.into()], dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        integration.apply(&mut launch, Some(&dir)).unwrap();
        launch.to_command()
    }

    #[cfg(windows)]
    #[test]
    fn powershell_integration_reports_commands_and_cwd() {
        let mut cmd = CommandBuilder::new("powershell.exe");
        let script = crate::shell::encoded_command();
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-NoExit",
            "-EncodedCommand",
            &script,
        ]);
        assert_integration_reports(cmd, "cmd /c exit 3");
    }

    // Linux only: macOS ships bash 3.2, without the PS0 the command line needs.
    #[cfg(target_os = "linux")]
    #[test]
    fn bash_integration_reports_commands_and_cwd() {
        let cmd = integrated("bash", crate::shell::Integration::Bash);
        assert_integration_reports(cmd, "echo a; (exit 3)");
    }

    #[cfg(unix)]
    #[test]
    fn zsh_integration_reports_commands_and_cwd() {
        if std::process::Command::new("zsh")
            .arg("-c")
            .arg(":")
            .status()
            .is_err()
        {
            eprintln!("zsh not installed, skipped");
            return;
        }
        let cmd = integrated("zsh", crate::shell::Integration::Zsh);
        assert_integration_reports(cmd, "echo a; (exit 3)");
    }

    // Needs a WSL distribution with bash: `cargo test -- --ignored` on a machine that has one.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn wsl_integration_reports_commands_and_cwd() {
        let cmd = integrated("wsl.exe", crate::shell::Integration::Wsl);
        let cwd = assert_integration_reports(cmd, "echo a; (exit 3)");
        assert!(std::path::Path::new(&cwd).is_dir(), "a Windows path: {cwd}");
    }
}
