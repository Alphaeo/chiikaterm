//! Claude Code agents: roles, system prompt, hook settings, git worktrees, and the state their
//! hooks report.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::channel::HookEvent;
use crate::protocol::AgentSnapshot;

/// Roles offered when adding an agent, with the duty added to its system prompt.
pub const ROLES: [(&str, &str); 4] = [
    (
        "lead",
        "You are the lead of the team. Turn the goal into small tasks, decide their order, keep \
         the team focused, and check that the result meets the team instructions.",
    ),
    (
        "coder",
        "You are a coder of the team. Implement one task at a time with focused, tested changes, \
         and explain what you changed.",
    ),
    (
        "reviewer",
        "You are the reviewer of the team. Look for bugs, security issues and unclear code in the \
         changes; report each finding with its file and line, and do not rewrite the code \
         yourself.",
    ),
    (
        "tester",
        "You are the tester of the team. Write and run tests for the team's changes, and report \
         every failure with exact steps to reproduce it.",
    ),
];

/// Hooks chiika listens to.
pub const HOOKS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
];

/// An agent silent this long after its start is probably waiting for an answer that Claude asks
/// before any hook runs (trusting a folder, logging in…).
const SILENT_START: Duration = Duration::from_secs(6);
const FILES_KEPT: usize = 6;

/// A permission request waits this long for an answer from chiika, then goes to the terminal.
pub const PERMISSION_WAIT: Duration = Duration::from_secs(120);
const MESSAGE_KEPT: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentState {
    Starting,
    /// Waiting for a prompt.
    Idle,
    Working,
    /// Needs the user: a permission, a question.
    Waiting,
    /// Finished its turn.
    Done,
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub team: u32,
    /// Where it works: the project folder, then the folder its hooks report (its worktree).
    pub folder: PathBuf,
    /// Name of the worktree Claude creates for it (`--worktree`); `None` in the project folder.
    pub worktree: Option<String>,
    pub state: AgentState,
    /// When `state` last changed.
    pub since: Instant,
    /// What it is doing: `✎ api.rs`, `$ cargo test`…
    pub tool: Option<String>,
    /// Its question, or its last answer.
    pub message: Option<String>,
    /// Files it edited, most recent last.
    pub files: Vec<String>,
    /// Since when chiika holds its permission request, waiting for the user's answer.
    pub permission: Option<Instant>,
}

impl AgentInfo {
    pub fn new(team: u32, folder: PathBuf, worktree: Option<String>, now: Instant) -> AgentInfo {
        AgentInfo {
            team,
            folder,
            worktree,
            state: AgentState::Starting,
            since: now,
            tool: None,
            message: None,
            files: Vec::new(),
            permission: None,
        }
    }

    /// What survives a reattach; `since` becomes "milliseconds ago".
    pub fn snapshot(&self, now: Instant) -> AgentSnapshot {
        AgentSnapshot {
            team: self.team,
            folder: self.folder.clone(),
            worktree: self.worktree.clone(),
            state: self.state,
            since_ms: now.saturating_duration_since(self.since).as_millis() as u64,
            tool: self.tool.clone(),
            message: self.message.clone(),
            files: self.files.clone(),
            permission_ms: self
                .permission
                .map(|t| now.saturating_duration_since(t).as_millis() as u64),
        }
    }

    pub fn from_snapshot(snapshot: &AgentSnapshot, now: Instant) -> AgentInfo {
        let since = Duration::from_millis(snapshot.since_ms);
        AgentInfo {
            team: snapshot.team,
            folder: snapshot.folder.clone(),
            worktree: snapshot.worktree.clone(),
            state: snapshot.state,
            since: now.checked_sub(since).unwrap_or(now),
            tool: snapshot.tool.clone(),
            message: snapshot.message.clone(),
            files: snapshot.files.clone(),
            permission: snapshot
                .permission_ms
                .and_then(|ms| now.checked_sub(Duration::from_millis(ms))),
        }
    }

