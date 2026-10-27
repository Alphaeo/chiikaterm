use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// `CSI n ~` codes of F5..F12.
const F_TILDE: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];

/// Keys that become `CSI 1;m X` (unmodified: `CSI X`, or `SS3 X` when the flag is set) and
/// `CSI n;m ~` (unmodified: `CSI n ~`).
enum Seq {
    Final(u8, bool),
    Tilde(u8),
}

/// Encodes a key press as the bytes an xterm-compatible terminal sends to the PTY.
pub fn encode_key(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let esc_if_alt = |bytes: &[u8]| [if alt { &b"\x1b"[..] } else { b"" }, bytes].concat();
    let seq = match key.code {
        KeyCode::Char(c) => {
            // Windows reports AltGr as Ctrl+Alt and already gives the final char ('@', '{', '€'…).
            if ctrl && alt && !c.is_ascii_alphabetic() && c != ' ' {
                return Some(c.to_string().into_bytes());
            }
            let bytes = if ctrl && c.is_ascii_alphabetic() {
                vec![c.to_ascii_lowercase() as u8 & 0x1f]
            } else if ctrl && c == ' ' {
                vec![0]
            } else {
                c.to_string().into_bytes()
            };
            return Some(esc_if_alt(&bytes));
        }
        KeyCode::Enter => return Some(esc_if_alt(b"\r")),
        KeyCode::Tab => return Some(esc_if_alt(b"\t")),
        KeyCode::BackTab => return Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace if ctrl => return Some(esc_if_alt(&[0x08])),
        KeyCode::Backspace => return Some(esc_if_alt(&[0x7f])),
        KeyCode::Esc => return Some(esc_if_alt(&[0x1b])),
        KeyCode::Up => Seq::Final(b'A', app_cursor),
        KeyCode::Down => Seq::Final(b'B', app_cursor),
        KeyCode::Right => Seq::Final(b'C', app_cursor),
        KeyCode::Left => Seq::Final(b'D', app_cursor),
        KeyCode::Home => Seq::Final(b'H', false),
        KeyCode::End => Seq::Final(b'F', false),
        KeyCode::Insert => Seq::Tilde(2),
        KeyCode::Delete => Seq::Tilde(3),
        KeyCode::PageUp => Seq::Tilde(5),
        KeyCode::PageDown => Seq::Tilde(6),
        KeyCode::F(n @ 1..=4) => Seq::Final(b'P' + n - 1, true),
        KeyCode::F(n @ 5..=12) => Seq::Tilde(F_TILDE[usize::from(n - 5)]),
        _ => return None,
    };
    // xterm modifier parameter: 1 + Shift + 2·Alt + 4·Ctrl.
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let bytes = match seq {
        Seq::Final(c, true) if m == 1 => vec![0x1b, b'O', c],
        Seq::Final(c, _) if m == 1 => vec![0x1b, b'[', c],
        Seq::Final(c, _) => format!("\x1b[1;{m}{}", c as char).into_bytes(),
        Seq::Tilde(n) if m == 1 => format!("\x1b[{n}~").into_bytes(),
        Seq::Tilde(n) => format!("\x1b[{n};{m}~").into_bytes(),
    };
    Some(bytes)
}

/// Bytes for pasted text: wrapped in bracketed-paste markers when the program asked for them,
/// otherwise newlines become `\r` like a typed Enter.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// A key with its modifiers, as written in the config (`"ctrl+q"`, `"alt+left"`, `"f12"`).
pub type Chord = (KeyCode, KeyModifiers);

/// Reads a chord; `None` if a part is unknown or there is not exactly one key.
pub fn parse_chord(text: &str) -> Option<Chord> {
    let mut modifiers = KeyModifiers::NONE;
    let mut code = None;
    for part in text.split('+').map(|p| p.trim().to_lowercase()) {
        let key = match part.as_str() {
            "ctrl" | "control" => {
                modifiers |= KeyModifiers::CONTROL;
                continue;
            }
            "alt" => {
                modifiers |= KeyModifiers::ALT;
                continue;
            }
            "shift" => {
                modifiers |= KeyModifiers::SHIFT;
                continue;
            }
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "insert" => KeyCode::Insert,
            "delete" | "del" => KeyCode::Delete,
            "tab" => KeyCode::Tab,
            "enter" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "space" => KeyCode::Char(' '),
            f if f.len() > 1 && f.starts_with('f') => match f[1..].parse::<u8>() {
                Ok(n @ 1..=12) => KeyCode::F(n),
                _ => return None,
            },
            c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?),
            _ => return None,
        };
        if code.replace(key).is_some() {
            return None;
        }
    }
    Some((code?, modifiers))
}

