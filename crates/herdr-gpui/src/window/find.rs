//! The find bar: a native search field over one pane, searching its
//! scrollback through the daemon. What to ask and how answers map onto the
//! grid lives in [`crate::find`]; this is the window's side of it, owning the
//! field, its focus, the mailbox poll, and scrolling a match into view.
//!
//! Typing in the field never reaches the terminal. Printable keys must stay
//! unhandled for the platform to insert them (and to compose them through an
//! IME), so the terminal's own key handler steps aside while the field has
//! focus instead of the field stopping every key.

use super::HerdrWindow;
use crate::{
    find::{Search, Step},
    scrollback::{Inbox, reveal_offset},
    search_input::{self, SearchInput},
    terminal_painter::Highlight,
};
use gpui::{prelude::*, *};
use herdr_client::{
    Method,
    protocol::{PaneSurfaceFrame, PaneSurfacePane},
    scrollback::{ScrollbackResponse, TextRange},
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) struct FindBar {
    input: Entity<SearchInput>,
    search: Search,
    boot_id: String,
    /// The mailbox of the connection the bar was opened on. A reconnect
    /// replaces it, which retires the bar with the connection.
    inbox: Arc<Mutex<Inbox>>,
    _changed: Subscription,
}

impl HerdrWindow {
    /// Opens the bar over the focused pane, or returns to it with its query
    /// selected when it is already open there.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.live.supports_copy_search {
            self.show_flash(super::Flash::warning("Find needs a newer Herdr daemon"), cx);
            return;
        }
        let Some(pane) = self.focused_surface_pane().cloned() else {
            return;
        };
        let Some(boot_id) = self.live.snapshot.as_ref().map(|s| s.boot_id.clone()) else {
            return;
        };
        if let Some(bar) = &self.find
            && bar.search.pane_id() == pane.pane_id
        {
            let input = bar.input.clone();
            input.update(cx, |input, cx| {
                let text = input.text().to_owned();
                input.set_text_selected(&text, cx);
            });
            let focus = input.read(cx).focus.clone();
            window.focus(&focus, cx);
            cx.notify();
            return;
        }
        let input = cx.new(SearchInput::new);
        input.update(cx, |input, cx| {
            input.set_placeholder("Find", cx);
            input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
        });
        let focus = input.read(cx).focus.clone();
        window.focus(&focus, cx);
        let changed = cx.subscribe(&input, |this, input, _: &search_input::Changed, cx| {
            let query = input.read(cx).text().to_owned();
            if let Some(bar) = &mut this.find {
                bar.search.set_query(&query);
            }
            this.flush_find(cx);
            cx.notify();
        });
        self.find = Some(FindBar {
            input,
            search: Search::new(&pane),
            boot_id,
            inbox: self.endpoints[self.selected_endpoint]
                .connection
                .scrollback
                .clone(),
            _changed: changed,
        });
        cx.notify();
    }

    /// Closes the bar and hands the keyboard back to the terminal. A search
    /// still in flight is answered into nothing.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = self.find.take() else {
            return;
        };
        if let Some(request) = bar.search.in_flight()
            && let Ok(mut inbox) = bar.inbox.lock()
        {
            inbox.discard(request);
        }
        if bar.input.read(cx).focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Whether the find field holds the keyboard, so the terminal must not
    /// act on a keystroke bubbling out of it.
    pub(crate) fn find_focused(&self, window: &Window, cx: &App) -> bool {
        self.find
            .as_ref()
            .is_some_and(|bar| bar.input.read(cx).focus.is_focused(window))
    }

    pub(crate) fn find_step(&mut self, step: Step, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else {
            return;
        };
        bar.search.step(step);
        self.flush_find(cx);
        cx.notify();
    }

    /// Runs every tick: retires a bar whose pane or connection is gone,
    /// applies an answer, and sends whatever search is due.
    pub(crate) fn poll_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else {
            return;
        };
        let connection = &self.endpoints[self.selected_endpoint].connection;
        let current = Arc::ptr_eq(&bar.inbox, &connection.scrollback)
            && self.live.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.boot_id == bar.boot_id
                    && snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == bar.search.pane_id())
            });
        if !current {
            self.close_find(window, cx);
            return;
        }
        // The reader holds this lock only to deliver; a busy mailbox is read
        // on the next tick rather than waited on.
        let answer = bar.search.in_flight().and_then(|request| {
            let answer = bar.inbox.try_lock().ok()?.take(request)?;
            Some((request.to_owned(), answer))
        });
        if let Some((request, answer)) = answer {
            let answer = answer.and_then(|answer| match answer {
                ScrollbackResponse::PaneCopySearch(result) => Ok(result),
                _ => Err(herdr_client::Error::ResponseType),
            });
            if let Some(range) = bar.search.answer(&request, answer) {
                self.reveal_find_match(range, cx);
            }
            cx.notify();
        }
        if let Some(bar) = &mut self.find
            && let Some(pane) = self
                .live
                .surface
                .as_deref()
                .and_then(|surface| pane_of(surface, bar.search.pane_id()))
        {
            bar.search.content_changed(pane, Instant::now());
        }
        self.flush_find(cx);
    }

    /// Sends the search that is due, if any, registering it in the mailbox
    /// before the worker can answer it.
    fn flush_find(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else {
            return;
        };
        let Some(pane) = self
            .live
            .surface
            .as_deref()
            .and_then(|surface| pane_of(surface, bar.search.pane_id()))
        else {
            return;
        };
        let Some((step, params)) = bar.search.next_request(pane) else {
            return;
        };
        let Some(handle) = &self.endpoints[self.selected_endpoint].connection.handle else {
            return;
        };
        let Ok(mut inbox) = bar.inbox.try_lock() else {
            return;
        };
        match inbox.send(|| handle.copy_search(&bar.boot_id, &params)) {
            Ok(request) => bar.search.sent(request, &params, step, Instant::now()),
            Err(error) => bar.search.send_failed(&error),
        }
        cx.notify();
    }

    /// Scrolls the bar's pane so `range` shows, when it does not already.
    fn reveal_find_match(&mut self, range: TextRange, cx: &mut Context<Self>) {
        let Some(bar) = &self.find else {
            return;
        };
        let Some(offset) = self
            .live
            .surface
            .as_deref()
            .and_then(|surface| pane_of(surface, bar.search.pane_id()))
            .and_then(|pane| reveal_offset(pane, range))
        else {
            return;
        };
        let Some(handle) = &self.endpoints[self.selected_endpoint].connection.handle else {
            return;
        };
        if let Err(error) = handle.request(
            &bar.boot_id,
            Method::PaneScroll,
            serde_json::json!({"pane_id": bar.search.pane_id(), "offset_from_bottom": offset}),
        ) {
            self.local_error = Some(format!("Scroll not sent: {error}"));
            cx.notify();
        }
    }

    /// The match highlights to paint over `surface`. A popup covers the
    /// panes, so nothing under it is tinted.
    pub(crate) fn find_highlights(&self, surface: &PaneSurfaceFrame) -> Vec<Highlight> {
        let Some(bar) = &self.find else {
            return Vec::new();
        };
        if surface.popup.is_some() {
            return Vec::new();
        }
        pane_of(surface, bar.search.pane_id())
            .map(|pane| bar.search.highlights(pane))
            .unwrap_or_default()
    }

    /// The focused pane as the live surface paints it.
    pub(super) fn focused_surface_pane(&self) -> Option<&PaneSurfacePane> {
        if !self.live.surface_ready() {
            return None;
        }
        let surface = self.live.surface.as_deref()?;
        if surface.popup.is_some() {
            return None;
        }
        let focused = self.live.snapshot.as_ref()?.focused_pane_id.as_deref()?;
        pane_of(surface, focused)
    }

    /// Keys the field leaves alone: Escape closes, Enter and the arrows move
    /// between matches. Everything else is the field's or the platform's.
    fn find_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = &self.find else {
            return;
        };
        if bar.input.read(cx).is_composing() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let step = match keystroke.key.as_str() {
            "escape" if !modifiers.modified() => {
                self.close_find(window, cx);
                None
            }
            "enter" if modifiers.shift => Some(Step::Newer),
            "enter" => Some(Step::Older),
            "up" if !modifiers.modified() => Some(Step::Older),
            "down" if !modifiers.modified() => Some(Step::Newer),
            "g" if modifiers.secondary() && modifiers.shift => Some(Step::Newer),
            "g" if modifiers.secondary() => Some(Step::Older),
            _ => return,
        };
        if let Some(step) = step {
            self.find_step(step, cx);
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    /// The bar, laid over the top right of its pane in the frame on screen.
    /// `gap` is the terminal's left padding, which absolute children sit
    /// inside of.
    pub(crate) fn render_find_bar(
        &self,
        surface: Option<&PaneSurfaceFrame>,
        gap: f32,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let bar = self.find.as_ref()?;
        let surface = surface.filter(|surface| surface.popup.is_none())?;
        let pane = pane_of(surface, bar.search.pane_id())?;
        let cell_height = self.config.terminal.line_height();
        let theme = &self.theme;
        let has_matches = bar.search.has_matches();
        let label = bar
            .search
            .error()
            .map_or_else(|| bar.search.label(), str::to_owned);
        let button = |id: &'static str, icon: &'static str, step: Option<Step>| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(crate::config::corners::CONTROL))
                .when(step.is_none() || has_matches, |button| {
                    button
                        .cursor_pointer()
                        .hover(|button| button.bg(rgba((theme.foreground << 8) | 0x14)))
                })
                .child(svg().path(icon).size(px(14.)).text_color(rgb(
                    if step.is_none() || has_matches {
                        theme.foreground
                    } else {
                        theme.muted
                    },
                )))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    match step {
                        Some(step) => this.find_step(step, cx),
                        None => this.close_find(window, cx),
                    }
                }))
        };
        Some(
            div()
                .absolute()
                .left(px(gap + f32::from(pane.rect.x) * self.cell_width))
                .top(px(f32::from(pane.rect.y) * cell_height))
                .w(px(f32::from(pane.rect.width) * self.cell_width))
                .flex()
                .justify_end()
                .p(px(6.))
                .child(
                    div()
                        .id("find-bar")
                        .debug_selector(|| "find-bar".into())
                        .occlude()
                        .min_w_0()
                        .w(px(320.))
                        .flex_shrink(1.)
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .p(px(4.))
                        .rounded(px(crate::config::corners::CONTROL))
                        .border_1()
                        .border_color(rgb(theme.active))
                        .bg(rgb(theme.surface))
                        .text_color(rgb(theme.foreground))
                        .text_size(px(self.config.ui.size))
                        .shadow_md()
                        .on_key_down(cx.listener(Self::find_key_down))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(div().flex_1().min_w_0().child(bar.input.clone()))
                        .child(
                            div()
                                .debug_selector(|| "find-count".into())
                                .flex_none()
                                .px(px(4.))
                                .text_color(rgb(if bar.search.error().is_some() {
                                    theme.ink(theme.palette[1])
                                } else {
                                    theme.muted
                                }))
                                .child(label),
                        )
                        .child(button(
                            "find-older",
                            "icons/chevron-up.svg",
                            Some(Step::Older),
                        ))
                        .child(button(
                            "find-newer",
                            "icons/chevron-down.svg",
                            Some(Step::Newer),
                        ))
                        .child(button("find-close", "icons/x.svg", None)),
                ),
        )
    }
}

