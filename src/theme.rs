use ratatui::style::Color;

/// Colours of the chiika chrome. The background is never painted: shells keep the terminal's.
pub struct Theme {
    pub name: &'static str,
    /// One accent per session, picked by id.
    pub accents: [Color; 6],
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    /// Background of the selected card and palette row.
    pub surface: Color,
    pub ok: Color,
    pub err: Color,
    /// Border gradient of the focused terminal.
    pub gradient: (Color, Color),
    /// Mode pill colours: workspace, terminal, text input (rename / palette).
    pub pill: [Color; 3],
    /// Text drawn on top of a pill.
    pub on_pill: Color,
}

impl Theme {
    pub fn accent(&self, session_id: u32) -> Color {
        self.accents[session_id as usize % self.accents.len()]
    }
}

pub const THEMES: [Theme; 2] = [
    Theme {
        name: "chiika",
        accents: [
            Color::Rgb(245, 169, 201),
            Color::Rgb(159, 226, 191),
            Color::Rgb(185, 167, 245),
            Color::Rgb(255, 192, 159),
            Color::Rgb(154, 209, 245),
            Color::Rgb(247, 224, 138),
        ],
        text: Color::Rgb(220, 224, 244),
        muted: Color::Rgb(127, 132, 156),
        border: Color::Rgb(88, 91, 112),
        surface: Color::Rgb(49, 50, 68),
        ok: Color::Rgb(166, 227, 161),
        err: Color::Rgb(243, 139, 168),
        gradient: (Color::Rgb(245, 169, 201), Color::Rgb(154, 209, 245)),
        pill: [
            Color::Rgb(185, 167, 245),
            Color::Rgb(245, 169, 201),
            Color::Rgb(255, 192, 159),
        ],
        on_pill: Color::Rgb(30, 30, 46),
    },
    Theme {
        name: "midnight",
        accents: [
            Color::Rgb(122, 162, 247),
            Color::Rgb(125, 207, 255),
            Color::Rgb(187, 154, 247),
            Color::Rgb(158, 206, 106),
            Color::Rgb(224, 175, 104),
            Color::Rgb(247, 118, 142),
        ],
        text: Color::Rgb(192, 202, 245),
        muted: Color::Rgb(86, 95, 137),
        border: Color::Rgb(59, 66, 97),
        surface: Color::Rgb(36, 40, 59),
        ok: Color::Rgb(158, 206, 106),
        err: Color::Rgb(247, 118, 142),
        gradient: (Color::Rgb(122, 162, 247), Color::Rgb(187, 154, 247)),
        pill: [
            Color::Rgb(122, 162, 247),
            Color::Rgb(187, 154, 247),
            Color::Rgb(224, 175, 104),
        ],
        on_pill: Color::Rgb(26, 27, 38),
    },
];
