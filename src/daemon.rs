//! The daemon owns the sessions so that they outlive the TUI. One per project: started by the
//! first `chiika` (`chiika daemon`), stopped when it has neither a running session nor a client.

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::PtySize;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::agent::{self, AgentInfo};
use crate::app::AppEvent;
use crate::channel::{self, Reply, Request};
use crate::config;
use crate::coord;
use crate::protocol::{self, ClientMsg, DaemonMsg, Frame};
use crate::session::{Session, Status, Tap};

/// A daemon nobody attached to stops after this long.
pub const IDLE_EXIT: Duration = Duration::from_secs(10);

/// Where a running daemon can be reached: `.chiika/daemon.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    pub pid: u32,
    pub port: u16,
    pub token: String,
}

pub fn runtime_file(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.toml")
}

/// `chiika daemon`: serves the project whose data is in `data_dir` until there is nothing left.
pub fn run(data_dir: &Path) -> anyhow::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let runtime = Runtime {
        pid: std::process::id(),
        port: listener.local_addr()?.port(),
        token: channel::random_token(),
    };
    let file = runtime_file(data_dir);
    config::save(&file, &runtime)?;
    tracing::info!(port = runtime.port, "daemon started");
    let build = std::env::var("CHIIKA_BUILD").unwrap_or_default();
    let result = serve(
        listener,
        &runtime.token,
        IDLE_EXIT,
        Some(data_dir.to_path_buf()),
        &build,
    );
    // Only our own file: a newer daemon may have replaced it.
    let ours = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| toml::from_str::<Runtime>(&text).ok())
        .is_some_and(|r| r.pid == runtime.pid);
    if ours {
        let _ = std::fs::remove_file(&file);
    }
    tracing::info!("daemon stopped");
    result
}

enum Control {
    Connected(TcpStream),
    /// From the client of this generation.
    Msg(u64, ClientMsg),
    Input(u64, u32, Vec<u8>),
    Gone(u64),
}

