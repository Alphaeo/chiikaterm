//! Canvas view: every shell is a mascot, arranged with the mouse in team boxes.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear};

use crate::agent::{AgentInfo, AgentState};
use crate::app::{App, Mode, Row, Target, View, truncate};
use crate::fx::{ease_out_back, lerp};
use crate::mascot::{self, Palette};
use crate::session::Session;

/// Size of one mascot slot: sprite (4 rows) + name + status, and a gap on the right.
pub const SLOT_W: i32 = 12;
pub const SLOT_H: i32 = 6;
/// Mascots per row in a team box.
const COLS: i32 = 3;
/// Height of the "no team" tray at the bottom.
const TRAY_H: u16 = 7;
/// Spacing of the default team positions.
const TEAM_STEP: (i32, i32) = (44, 18);
/// A dropped mascot flies to its place for this long.
pub const FLIGHT: Duration = Duration::from_millis(280);
/// A message between agents is drawn for this long.
pub const PULSE: Duration = Duration::from_millis(1200);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A rectangle in screen cells that may stick out of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub fn contains(&self, p: Position) -> bool {
        let (px, py) = (i32::from(p.x), i32::from(p.y));
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// The part visible inside `clip`.
    pub fn clip(&self, clip: Rect) -> Rect {
        let x0 = self.x.max(i32::from(clip.x));
        let y0 = self.y.max(i32::from(clip.y));
        let x1 = (self.x + self.w).min(i32::from(clip.right()));
        let y1 = (self.y + self.h).min(i32::from(clip.bottom()));
        if x1 <= x0 || y1 <= y0 {
            return Rect::default();
        }
        Rect::new(x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16)
    }
}

/// Where a dragged mascot would land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Team(u32),
    Tray,
}

/// A team as the layout needs it: canvas position and members in order.
pub struct TeamInput {
    pub id: u32,
    pub pos: (i32, i32),
    pub members: Vec<u32>,
    /// Show a `+ agent` slot after the members.
    pub add_slot: bool,
}

pub struct TeamBox {
    pub id: u32,
    pub rect: IRect,
    /// Session id and top-left corner of its slot.
    pub slots: Vec<(u32, (i32, i32))>,
    /// Free slot shown while a mascot hovers the box.
    pub placeholder: Option<(i32, i32)>,
    /// The `+ agent` slot.
    pub add_slot: Option<(i32, i32)>,
}

pub struct CanvasLayout {
    /// Area of the team boxes (above the tray).
    pub board: Rect,
    pub boxes: Vec<TeamBox>,
    pub tray: Rect,
    pub tray_slots: Vec<(u32, (i32, i32))>,
    pub tray_placeholder: Option<(i32, i32)>,
    pub add_button: Rect,
}

/// What is under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    AddButton,
    /// The `+ agent` slot of a team.
    AddAgent(u32),
    Mascot(u32),
    TeamTitle(u32),
    Team(u32),
    Tray,
    Board,
}

/// Places everything; `placeholder` adds a free slot where a dragged mascot hovers.
pub fn layout(
    area: Rect,
    pan: (i32, i32),
    teams: &[TeamInput],
    loose: &[u32],
    placeholder: Option<Zone>,
) -> CanvasLayout {
    let tray = if area.height > TRAY_H + 4 {
        Rect::new(area.x, area.bottom() - TRAY_H, area.width, TRAY_H)
    } else {
        Rect::default()
    };
    let board = Rect {
        height: area.height - tray.height,
        ..area
    };
    let boxes = teams
        .iter()
        .map(|team| {
            let extra = placeholder == Some(Zone::Team(team.id));
            let used = team.members.len() as i32 + i32::from(extra);
            let count = used + i32::from(team.add_slot);
            let cols = count.clamp(2, COLS);
            let rows = ((count + COLS - 1) / COLS).max(1);
            let x = i32::from(area.x) + team.pos.0 - pan.0;
            let y = i32::from(area.y) + team.pos.1 - pan.1;
            let slot = |i: i32| (x + 2 + (i % COLS) * SLOT_W, y + 3 + (i / COLS) * SLOT_H);
            TeamBox {
                id: team.id,
                rect: IRect {
                    x,
                    y,
                    w: cols * SLOT_W + 3,
                    h: 4 + rows * SLOT_H,
                },
                slots: (0..)
                    .zip(&team.members)
                    .map(|(i, &id)| (id, slot(i)))
                    .collect(),
                placeholder: extra.then(|| slot(team.members.len() as i32)),
                add_slot: team.add_slot.then(|| slot(used)),
            }
        })
        .collect();
    let tray_slot = |i: usize| {
        (
            i32::from(tray.x) + 2 + i as i32 * SLOT_W,
            i32::from(tray.y) + 1,
        )
    };
    CanvasLayout {
        board,
        boxes,
        tray,
        tray_slots: loose
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, tray_slot(i)))
            .collect(),
        tray_placeholder: (placeholder == Some(Zone::Tray) && !tray.is_empty())
            .then(|| tray_slot(loose.len())),
        add_button: Rect::new(
            area.right().saturating_sub(12).max(area.x),
            area.y,
            11.min(area.width),
            1.min(area.height),
        ),
    }
}

