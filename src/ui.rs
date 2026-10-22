use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Folder, Menu, Mode, Row, SWITCH_ANIM, TOAST_FADE, TOAST_LIFE, TOAST_SLIDE};
use crate::fx::{format_duration, gradient_border, sparkline, spinner, sweep};
use crate::session::{Session, Status};
use crate::theme::Theme;

/// Rows taken by one session card (two lines + a gap).
pub const CARD_HEIGHT: u16 = 3;

/// Rows taken by a folder header (one line + a gap).
const FOLDER_HEIGHT: u16 = 2;

/// Rows taken by any row of the collapsed sidebar (one icon + a gap).
const RAIL_ROW_HEIGHT: u16 = 2;

/// Width of the sidebar, open and collapsed to a rail of icons.
pub const SIDEBAR_WIDTH: u16 = 26;
pub const RAIL_WIDTH: u16 = 4;

/// Palette rows shown at once.
const PALETTE_ROWS: usize = 8;

/// Every key binding, by section: shown by the bindings window and `--help`.
pub const BINDINGS: &[(&str, &[(&str, &str)])] = &[
    (
        "Workspace",
        &[
            ("Enter", "focus the shell / open or close a folder"),
            ("j k ↑ ↓", "select"),
            ("n / N", "new shell / pick the shell"),
            ("|", "split vertical (side by side)"),
            ("-", "split horizontal (stacked)"),
            ("w", "unsplit (close the pane)"),
            ("r  F2", "rename"),
            ("m", "move to a folder"),
            ("x", "close the session / delete the folder"),
            ("b", "collapse or expand the sidebar"),
            (":  Ctrl+P", "command palette"),
            ("PgUp PgDn", "scroll the history"),
            ("?", "this list"),
            ("q", "quit"),
        ],
    ),
    (
        "Terminal",
        &[
            ("Ctrl+Q", "back to the workspace"),
            ("Shift+PgUp PgDn", "scroll the history"),
        ],
    ),
    (
        "Anywhere",
        &[
            ("Alt+arrows", "switch pane"),
            ("mouse wheel", "scroll the pane under the mouse"),
            ("click", "select / focus"),
            ("double-click", "rename a session"),
            ("right-click", "menu of a session, folder or pane"),
            ("≡ menu", "settings, bindings, detach"),
        ],
    ),
];

/// Splits the screen into (sidebar, main area, status line).
pub fn layout(area: Rect, sidebar_width: u16) -> (Rect, Rect, Rect) {
    let [body, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Fill(1)]).areas(body);
    (sidebar, main, status)
}

/// Terminal size (rows, cols) inside a bordered pane.
pub fn inner_size(pane: Rect) -> (u16, u16) {
    (
        pane.height.saturating_sub(2).max(1),
        pane.width.saturating_sub(2).max(1),
    )
}

/// Where the rows go inside the sidebar: inside the border, one blank row under the title.
pub fn cards_area(sidebar: Rect) -> Rect {
    Rect::new(
        sidebar.x + 1,
        sidebar.y + 2,
        sidebar.width.saturating_sub(2),
        sidebar.height.saturating_sub(5),
    )
}

/// The "≡ menu" button on the last row inside the sidebar; empty if the sidebar is too short.
pub fn menu_button(sidebar: Rect) -> Rect {
    if sidebar.height < 4 {
        return Rect::default();
    }
    Rect::new(
        sidebar.x + 1,
        sidebar.bottom() - 2,
        sidebar.width.saturating_sub(2),
        1,
    )
}

/// Height of a sidebar row; `compact` = collapsed sidebar.
pub fn row_height(row: Row, compact: bool) -> u16 {
    match row {
        _ if compact => RAIL_ROW_HEIGHT,
        Row::Folder(_) => FOLDER_HEIGHT,
        Row::Session(_) => CARD_HEIGHT,
    }
}

