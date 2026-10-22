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

#[cfg(test)]
mod tests {
    use super::*;

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
