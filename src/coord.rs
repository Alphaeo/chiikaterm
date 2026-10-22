//! The chiika tools agents use to work as a team. They run on the sessions of the daemon, or of
//! chiika itself when there is no daemon, for the agent (`caller`) that called them.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent::AgentState;
use crate::app::truncate;
use crate::keys::encode_paste;
use crate::session::{Session, Status};

/// Most screen lines `read_screen` returns.
const MAX_LINES: u64 = 200;
/// Board lines `board` returns (the most recent).
const BOARD_LINES: usize = 100;

/// What a tool answers.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub is_error: bool,
    /// For `send_to`: (from, to), drawn as a link on the canvas.
    pub link: Option<(u32, u32)>,
}

impl Outcome {
    fn ok(text: impl Into<String>) -> Outcome {
        Outcome {
            text: text.into(),
            is_error: false,
            link: None,
        }
    }

    fn fail(text: impl Into<String>) -> Outcome {
        Outcome {
            text: text.into(),
            is_error: true,
            link: None,
        }
    }

    /// The MCP `tools/call` result.
    pub fn to_mcp(&self) -> Vec<u8> {
        json!({
            "content": [{ "type": "text", "text": self.text }],
            "isError": self.is_error,
        })
        .to_string()
        .into_bytes()
    }
}