/// Index of the first row drawn so that the cursor row stays visible.
/// The trailing gap of the last row may fall outside the area.
pub fn first_visible(heights: &[u16], cursor: usize, height: u16) -> usize {
    let Some(last) = heights.len().checked_sub(1) else {
        return 0;
    };
    let cursor = cursor.min(last);
    let needed = |first: usize| {
        heights[first..=cursor]
            .iter()
            .map(|&h| u32::from(h))
            .sum::<u32>()
    };
    let mut first = 0;
    while first < cursor && needed(first) > u32::from(height) + 1 {
        first += 1;
    }
    first
}

/// Visible sidebar rows (by index) with the area of their content (gap excluded).
pub fn visible_rows(rows: &[Row], cursor: usize, area: Rect, compact: bool) -> Vec<(usize, Rect)> {
    let heights: Vec<u16> = rows.iter().map(|&r| row_height(r, compact)).collect();
    let mut y = area.y;
    let mut out = Vec::new();
    for (i, &h) in heights
        .iter()
        .enumerate()
        .skip(first_visible(&heights, cursor, area.height))
    {
        if y + h - 1 > area.bottom() {
            break;
        }
        out.push((i, Rect::new(area.x, y, area.width, h - 1)));
        y += h;
    }
    out
}

/// Screen area of a context menu: at the requested corner, moved to fit on screen.
pub fn menu_rect(menu: &Menu, screen: Rect) -> Rect {
    let longest = menu
        .items
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let width = longest as u16 + 6;
    let height = menu.items.len() as u16 + 2;
    let x = menu.at.x.min(screen.right().saturating_sub(width));
    let y = menu.at.y.min(screen.bottom().saturating_sub(height));
    Rect::new(x, y, width, height).intersection(screen)
}

pub fn draw(frame: &mut Frame, app: &App, now: Instant) {
    let (sidebar, main, status) = layout(frame.area(), app.sidebar_width());
    draw_sidebar(frame, app, sidebar, now);
    draw_panes(frame, app, main, now);
    draw_toasts(frame, app, main, now);
    match app.mode {
        Mode::Palette => draw_palette(frame, app),
        Mode::Menu => draw_menu(frame, app),
        Mode::Help => draw_bindings(frame, app),
        _ => {}
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
    let compact = app.sidebar_collapsed;
    let title = if compact { "✿" } else { " ✿ chiika ◂ " };
    let title = Span::styled(title, Style::new().fg(theme.accents[0]).bold());
    frame.render_widget(rounded(theme).title(title), sidebar);
    let button = menu_button(sidebar);
    let label = if compact { "≡" } else { "≡ menu" };
    let label = Line::from(vec![
        Span::raw(" "),
        Span::styled(label, Style::new().fg(theme.accents[0]).bold()),
    ]);
    frame
        .buffer_mut()
        .set_line(button.x, button.y, &label, button.width);

    let area = cards_area(sidebar);
    let rows = app.rows();
    if rows.is_empty() && !compact {
        let hint = Line::styled("no session", Style::new().fg(theme.muted));
        frame.render_widget(hint.centered(), area);
        return;
    }
    for (i, rect) in visible_rows(&rows, app.cursor, area, compact) {
        let selected = i == app.cursor;
        let buf = frame.buffer_mut();
        if compact {
            draw_rail_icon(buf, rect, rows[i], selected, app, now);
            continue;
        }
        match rows[i] {
            Row::Folder(id) => {
                if let Some(folder) = app.folder(id) {
                    draw_folder(buf, rect, folder, selected, app);
                }
            }
            Row::Session(id) => {
                if let Some(session) = app.session(id) {
                    draw_card(buf, rect, session, selected, app, now);
                }
            }
        }
    }
}

/// One row of the collapsed sidebar: the folder arrow or the session status icon.
fn draw_rail_icon(buf: &mut Buffer, area: Rect, row: Row, selected: bool, app: &App, now: Instant) {
    let theme = app.theme();
    if selected {
        buf.set_style(area, Style::new().bg(theme.surface));
    }
    let (accent, icon, color) = match row {
        Row::Folder(id) => {
            let collapsed = app.folder(id).is_some_and(|f| f.collapsed);
            let arrow = if collapsed { "▸" } else { "▾" };
            (theme.accents[0], arrow.to_string(), theme.accents[0])
        }
        Row::Session(id) => {
            let Some(s) = app.session(id) else {
                return;
            };
            let accent = theme.accent(s.id);
            match s.status {
                _ if s.is_busy(now) => (
                    accent,
                    spinner(now.saturating_duration_since(app.epoch)).to_string(),
                    accent,
                ),
                Status::Running => (accent, "●".into(), accent),
                Status::Exited(Some(0)) => (accent, "✔".into(), theme.ok),
                Status::Exited(_) => (accent, "✖".into(), theme.err),
            }
        }
    };
    let line = Line::from(vec![
        selection_bar(selected, accent),
        Span::styled(icon, Style::new().fg(color)),
    ]);
    buf.set_line(area.x, area.y, &line, area.width);
}

fn selection_bar(selected: bool, color: ratatui::style::Color) -> Span<'static> {
    if selected {
        Span::styled("▌", Style::new().fg(color))
    } else {
        Span::raw(" ")
    }
}