/// Default position of the `index`-th team: two columns, top to bottom.
pub fn team_position(index: usize) -> (i32, i32) {
    let i = index as i32;
    (2 + (i % 2) * TEAM_STEP.0, 1 + (i / 2) * TEAM_STEP.1)
}

/// The cells of a slot that react to the mouse (sprite, name and status).
fn slot_rect((x, y): (i32, i32)) -> IRect {
    IRect {
        x,
        y,
        w: mascot::WIDTH as i32,
        h: SLOT_H,
    }
}

impl CanvasLayout {
    pub fn hit(&self, pos: Position) -> Hit {
        if self.add_button.contains(pos) {
            return Hit::AddButton;
        }
        let mascot_at =
            |&(id, slot): &(u32, (i32, i32))| slot_rect(slot).contains(pos).then_some(id);
        if self.tray.contains(pos) {
            return self
                .tray_slots
                .iter()
                .find_map(mascot_at)
                .map_or(Hit::Tray, Hit::Mascot);
        }
        // Boxes drawn last are on top.
        for team in self.boxes.iter().rev() {
            if !team.rect.contains(pos) || !self.board.contains(pos) {
                continue;
            }
            if let Some(id) = team.slots.iter().find_map(mascot_at) {
                return Hit::Mascot(id);
            }
            if team
                .add_slot
                .is_some_and(|slot| slot_rect(slot).contains(pos))
            {
                return Hit::AddAgent(team.id);
            }
            if i32::from(pos.y) <= team.rect.y + 1 {
                return Hit::TeamTitle(team.id);
            }
            return Hit::Team(team.id);
        }
        Hit::Board
    }

    /// Where a mascot dropped at `pos` goes.
    pub fn zone_at(&self, pos: Position) -> Option<Zone> {
        if self.tray.contains(pos) {
            return Some(Zone::Tray);
        }
        self.boxes
            .iter()
            .rev()
            .find(|team| team.rect.contains(pos) && self.board.contains(pos))
            .map(|team| Zone::Team(team.id))
    }

    pub fn slot_of(&self, id: u32) -> Option<(i32, i32)> {
        self.boxes
            .iter()
            .flat_map(|team| &team.slots)
            .chain(&self.tray_slots)
            .find(|(slot_id, _)| *slot_id == id)
            .map(|&(_, slot)| slot)
    }
}

/// What the left button holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// A mascot; it becomes a drag once the pointer moves. `offset` = slot corner - pointer.
    Session {
        id: u32,
        offset: (i32, i32),
        from: Position,
        dragging: bool,
    },
    /// A team box by its title; `origin` = its canvas position when grabbed.
    Team {
        id: u32,
        origin: (i32, i32),
        from: Position,
    },
    /// The background: moves the view.
    Pan { origin: (i32, i32), from: Position },
}

/// A mascot flying to its slot after a drop.
pub struct Flight {
    pub session: u32,
    pub from: (i32, i32),
    pub started: Instant,
}

#[derive(Default)]
pub struct CanvasState {
    /// Canvas position shown at the top-left of the view.
    pub pan: (i32, i32),
    pub grab: Option<Grab>,
    pub pointer: Position,
    pub flights: Vec<Flight>,
    last_click: Option<(Instant, u32)>,
}

impl CanvasState {
    pub fn dragged_session(&self) -> Option<u32> {
        match self.grab {
            Some(Grab::Session {
                id, dragging: true, ..
            }) => Some(id),
            _ => None,
        }
    }

    /// Something is moving: the clock must run at full speed.
    pub fn animating(&self, now: Instant) -> bool {
        self.dragged_session().is_some()
            || self
                .flights
                .iter()
                .any(|f| now.saturating_duration_since(f.started) < FLIGHT)
    }
}

fn delta(from: Position, to: Position) -> (i32, i32) {
    (
        i32::from(to.x) - i32::from(from.x),
        i32::from(to.y) - i32::from(from.y),
    )
}

impl App {
    /// The canvas geometry for mouse handling, with a free slot under a dragged mascot.
    pub(crate) fn canvas_layout(&self) -> CanvasLayout {
        self.canvas_layout_in(self.main_area())
    }

    /// The canvas geometry in `area`. Drawing passes the area of the frame being drawn: after a
    /// resize it can be smaller than the last size the app was told about.
    pub(crate) fn canvas_layout_in(&self, area: Rect) -> CanvasLayout {
        let dragged = self.canvas.dragged_session();
        let members = |folder: Option<u32>| -> Vec<u32> {
            self.sessions
                .iter()
                .filter(|s| s.folder == folder && Some(s.id) != dragged && !s.ephemeral)
                .map(|s| s.id)
                .collect()
        };
        let teams: Vec<TeamInput> = self
            .folders
            .iter()
            .map(|f| TeamInput {
                id: f.id,
                pos: f.pos,
                members: members(Some(f.id)),
                add_slot: true,
            })
            .collect();
        let loose = members(None);
        let plain = layout(area, self.canvas.pan, &teams, &loose, None);
        if dragged.is_none() {
            return plain;
        }
        let zone = plain.zone_at(self.canvas.pointer);
        layout(area, self.canvas.pan, &teams, &loose, zone)
    }

