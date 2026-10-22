//! Split layout of the main area: a binary tree of panes, each showing one session.

use ratatui::layout::{Position, Rect};

/// How a pane is cut in two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// Side by side.
    Right,
    /// One above the other.
    Down,
}

/// Direction for moving the focus between panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// A pane showing the session with this id.
    Pane(u32),
    Split {
        dir: Split,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Every pane with its area, in reading order.
    pub fn rects(&self, area: Rect) -> Vec<(u32, Rect)> {
        let mut out = Vec::new();
        self.collect(area, &mut out);
        out
    }

    fn collect(&self, area: Rect, out: &mut Vec<(u32, Rect)>) {
        match self {
            Layout::Pane(id) => out.push((*id, area)),
            Layout::Split { dir, first, second } => {
                let (a, b) = halves(area, *dir);
                first.collect(a, out);
                second.collect(b, out);
            }
        }
    }

    /// Session ids of the panes, in reading order.
    pub fn panes(&self) -> Vec<u32> {
        self.rects(Rect::default())
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    pub fn contains(&self, id: u32) -> bool {
        self.panes().contains(&id)
    }

    /// Cuts the pane of `target` in two; `new` goes right of / below it.
    pub fn split(&mut self, target: u32, new: u32, dir: Split) -> bool {
        match self {
            Layout::Pane(id) if *id == target => {
                *self = Layout::Split {
                    dir,
                    first: Box::new(Layout::Pane(target)),
                    second: Box::new(Layout::Pane(new)),
                };
                true
            }
            Layout::Pane(_) => false,
            Layout::Split { first, second, .. } => {
                first.split(target, new, dir) || second.split(target, new, dir)
            }
        }
    }

    /// Shows session `new` in the pane of `old`.
    pub fn replace(&mut self, old: u32, new: u32) -> bool {
        match self {
            Layout::Pane(id) if *id == old => {
                *id = new;
                true
            }
            Layout::Pane(_) => false,
            Layout::Split { first, second, .. } => {
                first.replace(old, new) || second.replace(old, new)
            }
        }
    }

    /// Removes the pane of `target`; its sibling takes the freed space.
    /// Returns false if not found or if it is the last pane.
    pub fn remove(&mut self, target: u32) -> bool {
        let Layout::Split { first, second, .. } = self else {
            return false;
        };
        let keep = if **first == Layout::Pane(target) {
            std::mem::replace(&mut **second, Layout::Pane(0))
        } else if **second == Layout::Pane(target) {
            std::mem::replace(&mut **first, Layout::Pane(0))
        } else {
            return first.remove(target) || second.remove(target);
        };
        *self = keep;
        true
    }

    /// Closest pane in direction `dir` from the pane of `from`, among those facing it.
    pub fn neighbor(&self, area: Rect, from: u32, dir: Dir) -> Option<u32> {
        let rects = self.rects(area);
        let (_, r) = *rects.iter().find(|(id, _)| *id == from)?;
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|&(id, c)| {
                let rows_overlap = c.y < r.bottom() && r.y < c.bottom();
                let cols_overlap = c.x < r.right() && r.x < c.right();
                let distance = match dir {
                    Dir::Right if rows_overlap && c.x >= r.right() => c.x - r.right(),
                    Dir::Left if rows_overlap && c.right() <= r.x => r.x - c.right(),
                    Dir::Down if cols_overlap && c.y >= r.bottom() => c.y - r.bottom(),
                    Dir::Up if cols_overlap && c.bottom() <= r.y => r.y - c.bottom(),
                    _ => return None,
                };
                Some((distance, id))
            })
            .min_by_key(|&(distance, _)| distance)
            .map(|(_, id)| id)
    }

    /// The pane under `pos`, with its area.
    pub fn pane_at(&self, area: Rect, pos: Position) -> Option<(u32, Rect)> {
        self.rects(area).into_iter().find(|(_, r)| r.contains(pos))
    }
}

fn halves(area: Rect, dir: Split) -> (Rect, Rect) {
    match dir {
        Split::Right => {
            let w = area.width / 2;
            (
                Rect { width: w, ..area },
                Rect {
                    x: area.x + w,
                    width: area.width - w,
                    ..area
                },
            )
        }
        Split::Down => {
            let h = area.height / 2;
            (
                Rect { height: h, ..area },
                Rect {
                    y: area.y + h,
                    height: area.height - h,
                    ..area
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 | 2, with 2 split into 2 over 3.
    fn three_panes() -> Layout {
        let mut layout = Layout::Pane(1);
        assert!(layout.split(1, 2, Split::Right));
        assert!(layout.split(2, 3, Split::Down));
        layout
    }

    #[test]
    fn split_and_rects_cover_the_area() {
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(
            three_panes().rects(area),
            [
                (1, Rect::new(0, 0, 50, 40)),
                (2, Rect::new(50, 0, 50, 20)),
                (3, Rect::new(50, 20, 50, 20)),
            ]
        );
        assert_eq!(three_panes().panes(), [1, 2, 3]);
    }

    #[test]
    fn odd_sizes_lose_no_cell() {
        let mut layout = Layout::Pane(1);
        layout.split(1, 2, Split::Right);
        let rects = layout.rects(Rect::new(0, 0, 5, 3));
        assert_eq!(rects[0].1.width + rects[1].1.width, 5);
    }

    #[test]
    fn split_unknown_pane_does_nothing() {
        let mut layout = Layout::Pane(1);
        assert!(!layout.split(9, 2, Split::Right));
        assert_eq!(layout, Layout::Pane(1));
    }

    #[test]
    fn replace_swaps_the_session_of_a_pane() {
        let mut layout = three_panes();
        assert!(layout.replace(3, 7));
        assert_eq!(layout.panes(), [1, 2, 7]);
        assert!(!layout.replace(42, 8));
    }

    #[test]
    fn remove_gives_space_to_the_sibling() {
        let mut layout = three_panes();
        assert!(layout.remove(2));
        assert_eq!(layout.panes(), [1, 3]);
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(layout.rects(area)[1], (3, Rect::new(50, 0, 50, 40)));
        assert!(layout.remove(1));
        assert_eq!(layout, Layout::Pane(3));
    }

    #[test]
    fn last_pane_cannot_be_removed() {
        let mut layout = Layout::Pane(1);
        assert!(!layout.remove(1));
        assert!(!three_panes().remove(42));
    }

    #[test]
    fn neighbor_follows_directions() {
        let layout = three_panes();
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(layout.neighbor(area, 1, Dir::Right), Some(2));
        assert_eq!(layout.neighbor(area, 2, Dir::Down), Some(3));
        assert_eq!(layout.neighbor(area, 3, Dir::Up), Some(2));
        assert_eq!(layout.neighbor(area, 3, Dir::Left), Some(1));
        assert_eq!(layout.neighbor(area, 1, Dir::Left), None);
        assert_eq!(layout.neighbor(area, 1, Dir::Up), None);
        assert_eq!(layout.neighbor(area, 42, Dir::Up), None);
    }

    #[test]
    fn pane_at_finds_the_pane_under_the_mouse() {
        let layout = three_panes();
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(
            layout.pane_at(area, Position::new(10, 10)).map(|p| p.0),
            Some(1)
        );
        assert_eq!(
            layout.pane_at(area, Position::new(60, 30)).map(|p| p.0),
            Some(3)
        );
        assert_eq!(layout.pane_at(area, Position::new(200, 30)), None);
    }
}