/// Whether `key` is `chord`. Letters ignore case and Shift (`Ctrl+Shift+Q` is `Ctrl+Q`), as the
/// terminal reports them differently across layouts.
pub fn chord_matches(chord: Chord, key: KeyEvent) -> bool {
    let (code, modifiers) = chord;
    match (code, key.code) {
        (KeyCode::Char(want), KeyCode::Char(got)) => {
            want.eq_ignore_ascii_case(&got)
                && key.modifiers - KeyModifiers::SHIFT == modifiers - KeyModifiers::SHIFT
        }
        _ => code == key.code && key.modifiers == modifiers,
    }
}

/// `Ctrl+Q`, `Alt+←`, `Shift+PgUp`.
pub fn describe(chord: Chord) -> String {
    let (code, modifiers) = chord;
    let mut text = String::new();
    for (flag, name) in [
        (KeyModifiers::CONTROL, "Ctrl+"),
        (KeyModifiers::ALT, "Alt+"),
        (KeyModifiers::SHIFT, "Shift+"),
    ] {
        if modifiers.contains(flag) {
            text.push_str(name);
        }
    }
    let key = match code {
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    };
    text + &key
}

/// The keys chiika takes for itself even inside a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub leave: Chord,
    pub pane_left: Chord,
    pub pane_right: Chord,
    pub pane_up: Chord,
    pub pane_down: Chord,
    pub scroll_up: Chord,
    pub scroll_down: Chord,
    pub prev_command: Chord,
    pub next_command: Chord,
}

impl Default for Keys {
    fn default() -> Keys {
        Keys::from_config(&crate::config::KeysConfig::default()).0
    }
}

impl Keys {
    /// The configured keys; an unreadable one keeps its default and is reported.
    pub fn from_config(config: &crate::config::KeysConfig) -> (Keys, Vec<String>) {
        let defaults = crate::config::KeysConfig::default();
        let mut errors = Vec::new();
        let mut read = |name: &str, text: &str, default: &str| {
            parse_chord(text).unwrap_or_else(|| {
                errors.push(format!("{name} = \"{text}\""));
                parse_chord(default).expect("default keys are valid")
            })
        };
        let keys = Keys {
            leave: read("leave", &config.leave, &defaults.leave),
            pane_left: read("pane_left", &config.pane_left, &defaults.pane_left),
            pane_right: read("pane_right", &config.pane_right, &defaults.pane_right),
            pane_up: read("pane_up", &config.pane_up, &defaults.pane_up),
            pane_down: read("pane_down", &config.pane_down, &defaults.pane_down),
            scroll_up: read("scroll_up", &config.scroll_up, &defaults.scroll_up),
            scroll_down: read("scroll_down", &config.scroll_down, &defaults.scroll_down),
            prev_command: read("prev_command", &config.prev_command, &defaults.prev_command),
            next_command: read("next_command", &config.next_command, &defaults.next_command),
        };
        (keys, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_are_read_from_text() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(parse_chord("ctrl+q"), Some((KeyCode::Char('q'), ctrl)));
        assert_eq!(
            parse_chord(" Ctrl + Shift + PageUp "),
            Some((KeyCode::PageUp, ctrl | KeyModifiers::SHIFT))
        );
        assert_eq!(
            parse_chord("alt+left"),
            Some((KeyCode::Left, KeyModifiers::ALT))
        );
        assert_eq!(
            parse_chord("f12"),
            Some((KeyCode::F(12), KeyModifiers::NONE))
        );
        assert_eq!(parse_chord("ctrl+space"), Some((KeyCode::Char(' '), ctrl)));
        assert_eq!(parse_chord("ctrl+"), None, "no key");
        assert_eq!(parse_chord("ctrl+a+b"), None, "two keys");
        assert_eq!(parse_chord("hyper+q"), None);
        assert_eq!(parse_chord("f13"), None);
    }

    #[test]
    fn letters_match_whatever_the_case_and_shift() {
        let leave = parse_chord("ctrl+q").unwrap();
        let ctrl = KeyModifiers::CONTROL;
        assert!(chord_matches(
            leave,
            KeyEvent::new(KeyCode::Char('q'), ctrl)
        ));
        assert!(chord_matches(
            leave,
            KeyEvent::new(KeyCode::Char('Q'), ctrl | KeyModifiers::SHIFT)
        ));
        assert!(!chord_matches(
            leave,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        ));
        let up = parse_chord("shift+pageup").unwrap();
        assert!(chord_matches(
            up,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT)
        ));
        assert!(!chord_matches(
            up,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
        ));
    }