    pub(crate) fn canvas_press(&mut self, pos: Position) {
        let layout = self.canvas_layout();
        self.canvas.pointer = pos;
        if let Some(id) = self.inspected()
            && let Some(inspector) = inspector_layout(self.main_area())
            && inspector.panel.contains(pos)
        {
            let can_decide = self
                .session(id)
                .and_then(|s| s.agent.as_ref())
                .is_some_and(|a| a.can_decide(Instant::now()));
            if can_decide && inspector.allow.contains(pos) {
                self.decide(id, Some(true));
            } else if can_decide && inspector.deny.contains(pos) {
                self.decide(id, Some(false));
            } else if can_decide && inspector.in_terminal.contains(pos) {
                self.decide(id, None);
                self.agent_open(id);
            } else if inspector.open.contains(pos) {
                self.agent_open(id);
            } else if inspector.interrupt.contains(pos) {
                self.agent_interrupt(id);
            } else if inspector.resend.contains(pos) {
                self.agent_resend(id);
            }
            return;
        }
        match layout.hit(pos) {
            Hit::AddButton => {
                let id = self.new_folder();
                self.start_rename(Target::Folder(id));
            }
            Hit::AddAgent(team) => {
                self.select_row(Row::Folder(team));
                self.open_menu(self.role_menu(team), pos);
            }
            Hit::Mascot(id) => {
                let (x, y) = layout
                    .slot_of(id)
                    .unwrap_or((i32::from(pos.x), i32::from(pos.y)));
                self.canvas.grab = Some(Grab::Session {
                    id,
                    offset: (x - i32::from(pos.x), y - i32::from(pos.y)),
                    from: pos,
                    dragging: false,
                });
            }
            Hit::TeamTitle(id) => {
                let origin = self.folder(id).map_or((0, 0), |f| f.pos);
                self.canvas.grab = Some(Grab::Team {
                    id,
                    origin,
                    from: pos,
                });
                self.select_row(Row::Folder(id));
            }
            Hit::Team(id) => self.select_row(Row::Folder(id)),
            Hit::Tray | Hit::Board => {
                self.canvas.grab = Some(Grab::Pan {
                    origin: self.canvas.pan,
                    from: pos,
                });
            }
        }
    }

    pub(crate) fn canvas_drag(&mut self, pos: Position) {
        self.canvas.pointer = pos;
        match &mut self.canvas.grab {
            Some(Grab::Session { from, dragging, .. }) => *dragging |= pos != *from,
            Some(Grab::Team { id, origin, from }) => {
                let (dx, dy) = delta(*from, pos);
                let (id, origin) = (*id, *origin);
                if let Some(folder) = self.folders.iter_mut().find(|f| f.id == id) {
                    folder.pos = (origin.0 + dx, (origin.1 + dy).max(0));
                }
            }
            Some(Grab::Pan { origin, from }) => {
                let (dx, dy) = delta(*from, pos);
                self.canvas.pan = (origin.0 - dx, origin.1 - dy);
            }
            None => {}
        }
    }

    pub(crate) fn canvas_release(&mut self, pos: Position) {
        self.canvas.pointer = pos;
        let Some(grab) = self.canvas.grab else {
            return;
        };
        // Computed while the grab is still on, so the drop zone matches what was shown.
        let layout = self.canvas_layout();
        self.canvas.grab = None;
        match grab {
            Grab::Session {
                id,
                dragging: false,
                ..
            } => self.canvas_click(id),
            Grab::Session { id, offset, .. } => {
                match layout.zone_at(pos) {
                    Some(Zone::Team(team)) => self.move_to(id, Some(team)),
                    Some(Zone::Tray) => self.move_to(id, None),
                    // Dropped nowhere: flies back home.
                    None => {}
                }
                self.fly(
                    id,
                    (i32::from(pos.x) + offset.0, i32::from(pos.y) + offset.1),
                );
            }
            Grab::Team { .. } => self.save_canvas(),
            Grab::Pan { .. } => {}
        }
    }

    /// `Esc` during a drag: the mascot flies back. Returns whether something was held.
    pub(crate) fn canvas_cancel(&mut self) -> bool {
        if let Some(Grab::Session {
            id,
            offset,
            dragging: true,
            ..
        }) = self.canvas.grab
        {
            let pointer = self.canvas.pointer;
            self.fly(
                id,
                (
                    i32::from(pointer.x) + offset.0,
                    i32::from(pointer.y) + offset.1,
                ),
            );
        }
        self.canvas.grab.take().is_some()
    }

    fn fly(&mut self, session: u32, from: (i32, i32)) {
        self.canvas.flights.retain(|f| f.session != session);
        self.canvas.flights.push(Flight {
            session,
            from,
            started: Instant::now(),
        });
    }