fn draw_folder(buf: &mut Buffer, area: Rect, folder: &Folder, selected: bool, app: &App) {
    let theme = app.theme();
    if selected {
        buf.set_style(area, Style::new().bg(theme.surface));
    }
    let arrow = if folder.collapsed { "▸ " } else { "▾ " };
    let line = Line::from(vec![
        selection_bar(selected, theme.accents[0]),
        Span::styled(arrow, Style::new().fg(theme.accents[0])),
        Span::styled(folder.name.as_str(), Style::new().fg(theme.text).bold()),
    ]);
    buf.set_line(area.x, area.y, &line, area.width.saturating_sub(3));
    let count = app
        .sessions
        .iter()
        .filter(|s| s.folder == Some(folder.id))
        .count()
        .to_string();
    let x = area.right().saturating_sub(count.len() as u16 + 1);
    buf.set_string(x, area.y, count, Style::new().fg(theme.muted));
}

fn draw_card(buf: &mut Buffer, area: Rect, s: &Session, selected: bool, app: &App, now: Instant) {
    let theme = app.theme();
    let accent = theme.accent(s.id);
    if selected {
        buf.set_style(area, Style::new().bg(theme.surface));
    }
    let indent = if s.folder.is_some() { "  " } else { "" };
    let (glyph, color) = match s.status {
        Status::Running => ("●", accent),
        Status::Exited(Some(0)) => ("✔", theme.ok),
        Status::Exited(_) => ("✖", theme.err),
    };
    let name = Style::new().fg(theme.text);
    let name = if selected { name.bold() } else { name };
    let line = Line::from(vec![
        selection_bar(selected, accent),
        Span::raw(indent),
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
        selection_bar(selected, accent),
        Span::raw(indent),
        Span::raw("  "),
        Span::styled(sparkline(&s.activity.values()), Style::new().fg(spark)),
        Span::raw(" "),
        Span::styled(info, Style::new().fg(theme.muted)),
    ]);
    if area.height > 1 {
        buf.set_line(area.x, area.y + 1, &line, area.width);
    }
}

fn draw_panes(frame: &mut Frame, app: &App, main: Rect, now: Instant) {
    let theme = app.theme();
    let Some(layout) = &app.layout else {
        let muted = Style::new().fg(theme.muted);
        let text = vec![
            Line::raw(""),
            Line::styled("no session", muted),
            Line::styled("press n, or : for the palette", muted),
        ];
        frame.render_widget(Paragraph::new(text).centered().block(rounded(theme)), main);
        return;
    };
    let panes = layout.rects(main);
    let several = panes.len() > 1;
    for (id, rect) in panes {
        if let Some(session) = app.session(id) {
            let focused = app.focused == Some(id);
            draw_pane(frame, app, session, rect, focused, several, now);
        }
    }
}

