use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Mode, SWITCH_ANIM, TOAST_FADE, TOAST_LIFE, TOAST_SLIDE};
use crate::fx::{format_duration, gradient_border, sparkline, spinner, sweep};
use crate::session::{Session, Status};
use crate::theme::Theme;

/// Rows taken by one session card (two lines + a gap).
pub const CARD_HEIGHT: u16 = 3;

/// Palette rows shown at once.
const PALETTE_ROWS: usize = 8;

/// Splits the screen into (sidebar, main panel, status line).
pub fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let [body, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(26), Constraint::Fill(1)]).areas(body);
    (sidebar, main, status)
}

/// PTY size (rows, cols) that fits inside the bordered main panel.
pub fn pty_size(area: Rect) -> (u16, u16) {
    let (_, main, _) = layout(area);
    (
        main.height.saturating_sub(2).max(1),
        main.width.saturating_sub(2).max(1),
    )
}

/// Where the cards go inside the sidebar: inside the border, one blank row under the title.
pub fn cards_area(sidebar: Rect) -> Rect {
    Rect::new(
        sidebar.x + 1,
        sidebar.y + 2,
        sidebar.width.saturating_sub(2),
        sidebar.height.saturating_sub(3),
    )
}

/// Index of the first card drawn so that the selected one stays visible.
pub fn first_visible(selected: usize, count: usize, height: u16) -> usize {
    let visible = usize::from(height / CARD_HEIGHT).max(1);
    if selected < visible {
        0
    } else {
        (selected + 1 - visible).min(count.saturating_sub(1))
    }
}

pub fn draw(frame: &mut Frame, app: &App, now: Instant) {
    let (sidebar, main, status) = layout(frame.area());
    draw_sidebar(frame, app, sidebar, now);
    draw_main(frame, app, main, now);
    draw_toasts(frame, app, main, now);
    if app.mode == Mode::Palette {
        draw_palette(frame, app);
    }
    draw_status(frame, app, status, now);
}

fn rounded(theme: &Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
}

fn draw_sidebar(frame: &mut Frame, app: &App, sidebar: Rect, now: Instant) {
    let theme = app.theme();
    let title = Span::styled(" ✿ chiika ", Style::new().fg(theme.accents[0]).bold());
    frame.render_widget(rounded(theme).title(title), sidebar);

    let cards = cards_area(sidebar);
    if app.sessions.is_empty() {
        let hint = Line::styled("no session", Style::new().fg(theme.muted));
        frame.render_widget(hint.centered(), cards);
        return;
    }
    let first = first_visible(app.selected, app.sessions.len(), cards.height);
    for (row, (i, session)) in app.sessions.iter().enumerate().skip(first).enumerate() {
        let y = cards.y + row as u16 * CARD_HEIGHT;
        if y + 2 > cards.bottom() {
            break;
        }
        let area = Rect::new(cards.x, y, cards.width, 2);
        let selected = i == app.selected;
        draw_card(frame.buffer_mut(), area, session, selected, app, now);
    }
}

