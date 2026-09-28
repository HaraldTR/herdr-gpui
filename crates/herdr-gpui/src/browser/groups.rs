//! Editor groups: a workspace's view split into side-by-side groups, each
//! listing the same tabs and showing one of them, as an editor's groups do.
//!
//! Any tab may be picked in several groups at once, but the daemon projects a
//! single terminal surface and a native page can be placed only once, so a
//! tab is live in just one of them: the group used most recently. The others
//! stand in for it until they are used again. The terminal is live only in a
//! group that picked the daemon's focused tab; using a group that picked
//! another Herdr tab is what focuses that tab.
use super::TabId;

/// Neither group beside a divider may be dragged narrower than this share
/// of the row.
const MIN_SHARE: f32 = 0.08;

/// Names a group within a window. Unique across the window's workspaces, so
/// state keyed by group, such as an address field, is never shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GroupId(u64);

/// Hands out group IDs for one window.
#[derive(Debug, Default)]
pub(crate) struct GroupIds(u64);

impl GroupIds {
    pub(crate) fn next(&mut self) -> GroupId {
        self.0 += 1;
        GroupId(self.0)
    }
}

/// A group and where it sits, which names its debug selectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) id: GroupId,
    pub(crate) index: usize,
}

impl Slot {
    /// The first group keeps the unsplit names, so a window that never
    /// splits reads as it always has.
    pub(crate) fn selector(self, name: &str) -> String {
        match self.index {
            0 => name.to_owned(),
            index => format!("g{index}-{name}"),
        }
    }
}

/// The tab a group picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Herdr(String),
    Page(TabId),
}

/// What a group draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// The daemon's focused tab.
    Terminal,
    Page(TabId),
    /// A tab live in another group, or a Herdr tab the daemon does not
    /// focus: using the group brings it here.
    Elsewhere(Pick),
    /// Nothing to show yet.
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
struct Group {
    id: GroupId,
    /// `None` follows the daemon's focused tab. Only a lone group follows:
    /// beside another, following would move it whenever the other focuses
    /// a tab of its own.
    pick: Option<Pick>,
    /// When the group was last used; the latest one holds a shared tab.
    used: u64,
    /// The group's share of the row's width.
    share: f32,
}

/// One workspace's groups, left to right.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    groups: Vec<Group>,
    active: GroupId,
    clock: u64,
}

impl Layout {
    /// One group following the terminal, as an unsplit window shows.
    pub(crate) fn new(id: GroupId) -> Self {
        Self {
            groups: vec![Group {
                id,
                pick: None,
                used: 0,
                share: 1.,
            }],
            active: id,
            clock: 0,
        }
    }