    /// Click selects; double-click opens the shell in the terminals view.
    fn canvas_click(&mut self, id: u32) {
        let now = Instant::now();
        let double = self
            .canvas
            .last_click
            .is_some_and(|(t, last)| last == id && now.saturating_duration_since(t) < DOUBLE_CLICK);
        self.canvas.last_click = Some((now, id));
        if double {
            self.view = View::Terminals;
            self.show_session(id);
            self.mode = Mode::Terminal;
        } else {
            self.select_row(Row::Session(id));
        }
    }

    pub(crate) fn canvas_right_click(&mut self, pos: Position) {
        let layout = self.canvas_layout();
        let items = match layout.hit(pos) {
            Hit::Mascot(id) => {
                self.select_row(Row::Session(id));
                self.session_menu(id)
            }
            Hit::TeamTitle(id) | Hit::Team(id) | Hit::AddAgent(id) => {
                self.select_row(Row::Folder(id));
                self.folder_menu(id)
            }
            Hit::Board | Hit::Tray => {
                let area = self.main_area();
                let at = (
                    i32::from(pos.x) - i32::from(area.x) + self.canvas.pan.0,
                    i32::from(pos.y) - i32::from(area.y) + self.canvas.pan.1,
                );
                vec![(
                    "new team here".into(),
                    crate::app::MenuAction::NewTeamAt(at.0, at.1.max(0)),
                )]
            }
            Hit::AddButton => return,
        };
        self.open_menu(items, pos);
    }

    /// The agent shown in the inspector: the selected session, if it runs an agent.
    pub(crate) fn inspected(&self) -> Option<u32> {
        match self.rows().get(self.cursor)? {
            Row::Session(id) if self.session(*id)?.agent.is_some() => Some(*id),
            _ => None,
        }
    }

    pub(crate) fn canvas_wheel(&mut self, up: bool) {
        self.canvas.pan.1 += if up { -3 } else { 3 };
    }

    /// Forgets finished flights.
    pub(crate) fn canvas_tick(&mut self, now: Instant) {
        self.canvas
            .flights
            .retain(|f| now.saturating_duration_since(f.started) < FLIGHT);
    }
}

/// Writes `text` at (`x`, `y`), one char per cell, keeping only what falls inside `clip`.
fn put(buf: &mut Buffer, clip: Rect, x: i32, y: i32, text: &str, style: Style) {
    if y < i32::from(clip.y) || y >= i32::from(clip.bottom()) {
        return;
    }
    for (i, c) in text.chars().enumerate() {
        let cx = x + i as i32;
        if cx >= i32::from(clip.x) && cx < i32::from(clip.right()) {
            buf[(cx as u16, y as u16)].set_char(c).set_style(style);
        }
    }
}

