//! The chiika channel: a local TCP port where the agents' hooks report what they do and where
//! their chiika tools (`chiika mcp`) are run.
//!
//! The sender writes `token\nsession\n<JSON>`, closes its writing side, and reads the answer
//! until the end (often empty). Only 127.0.0.1 is listened on, and the token is random per run.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::app::AppEvent;

/// Larger messages are cut (they are then invalid JSON and ignored).
const MAX_MESSAGE: u64 = 256 * 1024;
/// Connecting and sending must never slow an agent down.
const TIMEOUT: Duration = Duration::from_millis(300);

/// What a Claude Code hook reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// What a message asks.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Hook(HookEvent),
    /// A chiika tool called by an agent (see `mcp.rs`).
    Tool {
        name: String,
        arguments: Value,
    },
}

/// The answer to a message: sent once, or dropped to answer nothing.
pub struct Reply(TcpStream);

impl Reply {
    pub fn send(mut self, body: &[u8]) {
        let _ = self.0.write_all(body);
        let _ = self.0.shutdown(Shutdown::Write);
    }
}

/// Where hooks and tools can reach this chiika.
pub struct Channel {
    pub port: u16,
    pub token: String,
}

/// Listens in the background and hands every valid message, with its reply, to the app.
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
                let Some((session, request)) = parse_message(&data, &expected) else {
                    continue;
                };
                // Only tools and permission requests wait for an answer; for the other hooks
                // the connection closes now, an empty answer that does not slow the agent.
                let answered = match &request {
                    Request::Tool { .. } => true,
                    Request::Hook(event) => event.name == "PermissionRequest",
                };
                let reply = answered.then_some(Reply(stream));
                if tx.send(AppEvent::Channel(session, request, reply)).is_err() {
                    return;
                }
            }
        })?;
    Ok(Channel { port, token })
}

/// Client side (`chiika hook`, `chiika mcp`): sends a message and waits up to `wait` for the
/// answer, empty when there is none.
pub fn request(
    port: u16,
    token: &str,
    session: &str,
    payload: &[u8],
    wait: Duration,
) -> io::Result<Vec<u8>> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("{token}\n{session}\n").as_bytes())?;
    stream.write_all(payload)?;
    stream.shutdown(Shutdown::Write)?;
    stream.set_read_timeout(Some(wait))?;
    let mut answer = Vec::new();
    stream.take(MAX_MESSAGE).read_to_end(&mut answer)?;
    Ok(answer)
}

/// The JSON of a tool call sent on the channel.
pub fn tool_message(name: &str, arguments: &Value) -> Vec<u8> {
    serde_json::json!({ "chiika": "tool", "name": name, "arguments": arguments })
        .to_string()
        .into_bytes()
}

/// Splits a message; `None` if malformed or if the token is wrong.
pub fn parse_message(data: &[u8], token: &str) -> Option<(u32, Request)> {
    let mut parts = data.splitn(3, |&b| b == b'\n');
    let (got, session, json) = (parts.next()?, parts.next()?, parts.next()?);
    if got != token.as_bytes() {
        tracing::warn!("channel: message with a wrong token ignored");
        return None;
    }
    let session = std::str::from_utf8(session).ok()?.trim().parse().ok()?;
    let value: Value = serde_json::from_slice(json).ok()?;
    let request = if value["chiika"] == "tool" {
        Request::Tool {
            name: value["name"].as_str()?.to_string(),
            arguments: value["arguments"].clone(),
        }
    } else {
        Request::Hook(hook_from(&value)?)
    };
    Some((session, request))
}

/// Reads the fields chiika uses from a hook's JSON input.
pub fn parse_hook(json: &[u8]) -> Option<HookEvent> {
    hook_from(&serde_json::from_slice(json).ok()?)
}

fn hook_from(value: &Value) -> Option<HookEvent> {
    let text = |v: &Value| v.as_str().map(str::to_string);
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
pub fn random_token() -> String {
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
    fn tool_calls_are_told_apart_from_hooks() {
        let mut message = b"t\n3\n".to_vec();
        message.extend(tool_message("post", &serde_json::json!({ "text": "hi" })));
        let (session, request) = parse_message(&message, "t").unwrap();
        assert_eq!(session, 3);
        assert_eq!(
            request,
            Request::Tool {
                name: "post".into(),
                arguments: serde_json::json!({ "text": "hi" }),
            }
        );
    }

    #[test]
    fn invalid_input_is_ignored() {
        assert_eq!(parse_hook(b"not json"), None);
        assert_eq!(parse_hook(br#"{"tool_name":"Edit"}"#), None);
        assert!(parse_message(b"only-a-token", "t").is_none());
        let stop = b"wrong\n1\n{\"hook_event_name\":\"Stop\"}";
        assert!(parse_message(stop, "right").is_none());
        let stop = b"t\nnot-a-number\n{\"hook_event_name\":\"Stop\"}";
        assert!(parse_message(stop, "t").is_none());
    }

    #[test]
    fn a_message_gets_its_answer_or_nothing() {
        let (tx, rx) = mpsc::channel();
        let channel = start(tx).unwrap();
        let answerer = thread::spawn(move || {
            for answer in [Some(&b"decided"[..]), None] {
                let Ok(AppEvent::Channel(session, Request::Hook(event), Some(reply))) =
                    rx.recv_timeout(Duration::from_secs(5))
                else {
                    panic!("expected a permission request with its reply");
                };
                assert_eq!((session, event.name.as_str()), (7, "PermissionRequest"));
                if let Some(answer) = answer {
                    reply.send(answer);
                }
            }
            rx
        });
        let hook = br#"{"hook_event_name":"PermissionRequest"}"#;
        let wait = Duration::from_secs(5);
        let request_answer = |token: &str| request(channel.port, token, "7", hook, wait);
        assert_eq!(request_answer(&channel.token).unwrap(), b"decided");
        assert_eq!(request_answer(&channel.token).unwrap(), b"");
        let rx = answerer.join().unwrap();
        let _ = request_answer("bad-token");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "refused"
        );
    }

    #[test]
    fn tokens_are_random() {
        let token = random_token();
        assert_eq!(token.len(), 32);
        assert_ne!(token, random_token());
    }
}