fn draw_card(buf: &mut Buffer, area: Rect, s: &Session, selected: bool, app: &App, now: Instant) {
    let theme = app.theme();
    let accent = theme.accent(s.id);
    if selected {
        buf.set_style(area, Style::new().bg(theme.surface));
    }
    let bar = if selected {
        Span::styled("▌", Style::new().fg(accent))
    } else {
        Span::raw(" ")
    };
    let (glyph, color) = match s.status {
        Status::Running => ("●", accent),
        Status::Exited(Some(0)) => ("✔", theme.ok),
        Status::Exited(_) => ("✖", theme.err),
    };
    let name = Style::new().fg(theme.text);
    let name = if selected { name.bold() } else { name };
    let line = Line::from(vec![
        bar.clone(),
        Span::styled(glyph, Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(s.name.as_str(), name),
    ]);
    buf.set_line(area.x, area.y, &line, area.width.saturating_sub(2));
    if s.is_busy(now) && area.width >= 4 {
        let frame = spinner(now.saturating_duration_since(app.epoch)).to_string();
        buf.set_string(area.right() - 2, area.y, frame, Style::new().fg(accent));
    }

    let info = match s.status {
        Status::Running => format_duration(now.saturating_duration_since(s.started)),
        Status::Exited(Some(code)) => format!("exit {code}"),
        Status::Exited(None) => "killed".into(),
    };
    let spark = if s.status == Status::Running {
        accent
    } else {
        theme.muted
    };
    let line = Line::from(vec![
        bar,
        Span::raw("  "),
        Span::styled(sparkline(&s.activity.values()), Style::new().fg(spark)),
        Span::raw(" "),
        Span::styled(info, Style::new().fg(theme.muted)),
    ]);
    buf.set_line(area.x, area.y + 1, &line, area.width);
}

fn draw_main(frame: &mut Frame, app: &App, main: Rect, now: Instant) {
    let theme = app.theme();
    let Some(session) = app.sessions.get(app.selected) else {
        let muted = Style::new().fg(theme.muted);
        let text = vec![
            Line::raw(""),
            Line::styled("no session", muted),
            Line::styled("press n, or : for the palette", muted),
        ];
        frame.render_widget(Paragraph::new(text).centered().block(rounded(theme)), main);
        return;
    };

    let accent = theme.accent(session.id);
    let parser = session.parser.lock().unwrap();
    let screen = parser.screen();
    let mut title = vec![
        Span::raw(" "),
        Span::styled("◆ ", Style::new().fg(accent)),
        Span::styled(session.name.as_str(), Style::new().fg(theme.text).bold()),
        Span::raw(" "),
    ];
    if screen.scrollback() > 0 {
        let scrolled = format!("↑{} lines ", screen.scrollback());
        title.push(Span::styled(scrolled, Style::new().fg(accent)));
    }
    let info = match session.status {
        Status::Running => format!(
            " ⏱ {} ",
            format_duration(now.saturating_duration_since(session.started))
        ),
        Status::Exited(Some(code)) => format!(" exit {code} "),
        Status::Exited(None) => " killed ".into(),
    };
    let block = rounded(theme)
        .title(Line::from(title))
        .title(Line::styled(info, Style::new().fg(theme.muted)).right_aligned());
    let inner = block.inner(main);
    frame.render_widget(PseudoTerminal::new(screen).block(block), main);
    drop(parser);

    let buf = frame.buffer_mut();
    if app.mode == Mode::Terminal {
        gradient_border(buf, main, theme.gradient.0, theme.gradient.1);
    }
    if let Some(t) = app.switched_at {
        let progress = now.saturating_duration_since(t).as_secs_f32() / SWITCH_ANIM.as_secs_f32();
        sweep(buf, inner, progress);
    }
}

/// Exit notifications, newest on top, sliding in from the right edge of the panel.
fn draw_toasts(frame: &mut Frame, app: &App, main: Rect, now: Instant) {
    let theme = app.theme();
    for (i, toast) in app.toasts.iter().rev().enumerate() {
        let y = main.y + 1 + i as u16 * 3;
        if y + 3 > main.bottom() {
            break;
        }
        let age = now.saturating_duration_since(toast.born);
        let (glyph, color) = if toast.ok {
            ("✔", theme.ok)
        } else {
            ("✖", theme.err)
        };
        let width = (toast.text.chars().count() as u16 + 6).min(main.width.saturating_sub(2));
        let slide = 1.0 - (age.as_secs_f32() / TOAST_SLIDE.as_secs_f32()).min(1.0);
        let x = main.right().saturating_sub(width + 1) + (f32::from(width + 1) * slide) as u16;
        let area = Rect::new(x, y, width, 3).intersection(main);
        if area.is_empty() {
            continue;
        }
        let fading = age > TOAST_LIFE - TOAST_FADE;
        let dim = |style: Style| {
            if fading {
                style.add_modifier(Modifier::DIM)
            } else {
                style
            }
        };
        let text = Line::from(vec![
            Span::styled(format!(" {glyph} "), dim(Style::new().fg(color).bold())),
            Span::styled(toast.text.as_str(), dim(Style::new().fg(theme.text))),
        ]);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(dim(Style::new().fg(color)));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(text).block(block), area);
    }
}

fn draw_palette(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let accent = theme.accents[0];
    let screen = frame.area();
    let items = app.palette_items();
    let rows = items.len().clamp(1, PALETTE_ROWS);
    let width = 52.min(screen.width.saturating_sub(4));
    let height = rows as u16 + 4;
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + screen.height / 5,
        width,
        height,
    )
    .intersection(screen);
    let block = rounded(theme).title(Span::styled(" palette ", Style::new().fg(accent).bold()));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    gradient_border(frame.buffer_mut(), area, theme.gradient.0, theme.gradient.1);
    if inner.height < 3 {
        return;
    }

    let input = Line::from(vec![
        Span::styled("❯ ", Style::new().fg(theme.pill[2]).bold()),
        Span::styled(app.input.as_str(), Style::new().fg(theme.text)),
        Span::styled("▏", Style::new().fg(accent)),
    ]);
    frame.render_widget(input, Rect { height: 1, ..inner });
    let rule = "─".repeat(usize::from(inner.width));
    frame
        .buffer_mut()
        .set_string(inner.x, inner.y + 1, rule, Style::new().fg(theme.border));

    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    if items.is_empty() {
        let none = Line::styled("  no match", Style::new().fg(theme.muted));
        frame.render_widget(none, list);
        return;
    }
    let first = app.picked.saturating_sub(rows - 1);
    for (row, (i, item)) in items.iter().enumerate().skip(first).take(rows).enumerate() {
        let line_area = Rect::new(list.x, list.y + row as u16, list.width, 1);
        let selected = i == app.picked;
        if selected {
            frame
                .buffer_mut()
                .set_style(line_area, Style::new().bg(theme.surface));
        }
        let marker = if selected { "▌ " } else { "  " };
        let mut spans = vec![Span::styled(marker, Style::new().fg(accent))];
        spans.extend(item.label.chars().enumerate().map(|(ci, c)| {
            let style = if item.hits.contains(&ci) {
                Style::new().fg(accent).bold()
            } else {
                Style::new().fg(theme.text)
            };
            Span::styled(c.to_string(), style)
        }));
        frame.render_widget(Line::from(spans), line_area);
    }
}

