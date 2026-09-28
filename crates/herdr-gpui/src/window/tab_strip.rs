//! Each editor group's tab strip and frame, and the row of groups. Every
//! group lists the same tabs, as an editor's groups do; which one a group
//! shows is its own choice.

use super::HerdrWindow;
use crate::{
    TAB_HEIGHT, TAB_WIDTH,
    browser::{GroupId, Pick, Shown, Slot},
    controls::Command,
    fonts::StyledFont,
};
use gpui::{prelude::*, *};

/// What a divider drags: the index of the group on its left. The row it
/// resizes reads the pointer.
#[derive(Clone, Copy)]
struct DividerDrag(usize);

impl HerdrWindow {
    /// A tab's colors. The chosen tab carries the theme's accent in the group
    /// in use and a quieter wash elsewhere, so a split shows which group has
    /// the keyboard; the rest recede into the strip.
    pub(crate) fn tab_colors(&self, selected: bool, group: GroupId) -> (u32, u32) {
        let active = !self.is_split() || self.active_group() == Some(group);
        match (selected, active) {
            (true, true) => {
                let background = self.theme.primary_wash();
                (background, self.theme.text_on(background))
            }
            (true, false) => (self.theme.active, self.theme.foreground),
            (false, _) => (self.theme.surface, self.theme.muted),
        }
    }

