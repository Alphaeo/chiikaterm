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
use crate::shell::{Finished, ShellMarks};

/// Seconds of output history kept for the activity sparkline.
pub const ACTIVITY_SECS: usize = 8;

/// A session counts as busy for this long after its last output.
const BUSY_FOR: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Exited(Option<u32>),
}

pub struct Session {
    pub id: u32,
    pub name: String,
    pub parser: Arc<Mutex<vt100::Parser<ShellMarks>>>,
    pub status: Status,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
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
            master: Some(pair.master),
            writer,
            child,
            started: Instant::now(),
            activity: Activity::default(),
            folder: None,
            ephemeral: false,
            agent: None,
            last_output: None,
        })
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
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Current screen size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        self.parser.lock().unwrap().screen().size()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        if let Some(master) = &self.master {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
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
        if self.status != Status::Running {
            return false;
        }
        if let Ok(Some(exit)) = self.child.try_wait() {
            let code = exit.signal().is_none().then(|| exit.exit_code());
            tracing::info!(id = self.id, ?code, "session exited");
            self.status = Status::Exited(code);
            self.master = None;
            return true;
        }
        false
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
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
        let (mut session, _rx) = spawn(cmd);
        let wait = |session: &Session, what: &str, done: &dyn Fn(&ShellMarks) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !session.marks(done) {
                assert!(Instant::now() < deadline, "timeout waiting for {what}");
                thread::sleep(Duration::from_millis(50));
            }
        };
        wait(&session, "first prompt", &|m| {
            m.integrated && m.cwd.is_some()
        });
        session.write(b"cmd /c exit 3\r").unwrap();
        wait(&session, "command end", &|m| m.last.is_some());
        let last = session.marks(|m| m.last.clone()).unwrap();
        assert_eq!(last.text, "cmd /c exit 3");
        assert_eq!(last.code, 3);
        assert_eq!(session.take_finished().len(), 1);
        assert!(!session.is_busy(Instant::now()));
        session.kill();
    }
}