    /// Follows a hook; returns the new state when it changed.
    pub fn apply(&mut self, event: &HookEvent, now: Instant) -> Option<AgentState> {
        let before = self.state;
        // Any later hook means the request was answered (here or in the terminal).
        self.permission = None;
        if let Some(cwd) = &event.cwd {
            self.folder = PathBuf::from(cwd);
        }
        match event.name.as_str() {
            "SessionStart" => self.state = AgentState::Idle,
            "UserPromptSubmit" => {
                self.state = AgentState::Working;
                self.tool = None;
                self.message = None;
            }
            "PreToolUse" | "PostToolUse" => {
                self.state = AgentState::Working;
                self.tool = describe(event);
                if let Some(file) = edited_file(event) {
                    self.files.retain(|f| *f != file);
                    self.files.push(file);
                    let excess = self.files.len().saturating_sub(FILES_KEPT);
                    self.files.drain(..excess);
                }
            }
            "PermissionRequest" => {
                self.state = AgentState::Waiting;
                self.permission = Some(now);
                let tool = event.tool.as_deref().unwrap_or("a tool");
                self.message = Some(format!("asks permission to use {tool}"));
            }
            // `idle_prompt`: waiting for the next prompt, which Stop already reported.
            "Notification" if event.notification.as_deref() == Some("idle_prompt") => {}
            "Notification" => {
                self.state = AgentState::Waiting;
                self.message.clone_from(&event.message);
            }
            "Stop" => {
                self.state = AgentState::Done;
                self.tool = None;
                self.message = event
                    .message
                    .as_deref()
                    .map(|m| m.chars().take(MESSAGE_KEPT).collect());
            }
            _ => {}
        }
        (self.state != before).then(|| {
            self.since = now;
            self.state
        })
    }

    /// chiika can still answer its permission request.
    pub fn can_decide(&self, now: Instant) -> bool {
        self.permission
            .is_some_and(|t| now.saturating_duration_since(t) < PERMISSION_WAIT)
    }

    /// The state to show: a silent start is shown as waiting for the user.
    pub fn shown_state(&self, now: Instant) -> AgentState {
        let silent = now.saturating_duration_since(self.since) > SILENT_START;
        if self.state == AgentState::Starting && silent {
            AgentState::Waiting
        } else {
            self.state
        }
    }