fn draw_pane(
    frame: &mut Frame,
    app: &App,
    session: &Session,
    area: Rect,
    focused: bool,
    several: bool,
    now: Instant,
) {
    let theme = app.theme();
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
    let border = if focused && app.mode != Mode::Terminal {
        accent
    } else {
        theme.border
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Line::from(title))
        .title(Line::styled(info, Style::new().fg(theme.muted)).right_aligned());
    let inner = block.inner(area);
    let mut widget = PseudoTerminal::new(screen).block(block);
    if !focused {
        // Only the focused pane shows a cursor.
        widget = widget.cursor(tui_term::widget::Cursor::default().visibility(false));
    }
    frame.render_widget(widget, area);
    drop(parser);

    let buf = frame.buffer_mut();
    if focused && app.mode == Mode::Terminal {
        gradient_border(buf, area, theme.gradient.0, theme.gradient.1);
    }
    if several && !focused {
        buf.set_style(inner, Style::new().add_modifier(Modifier::DIM));
    }
    if focused && let Some(t) = app.switched_at {
        let progress = now.saturating_duration_since(t).as_secs_f32() / SWITCH_ANIM.as_secs_f32();
        sweep(buf, inner, progress);
    }
}

/// Exit notifications, newest on top, sliding in from the right edge of the main area.
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

fn draw_menu(frame: &mut Frame, app: &App) {
    let Some(menu) = &app.menu else {
        return;
    };
    let theme = app.theme();
    let accent = theme.accents[0];
    let area = menu_rect(menu, frame.area());
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(accent));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    for (i, (label, action)) in menu.items.iter().enumerate() {
        let Some(y) = inner
            .y
            .checked_add(i as u16)
            .filter(|&y| y < inner.bottom())
        else {
            break;
        };
        let line_area = Rect::new(inner.x, y, inner.width, 1);
        let selected = i == menu.picked;
        if selected {
            frame
                .buffer_mut()
                .set_style(line_area, Style::new().bg(theme.surface));
        }
        let color = if !action.enabled() {
            theme.muted
        } else if action.destructive() {
            theme.err
        } else {
            theme.text
        };
        let line = Line::from(vec![
            selection_bar(selected, accent),
            Span::raw(" "),
            Span::styled(label.as_str(), Style::new().fg(color)),
        ]);
        frame.render_widget(line, line_area);
    }
}

