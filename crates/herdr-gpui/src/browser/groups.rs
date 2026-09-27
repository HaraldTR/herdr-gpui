//! What each side of a split window shows. Both sides list the same tabs, as
//! editor groups do, but the daemon projects a single terminal surface and a
//! native page can be placed only once, so no tab is ever shown on both sides:
//! choosing on one side what the other shows swaps them instead.
use super::TabId;

/// The share of the width the left side starts with.
pub(crate) const DEFAULT_RATIO: f32 = 0.5;
/// Neither side may be dragged narrower than this share of the width.
const MIN_RATIO: f32 = 0.15;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub(crate) const BOTH: [Self; 2] = [Self::Left, Self::Right];

    pub(crate) fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// Debug selectors keep their unsplit names on the left, so a window
    /// that never splits reads as it always has.
    pub(crate) fn selector(self, name: &str) -> String {
        match self {
            Self::Left => name.to_owned(),
            Self::Right => format!("right-{name}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// The workspace's focused Herdr tab.
    Terminal,
    Page(TabId),
    /// Nothing: the other side holds the terminal and this one no page.
    Empty,
}

/// One workspace's sides: the page covering each, and the side the terminal
/// sits on beneath its page, if any. Unsplit, only the left side is shown
/// and the terminal is always there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Groups {
    pages: [Option<TabId>; 2],
    terminal: Side,
}

impl Groups {
    pub(crate) fn content(&self, side: Side) -> Content {
        match self.pages[side.index()] {
            Some(id) => Content::Page(id),
            None if self.terminal == side => Content::Terminal,
            None => Content::Empty,
        }
    }

    /// The side showing `content`, if one does.
    pub(crate) fn side_of(&self, content: Content) -> Option<Side> {
        Side::BOTH
            .into_iter()
            .find(|side| self.content(*side) == content)
    }

    /// Shows `content` on `side`. What the other side already shows trades
    /// places with what this side showed, so a tab never appears twice.
    pub(crate) fn show(&mut self, side: Side, content: Content) {
        let other = side.other();
        if content != Content::Empty && self.content(other) == content {
            self.set(other, self.content(side));
        }
        self.set(side, content);
    }

    fn set(&mut self, side: Side, content: Content) {
        match content {
            Content::Terminal => {
                self.pages[side.index()] = None;
                self.terminal = side;
            }
            Content::Page(id) => self.pages[side.index()] = Some(id),
            Content::Empty => self.pages[side.index()] = None,
        }
    }

    /// Uncovers the terminal where it sits.
    pub(crate) fn uncover_terminal(&mut self) {
        self.pages[self.terminal.index()] = None;
    }

    /// Drops pages that closed. Returns whether anything changed.
    pub(crate) fn retain(&mut self, mut live: impl FnMut(TabId) -> bool) -> bool {
        let mut changed = false;
        for page in &mut self.pages {
            if page.is_some_and(|id| !live(id)) {
                *page = None;
                changed = true;
            }
        }
        changed
    }

    /// Folds a split back into one side that keeps what `kept` showed, or
    /// the terminal when that was nothing.
    pub(crate) fn unsplit(&mut self, kept: Side) {
        let page = self.pages[kept.index()];
        *self = Self::default();
        self.pages[Side::Left.index()] = page;
    }

    pub(crate) fn pages(&self) -> impl Iterator<Item = TabId> + '_ {
        self.pages.iter().flatten().copied()
    }
}

/// A window's split: which side has the keyboard and how wide the left is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Split {
    pub(crate) active: Side,
    ratio: f32,
}

impl Default for Split {
    fn default() -> Self {
        Self {
            active: Side::Right,
            ratio: DEFAULT_RATIO,
        }
    }
}

