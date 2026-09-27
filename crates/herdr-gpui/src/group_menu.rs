//! The menu behind a tab strip's "…" button: opening a browser tab on that
//! side, closing browser tabs in bulk, and splitting the window. Closing only
//! ever reaches browser tabs; a Herdr tab runs processes, so it closes one at
//! a time through its own confirmation.
use crate::{
    HerdrWindow,
    browser::{Content, Side, TabId},
    menu::Page,
};
use gpui::{prelude::*, *};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    NewBrowserTab,
    CloseOthers,
    CloseAll,
    Split,
    CloseSplit,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::NewBrowserTab => "New Browser Tab",
            Self::CloseOthers => "Close Other Browser Tabs",
            Self::CloseAll => "Close All Browser Tabs",
            Self::Split => "Split Right",
            Self::CloseSplit => "Close Split",
        }
    }

    /// Whether a rule separates this row from the one above it.
    fn starts_section(self) -> bool {
        matches!(self, Self::CloseOthers | Self::Split | Self::CloseSplit)
    }
}

pub(crate) struct GroupMenu {
    side: Side,
    selected: Option<usize>,
}

impl HerdrWindow {
    /// The focused workspace's browser tabs, and those a side shows.
    fn group_tabs(&self, cx: &App) -> (Vec<TabId>, Vec<TabId>) {
        let groups = self.browser_groups(cx);
        let shown = self
            .visible_sides()
            .iter()
            .filter_map(|side| match groups.content(*side) {
                Content::Page(id) => Some(id),
                Content::Terminal | Content::Empty => None,
            })
            .collect();
        let all = self.browser_tab_ids(cx);
        (all, shown)
    }

    /// The rows worth offering: a bulk close appears only when it would
    /// close something.
    fn group_actions(&self, cx: &App) -> Vec<Action> {
        let (all, shown) = self.group_tabs(cx);
        let mut actions = vec![Action::NewBrowserTab];
        if all.iter().any(|id| !shown.contains(id)) {
            actions.push(Action::CloseOthers);
        }
        if !all.is_empty() {
            actions.push(Action::CloseAll);
        }
        actions.push(if self.split().is_some() {
            Action::CloseSplit
        } else {
            Action::Split
        });
        actions
    }

    pub(crate) fn open_group_menu(
        &mut self,
        side: Side,
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
            side,
            selected: None,
        });
    }

    fn activate_group_menu(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(side) = self.menu.group.as_ref().map(|menu| menu.side) else {
            return;
        };
        self.dismiss_menu(window, cx);
        self.activate_side(side, window, cx);
        match action {
            Action::NewBrowserTab => self.open_browser_tab_on(side, None, window, cx),
            Action::CloseOthers => {
                let (_, shown) = self.group_tabs(cx);
                self.close_browser_tabs(|id| !shown.contains(&id), cx);
            }
            Action::CloseAll => self.close_browser_tabs(|_| true, cx),
            Action::Split | Action::CloseSplit => self.toggle_split(window, cx),
        }
    }

    pub(crate) fn group_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let actions = self.group_actions(cx);
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
        for (index, action) in self.group_actions(cx).into_iter().enumerate() {
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

    /// Opens blank tabs, which need no native page, without showing them.
    fn open_tabs(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, count: usize) {
        cx.update(|_, cx| {
            let scope = crate::browser::scope(&view.read(cx).endpoints[0]);
            Store::update(cx, |store| {
                for _ in 0..count {
                    store.open(scope.clone(), "w0", None, None);
                }
            });
        });
    }

    fn actions(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> Vec<Action> {
        cx.update(|_, cx| view.read(cx).group_actions(cx))
    }

    fn tab_ids(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> Vec<TabId> {
        cx.update(|_, cx| view.read(cx).group_tabs(cx).0)
    }

    #[gpui::test]
    fn bulk_closes_only_reach_browser_tabs(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        assert_eq!(actions(&view, cx), [Action::NewBrowserTab, Action::Split]);
        open_tabs(&view, cx, 3);
        let ids = tab_ids(&view, cx);
        assert_eq!(ids.len(), 3);
        assert_eq!(
            actions(&view, cx),
            [
                Action::NewBrowserTab,
                Action::CloseOthers,
                Action::CloseAll,
                Action::Split
            ]
        );
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.show_browser_tab(ids[1], window, cx))
        });
        // Close Others keeps the tab on show.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_group_menu(Side::Left, Point::default(), window, cx);
                view.activate_group_menu(Action::CloseOthers, window, cx);
            })
        });
        assert_eq!(tab_ids(&view, cx), [ids[1]]);
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
        assert_eq!(
            actions(&view, cx),
            [Action::NewBrowserTab, Action::CloseAll, Action::Split]
        );
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_group_menu(Side::Left, Point::default(), window, cx);
                view.activate_group_menu(Action::CloseAll, window, cx);
            })
        });
        assert!(tab_ids(&view, cx).is_empty());
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        assert!(cx.debug_bounds("terminal").is_some());
        // The Herdr tabs are untouched.
        assert!(cx.debug_bounds("tab-t0").is_some());
    }

    #[gpui::test]
    fn the_menu_opens_from_the_strip_and_steps_with_the_keyboard(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        let button = cx.debug_bounds("tab-actions").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        assert!(view.read_with(cx, |view, _| view.menu.page == Some(Page::Group)));
        assert!(cx.debug_bounds("group-menu-NewBrowserTab").is_some());
        assert!(cx.debug_bounds("group-menu-Split").is_some());
        assert!(cx.debug_bounds("group-menu-CloseAll").is_none());
        cx.simulate_keystrokes("up");
        assert_eq!(
            view.read_with(cx, |view, _| view.menu.group.as_ref().unwrap().selected),
            Some(1)
        );
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    }
}
