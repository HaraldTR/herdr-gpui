//! The window side of browser tabs: their entries in the tab strip, the page
//! and its toolbar in place of the terminal or beside it in a split, and
//! requests to open one.
#[cfg(any(target_os = "macos", windows))]
use super::Annotations;
use super::{
    Location, Scope, Store, Tab, TabId, WebUrl,
    groups::{Content, Groups, Side, Split},
};
use crate::{HerdrWindow, search_input::SearchInput, window::Flash};
#[cfg(unix)]
use crate::{
    NavigationTarget,
    control::{Placed, Target},
};
use gpui::{prelude::*, *};
use std::collections::{HashMap, HashSet};

/// One window's browser state. Which tab each side shows is the window's own
/// choice, like its focused workspace; the tabs themselves are the app's.
pub(crate) struct Browser {
    /// Per workspace, what each side shows. A workspace missing here shows
    /// its terminal on the left and nothing on the right.
    groups: HashMap<(Scope, String), Groups>,
    /// Set while the window shows two sides.
    split: Option<Split>,
    #[cfg(any(target_os = "macos", windows))]
    pub(super) pages: super::Pages,
    /// Each side's address field, and the tab whose address it last showed.
    address: [Entity<SearchInput>; 2],
    address_tab: [Option<TabId>; 2],
    /// Why a tab's page could not be created, shown in its place.
    failed: Option<(TabId, SharedString)>,
    /// The workspaces of the last snapshot and the boot they came from: one
    /// missing from the next snapshot of the same boot was closed.
    workspaces: Option<(Scope, String, HashSet<String>)>,
    #[cfg(any(target_os = "macos", windows))]
    pub(super) annotations: Annotations,
}

impl Browser {
    pub(crate) fn new(cx: &mut App) -> Self {
        let address = |cx: &mut App| {
            cx.new(|cx| {
                let mut input = SearchInput::new(cx);
                input.set_placeholder("Enter an address", cx);
                input
            })
        };
        Self {
            groups: HashMap::new(),
            split: None,
            #[cfg(any(target_os = "macos", windows))]
            pages: Default::default(),
            address: [address(cx), address(cx)],
            address_tab: [None, None],
            failed: None,
            workspaces: None,
            #[cfg(any(target_os = "macos", windows))]
            annotations: Annotations::new(cx),
        }
    }
}

/// Names the daemon behind an endpoint the way browser tabs remember it.
pub(crate) fn scope(endpoint: &crate::endpoint::Endpoint) -> Scope {
    match endpoint.connection.target.socket_path() {
        Ok(path) => Scope::local(&path),
        Err(_) => Scope::endpoint(&endpoint.id),
    }
}

/// Page titles run long; a tab shows the start of one, like a web browser.
fn tab_label(title: &str) -> SharedString {
    const MAX_CHARS: usize = 28;
    match title.char_indices().nth(MAX_CHARS) {
        Some((end, _)) => format!("{}\u{2026}", title[..end].trim_end()).into(),
        None => title.to_owned().into(),
    }
}

fn store(cx: &App) -> Option<&Store> {
    cx.try_global::<Store>()
}

impl HerdrWindow {
    fn browser_key(&self) -> Option<(Scope, String)> {
        let workspace = self.live.snapshot.as_ref()?.focused_workspace_id.clone()?;
        Some((scope(&self.endpoints[self.selected_endpoint]), workspace))
    }

    /// The window's split, if it shows two sides.
    pub(crate) fn split(&self) -> Option<Split> {
        self.browser.split
    }

    /// The side that has the keyboard: the only one when unsplit.
    pub(crate) fn active_side(&self) -> Side {
        self.browser.split.map_or(Side::Left, |split| split.active)
    }