fn pane_of<'a>(surface: &'a PaneSurfaceFrame, pane_id: &str) -> Option<&'a PaneSurfacePane> {
    surface.panes.iter().find(|pane| pane.pane_id == pane_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{controls::Command, sidebar::layout_tests::fixture_window, window::MockPeer};
    // `super::*` brings in gpui's `test`, which `#[gpui::test]` expands to.
    use core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};
    use herdr_client::{
        ClientEvent,
        protocol::{ClientMessage, PaneSurfaceScrollMetrics},
    };
    use serde_json::{Value, json};

    /// The next find or scroll request on the wire. Resizes and focus reports
    /// may come first, and other requests are answered so they do not hold
    /// the connection's one request slot; terminal input never may come.
    fn next_request(peer: &mut MockPeer) -> Value {
        loop {
            match peer.receive() {
                ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                    let request: Value = serde_json::from_str(&request).unwrap();
                    if matches!(
                        request["method"].as_str(),
                        Some("pane.copy_search" | "pane.scroll")
                    ) {
                        return request;
                    }
                    let id = request["id"].as_str().unwrap();
                    peer.respond(&boot_id, id, &json!({"id": id, "result": {"type": "ok"}}));
                }
                message @ (ClientMessage::ClientShellPaneInput { .. }
                | ClientMessage::ClientShellPopupInput { .. }) => {
                    panic!("find input reached the terminal: {message:?}")
                }
                _ => {}
            }
        }
    }

    /// Answers `request` over the wire and delivers the client's event the
    /// way the connection's reader does, then lets the window poll.
    fn answer(
        view: &Entity<HerdrWindow>,
        peer: &mut MockPeer,
        cx: &mut VisualTestContext,
        request: &Value,
        result: Value,
    ) {
        let id = request["id"].as_str().unwrap();
        let event = peer.respond("boot-v1", id, &json!({"id": id, "result": result}));
        assert!(matches!(&event, ClientEvent::Response { request_id, .. } if request_id == id));
        let inbox = view.read_with(cx, |view, _| {
            view.endpoints[0].connection.scrollback.clone()
        });
        assert!(
            inbox.lock().unwrap().apply(event).is_none(),
            "the scrollback inbox claims it"
        );
        cx.update(|window, cx| view.update(cx, |view, cx| view.poll_find(window, cx)));
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        methods: &[&str],
    ) -> (Entity<HerdrWindow>, MockPeer, &'a mut VisualTestContext) {
        let peer = MockPeer::advertising(methods);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            peer.prepare(&mut view);
            view.live.supports_copy_search = methods.contains(&"pane.copy_search");
            let surface = Arc::make_mut(view.live.surface.as_mut().unwrap());
            // 100 rows of history above a 24-row screen, scrolled to the bottom.
            surface.panes[0].content_revision = 2;
            surface.panes[0].scroll = Some(PaneSurfaceScrollMetrics {
                offset_from_bottom: 0,
                max_offset_from_bottom: 100,
                viewport_rows: 24,
            });
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        (view, peer, cx)
    }

    fn find(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| view.update(cx, |view, cx| view.command(Command::Find, window, cx)));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn label(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.find.as_ref().unwrap().search.label())
    }

    fn matches(rows: &[(u32, u16, u16)], current: u32, global: u64, total: u64) -> Value {
        json!({
            "type": "pane_copy_search",
            "pane_id": "w1:p1",
            "content_revision": 2,
            "matches": rows.iter().map(|(row, start, end)| json!({
                "start": {"row": row, "col": start},
                "end": {"row": row, "col": end},
            })).collect::<Vec<_>>(),
            "total": total,
            "current": current,
            "current_global": global,
        })
    }

    /// Typing goes to the field and becomes one search at a time; the
    /// terminal sees none of it. An answer is counted, highlighted, and
    /// scrolled into view, Enter moves to the next older match, and Escape
    /// hands the keyboard back to the terminal.
    #[gpui::test]
    fn the_find_bar_searches_the_pane_without_typing_into_it(cx: &mut TestAppContext) {
        let (view, mut peer, cx) = open(cx, &["pane.copy_search", "pane.scroll"]);
        find(&view, cx);
        assert!(cx.debug_bounds("find-bar").is_some());
        assert!(cx.update(|window, cx| view.read(cx).find_focused(window, cx)));

        cx.simulate_keystrokes("x");
        let first = next_request(&mut peer);
        assert_eq!(first["method"], "pane.copy_search");
        assert_eq!(
            first["params"],
            json!({
                "pane_id": "w1:p1",
                "query": "x",
                "direction": "backward",
                // The bottom of the screen when the bar opened: 100 + 24.
                "cursor": {"row": 124, "col": 0},
                "content_revision": 2,
            })
        );
        // A second key waits for the first answer, which is then stale.
        cx.simulate_keystrokes("y");
        answer(&view, &mut peer, cx, &first, matches(&[(3, 0, 0)], 0, 0, 1));
        assert_eq!(
            label(&view, cx),
            "",
            "an answer to the old query is dropped"
        );
        let second = next_request(&mut peer);
        assert_eq!(second["params"]["query"], "xy");

        answer(
            &view,
            &mut peer,
            cx,
            &second,
            matches(&[(10, 2, 3), (110, 0, 1), (120, 4, 5)], 0, 0, 3),
        );
        assert_eq!(label(&view, cx), "1 of 3");
        // Row 10 is above the screen: centered, 12 rows put row 0 on top.
        let scroll = next_request(&mut peer);
        assert_eq!(scroll["method"], "pane.scroll");
        assert_eq!(
            scroll["params"],
            json!({"pane_id": "w1:p1", "offset_from_bottom": 100})
        );
        // The connection holds one request at a time; the scroll's answer
        // frees it for the next search.
        let id = scroll["id"].as_str().unwrap();
        peer.respond("boot-v1", id, &json!({"id": id, "result": {"type": "ok"}}));
        // Matches on screen are tinted in the frame's grid.
        let highlights = view.read_with(cx, |view, _| {
            view.find_highlights(view.live.surface.as_deref().unwrap())
        });
        assert_eq!(highlights.len(), 2);
        assert_eq!(highlights[0].row, 10);
        assert_eq!(highlights[1].row, 20);

        cx.simulate_keystrokes("enter");
        let older = next_request(&mut peer);
        assert_eq!(older["params"]["direction"], "backward");
        assert_eq!(
            older["params"]["previous"],
            json!({"start": {"row": 10, "col": 2}, "end": {"row": 10, "col": 3}})
        );
        cx.simulate_keystrokes("shift-enter");
        answer(
            &view,
            &mut peer,
            cx,
            &older,
            matches(&[(10, 2, 3), (110, 0, 1), (120, 4, 5)], 2, 2, 3),
        );
        assert_eq!(label(&view, cx), "3 of 3");
        let newer = next_request(&mut peer);
        assert_eq!(newer["params"]["direction"], "forward");

        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.find.is_none()));
        cx.update(|window, cx| {
            assert!(
                view.read(cx).focus.is_focused(window),
                "the terminal has the keyboard again"
            );
        });
    }

    #[gpui::test]
    fn find_explains_an_old_daemon_instead_of_opening(cx: &mut TestAppContext) {
        let (view, _peer, cx) = open(cx, &["pane.scroll"]);
        find(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.find.is_none());
            let (flash, _) = view.flash.clone().expect("Find says why it did nothing");
            assert_eq!(
                flash,
                crate::window::Flash::warning("Find needs a newer Herdr daemon")
            );
        });
    }

    /// A reconnect replaces the connection's mailbox; the bar opened on the
    /// old one cannot be answered any more, so it closes.
    #[gpui::test]
    fn a_replaced_connection_retires_the_bar(cx: &mut TestAppContext) {
        let (view, _peer, cx) = open(cx, &["pane.copy_search"]);
        find(&view, cx);
        assert!(view.read_with(cx, |view, _| view.find.is_some()));
        // Cmd-F again keeps the bar on the same pane.
        find(&view, cx);
        assert!(view.read_with(cx, |view, _| view.find.is_some()));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.endpoints[0].connection.scrollback = Arc::default();
                view.poll_find(window, cx);
                assert!(view.find.is_none());
                assert!(view.focus.is_focused(window));
            })
        });
    }
}
