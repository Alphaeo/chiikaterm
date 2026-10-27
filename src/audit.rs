//! The audit log: what the project's agents did, one JSON line per event in
//! `.chiika/audit/YYYY-MM-DD.jsonl` (UTC days). Written by whoever receives the event (the
//! daemon, or chiika without one); a failed write is logged, never shown to the agent.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::agent::AgentInfo;
use crate::channel::HookEvent;

#[derive(Debug, Serialize)]
struct Line<'a> {
    /// UTC, RFC 3339.
    time: String,
    /// The OS user running chiika: who decided, for a permission.
    user: String,
    /// `agent_started`, `agent_resumed`, `agent_closed`, a hook name (`PreToolUse`…), or
    /// `permission_allowed` / `_denied` / `_to_terminal` / `_expired`.
    event: &'a str,
    session: u32,
    team: &'a str,
    role: &'a str,
    conversation: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<&'a str>,
}

/// Appends `event` of agent `session` to the log of the project whose data is in `data_dir`;
/// `hook` gives the tool and what it works on.
pub fn record(
    data_dir: &Path,
    session: u32,
    agent: &AgentInfo,
    event: &str,
    hook: Option<&HookEvent>,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = Line {
        time: rfc3339(now),
        user: std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_default(),
        event,
        session,
        team: &agent.team_name,
        role: &agent.role,
        conversation: agent.conversation.as_deref(),
        tool: hook.and_then(|h| h.tool.as_deref()),
        command: hook.and_then(|h| h.command.as_deref()),
        file: hook.and_then(|h| h.file.as_deref()),
    };
    let path = data_dir
        .join("audit")
        .join(format!("{}.jsonl", &line.time[..10]));
    if let Err(e) = append(&path, &line) {
        tracing::warn!("audit log {}: {e}", path.display());
    }
}

fn append(path: &Path, line: &Line) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    // One write per line in append mode: lines of the daemon and of chiika do not mix.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// `secs` since the Unix epoch as `2026-10-25T09:30:00Z`.
fn rfc3339(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The date `days` after 1970-01-01, proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;

    #[test]
    fn dates_are_civil_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(951_782_400),
            "2000-02-29T00:00:00Z",
            "leap day of a 400 year"
        );
        assert_eq!(
            rfc3339(4_107_542_399),
            "2100-02-28T23:59:59Z",
            "2100 is not leap"
        );
        assert_eq!(rfc3339(1_793_007_000), "2026-10-26T09:30:00Z");
    }

    #[test]
    fn each_event_is_one_json_line() {
        let dir = std::env::temp_dir().join(format!("chiika-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut agent = AgentInfo::new(1, PathBuf::from("/p"), None, Instant::now());
        agent.team_name = "backend".into();
        agent.role = "coder".into();
        agent.conversation = Some("c0ffee".into());
        let hook = HookEvent {
            name: "PreToolUse".into(),
            tool: Some("Bash".into()),
            command: Some("cargo test".into()),
            ..HookEvent::default()
        };
        record(&dir, 3, &agent, "agent_started", None);
        record(&dir, 3, &agent, "PreToolUse", Some(&hook));

        let logs: Vec<_> = std::fs::read_dir(dir.join("audit")).unwrap().collect();
        assert_eq!(logs.len(), 1);
        let text = std::fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "agent_started");
        assert_eq!(lines[0]["team"], "backend");
        assert_eq!(lines[0]["role"], "coder");
        assert_eq!(lines[0]["conversation"], "c0ffee");
        assert!(lines[0].get("tool").is_none());
        assert_eq!(lines[1]["tool"], "Bash");
        assert_eq!(lines[1]["command"], "cargo test");
        assert!(lines[1]["time"].as_str().unwrap().ends_with('Z'));
    }
}