    /// The sides the window draws, left first.
    pub(crate) fn visible_sides(&self) -> &'static [Side] {
        if self.browser.split.is_some() {
            &Side::BOTH
        } else {
            &[Side::Left]
        }
    }

    /// The focused workspace's sides, with pages the app no longer has read
    /// as closed until the next tick forgets them.
    pub(crate) fn browser_groups(&self, cx: &App) -> Groups {
        let mut groups = self
            .browser_key()
            .and_then(|key| self.browser.groups.get(&key).copied())
            .unwrap_or_default();
        let store = store(cx);
        groups.retain(|id| store.is_some_and(|store| store.get(id).is_some()));
        groups
    }

    /// The focused workspace's browser tabs, in the order they opened.
    pub(crate) fn browser_tab_ids(&self, cx: &App) -> Vec<TabId> {
        let (Some((scope, workspace)), Some(store)) = (self.browser_key(), store(cx)) else {
            return Vec::new();
        };
        store
            .in_workspace(&scope, &workspace)
            .map(|tab| tab.id)
            .collect()
    }

    /// What `side` shows in the focused workspace.
    pub(crate) fn side_content(&self, side: Side, cx: &App) -> Content {
        self.browser_groups(cx).content(side)
    }

    fn tab_on(&self, side: Side, cx: &App) -> Option<Tab> {
        match self.side_content(side, cx) {
            Content::Page(id) => store(cx)?.get(id).cloned(),
            Content::Terminal | Content::Empty => None,
        }
    }

    /// Makes `side` the one with the keyboard. The terminal takes it back
    /// when it is what that side shows.
    pub(crate) fn activate_side(
        &mut self,
        side: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(split) = &mut self.browser.split else {
            return;
        };
        if split.active == side {
            return;
        }
        split.active = side;
        if self.side_content(side, cx) == Content::Terminal {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Uncovers the focused workspace's terminal on the side it sits on.
    pub(crate) fn show_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.browser_key()
            && let Some(groups) = self.browser.groups.get_mut(&key)
        {
            let before = *groups;
            groups.uncover_terminal();
            if *groups != before {
                cx.notify();
            }
        }
    }

    /// Shows the focused workspace's terminal on `side`, trading places
    /// with whatever the other side showed if the terminal was there.
    pub(crate) fn show_terminal_on(&mut self, side: Side, cx: &mut Context<Self>) {
        let Some(key) = self.browser_key() else {
            return;
        };
        self.browser
            .groups
            .entry(key)
            .or_default()
            .show(side, Content::Terminal);
        if let Some(split) = &mut self.browser.split {
            split.active = side;
        }
        cx.notify();
    }

    /// Shows a tab on the active side.
    pub(crate) fn show_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_browser_tab_on(self.active_side(), id, window, cx);
    }

    pub(crate) fn show_browser_tab_on(
        &mut self,
        side: Side,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = store(cx).and_then(|store| store.get(id)).cloned() else {
            return;
        };
        let side = if self.browser.split.is_some() {
            side
        } else {
            Side::Left
        };
        self.browser
            .groups
            .entry((tab.scope.clone(), tab.workspace_id.clone()))
            .or_default()
            .show(side, Content::Page(id));
        if let Some(split) = &mut self.browser.split {
            split.active = side;
        }
        #[cfg(any(target_os = "macos", windows))]
        if let Err(error) = self.browser.pages.ensure(&tab, window, cx) {
            tracing::warn!(%error, "Cannot create a browser page");
            self.browser.failed = Some((id, error.to_string().into()));
        }
        self.sync_address(side, Some(&tab), true, window, cx);
        // A swap may have moved another page onto the other side.
        if self.browser.split.is_some() {
            let other = self.tab_on(side.other(), cx);
            self.sync_address(side.other(), other.as_ref(), false, window, cx);
        }
        if tab.location.is_none() {
            let focus = self.browser.address[side.index()].read(cx).focus.clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Closes a tab. A side of a split that showed it moves to a neighbouring
    /// tab the other side does not show, as an editor group does.
    pub(crate) fn close_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = store(cx).and_then(|store| store.get(id)).cloned() else {
            return;
        };
        let key = (tab.scope.clone(), tab.workspace_id.clone());
        let order: Vec<TabId> = store(cx)
            .map(|store| {
                store
                    .in_workspace(&key.0, &key.1)
                    .map(|tab| tab.id)
                    .collect()
            })
            .unwrap_or_default();
        let before = self.browser.groups.get(&key).copied().unwrap_or_default();
        Store::update(cx, |store| store.close(id));
        self.forget_browser_tabs(|tab| tab == id);
        if let Some(split) = self.browser.split {
            let after = self.browser.groups.get(&key).copied().unwrap_or_default();
            let position = order.iter().position(|tab| *tab == id).unwrap_or(0);
            for side in Side::BOTH {
                if before.content(side) != Content::Page(id)
                    || after.content(side) != Content::Empty
                {
                    continue;
                }
                let next = order[position + 1..]
                    .iter()
                    .chain(order[..position].iter().rev())
                    .copied()
                    .find(|tab| *tab != id && after.side_of(Content::Page(*tab)).is_none());
                if let Some(next) = next {
                    self.show_browser_tab_on(side, next, window, cx);
                }
            }
            // Filling a side is not choosing it.
            if let Some(current) = &mut self.browser.split {
                current.active = split.active;
            }
        }
        cx.notify();
    }

    /// Closes the focused workspace's browser tabs that `close` picks.
    pub(crate) fn close_browser_tabs(
        &mut self,
        mut close: impl FnMut(TabId) -> bool,
        cx: &mut Context<Self>,
    ) {
        let closing: Vec<TabId> = self
            .browser_tab_ids(cx)
            .into_iter()
            .filter(|id| close(*id))
            .collect();
        if closing.is_empty() {
            return;
        }
        Store::update(cx, |store| {
            for id in &closing {
                store.close(*id);
            }
        });
        self.forget_browser_tabs(|id| closing.contains(&id));
        cx.notify();
    }

    fn forget_browser_tabs(&mut self, mut gone: impl FnMut(TabId) -> bool) {
        for groups in self.browser.groups.values_mut() {
            groups.retain(|id| !gone(id));
        }
        #[cfg(any(target_os = "macos", windows))]
        self.browser.pages.retain(|id| !gone(id));
        if self
            .browser
            .failed
            .as_ref()
            .is_some_and(|(id, _)| gone(*id))
        {
            self.browser.failed = None;
        }
        #[cfg(any(target_os = "macos", windows))]
        let annotated: Vec<TabId> = self
            .browser
            .annotations
            .ids()
            .filter(|id| gone(*id))
            .collect();
        #[cfg(any(target_os = "macos", windows))]
        for id in annotated {
            self.browser.annotations.forget(id);
        }
    }

    /// Opens a tab in the focused workspace, or tells the user why not.
    pub(crate) fn open_browser_tab(
        &mut self,
        url: Option<WebUrl>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !super::EMBEDDED {
            match url {
                Some(url) => cx.open_url(url.as_str()),
                None => self.show_flash(Flash::warning("Browser tabs need macOS or Windows"), cx),
            }
            return;
        }
        let Some((scope, workspace)) = self.browser_key() else {
            self.show_flash(Flash::warning("Open a workspace first"), cx);
            return;
        };
        let location = url.map(|url| Location::Web { url });
        match Store::update(cx, |store| store.open(scope, &workspace, location, None)) {
            Some(id) => self.show_browser_tab(id, window, cx),
            None => self.show_flash(Flash::warning("Too many browser tabs are open"), cx),
        }
    }

    /// Opens a tab on `side`.
    pub(crate) fn open_browser_tab_on(
        &mut self,
        side: Side,
        url: Option<WebUrl>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(split) = &mut self.browser.split {
            split.active = side;
        }
        self.open_browser_tab(url, window, cx);
    }

    /// Moves the split's divider; see [`Split::drag`].
    pub(crate) fn drag_split(&mut self, offset: f32, width: f32) -> bool {
        self.browser
            .split
            .as_mut()
            .is_some_and(|split| split.drag(offset, width))
    }

    /// Splits the window in two, or closes the right side of a split.
    pub(crate) fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.split.is_some() {
            self.close_split(Side::Left, window, cx);
        } else {
            self.split_editor(window, cx);
        }
    }

    /// Splits the window: the page the window showed moves right with the
    /// terminal back on the left, or, over the terminal, the right side shows
    /// the workspace's latest browser tab, or a new one when it has none.
    fn split_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !super::EMBEDDED {
            self.show_flash(
                Flash::warning("Split needs browser tabs, which need macOS or Windows"),
                cx,
            );
            return;
        }
        let Some((scope, workspace)) = self.browser_key() else {
            self.show_flash(Flash::warning("Open a workspace first"), cx);
            return;
        };
        let shown = self.side_content(Side::Left, cx);
        self.browser.split = Some(Split::default());
        let latest = store(cx).and_then(|store| {
            store
                .in_workspace(&scope, &workspace)
                .last()
                .map(|tab| tab.id)
        });
        match (shown, latest) {
            (Content::Page(id), _) | (_, Some(id)) => {
                self.show_browser_tab_on(Side::Right, id, window, cx);
            }
            (_, None) => self.open_browser_tab(None, window, cx),
        }
        cx.notify();
    }

    /// Folds the split into one side showing what `kept` showed. Its tabs
    /// stay in the strip, as the other side's do.
    pub(crate) fn close_split(&mut self, kept: Side, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.split.take().is_none() {
            return;
        }
        for groups in self.browser.groups.values_mut() {
            groups.unsplit(kept);
        }
        self.browser.address_tab[Side::Right.index()] = None;
        self.sync_addresses(true, window, cx);
        if self.side_content(Side::Left, cx) == Content::Terminal {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The endpoint and workspace a control request names, if this window
    /// shows it. `strict` requires the caller's own daemon; otherwise any
    /// endpoint showing the named workspace qualifies. Only the Unix control
    /// socket asks.
    #[cfg(unix)]
    fn browser_target(&self, target: &Target<'_>, strict: bool) -> Option<(usize, String)> {
        self.endpoints
            .iter()
            .enumerate()
            .find_map(|(index, endpoint)| {
                if strict {
                    let matches = match target.daemon {
                        Some(daemon) => {
                            endpoint.connection.target.socket_path().ok().as_deref() == Some(daemon)
                        }
                        None => index == self.selected_endpoint,
                    };
                    if !matches {
                        return None;
                    }
                } else if target.workspace.is_none() {
                    return None;
                }
                let live = if index == self.selected_endpoint {
                    &self.live
                } else {
                    &endpoint.live
                };
                let snapshot = live.snapshot.as_ref()?;
                let workspace = match target.workspace {
                    Some(id) => id,
                    None => snapshot.focused_workspace_id.as_deref()?,
                };
                snapshot
                    .workspaces
                    .iter()
                    .any(|candidate| candidate.workspace_id == workspace)
                    .then(|| (index, workspace.to_owned()))
            })
    }

    /// Opens the tab a control request asks for, if this window shows its
    /// workspace. `None` leaves the request to another window.
    #[cfg(unix)]
    pub(crate) fn open_requested_browser_tab(
        &mut self,
        target: &Target<'_>,
        strict: bool,
        location: &Location,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Placed> {
        let (index, workspace) = self.browser_target(target, strict)?;
        let scope = scope(&self.endpoints[index]);
        // An agent showing the same page again gets its tab back, reloaded,
        // rather than another tab for every revision.
        let origin = target.pane;
        let before =
            store(cx).and_then(|store| store.opened_before(&scope, &workspace, origin, location));
        let id = match before {
            Some(id) => {
                #[cfg(any(target_os = "macos", windows))]
                self.browser.pages.reload(id, cx);
                id
            }
            None => {
                let opened = Store::update(cx, |store| {
                    store.open(
                        scope,
                        &workspace,
                        Some(location.clone()),
                        origin.map(str::to_owned),
                    )
                });
                let Some(id) = opened else {
                    return Some(Placed::Full);
                };
                id
            }
        };
        if focus {
            let focused = self
                .live
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.focused_workspace_id.as_deref());
            if index != self.selected_endpoint {
                let endpoint = self.endpoints[index].id.clone();
                self.navigate_endpoint(&endpoint, NavigationTarget::Workspace(&workspace), cx);
            } else if focused != Some(workspace.as_str()) {
                self.navigate(NavigationTarget::Workspace(&workspace), cx);
            }
            // Recorded against the workspace, so the tab shows once the
            // navigation lands even if it is still in flight.
            self.show_browser_tab(id, window, cx);
        }
        cx.notify();
        Some(Placed::Opened {
            workspace_id: workspace,
        })
    }

    /// Reloads this window's pages for `tabs`, as an agent asks after
    /// editing a page it showed.
    #[cfg(unix)]
    pub(crate) fn reload_browser_tabs(&mut self, tabs: &[TabId], cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        for id in tabs {
            self.browser.pages.reload(*id, cx);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = (tabs, cx);
    }

    /// Shows each visible side's tab address in its field, unless someone
    /// is typing there.
    fn sync_addresses(&mut self, force: bool, window: &Window, cx: &mut Context<Self>) {
        for side in self.visible_sides() {
            let tab = self.tab_on(*side, cx);
            self.sync_address(*side, tab.as_ref(), force, window, cx);
        }
    }

    fn sync_address(
        &mut self,
        side: Side,
        tab: Option<&Tab>,
        force: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let index = side.index();
        let id = tab.map(|tab| tab.id);
        let text = tab
            .and_then(|tab| tab.location.as_ref())
            .map(Location::display)
            .unwrap_or_default();
        let input = self.browser.address[index].read(cx);
        if !force
            && (input.focus.is_focused(window)
                || (self.browser.address_tab[index] == id && input.text() == text.as_str()))
        {
            return;
        }
        self.browser.address_tab[index] = id;
        self.browser.address[index].update(cx, |input, cx| input.set_text_selected(&text, cx));
    }

    /// Applies page reports, drops tabs other windows closed, and forgets the
    /// tabs of workspaces the daemon closed. Runs on every window tick.
    pub(crate) fn poll_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        self.apply_page_events(window, cx);
        if let Some(store) = store(cx) {
            let gone: HashSet<TabId> = self
                .browser
                .groups
                .values()
                .flat_map(Groups::pages)
                .filter(|id| store.get(*id).is_none())
                .collect();
            #[cfg(any(target_os = "macos", windows))]
            let gone: HashSet<TabId> = gone
                .into_iter()
                .chain(
                    self.browser
                        .pages
                        .ids()
                        .filter(|id| store.get(*id).is_none()),
                )
                .collect();
            if !gone.is_empty() {
                self.forget_browser_tabs(|id| gone.contains(&id));
                cx.notify();
            }
        }
        self.forget_closed_workspaces(cx);
        #[cfg(any(target_os = "macos", windows))]
        self.poll_deliveries(cx);
        self.sync_addresses(false, window, cx);
    }

    fn forget_closed_workspaces(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.live.snapshot.as_ref() else {
            return;
        };
        let scope = scope(&self.endpoints[self.selected_endpoint]);
        let unchanged = self
            .browser
            .workspaces
            .as_ref()
            .is_some_and(|(seen_scope, boot, seen)| {
                seen_scope == &scope
                    && boot == &snapshot.boot_id
                    && seen.len() == snapshot.workspaces.len()
                    && snapshot
                        .workspaces
                        .iter()
                        .all(|workspace| seen.contains(&workspace.workspace_id))
            });
        if unchanged {
            return;
        }
        let current: HashSet<String> = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect();
        let previous = self.browser.workspaces.replace((
            scope.clone(),
            snapshot.boot_id.clone(),
            current.clone(),
        ));
        // A restarted daemon or another session proves nothing was closed.
        let Some((_, _, seen)) = previous
            .filter(|(seen_scope, boot, _)| seen_scope == &scope && boot == &snapshot.boot_id)
        else {
            return;
        };
        let closed: Vec<String> = seen.difference(&current).cloned().collect();
        if closed.is_empty()
            || !store(cx).is_some_and(|store| store.has_workspaces(&scope, &closed))
        {
            return;
        }
        Store::update(cx, |store| store.forget_workspaces(&scope, &closed));
        cx.notify();
    }

    #[cfg(any(target_os = "macos", windows))]
    fn apply_page_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::native::Event;
        let events: Vec<Event> = self.browser.pages.drain().collect();
        for event in events {
            match event {
                Event::Title(id, title) => {
                    Store::update(cx, |store| store.visited(id, None, Some(&title)));
                }
                // The field follows on the next sync, unless someone is
                // typing in it.
                Event::Loaded(id, url) => {
                    let visited = store(cx)
                        .and_then(|store| store.get(id))
                        .and_then(|tab| tab.location.as_ref()?.visited(&url));
                    if let Some(location) = visited {
                        Store::update(cx, |store| store.visited(id, Some(location), None));
                    }
                    self.page_loaded(id, cx);
                }
                Event::Posted(id, body) => self.page_posted(id, &body, window, cx),
                #[cfg(target_os = "macos")]
                Event::Captured(id, capture, tiff) => self.page_captured(id, capture, tiff, cx),
                Event::NewWindow(id, url) => {
                    let parent = store(cx).and_then(|store| store.get(id)).cloned();
                    if let (Some(parent), Ok(url)) = (parent, WebUrl::try_from(url.as_str())) {
                        // The new tab opens where its opener shows.
                        let side = self
                            .browser
                            .groups
                            .get(&(parent.scope.clone(), parent.workspace_id.clone()))
                            .and_then(|groups| groups.side_of(Content::Page(id)))
                            .unwrap_or(self.active_side());
                        let opened = Store::update(cx, |store| {
                            store.open(
                                parent.scope,
                                &parent.workspace_id,
                                Some(Location::Web { url }),
                                parent.origin,
                            )
                        });
                        if let Some(opened) = opened {
                            self.show_browser_tab_on(side, opened, window, cx);
                        }
                    }
                }
            }
        }
    }

    fn submit_address(
        &mut self,
        side: Side,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.browser.address[side.index()]
            .read(cx)
            .text()
            .to_owned();
        let Ok(url) = WebUrl::from_typed(&text) else {
            self.show_flash(Flash::warning("Not an http or https address"), cx);
            return;
        };
        let location = Location::Web { url };
        Store::update(cx, |store| store.visited(id, Some(location.clone()), None));
        #[cfg(any(target_os = "macos", windows))]
        if self.browser.pages.contains(id) {
            self.browser.pages.load(id, &location, cx);
            self.browser.pages.focus(id, cx);
        }
        self.show_browser_tab_on(side, id, window, cx);
    }

    /// Shows or hides the native pages to match what the window draws. The
    /// pages sit above everything GPUI paints, so any overlay hides them.
    pub(crate) fn present_browser(&mut self, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        {
            let shown: Vec<TabId> = if self.menu.page.is_some() {
                Vec::new()
            } else {
                self.visible_sides()
                    .iter()
                    .filter_map(|side| match self.side_content(*side, cx) {
                        Content::Page(id) => Some(id),
                        Content::Terminal | Content::Empty => None,
                    })
                    .collect()
            };
            self.browser.pages.present(&shown, cx);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = cx;
    }

    /// The workspace's browser tabs, after its Herdr tabs in `side`'s strip.
    pub(crate) fn browser_tab_entries(
        &self,
        side: Side,
        shown: Option<TabId>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let (Some((scope, workspace)), Some(store)) = (self.browser_key(), store(cx)) else {
            return Vec::new();
        };
        let tabs: Vec<Tab> = store.in_workspace(&scope, &workspace).cloned().collect();
        tabs.into_iter()
            .map(|tab| {
                let id = tab.id;
                let (background, text) = self.tab_colors(shown == Some(id), side);
                div()
                    .id(SharedString::from(format!("browser-tab-{id}")))
                    .debug_selector(move || side.selector(&format!("browser-tab-{id}")))
                    .pl(px(10.))
                    .pr(px(3.))
                    .py(px(2.))
                    .min_w(px(crate::TAB_WIDTH))
                    .border_r_1()
                    .border_color(rgb(self.theme.active))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .bg(rgb(background))
                    .text_color(rgb(text))
                    .child(
                        svg()
                            .path("icons/globe.svg")
                            .size(px(12.))
                            .flex_none()
                            .text_color(rgb(text)),
                    )
                    .child(tab_label(&tab.title))
                    .child(
                        div()
                            .id("close-browser-tab")
                            .debug_selector(move || {
                                side.selector(&format!("close-browser-tab-{id}"))
                            })
                            .size(px(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(crate::config::corners::CONTROL))
                            .hover(move |s| s.bg(rgba((text << 8) | 0x24)))
                            .child(
                                svg()
                                    .path("icons/close.svg")
                                    .size(px(12.))
                                    .text_color(rgb(text)),
                            )
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_browser_tab(id, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_browser_tab_on(side, id, window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn toolbar_button(
        &self,
        side: Side,
        id: &'static str,
        icon: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let color = if enabled {
            self.theme.foreground
        } else {
            self.theme.muted
        };
        div()
            .id(id)
            .debug_selector(move || side.selector(id))
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::config::corners::CONTROL))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(self.theme.active)))
                    .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
            })
            .child(svg().path(icon).size(px(14.)).text_color(rgb(color)))
    }

    /// The page with its toolbar, drawn on `side` where the terminal would
    /// be. `keyboard` marks the one element holding the window's focus
    /// handle when no terminal is drawn to hold it.
    pub(crate) fn render_browser(
        &mut self,
        side: Side,
        tab: &Tab,
        gap: f32,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = tab.id;
        let loaded = tab.location.is_some();
        let external = match &tab.location {
            Some(Location::Web { url }) => Some(url.clone()),
            _ => None,
        };
        #[cfg(any(target_os = "macos", windows))]
        let (annotate_button, panel) = {
            let annotating = self.browser.annotations.armed(id);
            let panel = self
                .browser
                .annotations
                .open(id)
                .then(|| self.render_annotations(tab, cx));
            let button = {
                // Annotating needs the page itself, so a blank tab or a
                // build without pages has nothing to annotate.
                let enabled = loaded && super::EMBEDDED;
                let color = if annotating {
                    self.theme.text_on(self.theme.primary_wash())
                } else if enabled {
                    self.theme.foreground
                } else {
                    self.theme.muted
                };
                div()
                    .id("browser-annotate")
                    .debug_selector(move || side.selector("browser-annotate"))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(24.))
                    .px(px(6.))
                    .rounded(px(crate::config::corners::CONTROL))
                    .when(annotating, |button| {
                        button.bg(rgb(self.theme.primary_wash()))
                    })
                    .text_color(rgb(color))
                    .child(
                        svg()
                            .path("icons/pencil.svg")
                            .size(px(13.))
                            .text_color(rgb(color)),
                    )
                    .child("Annotate")
                    .when(enabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_annotating(id, window, cx);
                            }))
                    })
            };
            (Some(button), panel)
        };
        // Linux builds show no pages, so there is nothing to annotate.
        #[cfg(not(any(target_os = "macos", windows)))]
        let (annotate_button, panel): (Option<Stateful<Div>>, Option<AnyElement>) = (None, None);
        #[cfg(any(target_os = "macos", windows))]
        let page = self.browser.pages.page(id).cloned();
        #[cfg(not(any(target_os = "macos", windows)))]
        let page: Option<AnyView> = None;
        let failure = self
            .browser
            .failed
            .as_ref()
            .filter(|(failed, _)| *failed == id)
            .map(|(_, message)| message.clone());
        let placeholder: SharedString = match (&failure, loaded) {
            (Some(message), _) => format!("Could not show this page: {message}").into(),
            (None, false) => "Type an address above and press Return.".into(),
            (None, true) if !super::EMBEDDED => {
                "This build cannot show pages in the window; open it in the system browser.".into()
            }
            (None, true) => "Loading\u{2026}".into(),
        };
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .py(px(4.))
            .bg(rgb(self.theme.surface))
            .border_b_1()
            .border_color(rgb(self.theme.active))
            .child(self.toolbar_button(
                side,
                "browser-back",
                "icons/arrow-left.svg",
                loaded,
                cx,
                move |this, _, cx| {
                    #[cfg(any(target_os = "macos", windows))]
                    this.browser.pages.back(id, cx);
                    #[cfg(not(any(target_os = "macos", windows)))]
                    let _ = (this, cx);
                },
            ))
            .child(self.toolbar_button(
                side,
                "browser-forward",
                "icons/arrow-right.svg",
                loaded,
                cx,
                move |this, _, cx| {
                    #[cfg(any(target_os = "macos", windows))]
                    this.browser.pages.forward(id, cx);
                    #[cfg(not(any(target_os = "macos", windows)))]
                    let _ = (this, cx);
                },
            ))
            .child(self.toolbar_button(
                side,
                "browser-reload",
                "icons/refresh.svg",
                loaded,
                cx,
                move |this, _, cx| {
                    #[cfg(any(target_os = "macos", windows))]
                    this.browser.pages.reload(id, cx);
                    #[cfg(not(any(target_os = "macos", windows)))]
                    let _ = (this, cx);
                },
            ))
            .child(
                div()
                    .id("browser-address")
                    .debug_selector(move || side.selector("browser-address"))
                    .flex_1()
                    .min_w_0()
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "enter" => this.submit_address(side, id, window, cx),
                            "escape" => {
                                let tab = store(cx).and_then(|store| store.get(id)).cloned();
                                this.sync_address(side, tab.as_ref(), true, window, cx);
                                window.focus(&this.focus, cx);
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(self.browser.address[side.index()].clone()),
            )
            // A split shows the window's flash once, on the side in use.
            .children(
                self.flash
                    .as_ref()
                    .filter(|_| side == self.active_side())
                    .map(|(flash, _)| {
                        div()
                            .flex_none()
                            .max_w(px(240.))
                            .truncate()
                            .text_color(rgb(flash.accent(&self.theme)))
                            .child(flash.text.clone())
                    }),
            )
            .children(annotate_button)
            .child(self.toolbar_button(
                side,
                "browser-external",
                "icons/external.svg",
                external.is_some(),
                cx,
                move |_, _, cx| {
                    if let Some(url) = &external {
                        cx.open_url(url.as_str());
                    }
                },
            ));
        let content = match (page, &failure) {
            (Some(page), None) => div().flex_1().min_h_0().child(page).into_any_element(),
            _ => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                .text_color(rgb(self.theme.muted))
                .child(
                    div()
                        .debug_selector(move || side.selector("browser-placeholder"))
                        .child(placeholder),
                )
                .into_any_element(),
        };
        div()
            .id(SharedString::from(side.selector("browser")))
            .debug_selector(move || side.selector("browser"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pl(px(gap))
            .bg(rgb(self.theme.background))
            // Keeps window shortcuts reachable while the page does not hold
            // the keyboard; nothing here types into a terminal. A focus
            // handle belongs to one element, so a drawn terminal keeps it.
            .when(keyboard, |browser| browser.track_focus(&self.focus))
            .child(toolbar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex().flex_col().flex_1().min_w_0().child(content))
                    .children(panel),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_titles_are_shortened_on_a_character_boundary() {
        assert_eq!(super::tab_label("Example Domain"), "Example Domain");
        let long = "Rust Programming Language — Official Site";
        assert_eq!(
            super::tab_label(long),
            "Rust Programming Language \u{2014}\u{2026}"
        );
        assert_eq!(super::tab_label(&"é".repeat(40)).chars().count(), 29);
    }
}
