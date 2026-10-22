//! Shell integration: the PowerShell script chiika injects, and the OSC marks it emits
//! (`133;A/B/C/D` prompt and command boundaries, `633;E` command line, `9;9` current directory).

use std::time::{Duration, Instant};

use crate::protocol::{FinishedSnapshot, MarksSnapshot};

/// Script run by chiika's PowerShell sessions, see `encoded_command`.
const SCRIPT: &str = include_str!("shell_integration.ps1");

/// Argument of `-EncodedCommand`: the script as base64 of its UTF-16LE bytes.
pub fn encoded_command() -> String {
    let bytes: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A command the shell is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub text: String,
    pub started: Instant,
}

/// A command that has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub text: String,
    pub code: i64,
    pub duration: Duration,
    /// When it ended.
    pub at: Instant,
}

/// What the shell told us through its marks; fed by the vt100 parser, in stream order.
#[derive(Debug, Default)]
pub struct ShellMarks {
    /// The shell emits marks: busy/idle comes from them instead of output activity.
    pub integrated: bool,
    pub running: Option<Running>,
    pub last: Option<Finished>,
    pub cwd: Option<String>,
    /// Commands finished since the app last looked.
    finished: Vec<Finished>,
    /// Command line announced by `633;E`, waiting for `133;C`.
    pending: Option<String>,
}

impl ShellMarks {
    pub fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }

    /// What survives a reattach; times become "milliseconds ago".
    pub fn snapshot(&self, now: Instant) -> MarksSnapshot {
        let ago = |t: Instant| now.saturating_duration_since(t).as_millis() as u64;
        MarksSnapshot {
            integrated: self.integrated,
            cwd: self.cwd.clone(),
            running: self
                .running
                .as_ref()
                .map(|r| (r.text.clone(), ago(r.started))),
            last: self.last.as_ref().map(|f| FinishedSnapshot {
                text: f.text.clone(),
                code: f.code,
                duration_ms: f.duration.as_millis() as u64,
                ago_ms: ago(f.at),
            }),
        }
    }

    pub fn from_snapshot(snapshot: &MarksSnapshot, now: Instant) -> ShellMarks {
        let before = |ms: u64| now.checked_sub(Duration::from_millis(ms)).unwrap_or(now);
        ShellMarks {
            integrated: snapshot.integrated,
            cwd: snapshot.cwd.clone(),
            running: snapshot.running.as_ref().map(|(text, ago)| Running {
                text: text.clone(),
                started: before(*ago),
            }),
            last: snapshot.last.as_ref().map(|f| Finished {
                text: f.text.clone(),
                code: f.code,
                duration: Duration::from_millis(f.duration_ms),
                at: before(f.ago_ms),
            }),
            finished: Vec::new(),
            pending: None,
        }
    }

    fn on_osc(&mut self, params: &[&[u8]], now: Instant) {
        // Params are split on ';': join the tail back for free text.
        let text = |rest: &[&[u8]]| String::from_utf8_lossy(&rest.join(&b';')).into_owned();
        match params {
            [b"133", b"A" | b"B", ..] => self.integrated = true,
            [b"633", b"E", rest @ ..] => self.pending = Some(unescape(&text(rest))),
            [b"133", b"C", ..] => {
                self.integrated = true;
                let text = self.pending.take().unwrap_or_default();
                self.running = Some(Running {
                    text: text.replace('\n', " ").trim().to_string(),
                    started: now,
                });
            }
            [b"133", b"D", rest @ ..] => {
                self.integrated = true;
                // A prompt redrawn without a command (Ctrl+C, empty line) has nothing to finish.
                let Some(command) = self.running.take() else {
                    return;
                };
                if command.text.is_empty() {
                    return;
                }
                let code = rest
                    .first()
                    .and_then(|c| std::str::from_utf8(c).ok()?.parse().ok())
                    .unwrap_or(0);
                let done = Finished {
                    text: command.text,
                    code,
                    duration: now.saturating_duration_since(command.started),
                    at: now,
                };
                self.finished.push(done.clone());
                self.last = Some(done);
            }
            [b"9", b"9", rest @ ..] => {
                self.cwd = Some(text(rest).trim_matches('"').to_string());
            }
            _ => {}
        }
    }
}

