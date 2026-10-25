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
        /// Part of the space given to `first`, in thousandths.
        share: u16,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

/// The border between the two halves of a split, which the mouse can drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    /// The split, in the order of `dividers`.
    pub index: usize,
    pub dir: Split,
    /// The split's whole area.
    pub area: Rect,
    pub share: u16,
}

/// A half never gets less than this, in thousandths.
const MIN_SHARE: u16 = 150;

impl Divider {
    /// The share that puts the border under `pos`, kept between 15 % and 85 %.
    pub fn share_at(&self, pos: Position) -> u16 {
        let (offset, size) = match self.dir {
            Split::Right => (pos.x.saturating_sub(self.area.x), self.area.width),
            Split::Down => (pos.y.saturating_sub(self.area.y), self.area.height),
        };
        let share = u32::from(offset) * 1000 / u32::from(size.max(1));
        (share as u16).clamp(MIN_SHARE, 1000 - MIN_SHARE)
    }
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
            Layout::Split {
                dir,
                share,
                first,
                second,
            } => {
                let (a, b) = halves(area, *dir, *share);
                first.collect(a, out);
                second.collect(b, out);
            }
        }
    }

    /// Every split's border, parents before children.
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        let mut out = Vec::new();
        self.collect_dividers(area, &mut out);
        out
    }

    fn collect_dividers(&self, area: Rect, out: &mut Vec<Divider>) {
        if let Layout::Split {
            dir,
            share,
            first,
            second,
        } = self
        {
            out.push(Divider {
                index: out.len(),
                dir: *dir,
                area,
                share: *share,
            });
            let (a, b) = halves(area, *dir, *share);
            first.collect_dividers(a, out);
            second.collect_dividers(b, out);
        }
    }

    /// The border under `pos`: the two border columns (or rows) where two panes meet.
    pub fn divider_at(&self, area: Rect, pos: Position) -> Option<Divider> {
        // Children last: the innermost border wins where borders cross.
        self.dividers(area).into_iter().rev().find(|d| {
            let (first, _) = halves(d.area, d.dir, d.share);
            match d.dir {
                Split::Right => {
                    (first.right().saturating_sub(1)..=first.right()).contains(&pos.x)
                        && (d.area.top()..d.area.bottom()).contains(&pos.y)
                }
                Split::Down => {
                    (first.bottom().saturating_sub(1)..=first.bottom()).contains(&pos.y)
                        && (d.area.left()..d.area.right()).contains(&pos.x)
                }
            }
        })
    }

    /// Sets the share of the `index`-th split (see `dividers`).
    pub fn set_share(&mut self, index: usize, value: u16) {
        self.set_share_from(&mut 0, index, value);
    }

    fn set_share_from(&mut self, seen: &mut usize, index: usize, value: u16) {
        if let Layout::Split {
            share,
            first,
            second,
            ..
        } = self
        {
            if *seen == index {
                *share = value;
                return;
            }
            *seen += 1;
            first.set_share_from(seen, index, value);
            second.set_share_from(seen, index, value);
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
                    share: 500,
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

fn halves(area: Rect, dir: Split, share: u16) -> (Rect, Rect) {
    let part = |size: u16| (u32::from(size) * u32::from(share) / 1000) as u16;
    match dir {
        Split::Right => {
            let w = part(area.width);
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
            let h = part(area.height);
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
    fn borders_are_found_and_dragged() {
        let mut layout = three_panes();
        let area = Rect::new(0, 0, 100, 40);
        // 1 | (2 over 3): the vertical border at x 49-50, the horizontal one at y 19-20.
        let outer = layout.divider_at(area, Position::new(50, 5)).unwrap();
        assert_eq!((outer.index, outer.dir), (0, Split::Right));
        let inner = layout.divider_at(area, Position::new(70, 19)).unwrap();
        assert_eq!((inner.index, inner.dir), (1, Split::Down));
        assert_eq!(
            layout.divider_at(area, Position::new(20, 20)),
            None,
            "inside pane 1"
        );

        layout.set_share(0, outer.share_at(Position::new(30, 5)));
        assert_eq!(layout.rects(area)[0].1.width, 30);
        layout.set_share(1, 250);
        assert_eq!(layout.rects(area)[1].1.height, 10);
        assert_eq!(outer.share_at(Position::new(0, 5)), 150, "at least 15 %");
        assert_eq!(outer.share_at(Position::new(99, 5)), 850, "at most 85 %");
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
