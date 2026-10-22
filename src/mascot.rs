//! The little creature at the bottom of the sidebar, reacting to what the shells do.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::agent::{AgentInfo, AgentState};
use crate::fx::lerp;
use crate::session::Session;
use crate::theme::Theme;

/// Terminal rows used by the mascot (sprite + one row to bounce).
pub const HEIGHT: u16 = 5;

/// Sprite size in cells (10 × 8 pixels, two pixel rows per cell with half blocks).
pub const WIDTH: usize = 10;
pub const ROWS: usize = 4;

pub type Sprite = [&'static str; 8];

/// Colours of one mascot.
pub struct Palette {
    pub body: Color,
    pub eye: Color,
    pub blush: Color,
    pub tear: Color,
}

impl Palette {
    /// The white mascot of the sidebar.
    pub fn plain(theme: &Theme) -> Palette {
        Palette {
            body: Color::Rgb(250, 248, 245),
            eye: Color::Rgb(40, 40, 48),
            blush: theme.accents[0],
            tear: theme.accents[4],
        }
    }

    /// A mascot in a session's colour.
    pub fn tinted(accent: Color, theme: &Theme) -> Palette {
        Palette {
            body: accent,
            eye: Color::Rgb(40, 40, 48),
            blush: lerp(accent, Color::Rgb(235, 90, 130), 0.55),
            tear: theme.accents[4],
        }
    }

    /// Every pixel the same colour: a shadow.
    pub fn flat(color: Color) -> Palette {
        Palette {
            body: color,
            eye: color,
            blush: color,
            tear: color,
        }
    }
}

const SAD_FOR: Duration = Duration::from_secs(5);
const HAPPY_FOR: Duration = Duration::from_secs(3);
const SLEEP_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Working,
    Happy,
    Sad,
    Sleep,
    /// Needs the user.
    Alert,
}

/// Picks the mood: a recent failure beats a recent success, which beats work, then sleep.
/// `last_result` = when the last notable command ended and whether it succeeded.
pub fn mood(
    now: Instant,
    last_result: Option<(Instant, bool)>,
    busy: bool,
    last_activity: Option<Instant>,
) -> Mood {
    let age = |t: Instant| now.saturating_duration_since(t);
    match last_result {
        Some((t, false)) if age(t) < SAD_FOR => return Mood::Sad,
        Some((t, true)) if age(t) < HAPPY_FOR => return Mood::Happy,
        _ => {}
    }
    if busy {
        Mood::Working
    } else if last_activity.is_none_or(|t| age(t) > SLEEP_AFTER) {
        Mood::Sleep
    } else {
        Mood::Idle
    }
}

// Pixels: '.' empty, 'w' body, 'k' eyes and mouth, 'p' blush, 't' tears.
const OPEN: Sprite = [
    ".ww....ww.",
    ".wpw..wpw.",
    "wwwwwwwwww",
    "wwkwwwwkww",
    "wpwwkkwwpw",
    "wwwwwwwwww",
    ".wwwwwwww.",
    "..ww..ww..",
];
const CLOSED: Sprite = [
    ".ww....ww.",
    ".wpw..wpw.",
    "wwwwwwwwww",
    "wkkwwwwkkw",
    "wpwwwwwwpw",
    "wwwwwwwwww",
    ".wwwwwwww.",
    "..ww..ww..",
];
const CRYING: Sprite = [
    ".ww....ww.",
    ".wpw..wpw.",
    "wwwwwwwwww",
    "wwkwwwwkww",
    "wptwwwwtpw",
    "wwtwkkwtww",
    ".wwwwwwww.",
    "..ww..ww..",
];

/// Mood of one session: its own last command, activity and age.
pub fn session_mood(session: &Session, now: Instant) -> Mood {
    if let Some(agent) = &session.agent {
        return agent_mood(agent, now);
    }
    let last = session.marks(|m| m.last.as_ref().map(|f| (f.at, f.code == 0)));
    mood(now, last, session.is_busy(now), session.last_output())
}

/// Mood of an agent, from what its hooks reported.
pub fn agent_mood(agent: &AgentInfo, now: Instant) -> Mood {
    let quiet = now.saturating_duration_since(agent.since);
    match agent.shown_state(now) {
        AgentState::Waiting => Mood::Alert,
        AgentState::Working | AgentState::Starting => Mood::Working,
        AgentState::Done if quiet < HAPPY_FOR => Mood::Happy,
        _ if quiet > SLEEP_AFTER => Mood::Sleep,
        AgentState::Done | AgentState::Idle => Mood::Idle,
    }
}

/// Sprite, whether it is lifted by a bounce, and the speech bubble, at time `elapsed`.
pub fn pose(mood: Mood, elapsed: Duration) -> (&'static Sprite, bool, &'static str) {
    let ms = elapsed.as_millis();
    match mood {
        // Blink for a quarter second every four seconds.
        Mood::Idle if ms % 4000 < 250 => (&CLOSED, false, ""),
        Mood::Idle => (&OPEN, false, ""),
        Mood::Working => (&OPEN, (ms / 300).is_multiple_of(2), "…"),
        Mood::Happy => (&CLOSED, (ms / 150).is_multiple_of(2), "yay!"),
        Mood::Sad => (&CRYING, false, "oh no"),
        Mood::Sleep => (&CLOSED, false, ["z", "zz", "zzz"][(ms / 600 % 3) as usize]),
        Mood::Alert => (&OPEN, (ms / 180).is_multiple_of(2), "!"),
    }
}

