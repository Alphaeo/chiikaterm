//! Client side of the daemon: attaching to it (starting it if needed) and talking to it.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::app::AppEvent;
use crate::daemon::{self, Runtime};
use crate::protocol::{self, ClientMsg, DaemonMsg, Frame, SessionInfo};
use crate::shell::ShellMarks;

type Parser = Arc<Mutex<vt100::Parser<ShellMarks>>>;

/// How long to wait for a daemon that was just started.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// A connection to the daemon, shared by the sessions it hosts.
#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<TcpStream>,
    /// Screens fed by the daemon's output, by session id.
    parsers: Mutex<HashMap<u32, Parser>>,
}

/// A new connection: the daemon's sessions with their screens, and where agents send hooks.
pub struct Attached {
    pub link: Link,
    pub sessions: Vec<(SessionInfo, Vec<u8>)>,
    pub hooks: Option<(u16, String)>,
    /// Read side, handed to `Link::listen` once the sessions are registered.
    pub reader: TcpStream,
}

/// Connects to the daemon of the project whose data is in `data_dir`, starting it with
/// `exe daemon` if none answers.
pub fn attach(data_dir: &Path, exe: &Path) -> anyhow::Result<Attached> {
    if let Ok(attached) = connect_runtime(data_dir) {
        return Ok(attached);
    }
    start_daemon(exe, data_dir)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match connect_runtime(data_dir) {
            Ok(attached) => return Ok(attached),
            Err(e) if Instant::now() > deadline => {
                return Err(e.context("the daemon did not answer"));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn connect_runtime(data_dir: &Path) -> anyhow::Result<Attached> {
    let text = std::fs::read_to_string(daemon::runtime_file(data_dir))?;
    let runtime: Runtime = toml::from_str(&text)?;
    connect(runtime.port, &runtime.token)
}

/// Says hello and reads the welcome: the sessions and a screen for each.
pub fn connect(port: u16, token: &str) -> anyhow::Result<Attached> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    let hello = ClientMsg::Hello {
        token: token.to_string(),
    };
    protocol::write_msg(&mut stream, &hello)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let Frame::Msg(DaemonMsg::Welcome { sessions, hooks }) = protocol::read_frame(&mut stream)?
    else {
        bail!("the daemon did not welcome this client");
    };
    let mut screens = Vec::with_capacity(sessions.len());
    for info in sessions {
        match protocol::read_frame::<DaemonMsg>(&mut stream)? {
            Frame::Data(id, screen) if id == info.id => screens.push((info, screen)),
            _ => bail!("the daemon sent no screen for session {}", info.id),
        }
    }
    stream.set_read_timeout(None)?;
    let reader = stream.try_clone()?;
    Ok(Attached {
        link: Link {
            inner: Arc::new(Inner {
                writer: Mutex::new(stream),
                parsers: Mutex::default(),
            }),
        },
        sessions: screens,
        hooks,
        reader,
    })
}

impl Link {
    /// Feeds the output of the daemon to the registered screens and its news to the app.
    pub fn listen(&self, mut reader: TcpStream, tx: Sender<AppEvent>) {
        let link = self.clone();
        let spawned = thread::Builder::new()
            .name("daemon-link".into())
            .spawn(move || {
                loop {
                    let event = match protocol::read_frame::<DaemonMsg>(&mut reader) {
                        Ok(Frame::Data(id, bytes)) => {
                            let parser = link.inner.parsers.lock().unwrap().get(&id).cloned();
                            if let Some(parser) = parser {
                                parser.lock().unwrap().process(&bytes);
                            }
                            AppEvent::PtyOutput(id, bytes.len())
                        }
                        Ok(Frame::Msg(DaemonMsg::Exited { id, code })) => {
                            AppEvent::RemoteExited(id, code)
                        }
                        Ok(Frame::Msg(DaemonMsg::SpawnFailed { id, error })) => {
                            AppEvent::SpawnFailed(id, error)
                        }
                        Ok(Frame::Msg(DaemonMsg::Hook { id, event })) => AppEvent::Hook(id, event),
                        Ok(Frame::Msg(DaemonMsg::Replaced)) => {
                            AppEvent::DaemonGone("chiika was opened in another window".into())
                        }
                        Ok(Frame::Msg(DaemonMsg::Pulse { from, to })) => AppEvent::Pulse(from, to),
                        Ok(Frame::Msg(DaemonMsg::Welcome { .. })) => continue,
                        Err(_) => {
                            AppEvent::DaemonGone("the connection to the daemon was lost".into())
                        }
                    };
                    let gone = matches!(event, AppEvent::DaemonGone(_));
                    if tx.send(event).is_err() || gone {
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot listen to the daemon: {e}");
        }
    }

    /// The daemon's output for session `id` goes to `parser`.
    pub fn register(&self, id: u32, parser: Parser) {
        self.inner.parsers.lock().unwrap().insert(id, parser);
    }

    pub fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        protocol::write_msg(&mut *self.inner.writer.lock().unwrap(), msg)
    }

    /// Ends the connection (the daemon sees a detach).
    pub fn close(&self) {
        let _ = self
            .inner
            .writer
            .lock()
            .unwrap()
            .shutdown(std::net::Shutdown::Both);
    }

    /// What the user types in session `id`.
    pub fn input(&self, id: u32, bytes: &[u8]) -> io::Result<()> {
        protocol::write_data(&mut *self.inner.writer.lock().unwrap(), id, bytes)
    }
}

/// Starts `exe daemon` from the project folder, outside this terminal: it must survive the TUI
/// and the window.
fn start_daemon(exe: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let root = data_dir.parent().context("no project folder")?;
    std::fs::create_dir_all(data_dir)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn_detached(&mut cmd).context("starting the daemon")?;
    Ok(())
}

#[cfg(windows)]
fn spawn_detached(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // Out of the terminal's job when allowed, so that closing the window spares the daemon.
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| {
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                .spawn()
        })
}

#[cfg(unix)]
fn spawn_detached(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0).spawn()
}