    /// The message to show, including the hint for a silent start.
    pub fn shown_message(&self, now: Instant) -> Option<String> {
        if self.state == AgentState::Starting && self.shown_state(now) == AgentState::Waiting {
            return Some("Claude asks something before starting: open its terminal".into());
        }
        self.message.clone()
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `✎ api.rs`, `$ cargo test`, `read lib.rs`, or the tool's name.
fn describe(event: &HookEvent) -> Option<String> {
    let tool = event.tool.as_deref()?;
    let file = event.file.as_deref().map(file_name);
    Some(match (tool, file) {
        ("Edit" | "Write" | "MultiEdit" | "NotebookEdit", Some(file)) => format!("✎ {file}"),
        ("Read", Some(file)) => format!("read {file}"),
        ("Bash", _) => format!("$ {}", event.command.as_deref().unwrap_or("…")),
        (tool, _) => tool.to_lowercase(),
    })
}

fn edited_file(event: &HookEvent) -> Option<String> {
    match event.tool.as_deref()? {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => event.file.as_deref().map(file_name),
        _ => None,
    }
}

/// Text added to the agent's system prompt.
pub fn system_prompt(
    role: &str,
    team: &str,
    instructions: &str,
    folder: &Path,
    worktree: Option<&str>,
) -> String {
    let duty = ROLES
        .iter()
        .find(|(name, _)| *name == role)
        .map_or("", |(_, duty)| duty);
    let place = match worktree {
        Some(name) => format!(
            "You work in your own git worktree \"{name}\", created for this session: commit your \
             work on its branch."
        ),
        None => format!("You work in the project folder {}.", folder.display()),
    };
    format!(
        "You are an agent run by chiika, in the team \"{team}\".\n{duty}\n{place}\n\
         Work with your team through the chiika tools: `team` (who is there), `send_to` (message \
         another agent), `read_screen` (follow a member's terminal), `post` and `board` (the \
         team's shared board).\n\nTeam instructions:\n{}",
        instructions.trim()
    )
}

/// What `chiika hook` prints to answer a permission request.
pub fn decision(allow: bool) -> Vec<u8> {
    let decision = if allow {
        serde_json::json!({ "behavior": "allow" })
    } else {
        serde_json::json!({ "behavior": "deny", "message": "Denied by the user in chiika." })
    };
    serde_json::json!({
        "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision }
    })
    .to_string()
    .into_bytes()
}

/// `--mcp-config` content: the chiika tools, served by `<exe> mcp` for this session.
pub fn mcp_config(exe: &Path, port: u16, token: &str, session: u32) -> String {
    serde_json::json!({
        "mcpServers": {
            "chiika": {
                "type": "stdio",
                "command": exe.display().to_string(),
                "args": ["mcp"],
                "env": {
                    "CHIIKA_PORT": port.to_string(),
                    "CHIIKA_TOKEN": token,
                    "CHIIKA_SESSION": session.to_string(),
                },
            }
        }
    })
    .to_string()
}

/// Claude Code settings that run `<exe> hook` on every hook chiika listens to.
pub fn hook_settings(exe: &Path) -> String {
    // Claude runs hook commands with a POSIX shell (Git Bash on Windows): forward slashes.
    let command = format!("\"{}\" hook", exe.display().to_string().replace('\\', "/"));
    let hooks: serde_json::Map<String, serde_json::Value> = HOOKS
        .iter()
        .map(|&name| {
            // A permission request waits for the user's answer in chiika.
            let timeout = if name == "PermissionRequest" {
                PERMISSION_WAIT.as_secs() + 10
            } else {
                5
            };
            let entry = serde_json::json!([{
                "hooks": [{ "type": "command", "command": command, "timeout": timeout }]
            }]);
            (name.to_string(), entry)
        })
        .collect();
    serde_json::json!({
        "hooks": hooks,
        // The chiika tools never ask for permission.
        "permissions": { "allow": ["mcp__chiika"] },
    })
    .to_string()
}

/// Claude Code's executable. The npm install has `claude.cmd` next to `node_modules` holding the
/// real `claude.exe`, which is launched directly so no `cmd.exe` parses the arguments.
pub fn claude_exe() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    std::env::split_paths(&path).find_map(|dir| {
        let native = dir.join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        if cfg!(windows) && dir.join("claude.cmd").is_file() && native.is_file() {
            return Some(native);
        }
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// `backend team!` → `backend-team`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "team".into() } else { out }
}

/// Whether `root` is in a git repository with a commit, which `claude --worktree` needs.
pub fn has_commit(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// `<base>-<k>` for the first `k >= n` that no worktree of the repository at `root` uses.
pub fn free_worktree_name(root: &Path, base: &str, n: u32) -> String {
    let list = git(root, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let used = |name: &str| {
        list.lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|path| Path::new(path).file_name().is_some_and(|f| f == name))
    };
    (n..)
        .map(|k| format!("{base}-{k}"))
        .find(|name| !used(name))
        .expect("an unused name exists")
}

/// `.chiika/.gitignore` keeping the agents' files out of the project's git status.
pub fn ensure_gitignore(data_dir: &Path) -> std::io::Result<()> {
    const IGNORED: [&str; 3] = ["agents/", "bin/", "daemon.toml"];
    let path = data_dir.join(".gitignore");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let missing: Vec<&str> = IGNORED
        .into_iter()
        .filter(|line| !text.lines().any(|l| l.trim() == *line))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for line in missing {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(path, text)
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(name: &str) -> HookEvent {
        HookEvent {
            name: name.into(),
            ..HookEvent::default()
        }
    }

    fn agent(now: Instant) -> AgentInfo {
        AgentInfo::new(1, PathBuf::from("w"), None, now)
    }

    #[test]
    fn hooks_drive_the_state() {
        let now = Instant::now();
        let mut a = agent(now);
        assert_eq!(a.apply(&hook("SessionStart"), now), Some(AgentState::Idle));
        assert_eq!(
            a.apply(&hook("UserPromptSubmit"), now),
            Some(AgentState::Working)
        );
        let edit = HookEvent {
            tool: Some("Edit".into()),
            file: Some("C:\\p\\src\\api.rs".into()),
            ..hook("PreToolUse")
        };
        assert_eq!(a.apply(&edit, now), None, "still working");
        assert_eq!(a.tool.as_deref(), Some("✎ api.rs"));
        assert_eq!(a.files, ["api.rs"]);
        let permission = HookEvent {
            tool: Some("Bash".into()),
            ..hook("PermissionRequest")
        };
        assert_eq!(a.apply(&permission, now), Some(AgentState::Waiting));
        assert_eq!(a.message.as_deref(), Some("asks permission to use Bash"));
        let stop = HookEvent {
            message: Some("Done: 3 files changed.".into()),
            ..hook("Stop")
        };
        assert_eq!(a.apply(&stop, now), Some(AgentState::Done));
        assert_eq!(a.message.as_deref(), Some("Done: 3 files changed."));
        assert_eq!(a.tool, None);
    }

    #[test]
    fn idle_notification_does_not_raise_an_alert() {
        let now = Instant::now();
        let mut a = agent(now);
        a.apply(&hook("Stop"), now);
        let idle = HookEvent {
            notification: Some("idle_prompt".into()),
            ..hook("Notification")
        };
        assert_eq!(a.apply(&idle, now), None);
        let question = HookEvent {
            notification: Some("permission_prompt".into()),
            message: Some("Claude needs your permission".into()),
            ..hook("Notification")
        };
        assert_eq!(a.apply(&question, now), Some(AgentState::Waiting));
    }

    #[test]
    fn bash_and_files_are_described() {
        let bash = HookEvent {
            tool: Some("Bash".into()),
            command: Some("cargo test".into()),
            ..hook("PreToolUse")
        };
        assert_eq!(describe(&bash).as_deref(), Some("$ cargo test"));
        let grep = HookEvent {
            tool: Some("Grep".into()),
            ..hook("PreToolUse")
        };
        assert_eq!(describe(&grep).as_deref(), Some("grep"));
        assert_eq!(edited_file(&grep), None);
    }

    #[test]
    fn only_recent_edited_files_are_kept_once() {
        let now = Instant::now();
        let mut a = agent(now);
        for name in ["a", "b", "c", "d", "e", "f", "g", "b"] {
            let edit = HookEvent {
                tool: Some("Write".into()),
                file: Some(format!("/p/{name}.rs")),
                ..hook("PostToolUse")
            };
            a.apply(&edit, now);
        }
        assert_eq!(a.files, ["c.rs", "d.rs", "e.rs", "f.rs", "g.rs", "b.rs"]);
    }

    #[test]
    fn agents_survive_a_snapshot() {
        let now = Instant::now();
        let mut a = agent(now);
        a.state = AgentState::Waiting;
        a.tool = Some("✎ api.rs".into());
        a.files = vec!["api.rs".into()];
        let later = now + Duration::from_secs(3);
        let snapshot = a.snapshot(later);
        assert_eq!(snapshot.since_ms, 3000);
        let restored = AgentInfo::from_snapshot(&snapshot, later);
        assert_eq!(restored.snapshot(later), snapshot);
    }

    #[test]
    fn gitignore_gains_missing_lines_and_keeps_the_user_s() {
        let dir = std::env::temp_dir().join(format!("chiika-ignore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "agents/\nmine").unwrap();
        ensure_gitignore(&dir).unwrap();
        ensure_gitignore(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(text, "agents/\nmine\nbin/\ndaemon.toml\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_silent_start_is_shown_as_waiting() {
        let start = Instant::now();
        let a = agent(start);
        assert_eq!(a.shown_state(start), AgentState::Starting);
        let later = start + Duration::from_secs(7);
        assert_eq!(a.shown_state(later), AgentState::Waiting);
        assert!(
            a.shown_message(later)
                .unwrap()
                .contains("open its terminal")
        );
    }

    #[test]
    fn prompt_has_role_team_instructions_and_place() {
        let prompt = system_prompt(
            "reviewer",
            "backend",
            "# backend\nRust API.\n",
            Path::new("/w"),
            Some("backend-reviewer-3"),
        );
        assert!(prompt.contains("team \"backend\""));
        assert!(prompt.contains("You are the reviewer"));
        assert!(prompt.contains("Rust API."));
        assert!(prompt.contains("worktree \"backend-reviewer-3\""));
        let here = system_prompt("coder", "t", "", Path::new("/p"), None);
        assert!(here.contains("project folder"));
    }

    #[test]
    fn hooks_report_the_folder_the_agent_works_in() {
        let now = Instant::now();
        let mut a = agent(now);
        let start = HookEvent {
            cwd: Some("/p/.claude/worktrees/t-coder-1".into()),
            ..hook("SessionStart")
        };
        a.apply(&start, now);
        assert_eq!(a.folder, PathBuf::from("/p/.claude/worktrees/t-coder-1"));
    }

    #[test]
    fn settings_run_chiika_hook_for_every_event() {
        let settings = hook_settings(Path::new("C:\\bin\\chiika.exe"));
        let value: serde_json::Value = serde_json::from_str(&settings).unwrap();
        for name in HOOKS {
            let command = &value["hooks"][name][0]["hooks"][0]["command"];
            assert_eq!(command, "\"C:/bin/chiika.exe\" hook");
        }
        assert_eq!(value["permissions"]["allow"][0], "mcp__chiika");
        let permission = &value["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"];
        assert!(permission.as_u64().unwrap() > PERMISSION_WAIT.as_secs());
    }

    #[test]
    fn decisions_and_mcp_config_match_what_claude_reads() {
        let allow: serde_json::Value = serde_json::from_slice(&decision(true)).unwrap();
        let output = &allow["hookSpecificOutput"];
        assert_eq!(output["hookEventName"], "PermissionRequest");
        assert_eq!(output["decision"]["behavior"], "allow");
        let deny: serde_json::Value = serde_json::from_slice(&decision(false)).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        let config = mcp_config(Path::new("C:\\bin\\chiika.exe"), 4242, "tok", 7);
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let server = &config["mcpServers"]["chiika"];
        assert_eq!(server["args"][0], "mcp");
        assert_eq!(server["env"]["CHIIKA_PORT"], "4242");
        assert_eq!(server["env"]["CHIIKA_SESSION"], "7");
    }

    #[test]
    fn a_permission_request_is_held_until_another_hook() {
        let now = Instant::now();
        let mut a = agent(now);
        a.apply(&hook("PermissionRequest"), now);
        assert!(a.can_decide(now));
        assert!(!a.can_decide(now + PERMISSION_WAIT));
        a.apply(&hook("PostToolUse"), now);
        assert!(!a.can_decide(now));
    }

    #[test]
    fn slugs_are_safe_for_branches_and_folders() {
        assert_eq!(slug("Backend Team!"), "backend-team");
        assert_eq!(slug("  --x--  "), "x");
        assert_eq!(slug("équipe"), "quipe");
        assert_eq!(slug("!!!"), "team");
    }

    #[test]
    fn worktree_names_skip_existing_worktrees() {
        let root = std::env::temp_dir().join(format!("chiika-git-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(!has_commit(&root), "not a repo");
        git(&root, &["init", "-q"]).unwrap();
        assert!(!has_commit(&root), "no commit");
        git(
            &root,
            &[
                "-c",
                "user.name=chiika",
                "-c",
                "user.email=chiika@test",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        )
        .unwrap();
        assert!(has_commit(&root));
        assert_eq!(free_worktree_name(&root, "t-coder", 1), "t-coder-1");
        let taken = root.join("trees").join("t-coder-1");
        let taken = taken.to_string_lossy();
        git(&root, &["worktree", "add", "-q", "-b", "x", &taken, "HEAD"]).unwrap();
        assert_eq!(free_worktree_name(&root, "t-coder", 1), "t-coder-2");
        let _ = git(&root, &["worktree", "remove", "--force", &taken]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