/// The tools, as `tools/list` describes them.
pub fn tool_list() -> Value {
    let member = json!({ "type": "string", "description": "A member's name, as listed by team" });
    json!([
        {
            "name": "team",
            "description": "List the members of your chiika team: their name, whether they are an agent or a shell, and what they are doing.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "send_to",
            "description": "Send a message to another agent of your team: it is typed in its terminal as a new prompt, prefixed with your name.",
            "inputSchema": {
                "type": "object",
                "properties": { "to": member, "message": { "type": "string" } },
                "required": ["to", "message"]
            }
        },
        {
            "name": "read_screen",
            "description": "Read the last lines of a team member's terminal (an agent, or a shell such as a dev server).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "member": member,
                    "lines": { "type": "integer", "description": "How many lines (default 40, at most 200)" }
                },
                "required": ["member"]
            }
        },
        {
            "name": "post",
            "description": "Add a note to your team's shared board (decisions, task status, handoffs).",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }
        },
        {
            "name": "board",
            "description": "Read your team's shared board.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// `.chiika/teams/team-<id>/BOARD.md`.
pub fn board_file(data_dir: &Path, team: u32) -> PathBuf {
    data_dir
        .join("teams")
        .join(format!("team-{team}"))
        .join("BOARD.md")
}

/// Runs tool `name` for session `caller`; `data_dir` is the project's `.chiika` folder.
pub fn run(
    sessions: &mut [Session],
    data_dir: Option<&Path>,
    caller: u32,
    name: &str,
    args: &Value,
) -> Outcome {
    let Some(me) = sessions.iter().find(|s| s.id == caller) else {
        return Outcome::fail("unknown caller");
    };
    let Some(team) = me.folder else {
        return Outcome::fail(
            "you are not in a team: ask the user to put you in one on the chiika canvas",
        );
    };
    let me = me.name.clone();
    let find = |sessions: &[Session], wanted: &str| {
        sessions
            .iter()
            .position(|s| s.folder == Some(team) && s.name.eq_ignore_ascii_case(wanted.trim()))
    };
    match name {
        "team" => Outcome::ok(team_text(sessions, team, caller)),
        "send_to" => {
            let (Some(to), Some(message)) = (args["to"].as_str(), args["message"].as_str()) else {
                return Outcome::fail("send_to needs `to` and `message`");
            };
            let Some(index) = find(sessions, to) else {
                return Outcome::fail(format!("no member named {to} in your team (see team)"));
            };
            let target = &mut sessions[index];
            if target.id == caller {
                return Outcome::fail("that is you");
            }
            if target.agent.is_none() {
                return Outcome::fail(format!(
                    "{to} is a shell, not an agent: use read_screen to follow it"
                ));
            }
            if target.status != Status::Running {
                return Outcome::fail(format!("{to} has stopped"));
            }
            let bracketed = target.parser.lock().unwrap().screen().bracketed_paste();
            let mut bytes = encode_paste(&format!("[from {me}] {message}"), bracketed);
            bytes.push(b'\r');
            match target.write(&bytes) {
                Ok(()) => Outcome {
                    text: format!("sent to {}", target.name),
                    is_error: false,
                    link: Some((caller, target.id)),
                },
                Err(e) => Outcome::fail(format!("cannot write to {to}: {e}")),
            }
        }
        "read_screen" => {
            let Some(wanted) = args["member"].as_str() else {
                return Outcome::fail("read_screen needs `member`");
            };
            let Some(index) = find(sessions, wanted) else {
                return Outcome::fail(format!("no member named {wanted} in your team (see team)"));
            };
            let lines = args["lines"].as_u64().unwrap_or(40).clamp(1, MAX_LINES) as usize;
            let contents = sessions[index].parser.lock().unwrap().screen().contents();
            let all: Vec<&str> = contents.trim_end().lines().collect();
            Outcome::ok(all[all.len().saturating_sub(lines)..].join("\n"))
        }
        "post" => {
            let Some(text) = args["text"].as_str() else {
                return Outcome::fail("post needs `text`");
            };
            let Some(data_dir) = data_dir else {
                return Outcome::fail("no project folder to keep the board in");
            };
            match append_board(&board_file(data_dir, team), &me, text) {
                Ok(()) => Outcome::ok("posted on the board"),
                Err(e) => Outcome::fail(format!("cannot write the board: {e}")),
            }
        }
        "board" => {
            let text = data_dir
                .and_then(|dir| std::fs::read_to_string(board_file(dir, team)).ok())
                .unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let recent = lines[lines.len().saturating_sub(BOARD_LINES)..].join("\n");
            if recent.trim().is_empty() {
                Outcome::ok("the board is empty")
            } else {
                Outcome::ok(recent)
            }
        }
        _ => Outcome::fail(format!("unknown tool {name}")),
    }
}

fn team_text(sessions: &[Session], team: u32, caller: u32) -> String {
    sessions
        .iter()
        .filter(|s| s.folder == Some(team) && !s.ephemeral)
        .map(|s| {
            let you = if s.id == caller { " (you)" } else { "" };
            let what = match (&s.agent, s.status) {
                (_, Status::Exited(_)) => "stopped".to_string(),
                (None, Status::Running) => "shell".to_string(),
                (Some(agent), Status::Running) => {
                    let state = match agent.state {
                        AgentState::Starting => "starting",
                        AgentState::Idle => "idle",
                        AgentState::Working => "working",
                        AgentState::Waiting => "waiting for the user",
                        AgentState::Done => "done",
                    };
                    match &agent.tool {
                        Some(tool) => format!("agent, {state}: {}", truncate(tool, 40)),
                        None => format!("agent, {state}"),
                    }
                }
            };
            format!("- {}{you}: {what}", s.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_board(path: &Path, who: &str, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let text = text.trim().replace('\n', "\n  ");
    writeln!(file, "- {who}: {text}")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use portable_pty::{CommandBuilder, PtySize};

    use super::*;
    use crate::agent::AgentInfo;

    fn shell(id: u32, name: &str, team: Option<u32>, agent: bool) -> Session {
        let program = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let (tx, _rx) = mpsc::channel();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let mut session =
            Session::spawn(id, name.into(), CommandBuilder::new(program), size, tx).unwrap();
        session.folder = team;
        if agent {
            session.agent = Some(AgentInfo::new(1, PathBuf::new(), None, Instant::now()));
        }
        session
    }

    fn text_of(outcome: &Outcome) -> &str {
        &outcome.text
    }

    #[test]
    fn team_send_and_read_between_members() {
        let mut sessions = vec![
            shell(1, "lead", Some(1), true),
            shell(2, "coder", Some(1), true),
            shell(3, "server", Some(1), false),
            shell(4, "other", Some(2), true),
        ];
        let team = run(&mut sessions, None, 1, "team", &json!({}));
        assert!(text_of(&team).contains("- lead (you): agent, starting"));
        assert!(text_of(&team).contains("- server: shell"));
        assert!(!text_of(&team).contains("other"), "only my team");

        let sent = run(
            &mut sessions,
            None,
            1,
            "send_to",
            &json!({ "to": "Coder", "message": "echo from-lead" }),
        );
        assert_eq!(sent.link, Some((1, 2)));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let screen = run(
                &mut sessions,
                None,
                1,
                "read_screen",
                &json!({ "member": "coder" }),
            );
            if screen.text.contains("[from lead] echo from-lead") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "message not typed: {}",
                screen.text
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        sessions.iter_mut().for_each(Session::kill);
    }

    #[test]
    fn send_to_refuses_what_makes_no_sense() {
        let mut sessions = vec![
            shell(1, "lead", Some(1), true),
            shell(3, "server", Some(1), false),
            shell(4, "other", Some(2), true),
            shell(5, "loner", None, true),
        ];
        let send = |sessions: &mut Vec<Session>, caller, to: &str| {
            run(
                sessions,
                None,
                caller,
                "send_to",
                &json!({ "to": to, "message": "hi" }),
            )
        };
        assert!(send(&mut sessions, 1, "lead").is_error, "to myself");
        assert!(send(&mut sessions, 1, "server").is_error, "a shell");
        assert!(send(&mut sessions, 1, "other").is_error, "another team");
        assert!(send(&mut sessions, 5, "lead").is_error, "no team");
        assert!(run(&mut sessions, None, 1, "send_to", &json!({})).is_error);
        assert!(run(&mut sessions, None, 1, "dance", &json!({})).is_error);
        sessions.iter_mut().for_each(Session::kill);
    }

    #[test]
    fn the_board_is_shared_by_the_team() {
        let dir = std::env::temp_dir().join(format!("chiika-board-{}", std::process::id()));
        let mut sessions = vec![
            shell(1, "lead", Some(7), true),
            shell(2, "coder", Some(7), true),
        ];
        let empty = run(&mut sessions, Some(&dir), 2, "board", &json!({}));
        assert_eq!(empty.text, "the board is empty");
        let post = |sessions: &mut Vec<Session>, caller, text: &str| {
            run(
                sessions,
                Some(&dir),
                caller,
                "post",
                &json!({ "text": text }),
            )
        };
        assert!(!post(&mut sessions, 1, "API first, then tests").is_error);
        assert!(!post(&mut sessions, 2, "api.rs done\nneeds review").is_error);
        let board = run(&mut sessions, Some(&dir), 2, "board", &json!({}));
        assert_eq!(
            board.text,
            "- lead: API first, then tests\n- coder: api.rs done\n  needs review"
        );
        assert!(run(&mut sessions, None, 1, "post", &json!({ "text": "x" })).is_error);
        sessions.iter_mut().for_each(Session::kill);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_are_described_for_mcp() {
        let tools = tool_list();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["team", "send_to", "read_screen", "post", "board"]);
        let result: Value = serde_json::from_slice(&Outcome::fail("no").to_mcp()).unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "no");
    }
}