/// Pulse between 0 and 1, about once a second.
fn pulse(now: Instant, epoch: Instant) -> f32 {
    let t = now.saturating_duration_since(epoch).as_secs_f32();
    0.5 + 0.5 * (t * std::f32::consts::TAU * 1.2).sin()
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let theme = app.theme();
    let layout = app.canvas_layout_in(area);
    let dragged = app.canvas.dragged_session();
    let hovered = dragged.and_then(|_| layout.zone_at(app.canvas.pointer));
    let glow = pulse(now, app.epoch);
    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();

    // Dotted grid, moving with the view.
    for y in layout.board.top()..layout.board.bottom() {
        for x in layout.board.left()..layout.board.right() {
            let cx = i32::from(x - area.x) + app.canvas.pan.0;
            let cy = i32::from(y - area.y) + app.canvas.pan.1;
            if cx.rem_euclid(6) == 0 && cy.rem_euclid(3) == 0 {
                buf[(x, y)].set_char('·').set_fg(theme.border);
            }
        }
    }

    let selected = app.rows().get(app.cursor).copied();
    for team in &layout.boxes {
        let clip = team.rect.clip(layout.board);
        if clip.is_empty() {
            continue;
        }
        let Some(folder) = app.folder(team.id) else {
            continue;
        };
        let color = theme.accent(team.id + 3);
        let lit = hovered == Some(Zone::Team(team.id));
        let alert = team
            .slots
            .iter()
            .any(|&(id, _)| app.session(id).is_some_and(|s| s.needs_user(now)));
        let border = if lit {
            lerp(color, Color::Rgb(255, 255, 255), 0.25 + 0.5 * glow)
        } else if alert {
            lerp(color, theme.err, glow)
        } else {
            color
        };
        let mut title_style = Style::new().fg(color).bold();
        if selected == Some(Row::Folder(team.id)) {
            title_style = title_style.add_modifier(Modifier::REVERSED);
        }
        let title = Line::from(vec![
            Span::styled(format!(" {} ", folder.name), title_style),
            Span::styled(
                format!("· {} ", team.slots.len()),
                Style::new().fg(theme.muted),
            ),
        ]);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .title(title);
        ratatui::widgets::Widget::render(Clear, clip, buf);
        ratatui::widgets::Widget::render(block, clip, buf);
        let instructions = folder
            .consigne
            .as_deref()
            .unwrap_or("right-click › edit instructions");
        put(
            buf,
            clip,
            team.rect.x + 2,
            team.rect.y + 1,
            &truncate(instructions, (team.rect.w - 4).max(0) as usize),
            Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
        );
        for &(id, slot) in &team.slots {
            if let Some(session) = app.session(id) {
                draw_mascot(buf, clip, app, session, slot, now);
            }
        }
        if let Some(slot) = team.placeholder {
            draw_placeholder(buf, clip, slot, border);
        }
        if let Some(slot) = team.add_slot {
            draw_placeholder(buf, clip, slot, theme.border);
            let label = Style::new().fg(theme.muted);
            put(buf, clip, slot.0 + 1, slot.1 + 2, " + agent", label);
        }
    }

    if !layout.tray.is_empty() {
        let tray = layout.tray;
        for x in tray.left()..tray.right() {
            buf[(x, tray.y)].set_char('─').set_fg(theme.border);
        }
        let lit = hovered == Some(Zone::Tray);
        let label = Style::new().fg(if lit { theme.text } else { theme.muted });
        put(
            buf,
            tray,
            i32::from(tray.x) + 2,
            i32::from(tray.y),
            " no team ",
            label,
        );
        for &(id, slot) in &layout.tray_slots {
            if let Some(session) = app.session(id) {
                draw_mascot(buf, tray, app, session, slot, now);
            }
        }
        if let Some(slot) = layout.tray_placeholder {
            draw_placeholder(buf, tray, slot, theme.text);
        }
    }

    if app.folders.is_empty() {
        let hint = "[ + team ] to create a team, then drag the shells into it";
        let y = i32::from(layout.board.y) + i32::from(layout.board.height) / 2;
        let x = i32::from(layout.board.x)
            + (i32::from(layout.board.width) - hint.chars().count() as i32).max(0) / 2;
        put(buf, layout.board, x, y, hint, Style::new().fg(theme.muted));
    }

    let button = layout.add_button;
    put(
        buf,
        area,
        i32::from(button.x),
        i32::from(button.y),
        "[ + team ]",
        Style::new().fg(theme.accents[0]).bold(),
    );

    draw_pulses(buf, app, &layout, now);

    if let Some(id) = app.inspected()
        && let Some(inspector) = inspector_layout(area)
        && let Some(session) = app.session(id)
        && let Some(agent) = &session.agent
    {
        draw_inspector(buf, app, &inspector, session, agent, now);
    }

    // The dragged mascot, with its shadow, follows the pointer.
    if let Some(Grab::Session { id, offset, .. }) = app.canvas.grab
        && Some(id) == dragged
        && let Some(session) = app.session(id)
    {
        let x = i32::from(app.canvas.pointer.x) + offset.0;
        let y = i32::from(app.canvas.pointer.y) + offset.1;
        let elapsed = now.saturating_duration_since(app.epoch);
        let (sprite, _, _) = mascot::pose(mascot::Mood::Working, elapsed);
        mascot::draw_sprite(
            buf,
            area,
            x + 1,
            y + 1,
            sprite,
            &Palette::flat(theme.surface),
        );
        let palette = Palette::tinted(theme.accent(session.id), theme);
        mascot::draw_sprite(buf, area, x, y - 1, sprite, &palette);
        put(
            buf,
            area,
            x,
            y + mascot::ROWS as i32,
            &centered(&session.name),
            Style::new().fg(theme.text).bold(),
        );
    }
}

/// `name` cut and centered on a mascot's width.
fn centered(name: &str) -> String {
    let name = truncate(name, mascot::WIDTH);
    format!("{name:^width$}", width = mascot::WIDTH)
}

/// One mascot in its slot, or on its way there after a drop.
fn draw_mascot(
    buf: &mut Buffer,
    clip: Rect,
    app: &App,
    session: &Session,
    slot: (i32, i32),
    now: Instant,
) {
    let theme = app.theme();
    let (mut x, mut y) = slot;
    if let Some(flight) = app.canvas.flights.iter().find(|f| f.session == session.id) {
        let t = now.saturating_duration_since(flight.started).as_secs_f32() / FLIGHT.as_secs_f32();
        if t < 1.0 {
            let k = ease_out_back(t);
            x = flight.from.0 + ((slot.0 - flight.from.0) as f32 * k).round() as i32;
            y = flight.from.1 + ((slot.1 - flight.from.1) as f32 * k).round() as i32;
        }
    }
    // Each mascot has its own rhythm, so they do not all move together.
    let elapsed = now.saturating_duration_since(app.epoch)
        + Duration::from_millis(u64::from(session.id) * 137);
    let mood = mascot::session_mood(session, now);
    let (sprite, bounce, _) = mascot::pose(mood, elapsed);
    let palette = Palette::tinted(theme.accent(session.id), theme);
    mascot::draw_sprite(buf, clip, x, y - i32::from(bounce), sprite, &palette);
    if mood == mascot::Mood::Alert {
        let style = Style::new().fg(theme.err).bold();
        put(buf, clip, x + mascot::WIDTH as i32, y - 1, "!", style);
    }

    let selected = app.rows().get(app.cursor) == Some(&Row::Session(session.id));
    let mut name = Style::new().fg(theme.text);
    if selected {
        name = name.bg(theme.surface).bold();
    }
    let rows = mascot::ROWS as i32;
    put(buf, clip, x, y + rows, &centered(&session.name), name);
    let (status, color) = crate::ui::session_status(session, now, theme);
    put(
        buf,
        clip,
        x,
        y + rows + 1,
        &format!(
            "{:^width$}",
            truncate(&status, mascot::WIDTH),
            width = mascot::WIDTH
        ),
        Style::new().fg(color),
    );
}