/// The daemon loop: sessions, hooks, and one client at a time (the newest replaces the others).
/// `data_dir` is the project's `.chiika` folder (team boards).
pub fn serve(
    listener: TcpListener,
    token: &str,
    idle_exit: Duration,
    data_dir: Option<PathBuf>,
    build: &str,
) -> anyhow::Result<()> {
    let (app_tx, app_rx) = mpsc::channel::<AppEvent>();
    let (ctl_tx, ctl_rx) = mpsc::channel::<Control>();
    let hooks = channel::start(app_tx.clone())
        .inspect_err(|e| tracing::warn!("hook channel unavailable: {e}"))
        .ok()
        .map(|c| (c.port, c.token));
    let accept_tx = ctl_tx.clone();
    thread::Builder::new()
        .name("daemon-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if accept_tx.send(Control::Connected(stream)).is_err() {
                    return;
                }
            }
        })?;

    let tap: Tap = Arc::new(Mutex::new(None));
    let mut sessions: Vec<Session> = Vec::new();
    // Permission requests held for the user's answer, by agent session.
    let mut pending: HashMap<u32, (Reply, Instant)> = HashMap::new();
    let mut client: Option<u64> = None;
    let mut generation = 0;
    let mut had_client = false;
    let started = Instant::now();
    loop {
        while let Ok(event) = app_rx.try_recv() {
            let AppEvent::Channel(id, request, reply) = event else {
                continue;
            };
            match request {
                Request::Hook(event) => {
                    let agent = sessions
                        .iter_mut()
                        .find(|s| s.id == id)
                        .and_then(|s| s.agent.as_mut());
                    if let Some(agent) = agent {
                        agent.apply(&event, Instant::now());
                    }
                    // A permission request waits for the user only if someone can answer it;
                    // otherwise its reply is dropped and the agent asks in its terminal.
                    if let Some(reply) = reply
                        && client.is_some()
                    {
                        pending.insert(id, (reply, Instant::now()));
                    }
                    send(&tap, &DaemonMsg::Hook { id, event });
                }
                Request::Tool { name, arguments } => {
                    let outcome =
                        coord::run(&mut sessions, data_dir.as_deref(), id, &name, &arguments);
                    if let Some(reply) = reply {
                        reply.send(&outcome.to_mcp());
                    }
                    if let Some((from, to)) = outcome.link {
                        send(&tap, &DaemonMsg::Pulse { from, to });
                    }
                }
            }
        }
        pending.retain(|_, (_, since)| since.elapsed() < agent::PERMISSION_WAIT);
        match ctl_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Control::Connected(stream)) => {
                let Some(reader) = handshake(&stream, token) else {
                    continue;
                };
                generation += 1;
                send(&tap, &DaemonMsg::Replaced);
                attach(&tap, stream, &sessions, hooks.clone(), build);
                let gen_tx = ctl_tx.clone();
                let current = generation;
                thread::Builder::new()
                    .name(format!("daemon-client-{current}"))
                    .spawn(move || read_client(reader, current, &gen_tx))?;
                client = Some(generation);
                had_client = true;
            }
            Ok(Control::Msg(from, ClientMsg::Decide { id, allow })) if client == Some(from) => {
                // No answer (`None`): the reply is dropped, the agent asks in its terminal.
                if let Some((reply, _)) = pending.remove(&id)
                    && let Some(allow) = allow
                {
                    reply.send(&agent::decision(allow));
                }
            }
            Ok(Control::Msg(from, msg)) if client == Some(from) => {
                if !handle(msg, &mut sessions, &tap, &app_tx) {
                    sessions.iter_mut().for_each(Session::kill);
                    return Ok(());
                }
            }
            Ok(Control::Input(from, id, bytes)) if client == Some(from) => {
                if let Some(session) = sessions.iter_mut().find(|s| s.id == id) {
                    let _ = session.write(&bytes);
                }
            }
            Ok(Control::Gone(from)) if client == Some(from) => {
                client = None;
                pending.clear();
                *tap.lock().unwrap() = None;
                tracing::info!("client detached");
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        for session in &mut sessions {
            if session.poll_exit() {
                let code = match session.status {
                    Status::Exited(code) => code,
                    Status::Running => None,
                };
                send(
                    &tap,
                    &DaemonMsg::Exited {
                        id: session.id,
                        code,
                    },
                );
            }
        }
        let running = sessions.iter().any(|s| s.status == Status::Running);
        let waited = had_client || started.elapsed() > idle_exit;
        if client.is_none() && !running && waited {
            return Ok(());
        }
    }
}

/// Applies a client message; `false` means stop the daemon.
fn handle(
    msg: ClientMsg,
    sessions: &mut Vec<Session>,
    tap: &Tap,
    app_tx: &mpsc::Sender<AppEvent>,
) -> bool {
    let find = |sessions: &mut Vec<Session>, id: u32| sessions.iter().position(|s| s.id == id);
    match msg {
        ClientMsg::Spawn {
            id,
            name,
            launch,
            rows,
            cols,
        } => {
            let size = PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            };
            let cmd = launch.to_command();
            match Session::spawn_tapped(id, name, cmd, size, app_tx.clone(), Some(tap.clone())) {
                Ok(session) => sessions.push(session),
                Err(e) => send(
                    tap,
                    &DaemonMsg::SpawnFailed {
                        id,
                        error: format!("{e:#}"),
                    },
                ),
            }
        }
        ClientMsg::Resize { id, rows, cols } => {
            if let Some(i) = find(sessions, id)
                && let Err(e) = sessions[i].resize(rows, cols)
            {
                tracing::warn!(id, "resize failed: {e:#}");
            }
        }
        ClientMsg::Kill { id } => {
            if let Some(i) = find(sessions, id) {
                sessions.remove(i).kill();
            }
        }
        ClientMsg::Meta {
            id,
            name,
            folder,
            ephemeral,
            agent,
        } => {
            if let Some(i) = find(sessions, id) {
                let session = &mut sessions[i];
                session.name = name;
                session.folder = folder;
                session.ephemeral = ephemeral;
                session.agent = agent.map(|a| AgentInfo::from_snapshot(&a, Instant::now()));
            }
        }
        ClientMsg::Shutdown => return false,
        ClientMsg::Hello { .. } | ClientMsg::Decide { .. } => {}
    }
    true
}