    pub(crate) fn slots(&self) -> impl Iterator<Item = Slot> + '_ {
        self.groups.iter().enumerate().map(|(index, group)| Slot {
            id: group.id,
            index,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.groups.len()
    }

    pub(crate) fn active(&self) -> GroupId {
        self.active
    }

    fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|group| group.id == id)
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|group| group.id == id)
    }

    pub(crate) fn share(&self, id: GroupId) -> f32 {
        self.group(id).map_or(0., |group| group.share)
    }

    /// The tab `id` picked, reading a group that follows the terminal as
    /// the daemon's `focused` tab.
    pub(crate) fn pick(&self, id: GroupId, focused: Option<&str>) -> Option<Pick> {
        let group = self.group(id)?;
        group
            .pick
            .clone()
            .or_else(|| focused.map(|tab| Pick::Herdr(tab.to_owned())))
    }

    /// What `id` draws while the daemon focuses `focused`.
    pub(crate) fn shown(&self, id: GroupId, focused: Option<&str>) -> Shown {
        let Some(pick) = self.pick(id, focused) else {
            // Following a terminal the daemon has not focused: the terminal
            // area still shows, as it does before a workspace connects.
            let follower = self
                .groups
                .iter()
                .filter(|group| group.pick.is_none())
                .max_by_key(|group| group.used)
                .map(|group| group.id);
            return if follower == Some(id) {
                Shown::Terminal
            } else {
                Shown::Empty
            };
        };
        let live = match &pick {
            Pick::Herdr(tab) if Some(tab.as_str()) != focused => false,
            _ => self.holder(&pick, focused) == Some(id),
        };
        match (live, pick) {
            (true, Pick::Herdr(_)) => Shown::Terminal,
            (true, Pick::Page(page)) => Shown::Page(page),
            (false, pick) => Shown::Elsewhere(pick),
        }
    }

    /// The group a tab is live in: the latest used of those that picked it.
    fn holder(&self, pick: &Pick, focused: Option<&str>) -> Option<GroupId> {
        self.groups
            .iter()
            .filter(|group| self.pick(group.id, focused).as_ref() == Some(pick))
            .max_by_key(|group| group.used)
            .map(|group| group.id)
    }

    /// Makes `id` the group in use. Returns whether anything changed.
    pub(crate) fn activate(&mut self, id: GroupId) -> bool {
        let clock = self.clock;
        let active = self.active;
        let Some(group) = self.group_mut(id) else {
            return false;
        };
        if active == id && group.used == clock {
            return false;
        }
        group.used = clock + 1;
        self.clock = clock + 1;
        self.active = id;
        true
    }

    /// Picks `pick` in `id` and uses the group.
    pub(crate) fn choose(&mut self, id: GroupId, pick: Pick) {
        if let Some(group) = self.group_mut(id) {
            group.pick = Some(pick);
            self.activate(id);
        }
    }

    /// Splits `id`: a new group to its right picks the same tab, takes half
    /// its width, and becomes the group in use, so the tab moves with it.
    /// A group that followed the terminal keeps `focused`.
    pub(crate) fn split(&mut self, id: GroupId, new: GroupId, focused: Option<&str>) -> bool {
        let Some(index) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        for group in &mut self.groups {
            if group.pick.is_none() {
                group.pick = focused.map(|tab| Pick::Herdr(tab.to_owned()));
            }
        }
        let source = &mut self.groups[index];
        source.share /= 2.;
        let group = Group {
            id: new,
            pick: source.pick.clone(),
            used: 0,
            share: source.share,
        };
        self.groups.insert(index + 1, group);
        self.activate(new);
        true
    }

    /// Closes `id`, giving its width to the group on its left, or its right
    /// when it was first. The last group never closes.
    pub(crate) fn close(&mut self, id: GroupId) -> bool {
        if self.groups.len() < 2 {
            return false;
        }
        let Some(index) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        let closed = self.groups.remove(index);
        let neighbour = index.saturating_sub(1);
        self.groups[neighbour].share += closed.share;
        if self.active == id {
            let next = self.groups[neighbour].id;
            self.activate(next);
        }
        true
    }

    /// Moves the divider after the `divider`th group to `offset` into a row
    /// `width` wide. Returns whether it moved.
    pub(crate) fn drag(&mut self, divider: usize, offset: f32, width: f32) -> bool {
        if divider + 1 >= self.groups.len() || width.is_nan() || width <= 0. || !offset.is_finite()
        {
            return false;
        }
        let start: f32 = self.groups[..divider].iter().map(|group| group.share).sum();
        let pair = self.groups[divider].share + self.groups[divider + 1].share;
        if pair < 2. * MIN_SHARE {
            return false;
        }
        let left = (offset / width - start).clamp(MIN_SHARE, pair - MIN_SHARE);
        if left == self.groups[divider].share {
            return false;
        }
        self.groups[divider].share = left;
        self.groups[divider + 1].share = pair - left;
        true
    }

    /// Follows the daemon moving its focus from `old` to `new`, from a
    /// shortcut, an agent, or a closed tab. The latest used group that showed
    /// `old` now shows `new`; with none, the group in use does. Nothing moves
    /// when a group already asked for `new`, since using that group is what
    /// focused it.
    pub(crate) fn focus_moved(&mut self, old: Option<&str>, new: &str) {
        let asked = Pick::Herdr(new.to_owned());
        if self
            .groups
            .iter()
            .any(|group| group.pick.as_ref() == Some(&asked))
        {
            return;
        }
        let old = old.map(|tab| Pick::Herdr(tab.to_owned()));
        // A group following the terminal already shows `new`; one that
        // picked `old` follows along with it.
        if self.groups.iter().any(|group| group.pick.is_none()) {
            if let Some(old) = &old {
                self.replace(old, None, Some(new));
            }
            return;
        }
        let follower = self
            .groups
            .iter()
            .filter(|group| old.is_some() && group.pick == old)
            .max_by_key(|group| group.used)
            .map_or(self.active, |group| group.id);
        if let Some(group) = self.group_mut(follower) {
            group.pick = Some(asked);
        }
    }

    /// Replaces `closed` wherever it was picked, with `with` or, for `None`,
    /// with the terminal: following it when alone, else the `focused` tab.
    /// Returns whether any group had picked it.
    pub(crate) fn replace(
        &mut self,
        closed: &Pick,
        with: Option<Pick>,
        focused: Option<&str>,
    ) -> bool {
        let with = with.or_else(|| {
            (self.groups.len() > 1)
                .then(|| focused.map(|tab| Pick::Herdr(tab.to_owned())))
                .flatten()
        });
        let mut changed = false;
        for group in &mut self.groups {
            if group.pick.as_ref() == Some(closed) {
                group.pick = with.clone();
                changed = true;
            }
        }
        changed
    }

    /// Every tab some group picked.
    pub(crate) fn picks(&self) -> impl Iterator<Item = &Pick> + '_ {
        self.groups.iter().filter_map(|group| group.pick.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Shown::*;

    fn page(id: u64) -> Pick {
        Pick::Page(TabId::test(id))
    }

    fn herdr(id: &str) -> Pick {
        Pick::Herdr(id.into())
    }

    fn shown(layout: &Layout, focused: &str) -> Vec<Shown> {
        layout
            .slots()
            .map(|slot| layout.shown(slot.id, Some(focused)))
            .collect()
    }

    fn layout() -> (Layout, GroupIds, GroupId) {
        let mut ids = GroupIds::default();
        let first = ids.next();
        (Layout::new(first), ids, first)
    }

    #[test]
    fn an_unsplit_layout_follows_the_terminal_until_a_page_is_picked() {
        let (mut layout, _, a) = layout();
        assert_eq!(shown(&layout, "t1"), [Terminal]);
        assert_eq!(layout.shown(a, None), Terminal);
        layout.choose(a, page(1));
        assert_eq!(shown(&layout, "t1"), [Page(TabId::test(1))]);
        layout.choose(a, herdr("t1"));
        assert_eq!(shown(&layout, "t1"), [Terminal]);
        // Picking a Herdr tab the daemon does not focus waits for it.
        layout.choose(a, herdr("t2"));
        assert_eq!(shown(&layout, "t1"), [Elsewhere(herdr("t2"))]);
        assert_eq!(shown(&layout, "t2"), [Terminal]);
    }

    #[test]
    fn splitting_moves_the_tab_to_the_new_group_and_leaves_a_stand_in() {
        let (mut layout, mut ids, a) = layout();
        let (b, c) = (ids.next(), ids.next());
        assert!(layout.split(a, b, Some("t1")));
        assert_eq!(layout.len(), 2);
        assert_eq!(layout.active(), b);
        assert_eq!(shown(&layout, "t1"), [Elsewhere(herdr("t1")), Terminal]);
        assert_eq!(layout.share(a), 0.5);
        // Using the first group takes the terminal back.
        assert!(layout.activate(a));
        assert_eq!(shown(&layout, "t1"), [Terminal, Elsewhere(herdr("t1"))]);
        // Splits go as far as asked.
        layout.choose(b, page(7));
        assert!(layout.split(b, c, Some("t1")));
        assert_eq!(
            shown(&layout, "t1"),
            [Terminal, Elsewhere(page(7)), Page(TabId::test(7))]
        );
        assert_eq!(layout.share(b), 0.25);
        assert_eq!(layout.share(c), 0.25);
        assert!(!layout.split(ids.next(), ids.next(), Some("t1")));
    }

    #[test]
    fn a_page_is_live_in_the_latest_group_to_use_it() {
        let (mut layout, mut ids, a) = layout();
        let b = ids.next();
        layout.split(a, b, Some("t1"));
        layout.choose(b, page(1));
        layout.choose(a, page(1));
        assert_eq!(
            shown(&layout, "t1"),
            [Page(TabId::test(1)), Elsewhere(page(1))]
        );
        assert!(layout.activate(b));
        assert!(!layout.activate(b));
        assert_eq!(
            shown(&layout, "t1"),
            [Elsewhere(page(1)), Page(TabId::test(1))]
        );
    }

    #[test]
    fn closing_groups_hands_on_width_and_use() {
        let (mut layout, mut ids, a) = layout();
        let (b, c) = (ids.next(), ids.next());
        layout.split(a, b, Some("t1"));
        layout.split(b, c, Some("t1"));
        assert!(layout.close(c));
        assert_eq!(layout.active(), b);
        assert_eq!(layout.share(b), 0.5);
        assert!(layout.close(a));
        assert_eq!(layout.share(b), 1.);
        assert!(!layout.close(b));
        assert_eq!(layout.len(), 1);
    }

    #[test]
    fn dividers_drag_between_neighbours_only() {
        let (mut layout, mut ids, a) = layout();
        let (b, c) = (ids.next(), ids.next());
        layout.split(a, b, Some("t1"));
        layout.split(b, c, Some("t1"));
        // Shares 0.5, 0.25, 0.25: the second divider sits at 0.75.
        assert!(layout.drag(1, 600., 1000.));
        assert!((layout.share(b) - 0.1).abs() < 1e-6);
        assert!((layout.share(c) - 0.4).abs() < 1e-6);
        assert_eq!(layout.share(a), 0.5);
        assert!(layout.drag(0, 0., 1000.));
        assert_eq!(layout.share(a), MIN_SHARE);
        assert!(!layout.drag(2, 10., 1000.));
        assert!(!layout.drag(0, f32::NAN, 1000.));
        assert!(!layout.drag(0, 10., 0.));
    }

    #[test]
    fn daemon_focus_moves_the_group_that_showed_the_old_tab() {
        let (mut layout, mut ids, a) = layout();
        let b = ids.next();
        layout.split(a, b, Some("t1"));
        layout.choose(a, herdr("t1"));
        layout.choose(b, page(1));
        layout.focus_moved(Some("t1"), "t2");
        assert_eq!(shown(&layout, "t2"), [Terminal, Page(TabId::test(1))]);
        // A group that asked for the tab keeps the others where they were.
        layout.choose(b, herdr("t3"));
        layout.focus_moved(Some("t2"), "t3");
        assert_eq!(shown(&layout, "t3"), [Elsewhere(herdr("t2")), Terminal]);
        // With no group on the terminal, the group in use takes it.
        layout.choose(a, page(2));
        layout.choose(b, page(3));
        layout.focus_moved(Some("t3"), "t4");
        assert_eq!(shown(&layout, "t4"), [Page(TabId::test(2)), Terminal]);
    }

    #[test]
    fn a_split_pins_the_terminal_tab_each_group_showed() {
        let (mut layout, mut ids, a) = layout();
        let b = ids.next();
        layout.split(a, b, Some("t1"));
        layout.choose(a, page(1));
        layout.focus_moved(Some("t1"), "t2");
        assert_eq!(shown(&layout, "t2"), [Page(TabId::test(1)), Terminal]);
    }

    #[test]
    fn closed_tabs_leave_every_group_that_picked_them() {
        let (mut layout, mut ids, a) = layout();
        let b = ids.next();
        layout.split(a, b, Some("t1"));
        layout.choose(a, page(1));
        layout.choose(b, page(1));
        assert!(layout.replace(&page(1), Some(page(2)), Some("t1")));
        assert_eq!(layout.picks().collect::<Vec<_>>(), [&page(2), &page(2)]);
        assert!(layout.replace(&page(2), None, Some("t1")));
        assert!(!layout.replace(&page(2), None, Some("t1")));
        assert_eq!(shown(&layout, "t1"), [Elsewhere(herdr("t1")), Terminal]);
    }

    #[test]
    fn selectors_keep_their_names_in_the_first_group() {
        let (_, mut ids, _) = layout();
        let mut slot = |index| Slot {
            id: ids.next(),
            index,
        };
        assert_eq!(slot(0).selector("tab-t0"), "tab-t0");
        assert_eq!(slot(2).selector("tab-t0"), "g2-tab-t0");
    }
}
