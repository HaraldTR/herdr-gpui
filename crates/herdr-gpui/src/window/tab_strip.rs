//! Each side's tab strip and frame. A split shows the same tabs on both
//! sides, as editor groups do; which one a side shows is its own choice.

use super::HerdrWindow;
use crate::{
    TAB_HEIGHT, TAB_WIDTH,
    browser::{Content, Side},
    controls::Command,
    fonts::StyledFont,
    navigation::NavigationTarget,
};
use gpui::{prelude::*, *};

/// What the split divider drags; the row it resizes reads the pointer.
struct SplitDrag;

impl HerdrWindow {
    /// A tab's colors. The chosen tab carries the theme's accent on the side
    /// in use and a quieter wash on the other, so the split shows which side
    /// has the keyboard; the rest recede into the strip.
    pub(crate) fn tab_colors(&self, selected: bool, side: Side) -> (u32, u32) {
        match (selected, side == self.active_side()) {
            (true, true) => {
                let background = self.theme.primary_wash();
                (background, self.theme.text_on(background))
            }
            (true, false) => (self.theme.active, self.theme.foreground),
            (false, _) => (self.theme.surface, self.theme.muted),
        }
    }

    fn render_tab_strip(&mut self, side: Side, content: Content, cx: &mut Context<Self>) -> Div {
        let mut tabs = div()
            .id(SharedString::from(side.selector("tabs")))
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
                let (background, text) =
                    self.tab_colors(tab.focused && content == Content::Terminal, side);
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || side.selector(&format!("tab-{id}"))
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
                                    move || side.selector(&format!("close-tab-{id}"))
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
                                            move || side.selector(&format!("close-tab-icon-{id}"))
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
                            this.show_terminal_on(side, cx);
                            this.navigate(NavigationTarget::Tab(&id), cx);
                            window.focus(&this.focus, cx);
                        })),
                );
            }
        }
        let shown = match content {
            Content::Page(id) => Some(id),
            Content::Terminal | Content::Empty => None,
        };
        tabs = tabs.children(self.browser_tab_entries(side, shown, cx));
        let split = self.split();
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
                    .id(SharedString::from(side.selector("new-tab")))
                    .debug_selector(move || side.selector("new-tab"))
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
                            .debug_selector(move || side.selector("new-tab-icon"))
                            .size(px(14.))
                            // Quiet like the unselected tabs beside it.
                            .text_color(rgb(self.theme.muted)),
                    )
                    // A new Herdr tab is a terminal, which opens on the side
                    // that asked for it.
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_terminal_on(side, cx);
                        this.command(Command::Tab, window, cx);
                    })),
            )
            .child(div().flex_1().min_w_0())
            // Splitting belongs to the rightmost strip, as in an editor, where
            // it stays lit while the split is open and folds it again.
            .when(split.is_none() || side == Side::Right, |strip| {
                strip.child(self.strip_button(
                    side,
                    "split-editor",
                    "icons/split.svg",
                    split.is_some(),
                    cx,
                    |this, _, window, cx| this.toggle_split(window, cx),
                ))
            })
            .child(self.strip_button(
                side,
                "tab-actions",
                "icons/more.svg",
                false,
                cx,
                move |this, event, window, cx| {
                    this.open_group_menu(side, event.position(), window, cx);
                },
            ))
    }

    fn strip_button(
        &self,
        side: Side,
        id: &'static str,
        icon: &'static str,
        lit: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let color = if lit {
            self.theme.text_on(self.theme.primary_wash())
        } else {
            self.theme.muted
        };
        div()
            .id(SharedString::from(side.selector(id)))
            .debug_selector(move || side.selector(id))
            .w(px(30.))
            .min_h(px(TAB_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(lit, |button| button.bg(rgb(self.theme.primary_wash())))
            .hover(|s| s.bg(rgb(self.theme.active)))
            .child(svg().path(icon).size(px(14.)).text_color(rgb(color)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, event, window, cx| {
                cx.stop_propagation();
                action(this, event, window, cx);
            }))
    }

    /// A side of a split with no tab: the other side holds the terminal.
    pub(super) fn render_empty_side(
        &self,
        side: Side,
        gap: f32,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(side.selector("empty-side")))
            .debug_selector(move || side.selector("empty-side"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pl(px(gap))
            .items_center()
            .justify_center()
            .gap(px(12.))
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.muted))
            .when(keyboard, |empty| empty.track_focus(&self.focus))
            .child("No tab is open on this side.")
            .child(
                div()
                    .id(SharedString::from(side.selector("empty-side-browser")))
                    .debug_selector(move || side.selector("empty-side-browser"))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(crate::config::corners::CONTROL))
                    .border_1()
                    .border_color(rgb(self.theme.active))
                    .text_color(rgb(self.theme.foreground))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(self.theme.active)))
                    .child(
                        svg()
                            .path("icons/globe.svg")
                            .size(px(12.))
                            .text_color(rgb(self.theme.foreground)),
                    )
                    .child("New Browser Tab")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_browser_tab_on(side, None, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// One side: its strip above what it shows. Pressing anywhere in it
    /// gives it the keyboard.
    pub(super) fn render_side(
        &mut self,
        side: Side,
        content: Content,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let strip = self.render_tab_strip(side, content, cx);
        let ratio = self.split().map(|split| split.ratio());
        div()
            .id(SharedString::from(side.selector("side")))
            .debug_selector(move || side.selector("side"))
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .map(|column| match (ratio, side) {
                (Some(ratio), Side::Left) => column.flex_none().w(relative(ratio)),
                _ => column.flex_1(),
            })
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.menu.page.is_none() {
                    this.activate_side(side, window, cx);
                }
            }))
            .child(strip)
            .child(body)
            .into_any_element()
    }

    /// The sides in a row, with a divider between two that drags the split.
    pub(super) fn render_sides(
        &mut self,
        sides: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let [left, right]: [AnyElement; 2] = match sides.try_into() {
            Ok(pair) => pair,
            Err(single) => {
                return div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .children(single)
                    .into_any_element();
            }
        };
        div()
            .id("sides")
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<SplitDrag>, _, cx| {
                    let offset = f32::from(event.event.position.x - event.bounds.left());
                    if this.drag_split(offset, f32::from(event.bounds.size.width)) {
                        cx.notify();
                    }
                }),
            )
            .child(left)
            .child(
                div()
                    .id("split-divider")
                    .debug_selector(|| "split-divider".into())
                    .flex_none()
                    .w(px(5.))
                    .flex()
                    .justify_center()
                    .bg(rgb(self.theme.background))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .child(div().w(px(1.)).h_full().bg(rgb(self.theme.active)))
                    .on_drag(SplitDrag, |_, _, _, cx| cx.new(|_| EmptyView)),
            )
            .child(right)
            .into_any_element()
    }
}
