//! The menu behind a group's "…" button: closing tabs in the group,
//! opening a browser tab in it, and splitting it. Closing here only ever
//! takes tabs out of this group's strip, as an editor's group menu does: the
//! tabs stay open in Herdr, in the browser, and in every other group. Only a
//! tab's own close, unsplit, reaches Herdr, through its confirmation.
use crate::{
    HerdrWindow,
    browser::{GroupId, Pick},
    menu::Page,
};
use gpui::{prelude::*, *};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Close,
    CloseOthers,
    /// Closes the group, and with it every tab in it.
    CloseAll,
    NewBrowserTab,
    Split,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::CloseOthers => "Close Others",
            Self::CloseAll => "Close All",
            Self::NewBrowserTab => "New Browser Tab",
            Self::Split => "Split Right",
        }
    }

    /// Whether a rule separates this row from the one above it.
    fn starts_section(self) -> bool {
        matches!(self, Self::NewBrowserTab)
    }
}

pub(crate) struct GroupMenu {
    group: GroupId,
    selected: Option<usize>,
}

impl HerdrWindow {
    /// The rows worth offering for `group`. Closing belongs to a split: a
    /// lone group has nowhere else to keep its tabs.
    fn group_actions(&self, group: GroupId, cx: &App) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.is_split() {
            let pick = self.group_pick(group);
            if pick.is_some() {
                actions.push(Action::Close);
            }
            if self
                .group_tabs(group, cx)
                .iter()
                .any(|tab| Some(tab) != pick.as_ref())
            {
                actions.push(Action::CloseOthers);
            }
            actions.push(Action::CloseAll);
        }
        actions.extend([Action::NewBrowserTab, Action::Split]);
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
            Action::Close => {
                if let Some(pick) = self.group_pick(group) {
                    self.close_in_group(group, vec![pick], window, cx);
                }
            }
            Action::CloseOthers => {
                let keep = self.group_pick(group);
                let others: Vec<Pick> = self
                    .group_tabs(group, cx)
                    .into_iter()
                    .filter(|tab| Some(tab) != keep.as_ref())
                    .collect();
                self.close_in_group(group, others, window, cx);
            }
            Action::CloseAll => self.close_group(group, window, cx),
            Action::NewBrowserTab => self.open_browser_tab_in(group, window, cx),
            Action::Split => self.split_group(group, window, cx),
        }
    }

    /// Closes the tab `group` shows, for Close Tab: a page at once, a Herdr
    /// tab through its confirmation.
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
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{
        browser::Store,
        controls::Command,
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

    fn groups(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> Vec<GroupId> {
        draw(cx);
        view.read_with(cx, |view, _| {
            view.group_slots().into_iter().map(|slot| slot.id).collect()
        })
    }

    fn actions(
        view: &Entity<HerdrWindow>,
        cx: &mut VisualTestContext,
        group: GroupId,
    ) -> Vec<Action> {
        cx.update(|_, cx| view.read(cx).group_actions(group, cx))
    }

    fn run(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, group: GroupId, action: Action) {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_group_menu(group, Point::default(), window, cx);
                view.activate_group_menu(action, window, cx);
            })
        });
        draw(cx);
    }

    fn tabs(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, group: GroupId) -> Vec<Pick> {
        cx.update(|_, cx| view.read(cx).group_tabs(group, cx))
    }

    /// Every Herdr tab the daemon has, which closing in a group never
    /// touches.
    fn herdr_tabs(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            view.live.snapshot.as_ref().unwrap().tabs.len()
        })
    }

    #[gpui::test]
    fn a_lone_group_offers_no_close(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        let [group] = groups(&view, cx)[..] else {
            panic!("one group")
        };
        assert_eq!(
            actions(&view, cx, group),
            [Action::NewBrowserTab, Action::Split]
        );
    }

    #[gpui::test]
    fn closing_in_a_group_never_closes_a_tab(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        open_pages(&view, cx, 2);
        let herdr = herdr_tabs(&view, cx);
        let pages = cx.update(|_, cx| view.read(cx).browser_tab_ids(cx));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.command(Command::SplitEditor, window, cx)
            })
        });
        let [left, right] = groups(&view, cx)[..] else {
            panic!("two groups")
        };
        // Split, both groups list every tab.
        assert_eq!(tabs(&view, cx, left), tabs(&view, cx, right));
        assert_eq!(
            actions(&view, cx, right),
            [
                Action::Close,
                Action::CloseOthers,
                Action::CloseAll,
                Action::NewBrowserTab,
                Action::Split
            ]
        );

        // Close Others leaves the right group its own tab alone.
        let shown = view
            .read_with(cx, |view, _| view.group_pick(right))
            .unwrap();
        run(&view, cx, right, Action::CloseOthers);
        assert_eq!(tabs(&view, cx, right), std::slice::from_ref(&shown));
        assert_eq!(tabs(&view, cx, left).len(), herdr + pages.len());
        assert!(cx.debug_bounds("browser-tab-0").is_some());
        assert!(cx.debug_bounds("g1-browser-tab-0").is_none());

        // Close takes the left group to the next tab it lists.
        let left_shown = view.read_with(cx, |view, _| view.group_pick(left)).unwrap();
        run(&view, cx, left, Action::Close);
        let now = view.read_with(cx, |view, _| view.group_pick(left)).unwrap();
        assert_ne!(now, left_shown);
        assert!(!tabs(&view, cx, left).contains(&left_shown));

        // Close All closes the group, not its tabs.
        run(&view, cx, right, Action::CloseAll);
        assert_eq!(groups(&view, cx), [left]);
        // Nothing was closed in Herdr or the browser, and no dialog opened.
        assert_eq!(herdr_tabs(&view, cx), herdr);
        assert_eq!(cx.update(|_, cx| view.read(cx).browser_tab_ids(cx)), pages);
        view.read_with(cx, |view, _| assert!(view.menu.page.is_none()));
    }

    #[gpui::test]
    fn a_tabs_close_button_in_a_split_closes_it_in_its_group_alone(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        let herdr = herdr_tabs(&view, cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.command(Command::SplitEditor, window, cx)
            })
        });
        let [left, right] = groups(&view, cx)[..] else {
            panic!("two groups")
        };
        let close = cx.debug_bounds("g1-close-tab-t1").unwrap();
        cx.simulate_click(close.center(), Modifiers::none());
        draw(cx);
        // No confirmation, nothing sent to Herdr: the tab left one strip.
        view.read_with(cx, |view, _| assert!(view.menu.page.is_none()));
        assert_eq!(herdr_tabs(&view, cx), herdr);
        assert!(!tabs(&view, cx, right).contains(&Pick::Herdr("t1".into())));
        assert!(tabs(&view, cx, left).contains(&Pick::Herdr("t1".into())));
        assert!(cx.debug_bounds("tab-t1").is_some());
        assert!(cx.debug_bounds("g1-tab-t1").is_none());
    }

    #[gpui::test]
    fn a_group_left_with_nothing_closes(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.command(Command::SplitEditor, window, cx)
            })
        });
        let [_, right] = groups(&view, cx)[..] else {
            panic!("two groups")
        };
        let all = tabs(&view, cx, right);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_in_group(right, all, window, cx))
        });
        assert_eq!(groups(&view, cx).len(), 1);
    }

    #[gpui::test]
    fn the_menu_opens_from_the_strip_and_steps_with_the_keyboard(cx: &mut TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        let button = cx.debug_bounds("tab-actions").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        draw(cx);
        assert!(view.read_with(cx, |view, _| view.menu.page == Some(Page::Group)));
        for row in ["group-menu-NewBrowserTab", "group-menu-Split"] {
            assert!(cx.debug_bounds(row).is_some(), "{row}");
        }
        assert!(cx.debug_bounds("group-menu-CloseAll").is_none());
        cx.simulate_keystrokes("down");
        assert_eq!(
            view.read_with(cx, |view, _| view.menu.group.as_ref().unwrap().selected),
            Some(0)
        );
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    }
}
