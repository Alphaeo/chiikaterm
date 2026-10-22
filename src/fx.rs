//! Small visual effects, applied to the ratatui buffer after widgets are rendered.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Spinner frame for a clock that started at an arbitrary epoch.
pub fn spinner(elapsed: Duration) -> char {
    SPINNER[(elapsed.as_millis() / 80) as usize % SPINNER.len()]
}

/// One bar per value, scaled to the largest; zero is the lowest bar.
pub fn sparkline(values: &[u32]) -> String {
    let max = u64::from(values.iter().copied().max().unwrap_or(0).max(1));
    values
        .iter()
        .map(|&v| match u64::from(v) {
            0 => SPARK[0],
            v => SPARK[1 + (v * 6 / max) as usize],
        })
        .collect()
}

/// `42s`, `12m`, `1h05`.
pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{:02}", s / 3600, s / 60 % 60),
    }
}

/// Linear blend of two RGB colours; non-RGB colours are returned unchanged (`a`).
pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (a, b) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// Diagonal wipe revealing `area` from the top-left; `progress` goes from 0 (hidden) to 1 (done).
/// Hidden cells are blanked and the moving edge is dimmed, so it works with any colours.
pub fn sweep(buf: &mut Buffer, area: Rect, progress: f32) {
    if progress >= 1.0 || area.is_empty() {
        return;
    }
    const EDGE: f32 = 0.15;
    // Scale so the dimmed edge has fully left the area when progress reaches 1.
    let front = progress * (1.0 + EDGE);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let d = (f32::from(x - area.x) / f32::from(area.width)
                + f32::from(y - area.y) / f32::from(area.height))
                / 2.0;
            let cell = &mut buf[(x, y)];
            if d >= front {
                cell.reset();
            } else if d >= front - EDGE {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Recolours the border lines of `area` with a diagonal gradient; title text keeps its colour.
pub fn gradient_border(buf: &mut Buffer, area: Rect, from: Color, to: Color) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let span = f32::from(area.width - 1 + area.height - 1);
    let mut paint = |x: u16, y: u16| {
        let t = f32::from(x - area.x + y - area.y) / span;
        let cell = &mut buf[(x, y)];
        if matches!(cell.symbol(), "─" | "│" | "╭" | "╮" | "╰" | "╯") {
            cell.fg = lerp(from, to, t);
        }
    };
    for x in area.left()..area.right() {
        paint(x, area.top());
        paint(x, area.bottom() - 1);
    }
    for y in area.top() + 1..area.bottom() - 1 {
        paint(area.left(), y);
        paint(area.right() - 1, y);
    }
}

/// Case-insensitive subsequence match; returns the char indices of `text` that matched.
pub fn fuzzy_match(query: &str, text: &str) -> Option<Vec<usize>> {
    let mut wanted = query.chars().flat_map(char::to_lowercase).peekable();
    let mut hits = Vec::new();
    for (i, c) in text.chars().enumerate() {
        let Some(&w) = wanted.peek() else { break };
        if c.to_lowercase().eq(std::iter::once(w)) {
            hits.push(i);
            wanted.next();
        }
    }
    wanted.peek().is_none().then_some(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_scales_to_max() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0, 0]), "▁▁");
        assert_eq!(sparkline(&[0, 1, 50, 100]), "▁▂▅█");
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(format_duration(Duration::ZERO), "0s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(60)), "1m");
        assert_eq!(format_duration(Duration::from_secs(3900)), "1h05");
    }

    #[test]
    fn lerp_hits_both_ends() {
        let (a, b) = (Color::Rgb(0, 100, 200), Color::Rgb(200, 100, 0));
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
        assert_eq!(lerp(a, b, 0.5), Color::Rgb(100, 100, 100));
        assert_eq!(lerp(Color::Red, b, 0.5), Color::Red);
    }

    #[test]
    fn fuzzy_matches_subsequence_ignoring_case() {
        assert_eq!(fuzzy_match("np", "New pwsh"), Some(vec![0, 4]));
        assert_eq!(fuzzy_match("", "quit"), Some(vec![]));
        assert_eq!(fuzzy_match("qx", "quit"), None);
        assert_eq!(fuzzy_match("quitt", "quit"), None);
    }

    #[test]
    fn sweep_hides_everything_at_start_and_nothing_at_end() {
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::with_lines(["abcd", "efgh"]);
        sweep(&mut buf, area, 1.0);
        assert_eq!(buf, Buffer::with_lines(["abcd", "efgh"]));
        sweep(&mut buf, area, 0.0);
        assert_eq!(buf, Buffer::with_lines(["    ", "    "]));
    }

    #[test]
    fn spinner_cycles() {
        assert_eq!(spinner(Duration::ZERO), '⠋');
        assert_eq!(spinner(Duration::from_millis(80)), '⠙');
        assert_eq!(spinner(Duration::from_millis(800)), '⠋');
    }
}
