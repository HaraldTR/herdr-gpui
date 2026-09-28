//! The menu behind a group's "…" button: closing tabs, opening a browser
//! tab in the group, and splitting or closing the group. Closes reach every
//! tab of the workspace, Herdr tabs included; those run processes, so a bulk
//! close that includes one asks first, as closing a single Herdr tab does.
use crate::{
    Error, HerdrWindow,
    browser::{GroupId, Pick, TabId},
    menu::Page,
};
use gpui::{prelude::*, *};
use herdr_client::Method;
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Close,
    CloseOthers,
    CloseAll,
    NewBrowserTab,
    Split,
    CloseGroup,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::CloseOthers => "Close Others",
            Self::CloseAll => "Close All",
            Self::NewBrowserTab => "New Browser Tab",
            Self::Split => "Split Right",
            Self::CloseGroup => "Close Group",
        }
    }

    /// Whether a rule separates this row from the one above it.
    fn starts_section(self) -> bool {
        matches!(self, Self::NewBrowserTab | Self::Split)
    }
}

pub(crate) struct GroupMenu {
    group: GroupId,
    selected: Option<usize>,
}

/// A Herdr tab a bulk close still has to close, with the daemon boot and
/// workspace it was chosen under, so it never reaches a replaced daemon.
pub(crate) struct TabClose {
    boot: String,
    workspace: String,
    tab: String,
}

/// A bulk close waiting for its confirmation.
pub(crate) struct BulkClose {
    boot: String,
    workspace: String,
    tabs: Vec<String>,
    pages: Vec<TabId>,
    confirm_selected: bool,
}

impl HerdrWindow {
    /// The focused workspace's Herdr tabs and browser tabs, in strip order.
    fn workspace_tabs(&self, cx: &App) -> (Vec<String>, Vec<TabId>) {
        let herdr = self
            .live
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .tabs
                    .iter()
                    .filter(|tab| Some(&tab.workspace_id) == snapshot.focused_workspace_id.as_ref())
                    .map(|tab| tab.tab_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        (herdr, self.browser_tab_ids(cx))
    }

    /// The rows worth offering for `group`: a close appears only when it
    /// would close something.
    fn group_actions(&self, group: GroupId, cx: &App) -> Vec<Action> {
        let pick = self.group_pick(group);
        let (herdr, pages) = self.workspace_tabs(cx);
        let others = herdr
            .iter()
            .any(|tab| pick.as_ref() != Some(&Pick::Herdr(tab.clone())))
            || pages
                .iter()
                .any(|id| pick.as_ref() != Some(&Pick::Page(*id)));
        let mut actions = Vec::new();
        if pick.is_some() {
            actions.push(Action::Close);
        }
        if others && pick.is_some() {
            actions.push(Action::CloseOthers);
        }
        if !herdr.is_empty() || !pages.is_empty() {
            actions.push(Action::CloseAll);
        }
        actions.extend([Action::NewBrowserTab, Action::Split]);
        if self.is_split() {
            actions.push(Action::CloseGroup);
        }
        actions
    }

    pub(crate) fn open_group_menu(
        &mut self,
        group: GroupId,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Group);
        self.menu.group = Some(GroupMenu {
            group,
            selected: None,
        });
    }

    fn activate_group_menu(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.menu.group.as_ref().map(|menu| menu.group) else {
            return;
        };
        self.dismiss_menu(window, cx);
        self.activate_group(group, window, cx);
        match action {
            Action::Close => self.close_group_tab(group, window, cx),
            Action::CloseOthers => {
                let keep = self.group_pick(group);
                self.close_tabs(keep, window, cx);
            }
            Action::CloseAll => self.close_tabs(None, window, cx),
            Action::NewBrowserTab => self.open_browser_tab_in(group, window, cx),
            Action::Split => self.split_group(group, window, cx),
            Action::CloseGroup => self.close_group(group, window, cx),
        }
    }

    /// Closes the tab `group` picked: a page at once, a Herdr tab through
    /// its confirmation.
    pub(crate) fn close_group_tab(
        &mut self,
        group: GroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.group_pick(group) {
            Some(Pick::Herdr(tab)) => self.open_tab_close(&tab, window, cx),
            Some(Pick::Page(id)) => self.close_browser_tab(id, window, cx),
            None => {}
        }
    }

