//! The window side of browser tabs: their entries in the tab strip, the page
//! and its toolbar in place of the terminal, and requests to open one.
#[cfg(any(target_os = "macos", windows))]
use super::Annotations;
use super::{Location, Scope, Store, Tab, TabId, WebUrl};
use crate::{HerdrWindow, search_input::SearchInput, window::Flash};
#[cfg(unix)]
use crate::{
    NavigationTarget,
    control::{Placed, Target},
};
use gpui::{prelude::*, *};
use std::collections::{HashMap, HashSet};

/// One window's browser state. Which tab covers the terminal is the window's
/// own choice, like its focused workspace; the tabs themselves are the app's.
pub(crate) struct Browser {
    /// Per workspace, the browser tab shown instead of its terminal.
    focus: HashMap<(Scope, String), TabId>,
    #[cfg(any(target_os = "macos", windows))]
    pub(super) pages: super::Pages,
    address: Entity<SearchInput>,
    /// The tab whose address the field last showed.
    address_tab: Option<TabId>,
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
        let address = cx.new(|cx| {
            let mut input = SearchInput::new(cx);
            input.set_placeholder("Enter an address", cx);
            input
        });
        Self {
            focus: HashMap::new(),
            #[cfg(any(target_os = "macos", windows))]
            pages: Default::default(),
            address,
            address_tab: None,
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

    /// The browser tab covering the terminal, if one does.
    pub(crate) fn shown_browser_tab(&self, cx: &App) -> Option<Tab> {
        let id = self.browser.focus.get(&self.browser_key()?)?;
        store(cx)?.get(*id).cloned()
    }

    /// Returns the focused workspace to its terminal.
    pub(crate) fn show_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.browser_key()
            && self.browser.focus.remove(&key).is_some()
        {
            cx.notify();
        }
    }

    pub(crate) fn show_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = store(cx).and_then(|store| store.get(id)).cloned() else {
            return;
        };
        self.browser
            .focus
            .insert((tab.scope.clone(), tab.workspace_id.clone()), id);
        #[cfg(any(target_os = "macos", windows))]
        if let Err(error) = self.browser.pages.ensure(&tab, window, cx) {
            tracing::warn!(%error, "Cannot create a browser page");
            self.browser.failed = Some((id, error.to_string().into()));
        }
        self.sync_address(Some(&tab), true, window, cx);
        if tab.location.is_none() {
            let focus = self.browser.address.read(cx).focus.clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_browser_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        Store::update(cx, |store| store.close(id));
        self.forget_browser_tabs(|tab| tab == id);
        cx.notify();
    }

    fn forget_browser_tabs(&mut self, mut gone: impl FnMut(TabId) -> bool) {
        self.browser.focus.retain(|_, id| !gone(*id));
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

    /// Shows `tab`'s address in the field, unless someone is typing there.
    fn sync_address(
        &mut self,
        tab: Option<&Tab>,
        force: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let id = tab.map(|tab| tab.id);
        let text = tab
            .and_then(|tab| tab.location.as_ref())
            .map(Location::display)
            .unwrap_or_default();
        let input = self.browser.address.read(cx);
        if !force
            && (input.focus.is_focused(window)
                || (self.browser.address_tab == id && input.text() == text.as_str()))
        {
            return;
        }
        self.browser.address_tab = id;
        self.browser
            .address
            .update(cx, |input, cx| input.set_text_selected(&text, cx));
    }

    /// Applies page reports, drops tabs other windows closed, and forgets the
    /// tabs of workspaces the daemon closed. Runs on every window tick.
    pub(crate) fn poll_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        self.apply_page_events(window, cx);
        if let Some(store) = store(cx) {
            let gone: HashSet<TabId> = self
                .browser
                .focus
                .values()
                .copied()
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
        let shown = self.shown_browser_tab(cx);
        self.sync_address(shown.as_ref(), false, window, cx);
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
                        let opened = Store::update(cx, |store| {
                            store.open(
                                parent.scope,
                                &parent.workspace_id,
                                Some(Location::Web { url }),
                                parent.origin,
                            )
                        });
                        if let Some(opened) = opened {
                            self.show_browser_tab(opened, window, cx);
                        }
                    }
                }
            }
        }
    }

    fn submit_address(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.browser.address.read(cx).text().to_owned();
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
        self.show_browser_tab(id, window, cx);
    }

    /// Shows or hides the native pages to match what the window draws. The
    /// pages sit above everything GPUI paints, so any overlay hides them.
    pub(crate) fn present_browser(&mut self, shown: Option<TabId>, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        self.browser
            .pages
            .present(shown.filter(|_| self.menu.page.is_none()), cx);
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = (shown, cx);
    }

    /// The workspace's browser tabs, after its Herdr tabs in the strip.
    pub(crate) fn browser_tab_entries(
        &self,
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
                let (background, text) = if shown == Some(id) {
                    let background = self.theme.primary_wash();
                    (background, self.theme.text_on(background))
                } else {
                    (self.theme.surface, self.theme.muted)
                };
                div()
                    .id(SharedString::from(format!("browser-tab-{id}")))
                    .debug_selector(move || format!("browser-tab-{id}"))
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
                            .debug_selector(move || format!("close-browser-tab-{id}"))
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_browser_tab(id, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_browser_tab(id, window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn toolbar_button(
        &self,
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
            .debug_selector(move || id.into())
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

    /// The page with its toolbar, drawn where the terminal would be.
    pub(crate) fn render_browser(
        &mut self,
        tab: &Tab,
        gap: f32,
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
                    .debug_selector(|| "browser-annotate".into())
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
                    .debug_selector(|| "browser-address".into())
                    .flex_1()
                    .min_w_0()
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "enter" => this.submit_address(id, window, cx),
                            "escape" => {
                                let tab = store(cx).and_then(|store| store.get(id)).cloned();
                                this.sync_address(tab.as_ref(), true, window, cx);
                                window.focus(&this.focus, cx);
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(self.browser.address.clone()),
            )
            .children(self.flash.as_ref().map(|(flash, _)| {
                div()
                    .flex_none()
                    .max_w(px(240.))
                    .truncate()
                    .text_color(rgb(flash.accent(&self.theme)))
                    .child(flash.text.clone())
            }))
            .children(annotate_button)
            .child(self.toolbar_button(
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
                        .debug_selector(|| "browser-placeholder".into())
                        .child(placeholder),
                )
                .into_any_element(),
        };
        div()
            .id("browser")
            .debug_selector(|| "browser".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pl(px(gap))
            .bg(rgb(self.theme.background))
            // Keeps window shortcuts reachable while the page does not hold
            // the keyboard; nothing here types into a terminal.
            .track_focus(&self.focus)
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