impl vt100::Callbacks for ShellMarks {
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        self.on_osc(params, Instant::now());
    }
}

/// Undoes the script's escaping: `\\` and `\xHH`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(char::from(byte)),
                    Err(_) => {
                        out.push_str("\\x");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(bytes: &[u8]) -> vt100::Parser<ShellMarks> {
        let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, ShellMarks::default());
        parser.process(bytes);
        parser
    }

    #[test]
    fn base64_matches_rfc_4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encoded_command_is_utf16le_base64() {
        // The script starts with "# c": 23 00 20 00 63 00 in UTF-16LE.
        assert!(encoded_command().starts_with("IwAgAGMA"));
    }

    #[test]
    fn a_full_command_is_reported_once() {
        let mut parser = feed(
            b"\x1b]133;A\x07PS> \x1b]133;B\x07\x1b]633;E;cargo build\x07\x1b]133;C\x07out\x1b]133;D;3\x07",
        );
        let marks = parser.callbacks_mut();
        assert!(marks.integrated);
        assert_eq!(marks.running, None);
        let finished = marks.take_finished();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].text, "cargo build");
        assert_eq!(finished[0].code, 3);
        assert_eq!(marks.last.as_ref().map(|f| f.code), Some(3));
        assert!(marks.take_finished().is_empty());
        assert_eq!(parser.screen().contents(), "PS> out");
    }

    #[test]
    fn running_command_is_visible_until_it_ends() {
        let parser = feed(b"\x1b]633;E;Start-Sleep 5\x07\x1b]133;C\x07");
        let running = parser.callbacks().running.as_ref().map(|r| r.text.clone());
        assert_eq!(running.as_deref(), Some("Start-Sleep 5"));
    }

    #[test]
    fn prompt_without_command_or_empty_line_reports_nothing() {
        let mut parser = feed(b"\x1b]133;D;0\x07\x1b]633;E;\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        assert!(parser.callbacks_mut().take_finished().is_empty());
        assert_eq!(parser.callbacks().last, None);
    }

    #[test]
    fn escaped_command_text_is_restored() {
        assert_eq!(unescape(r"a\x3bb \\ c\x0ad"), "a;b \\ c\nd");
        assert_eq!(unescape(r"bad \xZZ"), r"bad \xZZ");
        assert_eq!(unescape("trailing \\"), "trailing \\");
        let parser = feed(b"\x1b]633;E;echo a\\x3b echo b\x07\x1b]133;C\x07");
        let running = parser.callbacks().running.as_ref().map(|r| r.text.clone());
        assert_eq!(running.as_deref(), Some("echo a; echo b"));
    }

    #[test]
    fn marks_survive_a_snapshot() {
        let mut parser = feed(
            b"\x1b]9;9;C:\\p\x07\x1b]633;E;cargo test\x07\x1b]133;C\x07\x1b]133;D;101\x07\x1b]633;E;sleep 9\x07\x1b]133;C\x07",
        );
        let now = Instant::now();
        let snapshot = parser.callbacks_mut().snapshot(now);
        let restored = ShellMarks::from_snapshot(&snapshot, now);
        assert!(restored.integrated);
        assert_eq!(restored.cwd.as_deref(), Some("C:\\p"));
        assert_eq!(
            restored.running.as_ref().map(|r| r.text.as_str()),
            Some("sleep 9")
        );
        let last = restored.last.clone().unwrap();
        assert_eq!((last.text.as_str(), last.code), ("cargo test", 101));
        assert_eq!(restored.snapshot(now), snapshot);
    }

    #[test]
    fn current_directory_is_tracked() {
        let parser = feed(b"\x1b]9;9;C:\\dev\\api\x07");
        assert_eq!(parser.callbacks().cwd.as_deref(), Some("C:\\dev\\api"));
        assert!(!parser.callbacks().integrated);
    }
}