fn draw_status(frame: &mut Frame, app: &App, status: Rect, now: Instant) {
    let theme = app.theme();
    let (label, color) = match app.mode {
        Mode::Workspace => ("WORKSPACE", theme.pill[0]),
        Mode::Terminal => ("TERMINAL", theme.pill[1]),
        Mode::Rename => ("RENAME", theme.pill[2]),
        Mode::Palette => ("PALETTE", theme.pill[2]),
    };
    let mut left = vec![
        Span::styled("▐", Style::new().fg(color)),
        Span::styled(
            format!(" {label} "),
            Style::new().fg(theme.on_pill).bg(color).bold(),
        ),
        Span::styled("▌ ", Style::new().fg(color)),
    ];
    match (&app.status_msg, app.sessions.get(app.selected)) {
        _ if app.mode == Mode::Rename => left.extend([
            Span::styled("❯ ", Style::new().fg(color)),
            Span::styled(app.input.as_str(), Style::new().fg(theme.text)),
            Span::styled("▏", Style::new().fg(color)),
        ]),
        (Some(msg), _) => left.push(Span::styled(msg.as_str(), Style::new().fg(theme.err))),
        (None, Some(s)) => {
            let uptime = format_duration(now.saturating_duration_since(s.started));
            let count = match app.sessions.len() {
                1 => "1 session".to_string(),
                n => format!("{n} sessions"),
            };
            left.extend([
                Span::styled("◆ ", Style::new().fg(theme.accent(s.id))),
                Span::styled(s.name.as_str(), Style::new().fg(theme.text)),
                Span::styled(
                    format!(" · {uptime}  │ {count}"),
                    Style::new().fg(theme.muted),
                ),
            ]);
        }
        (None, None) => {}
    }
    let left = Line::from(left);
    let hints = match app.mode {
        Mode::Workspace => "↵ focus · n new · : palette · x close · q quit ",
        Mode::Terminal => "Ctrl+Q workspace · Shift+PgUp scroll ",
        Mode::Rename => "↵ save · Esc cancel ",
        Mode::Palette => "↑↓ choose · ↵ run · Esc close ",
    };
    let fits = left.width() + hints.chars().count() + 2 <= usize::from(status.width);
    frame.render_widget(left, status);
    if fits {
        let hints = Line::styled(hints, Style::new().fg(theme.muted)).right_aligned();
        frame.render_widget(hints, status);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn draws_every_mode_at_any_size_without_panicking() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(Rect::new(0, 0, 120, 40), tx);
        let now = Instant::now();
        for mode in [Mode::Workspace, Mode::Terminal, Mode::Rename, Mode::Palette] {
            app.mode = mode;
            for (w, h) in [(120, 40), (40, 10), (27, 4), (5, 2), (1, 1)] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal.draw(|frame| draw(frame, &app, now)).unwrap();
            }
        }
        app.kill_all();
    }

    #[test]
    fn starts_typing_in_the_shell_and_shows_how_to_leave() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(Rect::new(0, 0, 120, 40), tx);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, Instant::now()))
            .unwrap();
        let status = row_text(&terminal, 39);
        assert_eq!(app.mode, Mode::Terminal);
        assert!(status.contains("TERMINAL"), "{status}");
        assert!(status.contains("Ctrl+Q workspace"), "{status}");
        app.kill_all();
    }

    #[test]
    fn first_visible_keeps_selection_on_screen() {
        // 9 rows = 3 cards.
        assert_eq!(first_visible(0, 2, 9), 0);
        assert_eq!(first_visible(2, 10, 9), 0);
        assert_eq!(first_visible(3, 10, 9), 1);
        assert_eq!(first_visible(9, 10, 9), 7);
        assert_eq!(first_visible(5, 10, 0), 5);
    }
}
