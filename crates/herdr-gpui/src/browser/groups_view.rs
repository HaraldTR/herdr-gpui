//! The window's editor groups: which group shows which tab, using and
//! splitting groups, and following the daemon's focus. The rules for what a
//! group draws live in [`super::groups`]; this applies them to the focused
//! workspace and carries out what they imply, such as focusing the Herdr tab
//! a group asks for.
use super::{
    Location, Scope, Tab, TabId,
    groups::{GroupId, Layout, Pick, Shown, Slot},
    view::store,
};
use crate::{HerdrWindow, NavigationTarget, search_input::SearchInput};
use gpui::{prelude::*, *};

impl HerdrWindow {
    /// The daemon's focused tab, when it belongs to the focused workspace.
    pub(crate) fn focused_herdr_tab(&self) -> Option<&str> {
        let snapshot = self.live.snapshot.as_ref()?;
        let tab = snapshot.focused_tab_id.as_deref()?;
        snapshot
            .tabs
            .iter()
            .any(|candidate| {
                candidate.tab_id == tab
                    && Some(&candidate.workspace_id) == snapshot.focused_workspace_id.as_ref()
            })
            .then_some(tab)
    }

    fn layout(&self) -> Option<&Layout> {
        self.browser.layouts.get(&self.browser_key()?)
    }

    fn layout_for(&mut self, key: (Scope, String)) -> &mut Layout {
        let browser = &mut self.browser;
        browser
            .layouts
            .entry(key)
            .or_insert_with(|| Layout::new(browser.group_ids.next()))
    }

    /// The focused workspace's groups, creating its first one. Render calls
    /// this before drawing so every group it draws has an ID.
    pub(crate) fn ensure_layout(&mut self) -> Option<&mut Layout> {
        let key = self.browser_key()?;
        Some(self.layout_for(key))
    }

    /// The focused workspace's groups, left to right. Without a workspace
    /// the window still draws one group, for the terminal area.
    pub(crate) fn group_slots(&self) -> Vec<Slot> {
        match self.layout() {
            Some(layout) => layout.slots().collect(),
            None => vec![Slot {
                id: self.browser.fallback_group,
                index: 0,
            }],
        }
    }

    pub(crate) fn is_split(&self) -> bool {
        self.layout().is_some_and(|layout| layout.len() > 1)
    }

    pub(crate) fn active_group(&self) -> Option<GroupId> {
        self.layout().map(Layout::active)
    }

    pub(crate) fn group_share(&self, group: GroupId) -> f32 {
        self.layout().map_or(1., |layout| layout.share(group))
    }

    /// The tab `group` picked, the focused one when it follows the terminal.
    pub(crate) fn group_pick(&self, group: GroupId) -> Option<Pick> {
        self.layout()?.pick(group, self.focused_herdr_tab())
    }

    /// What `group` draws. A page another window closed reads as nothing
    /// until the next tick forgets it.
    pub(crate) fn group_shown(&self, group: GroupId, cx: &App) -> Shown {
        let Some(layout) = self.layout() else {
            return Shown::Terminal;
        };
        let shown = layout.shown(group, self.focused_herdr_tab());
        match &shown {
            Shown::Page(id) | Shown::Elsewhere(Pick::Page(id))
                if store(cx).is_none_or(|store| store.get(*id).is_none()) =>
            {
                Shown::Empty
            }
            _ => shown,
        }
    }