/// Draws `sprite` with its top-left cell at (`x`, `y`), skipping cells outside `clip`.
pub fn draw_sprite(
    buf: &mut Buffer,
    clip: Rect,
    x: i32,
    y: i32,
    sprite: &Sprite,
    palette: &Palette,
) {
    let color = |pixel: u8| match pixel {
        b'w' => Some(palette.body),
        b'k' => Some(palette.eye),
        b'p' => Some(palette.blush),
        b't' => Some(palette.tear),
        _ => None,
    };
    for row in 0..ROWS {
        let (top, bottom) = (sprite[row * 2].as_bytes(), sprite[row * 2 + 1].as_bytes());
        for col in 0..WIDTH {
            let (cx, cy) = (x + col as i32, y + row as i32);
            let inside = cx >= i32::from(clip.x)
                && cx < i32::from(clip.right())
                && cy >= i32::from(clip.y)
                && cy < i32::from(clip.bottom());
            if !inside {
                continue;
            }
            let cell = &mut buf[(cx as u16, cy as u16)];
            match (color(top[col]), color(bottom[col])) {
                (None, None) => {}
                (Some(fg), bg) => {
                    cell.set_symbol("▀");
                    cell.set_style(Style::new().fg(fg).bg(bg.unwrap_or(Color::Reset)));
                }
                (None, Some(fg)) => {
                    cell.set_symbol("▄");
                    cell.set_style(Style::new().fg(fg).bg(Color::Reset));
                }
            }
        }
    }
}

/// Draws the sidebar mascot in `area` (at least `HEIGHT` rows); `elapsed` drives the animation.
pub fn draw(buf: &mut Buffer, area: Rect, mood: Mood, theme: &Theme, elapsed: Duration) {
    if area.height < HEIGHT || area.width < WIDTH as u16 + 2 {
        return;
    }
    let (sprite, bounce, bubble) = pose(mood, elapsed);
    let x0 = area.x + 1;
    let y0 = area.y + u16::from(!bounce);
    let palette = Palette::plain(theme);
    draw_sprite(buf, area, x0.into(), y0.into(), sprite, &palette);
    let bubble_x = x0 + WIDTH as u16 + 1;
    buf.set_stringn(
        bubble_x,
        area.y + 1,
        bubble,
        usize::from(area.right().saturating_sub(bubble_x)),
        Style::new().fg(theme.muted),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_beats_success_beats_work_beats_sleep() {
        let now = Instant::now() + Duration::from_secs(100);
        let just = now - Duration::from_secs(1);
        let long_ago = now - Duration::from_secs(90);
        assert_eq!(mood(now, Some((just, false)), true, Some(just)), Mood::Sad);
        assert_eq!(mood(now, Some((just, true)), true, Some(just)), Mood::Happy);
        assert_eq!(mood(now, None, true, Some(just)), Mood::Working);
        assert_eq!(mood(now, None, false, Some(just)), Mood::Idle);
        assert_eq!(mood(now, None, false, Some(long_ago)), Mood::Sleep);
        assert_eq!(mood(now, None, false, None), Mood::Sleep);
    }

    #[test]
    fn reactions_fade_out() {
        let now = Instant::now() + Duration::from_secs(100);
        let recent = Some(now - Duration::from_secs(1));
        let failed_long_ago = Some((now - Duration::from_secs(6), false));
        let succeeded_long_ago = Some((now - Duration::from_secs(4), true));
        assert_eq!(mood(now, failed_long_ago, false, recent), Mood::Idle);
        assert_eq!(mood(now, succeeded_long_ago, false, recent), Mood::Idle);
    }

    #[test]
    fn agents_show_what_their_hooks_say() {
        let start = Instant::now();
        let mut agent = AgentInfo::new(1, std::path::PathBuf::new(), None, start);
        assert_eq!(agent_mood(&agent, start), Mood::Working, "starting");
        agent.state = AgentState::Waiting;
        assert_eq!(agent_mood(&agent, start), Mood::Alert);
        agent.state = AgentState::Done;
        assert_eq!(agent_mood(&agent, start), Mood::Happy);
        assert_eq!(
            agent_mood(&agent, start + Duration::from_secs(10)),
            Mood::Idle
        );
        assert_eq!(
            agent_mood(&agent, start + Duration::from_secs(90)),
            Mood::Sleep
        );
    }

    #[test]
    fn sprites_are_rectangular() {
        for sprite in [OPEN, CLOSED, CRYING] {
            assert!(sprite.iter().all(|row| row.len() == WIDTH));
        }
    }

    #[test]
    fn sprites_are_clipped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        let palette = Palette::flat(Color::Red);
        draw_sprite(&mut buf, Rect::new(0, 0, 20, 10), -5, -2, &OPEN, &palette);
        draw_sprite(&mut buf, Rect::new(0, 0, 20, 10), 15, 8, &OPEN, &palette);
        assert_eq!(buf[(0, 0)].symbol(), "▀", "visible part drawn");
        assert_eq!(
            buf[(10, 5)].symbol(),
            " ",
            "nothing drawn outside the sprites"
        );
    }

    #[test]
    fn draws_nothing_in_a_too_small_area() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 3));
        let before = buf.clone();
        draw(
            &mut buf,
            Rect::new(0, 0, 5, 3),
            Mood::Idle,
            &crate::theme::THEMES[0],
            Duration::ZERO,
        );
        assert_eq!(buf, before);
    }
}