impl Split {
    pub(crate) fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Sets the left side's share from a pointer `offset` into a row `width`
    /// wide, keeping both sides usable. Returns whether it moved.
    pub(crate) fn drag(&mut self, offset: f32, width: f32) -> bool {
        if width.is_nan() || width <= 0. || !offset.is_finite() {
            return false;
        }
        let ratio = (offset / width).clamp(MIN_RATIO, 1. - MIN_RATIO);
        let moved = ratio != self.ratio;
        self.ratio = ratio;
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Content::*;

    fn tab(id: u64) -> TabId {
        TabId::test(id)
    }

    fn sides(groups: &Groups) -> [Content; 2] {
        Side::BOTH.map(|side| groups.content(side))
    }

    #[test]
    fn unsplit_the_terminal_shows_unless_a_page_covers_it() {
        let mut groups = Groups::default();
        assert_eq!(sides(&groups), [Terminal, Empty]);
        groups.show(Side::Left, Page(tab(1)));
        assert_eq!(sides(&groups), [Page(tab(1)), Empty]);
        groups.show(Side::Left, Terminal);
        assert_eq!(sides(&groups), [Terminal, Empty]);
    }

    #[test]
    fn choosing_what_the_other_side_shows_swaps_the_sides() {
        let mut groups = Groups::default();
        groups.show(Side::Right, Page(tab(1)));
        assert_eq!(sides(&groups), [Terminal, Page(tab(1))]);
        // The terminal moves right and the page takes its place.
        groups.show(Side::Right, Terminal);
        assert_eq!(sides(&groups), [Page(tab(1)), Terminal]);
        groups.show(Side::Right, Page(tab(1)));
        assert_eq!(sides(&groups), [Terminal, Page(tab(1))]);
        groups.show(Side::Left, Page(tab(2)));
        assert_eq!(sides(&groups), [Page(tab(2)), Page(tab(1))]);
        groups.show(Side::Left, Page(tab(1)));
        assert_eq!(sides(&groups), [Page(tab(1)), Page(tab(2))]);
        // The covered terminal is uncovered where it sits.
        groups.uncover_terminal();
        assert_eq!(sides(&groups), [Terminal, Page(tab(2))]);
    }

    #[test]
    fn moving_the_terminal_away_from_an_empty_side_leaves_it_empty() {
        let mut groups = Groups::default();
        groups.show(Side::Right, Terminal);
        assert_eq!(sides(&groups), [Empty, Terminal]);
        assert_eq!(groups.side_of(Terminal), Some(Side::Right));
        groups.show(Side::Left, Page(tab(3)));
        groups.show(Side::Right, Page(tab(4)));
        // The terminal stays on the right beneath its page.
        assert_eq!(groups.side_of(Terminal), None);
        groups.uncover_terminal();
        assert_eq!(sides(&groups), [Page(tab(3)), Terminal]);
    }

    #[test]
    fn closed_pages_leave_their_side() {
        let mut groups = Groups::default();
        groups.show(Side::Left, Page(tab(1)));
        groups.show(Side::Right, Page(tab(2)));
        assert!(groups.retain(|id| id != tab(2)));
        assert!(!groups.retain(|id| id != tab(2)));
        assert_eq!(sides(&groups), [Page(tab(1)), Empty]);
        assert_eq!(groups.pages().collect::<Vec<_>>(), [tab(1)]);
        assert!(groups.retain(|_| false));
        assert_eq!(sides(&groups), [Terminal, Empty]);
    }

    #[test]
    fn unsplitting_keeps_one_sides_content_on_the_left() {
        let split = {
            let mut groups = Groups::default();
            groups.show(Side::Right, Page(tab(1)));
            groups
        };
        let mut kept_page = split;
        kept_page.unsplit(Side::Right);
        assert_eq!(sides(&kept_page), [Page(tab(1)), Empty]);
        let mut kept_terminal = split;
        kept_terminal.unsplit(Side::Left);
        assert_eq!(sides(&kept_terminal), [Terminal, Empty]);
        let mut empty = Groups::default();
        empty.show(Side::Right, Terminal);
        empty.unsplit(Side::Left);
        assert_eq!(sides(&empty), [Terminal, Empty]);
    }

    #[test]
    fn dragging_keeps_both_sides_usable() {
        let mut split = Split::default();
        assert_eq!(split.ratio(), DEFAULT_RATIO);
        assert!(split.drag(300., 1000.));
        assert_eq!(split.ratio(), 0.3);
        assert!(!split.drag(300., 1000.));
        assert!(split.drag(0., 1000.));
        assert_eq!(split.ratio(), MIN_RATIO);
        assert!(split.drag(5000., 1000.));
        assert_eq!(split.ratio(), 1. - MIN_RATIO);
        assert!(!split.drag(10., 0.));
        assert!(!split.drag(f32::NAN, 1000.));
        assert_eq!(split.ratio(), 1. - MIN_RATIO);
    }

    #[test]
    fn selectors_keep_their_names_on_the_left() {
        assert_eq!(Side::Left.selector("tab-t0"), "tab-t0");
        assert_eq!(Side::Right.selector("tab-t0"), "right-tab-t0");
    }
}