/// The first frame must be `Hello` with the right token; returns the read side.
fn handshake(stream: &TcpStream, token: &str) -> Option<TcpStream> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut reader = stream.try_clone().ok()?;
    match protocol::read_frame::<ClientMsg>(&mut reader) {
        Ok(Frame::Msg(ClientMsg::Hello { token: got })) if got == token => {}
        _ => {
            tracing::warn!("daemon: connection refused");
            return None;
        }
    }
    reader.set_read_timeout(None).ok()?;
    Some(reader)
}

/// Sends the welcome and every screen with its history, then makes `stream` the client. All screens stay locked
/// meanwhile: output that follows a screen is sent after it, none is lost or doubled.
fn attach(
    tap: &Tap,
    mut stream: TcpStream,
    sessions: &[Session],
    hooks: Option<(u16, String)>,
    build: &str,
) {
    let now = Instant::now();
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut screens: Vec<_> = sessions.iter().map(|s| s.parser.lock().unwrap()).collect();
    let welcome = DaemonMsg::Welcome {
        sessions: sessions
            .iter()
            .zip(&screens)
            .map(|(s, screen)| s.info(screen, now))
            .collect(),
        hooks,
        build: build.to_string(),
    };
    let mut tap = tap.lock().unwrap();
    let sent = protocol::write_msg(&mut stream, &welcome).and_then(|()| {
        sessions
            .iter()
            .zip(&mut screens)
            .try_for_each(|(s, screen)| {
                protocol::write_data(&mut stream, s.id, &Session::screen_with_history(screen))
            })
    });
    *tap = match sent {
        Ok(()) => Some(Box::new(stream) as Box<dyn Write + Send>),
        Err(e) => {
            tracing::warn!("attach failed: {e}");
            None
        }
    };
}

fn read_client(mut reader: TcpStream, generation: u64, tx: &mpsc::Sender<Control>) {
    loop {
        let control = match protocol::read_frame::<ClientMsg>(&mut reader) {
            Ok(Frame::Msg(msg)) => Control::Msg(generation, msg),
            Ok(Frame::Data(id, bytes)) => Control::Input(generation, id, bytes),
            Err(_) => {
                let _ = tx.send(Control::Gone(generation));
                return;
            }
        };
        if tx.send(control).is_err() {
            return;
        }
    }
}