/// Centered window listing `BINDINGS`.
fn draw_bindings(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let accent = theme.accents[0];
    let mut lines = Vec::new();
    for (i, (section, keys)) in BINDINGS.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(*section, Style::new().fg(accent).bold()));
        for (keys, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<18}"), Style::new().fg(theme.pill[2])),
                Span::styled(*what, Style::new().fg(theme.text)),
            ]));
        }
    }
    let screen = frame.area();
    let width = 64.min(screen.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(screen.height.saturating_sub(2));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let title = Span::styled(" bindings ", Style::new().fg(accent).bold());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(rounded(theme).title(title)),
        area,
    );
    gradient_border(frame.buffer_mut(), area, theme.gradient.0, theme.gradient.1);
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
        Mode::Menu => ("MENU", theme.pill[2]),
        Mode::Help => ("BINDINGS", theme.pill[2]),
    };
    let mut left = vec![
        Span::styled("▐", Style::new().fg(color)),
        Span::styled(
            format!(" {label} "),
            Style::new().fg(theme.on_pill).bg(color).bold(),
        ),
        Span::styled("▌ ", Style::new().fg(color)),
    ];
    let focused = app.focused.and_then(|id| app.session(id));
    match (&app.status_msg, focused) {
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
        Mode::Workspace => "| - split · w unsplit · b sidebar · : palette ",
        Mode::Terminal => "Ctrl+Q workspace · Alt+←→↑↓ panes · Shift+PgUp scroll ",
        Mode::Rename => "↵ save · Esc cancel ",
        Mode::Palette => "↑↓ choose · ↵ run · Esc close ",
        Mode::Menu => "↑↓ choose · ↵ run · Esc close ",
        Mode::Help => "Esc close ",
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
    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::*;
    use crate::app::AppEvent;

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        app.handle(AppEvent::Input(Event::Key(key)));
    }

    fn right_click(app: &mut App, column: u16, row: u16) {
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.handle(AppEvent::Input(Event::Mouse(mouse)));
    }

    #[test]
    fn first_visible_keeps_cursor_on_screen() {
        // Three cards fit in 8 rows (the last gap may overflow).
        let cards = [3, 3, 3, 3, 3];
        assert_eq!(first_visible(&cards, 0, 8), 0);
        assert_eq!(first_visible(&cards, 2, 8), 0);
        assert_eq!(first_visible(&cards, 3, 8), 1);
        assert_eq!(first_visible(&cards, 9, 8), 2);
        assert_eq!(first_visible(&[2, 3, 3], 2, 5), 1);
        assert_eq!(first_visible(&[], 3, 8), 0);
        assert_eq!(first_visible(&cards, 4, 0), 4);
    }

    #[test]
    fn visible_rows_match_their_heights() {
        let rows = [Row::Folder(1), Row::Session(1), Row::Session(2)];
        assert_eq!(
            visible_rows(&rows, 0, Rect::new(1, 2, 20, 8), false),
            [
                (0, Rect::new(1, 2, 20, 1)),
                (1, Rect::new(1, 4, 20, 2)),
                (2, Rect::new(1, 7, 20, 2)),
            ]
        );
    }

    #[test]
    fn menu_button_sits_below_the_rows() {
        let sidebar = Rect::new(0, 0, 26, 20);
        let button = menu_button(sidebar);
        assert_eq!(button, Rect::new(1, 18, 24, 1));
        assert!(cards_area(sidebar).bottom() < button.y);
        assert!(menu_button(Rect::new(0, 0, 26, 3)).is_empty());
    }

    #[test]
    fn collapsed_rows_are_one_icon_each() {
        let rows = [Row::Folder(1), Row::Session(1), Row::Session(2)];
        assert_eq!(
            visible_rows(&rows, 0, Rect::new(1, 2, 2, 8), true),
            [
                (0, Rect::new(1, 2, 2, 1)),
                (1, Rect::new(1, 4, 2, 1)),
                (2, Rect::new(1, 6, 2, 1)),
            ]
        );
    }

    #[test]
    fn draws_splits_folders_and_menus_at_any_size_without_panicking() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(Rect::new(0, 0, 120, 40), tx);
        app.mode = Mode::Workspace;
        press(&mut app, KeyCode::Char('|'));
        press(&mut app, KeyCode::Char('-'));
        app.handle(AppEvent::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::NONE,
        ))));
        // "+ new folder" is the last entry of the move menu.
        let last = app.menu.as_ref().map_or(0, |m| m.items.len() - 1);
        for _ in 0..last {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.layout.as_ref().map(|l| l.panes().len()), Some(3));
        assert_eq!(app.folders.len(), 1);
        right_click(&mut app, 2, 2);
        assert_eq!(app.mode, Mode::Menu);

        let now = Instant::now();
        for (mode, collapsed) in [
            Mode::Workspace,
            Mode::Terminal,
            Mode::Rename,
            Mode::Palette,
            Mode::Menu,
            Mode::Help,
        ]
        .into_iter()
        .flat_map(|mode| [(mode, false), (mode, true)])
        {
            app.sidebar_collapsed = collapsed;
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
}