    fn render_tab_strip(&mut self, slot: Slot, cx: &mut Context<Self>) -> Div {
        let pick = self.group_pick(slot.id);
        let mut tabs = div()
            .id(SharedString::from(slot.selector("tabs")))
            .flex()
            .flex_none()
            .h(px((self.config.tabs.size * 1.6 + 4.).max(TAB_HEIGHT)))
            .text_font(&self.config.tabs)
            .text_size(px(self.config.tabs.size))
            .overflow_x_scroll()
            .bg(rgb(self.theme.surface))
            .text_color(rgb(self.theme.foreground))
            .items_center();
        if let Some(snapshot) = &self.live.snapshot {
            for tab in snapshot
                .tabs
                .iter()
                .filter(|t| Some(&t.workspace_id) == snapshot.focused_workspace_id.as_ref())
            {
                let id = tab.tab_id.clone();
                let context_id = id.clone();
                let close_id = id.clone();
                let selected = matches!(&pick, Some(Pick::Herdr(picked)) if *picked == id);
                let (background, text) = self.tab_colors(selected, slot.id);
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || slot.selector(&format!("tab-{id}"))
                        })
                        .pl(px(12.))
                        // The close button hugs the tab's inner right edge, well
                        // clear of the label it would otherwise crowd.
                        .pr(px(3.))
                        .py(px(2.))
                        // Even cells divided by a single rule, as in the reference UI.
                        .min_w(px(TAB_WIDTH))
                        .border_r_1()
                        .border_color(rgb(self.theme.active))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .bg(rgb(background))
                        .text_color(rgb(text))
                        .child(tab.label.clone())
                        .child(
                            div()
                                .id("close-tab")
                                .debug_selector({
                                    let id = id.clone();
                                    move || slot.selector(&format!("close-tab-{id}"))
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
                                        .debug_selector({
                                            let id = id.clone();
                                            move || slot.selector(&format!("close-tab-icon-{id}"))
                                        })
                                        .size(px(12.))
                                        .text_color(rgb(text)),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_tab_close(&close_id, window, cx);
                                })),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_tab_menu(&context_id, event.position, window, cx);
                                this.menu.opening_right_click =
                                    this.menu.page == Some(crate::menu::Page::Tab);
                            }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_herdr_tab(slot.id, &id, window, cx);
                        })),
                );
            }
        }
        let shown = match pick {
            Some(Pick::Page(id)) => Some(id),
            _ => None,
        };
        tabs = tabs.children(self.browser_tab_entries(slot, shown, cx));
        div()
            .flex()
            .flex_none()
            .bg(rgb(self.theme.surface))
            .text_color(rgb(self.theme.foreground))
            // Tabs size to their content and shrink when the row is full, so
            // the button sits after the last tab instead of at the far right
            // of the window.
            .child(tabs.flex_shrink_1().min_w_0())
            .child(
                div()
                    .id(SharedString::from(slot.selector("new-tab")))
                    .debug_selector(move || slot.selector("new-tab"))
                    .w(px(34.))
                    .min_h(px(TAB_HEIGHT))
                    .border_r_1()
                    .border_color(rgb(self.theme.active))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(self.theme.active)))
                    .child(
                        svg()
                            .path("icons/plus.svg")
                            .debug_selector(move || slot.selector("new-tab-icon"))
                            .size(px(14.))
                            // Quiet like the unselected tabs beside it.
                            .text_color(rgb(self.theme.muted)),
                    )
                    // A new Herdr tab opens in the group that asked for it.
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.expect_new_tab_in(slot.id);
                        this.command(Command::Tab, window, cx);
                    })),
            )
            .child(div().flex_1().min_w_0())
            .child(self.strip_button(
                slot,
                "split-editor",
                "icons/split.svg",
                cx,
                move |this, _, window, cx| this.split_group(slot.id, window, cx),
            ))
            .child(self.strip_button(
                slot,
                "tab-actions",
                "icons/more.svg",
                cx,
                move |this, event, window, cx| {
                    this.open_group_menu(slot.id, event.position(), window, cx);
                },
            ))
    }

    fn strip_button(
        &self,
        slot: Slot,
        id: &'static str,
        icon: &'static str,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(slot.selector(id)))
            .debug_selector(move || slot.selector(id))
            .w(px(30.))
            .min_h(px(TAB_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(self.theme.active)))
            .child(
                svg()
                    .path(icon)
                    .size(px(14.))
                    .text_color(rgb(self.theme.muted)),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, event, window, cx| {
                cx.stop_propagation();
                action(this, event, window, cx);
            }))
    }

    /// Stands in for a tab live in another group, or for nothing. Pressing
    /// the group, as the button invites, brings the tab here.
    pub(super) fn render_stand_in(
        &self,
        slot: Slot,
        shown: &Shown,
        gap: f32,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, note): (SharedString, &str) = match shown {
            Shown::Elsewhere(Pick::Herdr(tab)) => {
                let label = self
                    .live
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .tabs
                            .iter()
                            .find(|candidate| &candidate.tab_id == tab)
                    })
                    .map_or_else(|| tab.clone(), |tab| tab.label.clone());
                let note = if self.focused_herdr_tab() == Some(tab.as_str()) {
                    "Shown in another group."
                } else {
                    "Not the focused Herdr tab."
                };
                (label.into(), note)
            }
            Shown::Elsewhere(Pick::Page(id)) => {
                let title = cx
                    .try_global::<crate::browser::Store>()
                    .and_then(|store| store.get(*id))
                    .map_or_else(String::new, |tab| tab.title.clone());
                (title.into(), "Shown in another group.")
            }
            Shown::Terminal | Shown::Page(_) | Shown::Empty => ("".into(), "Nothing to show."),
        };
        let elsewhere = matches!(shown, Shown::Elsewhere(_));
        div()
            .id(SharedString::from(slot.selector("stand-in")))
            .debug_selector(move || slot.selector("stand-in"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pl(px(gap))
            .items_center()
            .justify_center()
            .gap(px(10.))
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.muted))
            .when(keyboard, |stand_in| stand_in.track_focus(&self.focus))
            .when(!title.is_empty(), |stand_in| {
                stand_in.child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_color(rgb(self.theme.foreground))
                        .child(title),
                )
            })
            .child(note)
            // The group in use carries the window's flash, as a page's
            // toolbar and the terminal do.
            .children(
                self.flash
                    .as_ref()
                    .filter(|_| self.active_group() == Some(slot.id))
                    .map(|(flash, _)| {
                        div()
                            .debug_selector(|| "flash".into())
                            .max_w_full()
                            .truncate()
                            .text_color(rgb(flash.accent(&self.theme)))
                            .child(flash.text.clone())
                    }),
            )
            .when(elsewhere, |stand_in| {
                stand_in.child(
                    div()
                        .id(SharedString::from(slot.selector("show-here")))
                        .debug_selector(move || slot.selector("show-here"))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(crate::config::corners::CONTROL))
                        .border_1()
                        .border_color(rgb(self.theme.active))
                        .text_color(rgb(self.theme.foreground))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(self.theme.active)))
                        .child("Show Here")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_group(slot.id, window, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// One group: its strip above what it shows. Pressing anywhere in it
    /// makes it the group in use.
    pub(super) fn render_group(
        &mut self,
        slot: Slot,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let strip = self.render_tab_strip(slot, cx);
        let share = self.is_split().then(|| self.group_share(slot.id));
        div()
            .id(SharedString::from(slot.selector("group")))
            .debug_selector(move || slot.selector("group"))
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .map(|column| match share {
                // Dividers take their width from every group alike.
                Some(share) => column.flex_shrink(1.).w(relative(share)),
                None => column.flex_1(),
            })
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.menu.page.is_none() {
                    this.activate_group(slot.id, window, cx);
                }
            }))
            .child(strip)
            .child(body)
            .into_any_element()
    }

    /// The groups in a row, with a divider between each pair that drags
    /// the two apart.
    pub(super) fn render_groups(
        &mut self,
        groups: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = groups.len();
        let mut row = div()
            .id("groups")
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DividerDrag>, _, cx| {
                    let DividerDrag(divider) = *event.drag(cx);
                    let offset = f32::from(event.event.position.x - event.bounds.left());
                    if this.drag_divider(divider, offset, f32::from(event.bounds.size.width)) {
                        cx.notify();
                    }
                }),
            );
        for (index, group) in groups.into_iter().enumerate() {
            row = row.child(group);
            if index + 1 < count {
                row = row.child(
                    div()
                        .id(("group-divider", index))
                        .debug_selector(move || format!("group-divider-{index}"))
                        .flex_none()
                        .w(px(5.))
                        .flex()
                        .justify_center()
                        .bg(rgb(self.theme.background))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .child(div().w(px(1.)).h_full().bg(rgb(self.theme.active)))
                        .on_drag(DividerDrag(index), |_, _, _, cx| cx.new(|_| EmptyView)),
                );
            }
        }
        row.into_any_element()
    }
}