    /// The group `pick` is live in, if any.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn group_showing(&self, pick: &Pick, cx: &App) -> Option<GroupId> {
        self.group_slots()
            .into_iter()
            .map(|slot| slot.id)
            .find(|group| match (self.group_shown(*group, cx), pick) {
                (Shown::Terminal, Pick::Herdr(_)) => self.group_pick(*group).as_ref() == Some(pick),
                (Shown::Page(id), Pick::Page(page)) => id == *page,
                _ => false,
            })
    }

    /// The pages live in some group, which the window presents.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn live_pages(&self, cx: &App) -> Vec<TabId> {
        self.group_slots()
            .into_iter()
            .filter_map(|slot| match self.group_shown(slot.id, cx) {
                Shown::Page(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// Focuses the Herdr tab `group` picked if the daemon focuses another,
    /// and hands the keyboard to the terminal when the group shows it.
    fn bring_group_tab(&mut self, group: GroupId, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused_herdr_tab().map(str::to_owned);
        if let Some(Pick::Herdr(tab)) = self.group_pick(group)
            && focused.as_deref() != Some(tab.as_str())
        {
            self.navigate(NavigationTarget::Tab(&tab), cx);
        }
        if matches!(self.group_pick(group), Some(Pick::Herdr(_)) | None) {
            window.focus(&self.focus, cx);
        }
    }

    /// Makes `group` the one in use, which brings it the tab it picked.
    pub(crate) fn activate_group(
        &mut self,
        group: GroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.ensure_layout() else {
            return;
        };
        if !layout.activate(group) {
            return;
        }
        self.bring_group_tab(group, window, cx);
        cx.notify();
    }

    /// Shows Herdr tab `tab` in `group`.
    pub(crate) fn choose_herdr_tab(
        &mut self,
        group: GroupId,
        tab: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.ensure_layout() else {
            return;
        };
        layout.choose(group, Pick::Herdr(tab.to_owned()));
        self.bring_group_tab(group, window, cx);
        cx.notify();
    }

    /// The next Herdr tab the daemon focuses goes to `group`: its "+" asked
    /// for it.
    pub(crate) fn expect_new_tab_in(&mut self, group: GroupId) {
        self.browser.new_tab_group = self.browser_key().map(|key| (key, group));
    }

    /// Follows the daemon moving its focus within the workspace, from a
    /// shortcut, an agent, a closed tab, or a group's "+".
    pub(crate) fn terminal_focus_moved(
        &mut self,
        old: Option<&str>,
        new: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.browser_key() else {
            return;
        };
        let asked = self
            .browser
            .new_tab_group
            .take_if(|(asked, _)| *asked == key)
            .map(|(_, group)| group);
        let layout = self.layout_for(key);
        match asked {
            Some(group) => layout.choose(group, Pick::Herdr(new.to_owned())),
            None => layout.focus_moved(old, new),
        }
        cx.notify();
    }

    /// Lets groups that picked a Herdr tab the daemon closed follow the
    /// terminal instead.
    pub(crate) fn forget_closed_herdr_tabs(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some(snapshot)) = (self.browser_key(), self.live.snapshot.clone()) else {
            return;
        };
        let focused = self.focused_herdr_tab().map(str::to_owned);
        let Some(layout) = self.browser.layouts.get_mut(&key) else {
            return;
        };
        let closed: Vec<Pick> = layout
            .picks()
            .filter(|pick| {
                matches!(pick, Pick::Herdr(tab) if !snapshot.tabs.iter().any(|candidate| {
                    &candidate.tab_id == tab && candidate.workspace_id == key.1
                }))
            })
            .cloned()
            .collect();
        for pick in &closed {
            layout.replace(pick, None, focused.as_deref());
        }
        if !closed.is_empty() {
            cx.notify();
        }
    }

    /// Shows a browser tab in the group in use.
    pub(crate) fn show_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_browser_tab_in(None, id, window, cx);
    }

    /// Shows a browser tab in `group`, or in the group in use of the tab's
    /// workspace.
    pub(crate) fn show_browser_tab_in(
        &mut self,
        group: Option<GroupId>,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = store(cx).and_then(|store| store.get(id)).cloned() else {
            return;
        };
        let layout = self.layout_for((tab.scope.clone(), tab.workspace_id.clone()));
        let group = group.unwrap_or(layout.active());
        layout.choose(group, Pick::Page(id));
        #[cfg(any(target_os = "macos", windows))]
        if let Err(error) = self.browser.pages.ensure(&tab, window, cx) {
            tracing::warn!(%error, "Cannot create a browser page");
            self.browser.failed = Some((id, error.to_string().into()));
        }
        self.sync_address(group, Some(&tab), true, window, cx);
        if tab.location.is_none() {
            let focus = self.group_address(group, cx).read(cx).focus.clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Opens a blank browser tab in `group`.
    pub(crate) fn open_browser_tab_in(
        &mut self,
        group: GroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(layout) = self.ensure_layout() {
            layout.activate(group);
        }
        self.open_browser_tab(None, window, cx);
    }

    /// Closes a browser tab. Groups that picked it move to the browser tab
    /// after it, or before it, or else follow the terminal.
    pub(crate) fn close_browser_tab(
        &mut self,
        id: TabId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = store(cx).and_then(|store| store.get(id)).cloned() else {
            return;
        };
        let order: Vec<TabId> = store(cx)
            .map(|store| {
                store
                    .in_workspace(&tab.scope, &tab.workspace_id)
                    .map(|tab| tab.id)
                    .collect()
            })
            .unwrap_or_default();
        let position = order.iter().position(|tab| *tab == id).unwrap_or(0);
        let next = order
            .get(position + 1)
            .or_else(|| position.checked_sub(1).and_then(|before| order.get(before)))
            .copied();
        let key = (tab.scope.clone(), tab.workspace_id.clone());
        let focused = (self.browser_key().as_ref() == Some(&key))
            .then(|| self.focused_herdr_tab().map(str::to_owned))
            .flatten();
        if let Some(layout) = self.browser.layouts.get_mut(&key) {
            layout.replace(&Pick::Page(id), next.map(Pick::Page), focused.as_deref());
        }
        crate::browser::Store::update(cx, |store| store.close(id));
        self.forget_browser_tabs(|tab| tab == id);
        cx.notify();
    }

    /// Splits `group`: a new group to its right shows the same tab.
    pub(crate) fn split_group(
        &mut self,
        group: GroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new = self.browser.group_ids.next();
        let focused = self.focused_herdr_tab().map(str::to_owned);
        let Some(layout) = self.ensure_layout() else {
            return;
        };
        if !layout.split(group, new, focused.as_deref()) {
            return;
        }
        self.bring_group_tab(new, window, cx);
        self.sync_addresses(true, window, cx);
        cx.notify();
    }

    /// Splits the group in use.
    pub(crate) fn split_active_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(group) = self.ensure_layout().map(|layout| layout.active()) {
            self.split_group(group, window, cx);
        }
    }

    /// Closes `group`; its tabs stay in every other group's strip.
    pub(crate) fn close_group(
        &mut self,
        group: GroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.ensure_layout() else {
            return;
        };
        if !layout.close(group) {
            return;
        }
        let active = layout.active();
        self.browser.addresses.remove(&group);
        self.bring_group_tab(active, window, cx);
        self.sync_addresses(true, window, cx);
        cx.notify();
    }

    /// Moves the divider after the `divider`th group; see [`Layout::drag`].
    pub(crate) fn drag_divider(&mut self, divider: usize, offset: f32, width: f32) -> bool {
        self.ensure_layout()
            .is_some_and(|layout| layout.drag(divider, offset, width))
    }

    /// `group`'s address field, made the first time it shows a page.
    pub(crate) fn group_address(&mut self, group: GroupId, cx: &mut App) -> Entity<SearchInput> {
        self.browser
            .addresses
            .entry(group)
            .or_insert_with(|| {
                let input = cx.new(|cx| {
                    let mut input = SearchInput::new(cx);
                    input.set_placeholder("Enter an address", cx);
                    input
                });
                (input, None)
            })
            .0
            .clone()
    }

    /// Shows each live page's address in its group's field, unless someone
    /// is typing there.
    pub(crate) fn sync_addresses(&mut self, force: bool, window: &Window, cx: &mut Context<Self>) {
        for slot in self.group_slots() {
            if let Shown::Page(id) = self.group_shown(slot.id, cx) {
                let tab = store(cx).and_then(|store| store.get(id)).cloned();
                self.sync_address(slot.id, tab.as_ref(), force, window, cx);
            }
        }
    }

    pub(crate) fn sync_address(
        &mut self,
        group: GroupId,
        tab: Option<&Tab>,
        force: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.group_address(group, cx);
        let id = tab.map(|tab| tab.id);
        let text = tab
            .and_then(|tab| tab.location.as_ref())
            .map(Location::display)
            .unwrap_or_default();
        let shown = self.browser.addresses.get(&group).and_then(|(_, tab)| *tab);
        let field = input.read(cx);
        if !force
            && (field.focus.is_focused(window) || (shown == id && field.text() == text.as_str()))
        {
            return;
        }
        if let Some((_, tab)) = self.browser.addresses.get_mut(&group) {
            *tab = id;
        }
        input.update(cx, |input, cx| input.set_text_selected(&text, cx));
    }
}
