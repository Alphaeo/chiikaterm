//! Client ↔ daemon protocol over a local TCP connection.
//!
//! A frame is `u32 LE length | u8 kind | body`, the length counting kind and body. Kind 0 is a
//! JSON message (`ClientMsg` or `DaemonMsg`); kind 1 is data: `u32 LE session | bytes`, the
//! output of a session (daemon → client) or what the user types (client → daemon).

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use portable_pty::CommandBuilder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::agent::AgentState;
use crate::channel::HookEvent;

const MSG: u8 = 0;
const DATA: u8 = 1;
/// Larger frames are refused (a screen snapshot is a few hundred KB at most).
const MAX_FRAME: u32 = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Frame<M> {
    Msg(M),
    Data(u32, Vec<u8>),
}

pub fn write_msg<M: Serialize>(w: &mut impl Write, msg: &M) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, MSG, &body)
}

pub fn write_data(w: &mut impl Write, session: u32, bytes: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(4 + bytes.len());
    body.extend(session.to_le_bytes());
    body.extend(bytes);
    write_frame(w, DATA, &body)
}

/// One `write_all` per frame: frames of several threads never interleave as long as each
/// thread holds the writer's lock for the call.
fn write_frame(w: &mut impl Write, kind: u8, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len() + 1).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.extend(len.to_le_bytes());
    frame.push(kind);
    frame.extend(body);
    w.write_all(&frame)?;
    w.flush()
}

pub fn read_frame<M: DeserializeOwned>(r: &mut impl Read) -> io::Result<Frame<M>> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len == 0 || len > MAX_FRAME {
        return Err(invalid("bad frame length"));
    }
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body)?;
    match body[0] {
        MSG => serde_json::from_slice(&body[1..])
            .map(Frame::Msg)
            .map_err(|e| invalid(&e.to_string())),
        DATA if body.len() >= 5 => {
            let session = u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
            Ok(Frame::Data(session, body[5..].to_vec()))
        }
        _ => Err(invalid("bad frame kind")),
    }
}

/// How to start a session's program; an empty `argv` is the user's default shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

impl Launch {
    pub fn new(argv: Vec<String>, cwd: PathBuf) -> Launch {
        Launch {
            argv,
            cwd,
            env: vec![("TERM".into(), "xterm-256color".into())],
            env_remove: Vec::new(),
        }
    }

    pub fn to_command(&self) -> CommandBuilder {
        let mut cmd = if self.argv.is_empty() {
            CommandBuilder::new_default_prog()
        } else {
            CommandBuilder::from_argv(self.argv.iter().map(OsString::from).collect())
        };
        cmd.cwd(&self.cwd);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        for key in &self.env_remove {
            cmd.env_remove(key);
        }
        cmd
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// First message of a connection.
    Hello {
        token: String,
    },
    /// `id` is chosen by the client.
    Spawn {
        id: u32,
        name: String,
        launch: Launch,
        rows: u16,
        cols: u16,
    },
    Resize {
        id: u32,
        rows: u16,
        cols: u16,
    },
    Kill {
        id: u32,
    },
    /// What the daemon keeps for the next attach.
    Meta {
        id: u32,
        name: String,
        folder: Option<u32>,
        ephemeral: bool,
        agent: Option<Box<AgentSnapshot>>,
    },
    /// Answer the permission request held for agent `id`: allow, deny, or `None` to let the
    /// agent ask in its terminal.
    Decide {
        id: u32,
        allow: Option<bool>,
    },
    /// Kill every session and stop the daemon.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMsg {
    /// Answer to `Hello`, followed by one data frame per session: its screen.
    Welcome {
        sessions: Vec<SessionInfo>,
        /// Port and token of the hook channel, for the agents.
        hooks: Option<(u16, String)>,
        /// Which build of chiika the daemon runs (empty for daemons older than this field).
        #[serde(default)]
        build: String,
    },
    SpawnFailed {
        id: u32,
        error: String,
    },
    Exited {
        id: u32,
        code: Option<u32>,
    },
    Hook {
        id: u32,
        event: HookEvent,
    },
    /// Another client attached: this one is detached.
    Replaced,
    /// Agent `from` sent a message to agent `to`.
    Pulse {
        from: u32,
        to: u32,
    },
}

/// A session as the daemon describes it to an attaching client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u32,
    pub name: String,
    pub folder: Option<u32>,
    pub ephemeral: bool,
    pub rows: u16,
    pub cols: u16,
    /// Milliseconds since it started.
    pub age_ms: u64,
    /// `Some(code)` once exited.
    pub exited: Option<Option<u32>>,
    pub marks: MarksSnapshot,
    pub agent: Option<AgentSnapshot>,
}