    #[test]
    fn chords_are_described_for_people() {
        assert_eq!(describe(parse_chord("ctrl+q").unwrap()), "Ctrl+Q");
        assert_eq!(describe(parse_chord("alt+left").unwrap()), "Alt+←");
        assert_eq!(describe(parse_chord("shift+pageup").unwrap()), "Shift+PgUp");
    }

    #[test]
    fn bad_config_keys_fall_back_and_are_reported() {
        let config = crate::config::KeysConfig {
            leave: "ctrl+w".into(),
            pane_left: "nope".into(),
            ..crate::config::KeysConfig::default()
        };
        let (keys, errors) = Keys::from_config(&config);
        assert_eq!(keys.leave, (KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(keys.pane_left, (KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(errors, ["pane_left = \"nope\""]);
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(KeyEvent::new(code, mods), false)
    }

    #[test]
    fn plain_char_is_utf8() {
        assert_eq!(
            enc(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(b"a".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Char('é'), KeyModifiers::NONE),
            Some(vec![0xc3, 0xa9])
        );
    }

    #[test]
    fn ctrl_letter_is_control_byte() {
        assert_eq!(
            enc(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec![0x03])
        );
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(enc(KeyCode::Char('C'), ctrl_shift), Some(vec![0x03]));
        assert_eq!(
            enc(KeyCode::Char(' '), KeyModifiers::CONTROL),
            Some(vec![0x00])
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            enc(KeyCode::Char('x'), KeyModifiers::ALT),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn altgr_char_is_sent_as_is() {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(enc(KeyCode::Char('@'), altgr), Some(b"@".to_vec()));
        assert_eq!(
            enc(KeyCode::Char('€'), altgr),
            Some("€".as_bytes().to_vec())
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(
            enc(KeyCode::Enter, KeyModifiers::NONE),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Backspace, KeyModifiers::NONE),
            Some(vec![0x7f])
        );
        assert_eq!(
            enc(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            enc(KeyCode::F(5), KeyModifiers::NONE),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            enc(KeyCode::F(12), KeyModifiers::NONE),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Delete, KeyModifiers::NONE),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(encode_key(up, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(up, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn unknown_keys_are_dropped() {
        assert_eq!(enc(KeyCode::Null, KeyModifiers::NONE), None);
        assert_eq!(enc(KeyCode::F(0), KeyModifiers::NONE), None);
        assert_eq!(enc(KeyCode::F(13), KeyModifiers::NONE), None);
    }

    #[test]
    fn modified_navigation_keys_use_xterm_parameters() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(enc(KeyCode::Left, ctrl), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(
            enc(KeyCode::Right, KeyModifiers::SHIFT),
            Some(b"\x1b[1;2C".to_vec())
        );
        assert_eq!(enc(KeyCode::Home, ctrl), Some(b"\x1b[1;5H".to_vec()));
        assert_eq!(enc(KeyCode::Delete, ctrl), Some(b"\x1b[3;5~".to_vec()));
        assert_eq!(
            enc(KeyCode::F(1), KeyModifiers::SHIFT),
            Some(b"\x1b[1;2P".to_vec())
        );
        assert_eq!(enc(KeyCode::F(5), ctrl), Some(b"\x1b[15;5~".to_vec()));
    }

    #[test]
    fn modified_arrow_ignores_app_cursor_mode() {
        let ctrl_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(encode_key(ctrl_up, true), Some(b"\x1b[1;5A".to_vec()));
    }

    #[test]
    fn word_delete_keys() {
        assert_eq!(
            enc(KeyCode::Backspace, KeyModifiers::ALT),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Backspace, KeyModifiers::CONTROL),
            Some(vec![0x08])
        );
    }

    #[test]
    fn paste_converts_newlines() {
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(encode_paste("", false), Vec::<u8>::new());
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        assert_eq!(encode_paste("ls", true), b"\x1b[200~ls\x1b[201~".to_vec());
    }
}