/// Sends `msg` to the client, if any; a client that cannot be written to is dropped.
fn send(tap: &Tap, msg: &DaemonMsg) {
    let mut tap = tap.lock().unwrap();
    if let Some(client) = tap.as_mut()
        && protocol::write_msg(client, msg).is_err()
    {
        *tap = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::link;
    use crate::protocol::{Launch, MarksSnapshot, SessionInfo};

    fn start(idle_exit: Duration) -> (u16, thread::JoinHandle<anyhow::Result<()>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || serve(listener, "secret", idle_exit, None, "b1"));
        (port, server)
    }

    fn screen_text(screen: &[u8]) -> String {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(screen);
        parser.screen().contents()
    }

    #[test]
    fn sessions_survive_a_detach_and_the_newest_client_wins() {
        let (port, server) = start(IDLE_EXIT);
        let first = link::connect(port, "secret").unwrap();
        assert!(first.sessions.is_empty());
        assert_eq!(first.build, "b1");
        let info = SessionInfo {
            id: 1,
            name: "s".into(),
            folder: None,
            ephemeral: false,
            rows: 24,
            cols: 80,
            age_ms: 0,
            exited: None,
            marks: MarksSnapshot::default(),
            agent: None,
        };
        let session = Session::remote(&info, &[], first.link.clone(), Instant::now());
        let (tx, _rx) = mpsc::channel();
        first.link.listen(first.reader, tx);
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let spawn = ClientMsg::Spawn {
            id: 1,
            name: "s".into(),
            launch: Launch::new(vec![shell.into()], std::env::temp_dir()),
            rows: 24,
            cols: 80,
        };
        first.link.send(&spawn).unwrap();
        let meta = ClientMsg::Meta {
            id: 1,
            name: "renamed".into(),
            folder: Some(4),
            ephemeral: false,
            agent: None,
        };
        first.link.send(&meta).unwrap();
        first.link.input(1, b"echo chiika-daemon\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // The typed line and the echo's output: twice.
        let printed = |session: &Session| {
            let parser = session.parser.lock().unwrap();
            parser.screen().contents().matches("chiika-daemon").count()
        };
        while printed(&session) < 2 {
            assert!(Instant::now() < deadline, "no output from the daemon");
            thread::sleep(Duration::from_millis(50));
        }
        first.link.close();
        thread::sleep(Duration::from_millis(300));

        let second = link::connect(port, "secret").unwrap();
        assert_eq!(
            second.sessions.len(),
            1,
            "the shell outlived the first client"
        );
        let (info, screen) = &second.sessions[0];
        assert_eq!((info.id, info.name.as_str()), (1, "renamed"));
        assert_eq!(info.folder, Some(4));
        assert_eq!(info.exited, None);
        assert!(screen_text(screen).contains("chiika-daemon"));
        let (tx, rx) = mpsc::channel();
        second.link.listen(second.reader, tx);

        let third = link::connect(port, "secret").unwrap();
        let replaced = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(replaced, AppEvent::DaemonGone(_)),
            "second was told"
        );
        third.link.send(&ClientMsg::Shutdown).unwrap();
        server.join().unwrap().unwrap();
    }

    #[test]
    fn agents_get_tools_and_the_user_answers_their_permissions() {
        let (port, server) = start(IDLE_EXIT);
        let attached = link::connect(port, "secret").unwrap();
        let (hook_port, hook_token) = attached.hooks.clone().unwrap();
        let (tx, rx) = mpsc::channel();
        attached.link.listen(attached.reader, tx);
        let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
        attached
            .link
            .send(&ClientMsg::Spawn {
                id: 1,
                name: "coder".into(),
                launch: Launch::new(vec![shell.into()], std::env::temp_dir()),
                rows: 24,
                cols: 80,
            })
            .unwrap();
        let agent = AgentInfo::new(5, PathBuf::new(), None, Instant::now());
        attached
            .link
            .send(&ClientMsg::Meta {
                id: 1,
                name: "coder".into(),
                folder: Some(5),
                ephemeral: false,
                agent: Some(agent.snapshot(Instant::now())),
            })
            .unwrap();
        thread::sleep(Duration::from_millis(300));

        // A tool call through the hook channel, as `chiika mcp` makes it.
        let wait = Duration::from_secs(5);
        let message = channel::tool_message("team", &serde_json::json!({}));
        let answer = channel::request(hook_port, &hook_token, "1", &message, wait).unwrap();
        let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
        assert_eq!(
            answer["content"][0]["text"],
            "- coder (you): agent, starting"
        );

        // A permission request waits for the user's answer.
        let hook = br#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#.to_vec();
        let asking = thread::spawn(move || {
            channel::request(hook_port, &hook_token, "1", &hook, Duration::from_secs(10))
        });
        let seen = loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                AppEvent::Hook(id, event) => break (id, event.name),
                _ => continue,
            }
        };
        assert_eq!(seen, (1, "PermissionRequest".to_string()));
        attached
            .link
            .send(&ClientMsg::Decide {
                id: 1,
                allow: Some(true),
            })
            .unwrap();
        let decision = asking.join().unwrap().unwrap();
        assert_eq!(decision, agent::decision(true));

        attached.link.close();
        thread::sleep(Duration::from_millis(300));
        let second = link::connect(port, "secret").unwrap();
        second.link.send(&ClientMsg::Shutdown).unwrap();
        server.join().unwrap().unwrap();
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (port, server) = start(Duration::from_millis(300));
        assert!(link::connect(port, "guess").is_err());
        server.join().unwrap().unwrap();
    }

    #[test]
    fn an_unused_daemon_stops_by_itself() {
        let (_, server) = start(Duration::from_millis(200));
        let started = Instant::now();
        server.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