    /// Closes every tab of the focused workspace but `keep`, asking first
    /// when that includes a Herdr tab.
    fn close_tabs(&mut self, keep: Option<Pick>, window: &mut Window, cx: &mut Context<Self>) {
        let (herdr, pages) = self.workspace_tabs(cx);
        let tabs: Vec<String> = herdr
            .into_iter()
            .filter(|tab| keep.as_ref() != Some(&Pick::Herdr(tab.clone())))
            .collect();
        let pages: Vec<TabId> = pages
            .into_iter()
            .filter(|id| keep.as_ref() != Some(&Pick::Page(*id)))
            .collect();
        let Some(snapshot) = self.live.snapshot.as_ref() else {
            return;
        };
        let (Some(workspace), false) = (
            snapshot.focused_workspace_id.clone(),
            tabs.is_empty() && pages.is_empty(),
        ) else {
            return;
        };
        let bulk = BulkClose {
            boot: snapshot.boot_id.clone(),
            workspace,
            tabs,
            pages,
            confirm_selected: false,
        };
        if bulk.tabs.is_empty() || !self.config.confirm_close_tab {
            self.run_bulk_close(bulk, window, cx);
            return;
        }
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::ConfirmCloseTabs);
        self.menu.bulk_close = Some(bulk);
    }

    fn run_bulk_close(&mut self, bulk: BulkClose, window: &mut Window, cx: &mut Context<Self>) {
        for id in bulk.pages {
            self.close_browser_tab(id, window, cx);
        }
        self.browser
            .tab_closes
            .extend(bulk.tabs.into_iter().map(|tab| TabClose {
                boot: bulk.boot.clone(),
                workspace: bulk.workspace.clone(),
                tab,
            }));
        self.flush_tab_closes(cx);
        cx.notify();
    }

    fn confirm_bulk_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bulk) = self.menu.bulk_close.take() else {
            return;
        };
        self.dismiss_menu(window, cx);
        self.run_bulk_close(bulk, window, cx);
    }

    /// Sends the next queued Herdr tab close once the connection takes
    /// input again, so each close lands before the next is sent. Tabs that
    /// are already gone are skipped, and a replaced daemon drops them all.
    pub(crate) fn flush_tab_closes(&mut self, cx: &mut Context<Self>) {
        if self.browser.tab_closes.is_empty() || !self.input_ready() {
            return;
        }
        let Some(snapshot) = self.live.snapshot.clone() else {
            self.browser.tab_closes.clear();
            return;
        };
        while let Some(close) = self.browser.tab_closes.pop_front() {
            if close.boot != snapshot.boot_id {
                self.browser.tab_closes.clear();
                return;
            }
            let open = snapshot
                .tabs
                .iter()
                .any(|tab| tab.tab_id == close.tab && tab.workspace_id == close.workspace);
            if !open {
                continue;
            }
            let params = json!({"tab_id": close.tab});
            if !self.request_focus_change(Method::TabClose.as_str(), None, |handle, boot| {
                handle.request(boot, Method::TabClose, params)
            }) {
                // The error is the window's to show; the rest would fail too.
                self.browser.tab_closes.clear();
                if self.local_error.is_none() {
                    self.local_error = Some(Error::NotConnected.to_string());
                }
            }
            cx.notify();
            return;
        }
    }

    pub(crate) fn group_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.menu.group.as_ref().map(|menu| menu.group) else {
            return;
        };
        let actions = self.group_actions(group, cx);
        let Some(menu) = &mut self.menu.group else {
            return;
        };
        let key = event.keystroke.key.as_str();
        cx.stop_propagation();
        window.prevent_default();
        let count = actions.len();
        match key {
            "escape" => self.dismiss_menu(window, cx),
            "up" | "down" => {
                menu.selected = Some(match (menu.selected, key) {
                    (None, "up") => count - 1,
                    (None, _) => 0,
                    (Some(i), "up") => (i + count - 1) % count,
                    (Some(i), _) => (i + 1) % count,
                });
                cx.notify();
            }
            "enter" => {
                if let Some(action) = menu.selected.and_then(|index| actions.get(index)) {
                    self.activate_group_menu(*action, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_group_menu(&self, cx: &mut Context<Self>) -> Div {
        let Some(menu) = &self.menu.group else {
            return div();
        };
        let theme = &self.theme;
        let mut body = div().flex().flex_col();
        for (index, action) in self.group_actions(menu.group, cx).into_iter().enumerate() {
            body = body.child(
                div()
                    .id(("group-menu-action", index))
                    .debug_selector(move || format!("group-menu-{action:?}"))
                    .when(index > 0 && action.starts_section(), |row| {
                        row.mt(px(4.)).border_t_1().border_color(rgb(theme.active))
                    })
                    .min_h(px(self.config.ui.line_height() + 12.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(menu.selected == Some(index), |row| {
                        row.bg(rgb(theme.active))
                    })
                    .hover(|row| row.bg(rgb(theme.active)))
                    .child(action.label())
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered && let Some(menu) = &mut this.menu.group {
                            menu.selected = Some(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_group_menu(action, window, cx)
                    })),
            );
        }
        body
    }

    pub(crate) fn bulk_close_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss_menu(window, cx),
            "tab" | "left" | "right" => {
                if let Some(bulk) = &mut self.menu.bulk_close {
                    bulk.confirm_selected = !bulk.confirm_selected;
                }
                cx.notify();
            }
            "enter" => {
                if self
                    .menu
                    .bulk_close
                    .as_ref()
                    .is_some_and(|bulk| bulk.confirm_selected)
                {
                    self.confirm_bulk_close(window, cx);
                } else {
                    self.dismiss_menu(window, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_bulk_close(&self, cx: &mut Context<Self>) -> Div {
        let Some(bulk) = &self.menu.bulk_close else {
            return div();
        };
        let theme = &self.theme;
        let count = bulk.tabs.len() + bulk.pages.len();
        let plural = |count: usize, one: &'static str, many: &'static str| {
            if count == 1 { one } else { many }
        };
        let detail = format!(
            "This terminates every pane and running process in {} Herdr {}{}. This cannot be undone.",
            bulk.tabs.len(),
            plural(bulk.tabs.len(), "tab", "tabs"),
            match bulk.pages.len() {
                0 => String::new(),
                pages => format!(
                    " and closes {pages} browser {}",
                    plural(pages, "tab", "tabs")
                ),
            },
        );
        let button = |id: &'static str, label: SharedString, lit: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(12.))
                .py(px(6.))
                .rounded(px(crate::config::corners::CONTROL))
                .border_1()
                .border_color(rgb(if lit { theme.foreground } else { theme.active }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme.active)))
                .child(label)
        };
        div()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(self.config.ui.size * 1.35))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Close {count} {}?", plural(count, "tab", "tabs"))),
            )
            .child(div().text_color(rgb(theme.muted)).child(detail))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("bulk-close-cancel", "Cancel".into(), !bulk.confirm_selected)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx)),
                            ),
                    )
                    .child(
                        button(
                            "bulk-close-confirm",
                            format!("Close {}", plural(count, "Tab", "Tabs")).into(),
                            bulk.confirm_selected,
                        )
                        .bg(rgb(theme.active))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_bulk_close(window, cx)),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{
        browser::Store,
        sidebar::layout_tests::{fixture_window, full_draw, snapshot},
    };
    use core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};
    use std::sync::Arc;

    fn window(cx: &mut TestAppContext) -> (Entity<HerdrWindow>, &mut VisualTestContext) {
        cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut shown = snapshot(40);
            shown.focused_workspace_id = Some("w0".into());
            shown.focused_tab_id = Some("t0".into());
            view.live.snapshot = Some(Arc::new(shown));
            view
        })
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
    }

    /// Opens blank tabs, which need no native page, without showing them.
    fn open_pages(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, count: usize) {
        cx.update(|_, cx| {
            let scope = crate::browser::scope(&view.read(cx).endpoints[0]);
            Store::update(cx, |store| {
                for _ in 0..count {
                    store.open(scope.clone(), "w0", None, None);
                }
            });
        });
    }

    fn group(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> GroupId {
        draw(cx);
        view.read_with(cx, |view, _| view.active_group().unwrap())
    }

    fn actions(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> Vec<Action> {
        let group = group(view, cx);
        cx.update(|_, cx| view.read(cx).group_actions(group, cx))
    }

    fn run(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, action: Action) {
        let group = group(view, cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_group_menu(group, Point::default(), window, cx);
                view.activate_group_menu(action, window, cx);
            })
        });
    }

    fn herdr_tabs(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            let snapshot = view.live.snapshot.as_ref().unwrap();
            snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == "w0")
                .count()
        })
    }

    #[gpui::test]
    fn bulk_closes_reach_herdr_tabs_after_asking(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        let herdr = herdr_tabs(&view, cx);
        assert!(herdr > 0);
        let all = actions(&view, cx);
        assert!(all.contains(&Action::Close) && all.contains(&Action::CloseAll));
        assert!(!all.contains(&Action::CloseGroup));
        open_pages(&view, cx, 2);
        let pages = cx.update(|_, cx| view.read(cx).browser_tab_ids(cx));

        // Close Others keeps the group's tab and asks about the Herdr ones.
        run(&view, cx, Action::CloseOthers);
        view.read_with(cx, |view, _| {
            assert_eq!(view.menu.page, Some(Page::ConfirmCloseTabs));
            let bulk = view.menu.bulk_close.as_ref().unwrap();
            assert_eq!(bulk.tabs.len(), herdr - 1);
            assert!(!bulk.tabs.contains(&"t0".to_owned()));
            assert_eq!(bulk.pages, pages);
        });
        draw(cx);
        assert!(cx.debug_bounds("bulk-close-confirm").is_some());
        // Cancelling closes nothing.
        cx.simulate_keystrokes("escape");
        assert_eq!(cx.update(|_, cx| view.read(cx).browser_tab_ids(cx)), pages);

        run(&view, cx, Action::CloseAll);
        cx.simulate_keystrokes("tab enter");
        view.read_with(cx, |view, _| {
            assert!(view.menu.page.is_none());
            // The fixture takes no input, so every Herdr close still waits.
            let queued: Vec<&str> = view
                .browser
                .tab_closes
                .iter()
                .map(|close| close.tab.as_str())
                .collect();
            assert_eq!(queued.len(), herdr);
            assert!(queued.contains(&"t0"));
        });
        assert!(
            cx.update(|_, cx| view.read(cx).browser_tab_ids(cx))
                .is_empty()
        );
    }

    #[gpui::test]
    fn a_group_closes_its_own_page_without_asking(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        open_pages(&view, cx, 2);
        let pages = cx.update(|_, cx| view.read(cx).browser_tab_ids(cx));
        let group = group(&view, cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_browser_tab_in(Some(group), pages[0], window, cx)
            })
        });
        run(&view, cx, Action::Close);
        assert_eq!(
            cx.update(|_, cx| view.read(cx).browser_tab_ids(cx)),
            [pages[1]]
        );
        view.read_with(cx, |view, _| {
            assert!(view.menu.page.is_none());
            // The group moves on to the next page.
            assert_eq!(view.group_pick(group), Some(Pick::Page(pages[1])));
        });
    }

    #[gpui::test]
    fn queued_closes_wait_for_the_connection(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let boot = view.live.snapshot.as_ref().unwrap().boot_id.clone();
                view.browser.tab_closes.push_back(TabClose {
                    boot,
                    workspace: "w0".into(),
                    tab: "t0".into(),
                });
                // The fixture never takes input, so nothing is sent.
                view.flush_tab_closes(cx);
                assert_eq!(view.browser.tab_closes.len(), 1);
            })
        });
    }

    #[gpui::test]
    fn the_menu_opens_from_the_strip_and_steps_with_the_keyboard(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        let button = cx.debug_bounds("tab-actions").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        draw(cx);
        assert!(view.read_with(cx, |view, _| view.menu.page == Some(Page::Group)));
        for row in [
            "group-menu-Close",
            "group-menu-CloseAll",
            "group-menu-NewBrowserTab",
            "group-menu-Split",
        ] {
            assert!(cx.debug_bounds(row).is_some(), "{row}");
        }
        cx.simulate_keystrokes("down");
        assert_eq!(
            view.read_with(cx, |view, _| view.menu.group.as_ref().unwrap().selected),
            Some(0)
        );
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    }
}