/// The agent panel of the canvas and its buttons.
pub struct Inspector {
    pub panel: Rect,
    pub open: Rect,
    pub interrupt: Rect,
    pub resend: Rect,
    /// Answers to a permission request (shown only while one is held).
    pub allow: Rect,
    pub deny: Rect,
    pub in_terminal: Rect,
}

const INSPECTOR_W: u16 = 40;
const INSPECTOR_H: u16 = 14;

/// Top-right of the canvas, under the `+ team` button; `None` if the canvas is too small.
pub fn inspector_layout(area: Rect) -> Option<Inspector> {
    if area.width < INSPECTOR_W + 20 || area.height < INSPECTOR_H + 3 {
        return None;
    }
    let panel = Rect::new(
        area.right() - INSPECTOR_W - 1,
        area.y + 2,
        INSPECTOR_W,
        INSPECTOR_H,
    );
    let y = panel.bottom() - 2;
    let answers = y - 1;
    Some(Inspector {
        panel,
        open: Rect::new(panel.x + 2, y, 8, 1),
        interrupt: Rect::new(panel.x + 12, y, 13, 1),
        resend: Rect::new(panel.x + 27, y, 10, 1),
        allow: Rect::new(panel.x + 2, answers, 9, 1),
        deny: Rect::new(panel.x + 13, answers, 8, 1),
        in_terminal: Rect::new(panel.x + 23, answers, 15, 1),
    })
}

/// Splits `text` into at most `lines` lines of `width` chars, on spaces when possible.
pub fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        for piece in word.chars().collect::<Vec<_>>().chunks(width.max(1)) {
            let piece: String = piece.iter().collect();
            let fits = current.chars().count() + 1 + piece.chars().count() <= width;
            if !current.is_empty() && !fits {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&piece);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.len() > lines {
        out.truncate(lines);
        if let Some(last) = out.last_mut() {
            *last = truncate(&format!("{last} …"), width);
        }
    }
    out
}

fn draw_inspector(
    buf: &mut Buffer,
    app: &App,
    inspector: &Inspector,
    session: &Session,
    agent: &AgentInfo,
    now: Instant,
) {
    let theme = app.theme();
    let panel = inspector.panel;
    let team = app
        .folder(agent.team)
        .map_or("no team", |f| f.name.as_str());
    let accent = theme.accent(session.id);
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", session.name),
            Style::new().fg(accent).bold(),
        ),
        Span::styled(format!("· {team} "), Style::new().fg(theme.muted)),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(accent))
        .title(title);
    ratatui::widgets::Widget::render(Clear, panel, buf);
    ratatui::widgets::Widget::render(block, panel, buf);

    let width = usize::from(panel.width - 4);
    let (x, mut y) = (i32::from(panel.x) + 2, i32::from(panel.y) + 1);
    let mut line = |text: &str, style: Style| {
        put(buf, panel, x, y, &truncate(text, width), style);
        y += 1;
    };
    let (state, color) = match agent.shown_state(now) {
        AgentState::Starting => ("● starting", theme.muted),
        AgentState::Idle => ("● idle: waiting for a prompt", theme.muted),
        AgentState::Working => ("● working", accent),
        AgentState::Waiting => ("● needs you", theme.err),
        AgentState::Done => ("● done", theme.ok),
    };
    line(state, Style::new().fg(color).bold());
    line(
        agent.tool.as_deref().unwrap_or(""),
        Style::new().fg(theme.text),
    );
    line("", Style::new());
    let muted = Style::new().fg(theme.muted);
    line(
        &format!(
            "tree    {}",
            agent.worktree.as_deref().unwrap_or("(project folder)")
        ),
        muted,
    );
    let folder = agent.folder.display().to_string();
    let keep = width.saturating_sub(8);
    let tail: String = folder
        .chars()
        .skip(folder.chars().count().saturating_sub(keep))
        .collect();
    line(&format!("folder  {tail}"), muted);
    line(&format!("files   {}", agent.files.join(", ")), muted);
    line("", Style::new());
    let message = agent.shown_message(now).unwrap_or_default();
    for text in wrap(&message, width, 3) {
        line(
            &text,
            Style::new().fg(theme.text).add_modifier(Modifier::ITALIC),
        );
    }
    if agent.can_decide(now) {
        let answer = |color| Style::new().fg(theme.on_pill).bg(color).bold();
        for (rect, label, style) in [
            (inspector.allow, "[ allow ]", answer(theme.ok)),
            (inspector.deny, "[ deny ]", answer(theme.err)),
            (
                inspector.in_terminal,
                "[ in terminal ]",
                answer(theme.muted),
            ),
        ] {
            put(
                buf,
                panel,
                i32::from(rect.x),
                i32::from(rect.y),
                label,
                style,
            );
        }
    }
    let button = Style::new().fg(theme.on_pill).bg(theme.pill[0]).bold();
    for (rect, label) in [
        (inspector.open, "[ open ]"),
        (inspector.interrupt, "[ interrupt ]"),
        (inspector.resend, "[ resend ]"),
    ] {
        put(
            buf,
            panel,
            i32::from(rect.x),
            i32::from(rect.y),
            label,
            button,
        );
    }
}

