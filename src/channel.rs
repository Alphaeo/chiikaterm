//! The chiika channel: a local TCP port where the agents' hooks report what they do.
//!
//! A message is `token\nsession\n<hook JSON>`, sent by `chiika hook` (see `main.rs`) and
//! closed by the sender. Only 127.0.0.1 is listened on, and the token is random per run.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::app::AppEvent;

/// Hook payloads larger than this are cut (they are then invalid JSON and ignored).
const MAX_MESSAGE: u64 = 256 * 1024;
/// A hook must never slow an agent down.
const TIMEOUT: Duration = Duration::from_millis(300);

/// What a Claude Code hook reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookEvent {
    /// `hook_event_name`: `PreToolUse`, `Stop`…
    pub name: String,
    pub tool: Option<String>,
    /// `tool_input.file_path` (Edit, Write, Read…).
    pub file: Option<String>,
    /// `tool_input.command` (Bash).
    pub command: Option<String>,
    /// `message` (Notification) or `last_assistant_message` (Stop).
    pub message: Option<String>,
    /// `notification_type`: `permission_prompt`, `idle_prompt`…
    pub notification: Option<String>,
    /// `cwd`: the folder the agent works in.
    pub cwd: Option<String>,
}

/// Where hooks can reach this chiika.
pub struct Channel {
    pub port: u16,
    pub token: String,
}

/// Listens in the background and sends every valid message to the app.
pub fn start(tx: Sender<AppEvent>) -> io::Result<Channel> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let expected = token.clone();
    thread::Builder::new()
        .name("channel".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut data = Vec::new();
                let read = stream
                    .set_read_timeout(Some(TIMEOUT))
                    .and_then(|()| (&mut stream).take(MAX_MESSAGE).read_to_end(&mut data));
                if read.is_err() {
                    continue;
                }
                if let Some((session, event)) = parse_message(&data, &expected)
                    && tx.send(AppEvent::Hook(session, event)).is_err()
                {
                    return;
                }
            }
        })?;
    Ok(Channel { port, token })
}

/// Client side, used by `chiika hook`.
pub fn send(port: u16, token: &str, session: &str, payload: &[u8]) -> io::Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("{token}\n{session}\n").as_bytes())?;
    stream.write_all(payload)
}

/// Splits a message; `None` if malformed or if the token is wrong.
pub fn parse_message(data: &[u8], token: &str) -> Option<(u32, HookEvent)> {
    let mut parts = data.splitn(3, |&b| b == b'\n');
    let (got, session, json) = (parts.next()?, parts.next()?, parts.next()?);
    if got != token.as_bytes() {
        tracing::warn!("channel: message with a wrong token ignored");
        return None;
    }
    let session = std::str::from_utf8(session).ok()?.trim().parse().ok()?;
    Some((session, parse_hook(json)?))
}

/// Reads the fields chiika uses from a hook's JSON input.
pub fn parse_hook(json: &[u8]) -> Option<HookEvent> {
    let value: serde_json::Value = serde_json::from_slice(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().map(str::to_string);
    let input = &value["tool_input"];
    Some(HookEvent {
        name: text(&value["hook_event_name"])?,
        tool: text(&value["tool_name"]),
        file: text(&input["file_path"]).or_else(|| text(&input["notebook_path"])),
        command: text(&input["command"]),
        message: text(&value["message"]).or_else(|| text(&value["last_assistant_message"])),
        notification: text(&value["notification_type"]),
        cwd: text(&value["cwd"]),
    })
}

/// 128 random bits, hex. `RandomState` is seeded by the OS: enough for a local per-run token.
fn random_token() -> String {
    (0..2u64)
        .map(|i| {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn reads_the_fields_of_each_hook() {
        let pre = br#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"C:\\p\\src\\api.rs"}}"#;
        let event = parse_hook(pre).unwrap();
        assert_eq!(event.name, "PreToolUse");
        assert_eq!(event.tool.as_deref(), Some("Edit"));
        assert_eq!(event.file.as_deref(), Some("C:\\p\\src\\api.rs"));

        let bash = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
        assert_eq!(
            parse_hook(bash).unwrap().command.as_deref(),
            Some("cargo test")
        );

        let notification = br#"{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}"#;
        let event = parse_hook(notification).unwrap();
        assert_eq!(event.notification.as_deref(), Some("permission_prompt"));
        assert!(event.message.unwrap().contains("permission"));

        let stop = br#"{"hook_event_name":"Stop","last_assistant_message":"All tests pass."}"#;
        assert_eq!(
            parse_hook(stop).unwrap().message.as_deref(),
            Some("All tests pass.")
        );
    }

    #[test]
    fn invalid_input_is_ignored() {
        assert_eq!(parse_hook(b"not json"), None);
        assert_eq!(parse_hook(br#"{"tool_name":"Edit"}"#), None);
        assert_eq!(parse_message(b"only-a-token", "t"), None);
        assert_eq!(
            parse_message(b"wrong\n1\n{\"hook_event_name\":\"Stop\"}", "right"),
            None
        );
        assert_eq!(
            parse_message(b"t\nnot-a-number\n{\"hook_event_name\":\"Stop\"}", "t"),
            None
        );
    }

    #[test]
    fn a_hook_reaches_the_app_through_the_channel() {
        let (tx, rx) = mpsc::channel();
        let channel = start(tx).unwrap();
        send(
            channel.port,
            &channel.token,
            "7",
            br#"{"hook_event_name":"SessionStart"}"#,
        )
        .unwrap();
        send(
            channel.port,
            "bad-token",
            "7",
            br#"{"hook_event_name":"Stop"}"#,
        )
        .unwrap();
        let received = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let AppEvent::Hook(session, event) = received else {
            panic!("expected a hook event");
        };
        assert_eq!(session, 7);
        assert_eq!(event.name, "SessionStart");
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn tokens_are_random() {
        let token = random_token();
        assert_eq!(token.len(), 32);
        assert_ne!(token, random_token());
    }
}