/// Shell integration state; times are milliseconds before the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarksSnapshot {
    pub integrated: bool,
    #[serde(default)]
    pub prompts: Vec<String>,
    pub cwd: Option<String>,
    /// Command text, started this long ago.
    pub running: Option<(String, u64)>,
    pub last: Option<FinishedSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedSnapshot {
    pub text: String,
    pub code: i64,
    pub duration_ms: u64,
    pub ago_ms: u64,
}

/// Agent state; `since_ms` is how long ago `state` was entered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub team: u32,
    pub folder: PathBuf,
    pub worktree: Option<String>,
    pub state: AgentState,
    pub since_ms: u64,
    pub tool: Option<String>,
    pub message: Option<String>,
    pub files: Vec<String>,
    /// How long chiika has held its permission request, if it does.
    pub permission_ms: Option<u64>,
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub team_name: String,
    #[serde(default)]
    pub rules: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_data_round_trip() {
        let mut wire = Vec::new();
        let hello = ClientMsg::Hello {
            token: "abc".into(),
        };
        write_msg(&mut wire, &hello).unwrap();
        write_data(&mut wire, 7, b"ls\r").unwrap();
        write_data(&mut wire, 8, b"").unwrap();
        let mut r = wire.as_slice();
        assert_eq!(read_frame::<ClientMsg>(&mut r).unwrap(), Frame::Msg(hello));
        assert_eq!(
            read_frame::<ClientMsg>(&mut r).unwrap(),
            Frame::Data(7, b"ls\r".to_vec())
        );
        assert_eq!(
            read_frame::<ClientMsg>(&mut r).unwrap(),
            Frame::Data(8, Vec::new())
        );
        assert!(read_frame::<ClientMsg>(&mut r).is_err(), "end of stream");
    }

    #[test]
    fn bad_frames_are_refused() {
        let mut r: &[u8] = &[0, 0, 0, 0];
        assert!(read_frame::<ClientMsg>(&mut r).is_err(), "empty");
        let mut r: &[u8] = &[2, 0, 0, 0, 9, 0];
        assert!(read_frame::<ClientMsg>(&mut r).is_err(), "unknown kind");
        let mut r: &[u8] = &[0xff, 0xff, 0xff, 0x7f];
        assert!(read_frame::<ClientMsg>(&mut r).is_err(), "too long");
        let mut r: &[u8] = &[3, 0, 0, 0, 0, b'{', b'x'];
        assert!(read_frame::<ClientMsg>(&mut r).is_err(), "invalid json");
    }

    #[test]
    fn launch_builds_the_command() {
        let mut launch = Launch::new(
            vec!["pwsh.exe".into(), "-NoLogo".into()],
            PathBuf::from("/p"),
        );
        launch.env.push(("CHIIKA_SESSION".into(), "3".into()));
        launch.env_remove.push("CLAUDE_CODE_CHILD_SESSION".into());
        let cmd = launch.to_command();
        assert_eq!(
            cmd.get_argv(),
            &[OsString::from("pwsh.exe"), "-NoLogo".into()]
        );
        assert_eq!(cmd.get_cwd(), Some(&OsString::from("/p")));
        assert_eq!(
            cmd.get_env("CHIIKA_SESSION"),
            Some(OsString::from("3").as_os_str())
        );
        assert_eq!(cmd.get_env("CLAUDE_CODE_CHILD_SESSION"), None);
        assert!(
            Launch::new(Vec::new(), PathBuf::new())
                .to_command()
                .is_default_prog()
        );
    }
}