/// The cells of a straight line from `a` to `b`, both included (Bresenham).
pub fn line(a: (i32, i32), b: (i32, i32)) -> Vec<(i32, i32)> {
    let (dx, dy) = ((b.0 - a.0).abs(), -(b.1 - a.1).abs());
    let (sx, sy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
    let (mut x, mut y, mut err) = (a.0, a.1, dx + dy);
    let mut cells = vec![(x, y)];
    while (x, y) != b {
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
        cells.push((x, y));
    }
    cells
}

/// Messages between agents: a dotted line between the two mascots, with a dot travelling from
/// the sender to the receiver.
fn draw_pulses(buf: &mut Buffer, app: &App, layout: &CanvasLayout, now: Instant) {
    let theme = app.theme();
    let center = |(x, y): (i32, i32)| (x + mascot::WIDTH as i32 / 2, y + 2);
    let slots: Vec<IRect> = layout
        .boxes
        .iter()
        .flat_map(|team| &team.slots)
        .chain(&layout.tray_slots)
        .map(|&(_, slot)| slot_rect(slot))
        .collect();
    for &(from, to, started) in &app.pulses {
        let (Some(a), Some(b)) = (layout.slot_of(from), layout.slot_of(to)) else {
            continue;
        };
        let cells = line(center(a), center(b));
        let t = now.saturating_duration_since(started).as_secs_f32() / PULSE.as_secs_f32();
        let head = ((cells.len() - 1) as f32 * t.min(1.0)).round() as usize;
        let color = theme.accent(from);
        for (i, &(x, y)) in cells.iter().enumerate() {
            let on_mascot = slots
                .iter()
                .any(|slot| slot.contains(Position::new(x.max(0) as u16, y.max(0) as u16)));
            if on_mascot {
                continue;
            }
            let (symbol, style) = match head.abs_diff(i) {
                0 => ("●", Style::new().fg(color).bold()),
                1 => ("•", Style::new().fg(color)),
                _ => ("·", Style::new().fg(theme.border)),
            };
            put(buf, layout.board, x, y, symbol, style);
        }
    }
}

/// A dashed outline where the dragged mascot will land.
fn draw_placeholder(buf: &mut Buffer, clip: Rect, (x, y): (i32, i32), color: Color) {
    let style = Style::new().fg(color);
    let inner = mascot::WIDTH - 2;
    put(buf, clip, x, y, &format!("╭{}╮", "┄".repeat(inner)), style);
    for row in 1..SLOT_H - 2 {
        put(
            buf,
            clip,
            x,
            y + row,
            &format!("┆{}┆", " ".repeat(inner)),
            style,
        );
    }
    put(
        buf,
        clip,
        x,
        y + SLOT_H - 2,
        &format!("╰{}╯", "┄".repeat(inner)),
        style,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: u32, pos: (i32, i32), members: &[u32]) -> TeamInput {
        TeamInput {
            id,
            pos,
            members: members.to_vec(),
            add_slot: false,
        }
    }

    #[test]
    fn add_agent_slot_follows_the_members() {
        let teams = [TeamInput {
            add_slot: true,
            ..team(1, (0, 0), &[7, 8, 9])
        }];
        let layout = layout(AREA, (0, 0), &teams, &[], None);
        let team = &layout.boxes[0];
        assert_eq!(team.add_slot, Some((26 + 2, 3 + SLOT_H)));
        assert_eq!(team.rect.h, 4 + 2 * SLOT_H);
        assert_eq!(
            layout.hit(Position::new(30, 3 + SLOT_H as u16 + 1)),
            Hit::AddAgent(1)
        );
    }

    #[test]
    fn inspector_fits_in_the_canvas_or_hides() {
        let inspector = inspector_layout(AREA).unwrap();
        assert!(AREA.contains(inspector.panel.as_position()));
        assert!(inspector.panel.right() <= AREA.right());
        assert!(inspector.panel.contains(inspector.resend.as_position()));
        assert!(inspector.open.right() <= inspector.interrupt.x);
        assert!(inspector_layout(Rect::new(0, 0, 50, 40)).is_none());
    }

    #[test]
    fn lines_join_both_ends() {
        assert_eq!(line((0, 0), (3, 0)), [(0, 0), (1, 0), (2, 0), (3, 0)]);
        assert_eq!(line((2, 2), (0, 0)), [(2, 2), (1, 1), (0, 0)]);
        assert_eq!(line((1, 1), (1, 1)), [(1, 1)]);
        let steep = line((0, 0), (1, 5));
        assert_eq!(
            (steep[0], steep[steep.len() - 1], steep.len()),
            ((0, 0), (1, 5), 6)
        );
    }

    #[test]
    fn wrap_cuts_long_text() {
        assert_eq!(wrap("one two three", 7, 3), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4, 5), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one two three four", 7, 1), ["one tw…"]);
        assert!(wrap("", 10, 3).is_empty());
    }

    const AREA: Rect = Rect::new(26, 0, 100, 40);

    #[test]
    fn boxes_grow_with_their_members() {
        let teams = [
            team(1, (0, 0), &[]),
            team(2, (0, 20), &[7, 8, 9]),
            team(3, (50, 0), &[1, 2, 3, 4]),
        ];
        let layout = layout(AREA, (0, 0), &teams, &[], None);
        let sizes: Vec<(i32, i32)> = layout.boxes.iter().map(|b| (b.rect.w, b.rect.h)).collect();
        // Two columns minimum, three per row, one row of slots minimum.
        assert_eq!(sizes, [(27, 10), (39, 10), (39, 16)]);
        assert_eq!(layout.boxes[1].rect.x, 26);
        assert_eq!(layout.boxes[2].slots[3].1, (26 + 50 + 2, 3 + SLOT_H));
    }

    #[test]
    fn placeholder_takes_the_next_free_slot() {
        let teams = [team(1, (0, 0), &[7, 8, 9])];
        let layout = layout(AREA, (0, 0), &teams, &[5], Some(Zone::Team(1)));
        let team = &layout.boxes[0];
        assert_eq!(team.placeholder, Some((26 + 2, 3 + SLOT_H)));
        assert_eq!(team.rect.h, 4 + 2 * SLOT_H);
        assert_eq!(layout.tray_placeholder, None);
    }

    #[test]
    fn tray_sits_at_the_bottom_with_loose_mascots() {
        let tall = layout(AREA, (0, 0), &[], &[4, 5], Some(Zone::Tray));
        assert_eq!(tall.tray, Rect::new(26, 33, 100, 7));
        assert_eq!(tall.board.height, 33);
        assert_eq!(tall.tray_slots, [(4, (28, 34)), (5, (40, 34))]);
        assert_eq!(tall.tray_placeholder, Some((52, 34)));
        let short = layout(Rect::new(0, 0, 50, 8), (0, 0), &[], &[4], None);
        assert!(short.tray.is_empty());
    }

    #[test]
    fn panning_moves_boxes_but_not_the_tray() {
        let teams = [team(1, (10, 10), &[])];
        let layout = layout(AREA, (5, 8), &teams, &[3], None);
        assert_eq!((layout.boxes[0].rect.x, layout.boxes[0].rect.y), (31, 2));
        assert_eq!(layout.tray_slots[0].1, (28, 34));
    }

    #[test]
    fn hit_test_finds_what_is_under_the_mouse() {
        let teams = [team(1, (0, 0), &[7])];
        let layout = layout(AREA, (0, 0), &teams, &[4], None);
        assert_eq!(layout.hit(Position::new(30, 4)), Hit::Mascot(7));
        assert_eq!(layout.hit(Position::new(40, 0)), Hit::TeamTitle(1));
        assert_eq!(layout.hit(Position::new(48, 6)), Hit::Team(1));
        assert_eq!(layout.hit(Position::new(29, 35)), Hit::Mascot(4));
        assert_eq!(layout.hit(Position::new(90, 36)), Hit::Tray);
        assert_eq!(layout.hit(Position::new(90, 20)), Hit::Board);
        assert_eq!(layout.hit(Position::new(118, 0)), Hit::AddButton);
        assert_eq!(layout.zone_at(Position::new(48, 6)), Some(Zone::Team(1)));
        assert_eq!(layout.zone_at(Position::new(90, 36)), Some(Zone::Tray));
        assert_eq!(layout.zone_at(Position::new(90, 20)), None);
    }

    #[test]
    fn clipping_keeps_the_visible_part_only() {
        let rect = IRect {
            x: -5,
            y: 2,
            w: 10,
            h: 3,
        };
        assert_eq!(rect.clip(Rect::new(0, 0, 20, 20)), Rect::new(0, 2, 5, 3));
        assert!(rect.clip(Rect::new(10, 0, 5, 5)).is_empty());
    }

    #[test]
    fn default_team_positions_do_not_overlap() {
        assert_eq!(team_position(0), (2, 1));
        assert_eq!(team_position(1), (2 + TEAM_STEP.0, 1));
        assert_eq!(team_position(2), (2, 1 + TEAM_STEP.1));
    }
}
