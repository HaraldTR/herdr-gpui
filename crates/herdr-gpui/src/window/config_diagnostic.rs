//! The selected endpoint's daemon config diagnostic, drawn like a notification
//! card over the top-right of the terminal area, where Herdr draws it.
use super::HerdrWindow;
use crate::notifications::safe_text;
use gpui::{prelude::*, *};
use std::sync::Arc;

const MAX_WIDTH: f32 = 420.;

impl HerdrWindow {
    pub(super) fn render_config_diagnostic(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.menu.page.is_some() {
            return None;
        }
        let endpoint = self.endpoints.get(self.selected_endpoint)?;
        let lines = endpoint.config_diagnostic.visible()?.clone();
        let endpoint_id = endpoint.id.clone();
        let generation = endpoint.generation;
        let inbox = endpoint.connection.inbox.clone();
        let accent = self.theme.ink(self.theme.palette[3]);
        Some(
            div()
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .left(px(8.))
                .flex()
                .justify_end()
                .child(
                    div()
                        .id("config-diagnostic")
                        .debug_selector(|| "config-diagnostic".into())
                        .occlude()
                        .min_w_0()
                        .max_w(px(MAX_WIDTH))
                        .flex()
                        .gap(px(8.))
                        .p(px(10.))
                        .rounded(px(crate::config::corners::PANEL))
                        .border_1()
                        .border_color(rgb(accent))
                        .bg(rgb(self.theme.surface))
                        .text_color(rgb(self.theme.foreground))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .mt(px(6.))
                                .size(px(6.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(accent)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_color(rgb(self.theme.muted))
                                        .child(safe_text(&endpoint.label, 80)),
                                )
                                .children(lines.iter().enumerate().map(|(index, line)| {
                                    div()
                                        .debug_selector(move || {
                                            format!("config-diagnostic-line-{index}")
                                        })
                                        .truncate()
                                        .child(line.clone())
                                })),
                        )
                        .child(
                            div()
                                .id("config-diagnostic-dismiss")
                                .debug_selector(|| "config-diagnostic-dismiss".into())
                                .flex_none()
                                .size(px(24.))
                                .rounded(px(crate::config::corners::CONTROL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(self.theme.active)))
                                .child(
                                    svg()
                                        .path("icons/close.svg")
                                        .size(px(12.))
                                        .text_color(rgb(self.theme.foreground)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_config_diagnostic(
                                        &endpoint_id,
                                        generation,
                                        &inbox,
                                        &lines,
                                        cx,
                                    );
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Dismisses the banner only for the connection and text it was drawn
    /// from: a click delivered after either changed does nothing.
    fn dismiss_config_diagnostic(
        &mut self,
        endpoint_id: &str,
        generation: u64,
        inbox: &Arc<std::sync::Mutex<crate::state::LiveState>>,
        lines: &Arc<[String]>,
        cx: &mut Context<Self>,
    ) {
        let Some(endpoint) = self.endpoints.iter_mut().find(|e| {
            e.id == endpoint_id
                && e.generation == generation
                && Arc::ptr_eq(&e.connection.inbox, inbox)
        }) else {
            return;
        };
        if endpoint.config_diagnostic.dismiss(lines) {
            cx.notify();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::sidebar::layout_tests::fixture_window;
    use gpui::{TestAppContext, VisualTestContext, px, size};
    use herdr_client::protocol::ClientShellSnapshot;
    use std::sync::Arc;

    fn set_diagnostic(
        view: &gpui::Entity<crate::HerdrWindow>,
        index: usize,
        diagnostic: Option<&str>,
        cx: &mut VisualTestContext,
    ) {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        snapshot.config_diagnostic = diagnostic.map(str::to_owned);
        view.update(cx, |view, cx| {
            let endpoint = &mut view.endpoints[index];
            endpoint.live.snapshot = Some(Arc::new(snapshot));
            endpoint.sync_live();
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui::test]
    fn banner_follows_snapshot_dismissal_and_selected_endpoint(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture_window);
        cx.simulate_resize(size(px(1000.), px(600.)));
        view.update(cx, |view, _| {
            view.endpoints.push(crate::endpoint::Endpoint::new(
                "remote".into(),
                "Remote".into(),
                herdr_client::ConnectTarget::Socket("/unused-config-diagnostic.sock".into()),
                true,
            ));
        });

        set_diagnostic(&view, 0, None, cx);
        assert!(cx.debug_bounds("config-diagnostic").is_none());

        set_diagnostic(&view, 0, Some("config.toml invalid; using defaults"), cx);
        let banner = cx.debug_bounds("config-diagnostic").unwrap();
        assert!(banner.right() <= px(1000.) && banner.left() >= px(0.));
        assert!(cx.debug_bounds("config-diagnostic-line-0").is_some());

        // Dismissal hides it while the daemon repeats the same text.
        let dismiss = cx.debug_bounds("config-diagnostic-dismiss").unwrap();
        cx.simulate_click(dismiss.center(), Default::default());
        draw(cx);
        assert!(cx.debug_bounds("config-diagnostic").is_none());
        set_diagnostic(&view, 0, Some("config.toml invalid; using defaults"), cx);
        assert!(cx.debug_bounds("config-diagnostic").is_none());

        // Changed text comes back.
        set_diagnostic(&view, 0, Some("config.toml has unknown keys"), cx);
        assert!(cx.debug_bounds("config-diagnostic").is_some());

        // Another endpoint's diagnostic shows only while it is selected.
        set_diagnostic(&view, 1, Some("remote: config.toml invalid"), cx);
        set_diagnostic(&view, 0, None, cx);
        assert!(cx.debug_bounds("config-diagnostic").is_none());
        view.update(cx, |view, _| view.selected_endpoint = 1);
        draw(cx);
        assert!(cx.debug_bounds("config-diagnostic").is_some());
        // Dismissing it leaves the other endpoint's state alone.
        let dismiss = cx.debug_bounds("config-diagnostic-dismiss").unwrap();
        cx.simulate_click(dismiss.center(), Default::default());
        set_diagnostic(&view, 0, Some("local again"), cx);
        view.read_with(cx, |view, _| {
            assert!(view.endpoints[1].config_diagnostic.visible().is_none());
            assert!(view.endpoints[0].config_diagnostic.visible().is_some());
        });
        view.update(cx, |view, _| view.selected_endpoint = 0);
        draw(cx);
        assert!(cx.debug_bounds("config-diagnostic").is_some());

        // Narrow windows keep it inside the viewport.
        cx.simulate_resize(size(px(320.), px(400.)));
        draw(cx);
        let banner = cx.debug_bounds("config-diagnostic").unwrap();
        assert!(banner.left() >= px(0.) && banner.right() <= px(320.));
    }

    #[gpui::test]
    fn stale_dismissal_cannot_hide_new_text(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture_window);
        set_diagnostic(&view, 0, Some("old"), cx);
        let (id, generation, inbox, lines) = view.read_with(cx, |view, _| {
            let endpoint = &view.endpoints[0];
            (
                endpoint.id.clone(),
                endpoint.generation,
                endpoint.connection.inbox.clone(),
                endpoint.config_diagnostic.visible().unwrap().clone(),
            )
        });
        set_diagnostic(&view, 0, Some("new"), cx);
        view.update(cx, |view, cx| {
            view.dismiss_config_diagnostic(&id, generation, &inbox, &lines, cx);
            assert!(view.endpoints[0].config_diagnostic.visible().is_some());
            // A retired connection generation cannot dismiss either.
            let current = view.endpoints[0]
                .config_diagnostic
                .visible()
                .unwrap()
                .clone();
            view.dismiss_config_diagnostic(&id, generation + 1, &inbox, &current, cx);
            assert!(view.endpoints[0].config_diagnostic.visible().is_some());
            view.dismiss_config_diagnostic(&id, generation, &inbox, &current, cx);
            assert!(view.endpoints[0].config_diagnostic.visible().is_none());
        });
    }
}
