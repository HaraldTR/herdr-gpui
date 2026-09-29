use crate::{HerdrWindow, close_modal::CloseConfirmation, menu::Page, search_input::SearchInput};
use gpui::{prelude::*, *};
use herdr_client::{Method, protocol::ClientShellSnapshot};
use serde_json::{Value, json};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::prelude::v1::test;
    use herdr_client::ClientEvent;
    use std::sync::Arc;

    fn snapshot() -> ClientShellSnapshot {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        let mut pane = snapshot.panes[0].clone();
        pane.pane_id = "inactive".into();
        pane.label = Some("Original label".into());
        snapshot.panes.push(pane);
        snapshot
    }

    #[test]
    fn captured_pane_payloads_ignore_focus_and_reject_stale_membership() {
        let original = snapshot();
        let target = Target::capture(&original, "inactive").unwrap();
        let mut snapshot = original.clone();
        snapshot.focused_pane_id = None;
        snapshot.focused_tab_id = None;
        snapshot.focused_workspace_id = None;
        assert!(target.validate(&snapshot).is_ok());
        assert_eq!(
            target.rename_params("  \u{4e2d}  "),
            json!({"pane_id":"inactive", "label":"\u{4e2d}"})
        );
        assert_eq!(
            target.rename_params(" \u{2003}\t"),
            json!({"pane_id":"inactive", "label":""})
        );
        for (action, direction) in [(Action::SplitRight, "right"), (Action::SplitDown, "down")] {
            assert_eq!(
                action.request(&target).unwrap(),
                (
                    Method::PaneSplit,
                    json!({"target_pane_id":"inactive", "direction":direction, "focus":true})
                )
            );
        }
        assert_eq!(
            Action::Zoom.request(&target).unwrap(),
            (
                Method::PaneZoom,
                json!({"pane_id":"inactive", "mode":"toggle"})
            )
        );
        for case in 0..7 {
            let mut snapshot = original.clone();
            match case {
                0 => snapshot.boot_id.push('x'),
                1 => snapshot.workspaces.clear(),
                2 => snapshot.tabs.clear(),
                3 => snapshot.tabs[0].workspace_id.push('x'),
                4 => snapshot.panes[1].tab_id.push('x'),
                5 => snapshot.panes[1].workspace_id.push('x'),
                _ => snapshot.panes.truncate(1),
            }
            assert!(matches!(
                target.validate(&snapshot),
                Err(crate::Error::StalePane)
            ));
        }
    }

    #[gpui::test]
    fn menu_isolation_rename_composition_close_and_endpoint_fences(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            view.live.snapshot = Some(Arc::new(snapshot()));
            view
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_pane_menu("inactive", point(px(799.), px(599.)), window, cx)
            });
            window.draw(cx).clear(cx);
        });
        let panel = cx.debug_bounds("menu-panel").unwrap();
        assert_eq!(panel.left(), (px(788.) - panel.size.width).round());
        assert_eq!(panel.top(), (px(588.) - panel.size.height).round());
        assert!(panel.right() <= px(800.) && panel.bottom() <= px(600.));
        for selector in [
            "pane-menu-0",
            "pane-menu-1",
            "pane-menu-2",
            "pane-menu-3",
            "pane-menu-4",
        ] {
            assert!(cx.debug_bounds(selector).is_some());
        }
        assert!(cx.debug_bounds("pane-menu-5").is_none());
        cx.simulate_keystrokes("enter cmd-t cmd-w cmd-b");
        view.read_with(cx, |v, _| {
            assert_eq!(v.menu.page, Some(Page::Pane));
            assert!(v.sidebar_visible && v.pending_navigation.is_none());
            assert_ne!(
                v.live.snapshot.as_ref().unwrap().focused_pane_id.as_deref(),
                Some("inactive")
            );
        });
        cx.simulate_keystrokes("down enter");
        let input = view.read_with(cx, |v, _| {
            assert_eq!(v.menu.page, Some(Page::RenamePane));
            v.menu.pane.as_ref().unwrap().input.clone().unwrap()
        });
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                assert_eq!(input.text(), "Original label");
                assert_eq!(
                    input.selected_text_range(false, window, cx).unwrap().range,
                    0..14
                );
                input.replace_and_mark_text_in_range(None, "\u{4e2d}", Some(0..1), window, cx);
            });
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::RenamePane)));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.set_text_selected("", cx);
                input.replace_and_mark_text_in_range(None, "\u{4e2d}", Some(0..1), window, cx);
            });
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("escape");
        assert!(
            view.read_with(cx, |v, _| v.menu.page == Some(Page::RenamePane)
                && v.menu.pane.as_ref().unwrap().pending.is_none())
        );
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "", window, cx)
            })
        });
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));

        for fence in [false, true] {
            cx.update(|window, cx| {
                view.update(cx, |v, cx| {
                    v.open_pane_menu("inactive", Point::default(), window, cx);
                    if fence {
                        v.endpoints[0].generation += 1;
                    } else {
                        v.selection_epoch += 1;
                    }
                    for (action, _) in ACTIONS {
                        v.activate_pane_menu(action, window, cx);
                        assert_eq!(v.menu.page, Some(Page::Pane));
                        assert!(v.menu.pane.as_ref().unwrap().error.is_some());
                        assert!(v.menu.close.is_none());
                    }
                    v.dismiss_menu(window, cx);
                })
            });
        }
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_pane_menu("inactive", Point::default(), window, cx);
                v.activate_pane_menu(Action::Close, window, cx);
                assert_eq!(v.menu.page, Some(Page::ConfirmClose));
            })
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.menu.page.is_none()));
        for button in [MouseButton::Left, MouseButton::Right] {
            cx.update(|window, cx| {
                view.update(cx, |v, cx| {
                    v.open_pane_menu("inactive", point(px(799.), px(599.)), window, cx)
                });
                window.draw(cx).clear(cx);
            });
            cx.simulate_mouse_down(point(px(5.), px(5.)), button, Modifiers::default());
            assert!(view.read_with(cx, |v, _| v.menu.page.is_none()));
        }
    }

    #[gpui::test]
    fn right_click_targets_inactive_pane_and_blocks_stale_surfaces(cx: &mut TestAppContext) {
        use herdr_client::protocol::{FrameData, PaneSurfaceFrame, PaneSurfacePane, SurfaceRect};
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            crate::sidebar::layout_tests::fixture_window(window, cx)
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let position = cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                // Initialize surface interest without connecting to a personal daemon.
                // A cancelled handle lets queue-failure paths run deterministically.
                v.reconnect();
                let client = herdr_client::connect(
                    herdr_client::ConnectTarget::Socket("/unused-pane-menu-test.sock".into()),
                    v.options,
                )
                .unwrap();
                client.handle.disconnect();
                v.endpoints[0].connection.handle = Some(client.handle);
                let snapshot = snapshot();
                let rect = SurfaceRect {
                    x: 0,
                    y: 0,
                    width: v.options.surface_size.cols,
                    height: v.options.surface_size.rows,
                };
                v.live.surface = Some(Arc::new(PaneSurfaceFrame {
                    boot_id: snapshot.boot_id.clone(),
                    projection_revision: snapshot.revision,
                    surface_revision: 1,
                    frame: FrameData {
                        width: rect.width,
                        height: rect.height,
                        cells: vec![],
                        cursor: None,
                        hyperlinks: vec![],
                        graphics: vec![],
                    },
                    panes: vec![PaneSurfacePane {
                        pane_id: "inactive".into(),
                        content_revision: 1,
                        rect,
                        inner_rect: rect,
                        scrollbar_rect: None,
                        scroll: None,
                        focused: false,
                        mouse_reporting: false,
                        sgr_pixel_mouse: false,
                        alternate_screen_active: false,
                        pixel_width: 0,
                        pixel_height: 0,
                    }],
                    splits: vec![],
                    popup: None,
                    graphics: Default::default(),
                }));
                v.live.snapshot = Some(Arc::new(snapshot));
                v.live.status = crate::state::ConnectionStatus::Connected;
                assert!(v.input_ready());
                v.dismiss_menu(window, cx);
                v.bounds.origin
                    + point(
                        px(v.cell_width * 2.),
                        px(v.config.terminal.line_height() * 2.),
                    )
            })
        });
        // Use the actual terminal mouse handler, not just menu construction.
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        // A second press before release must not dismiss the menu just opened.
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::Pane)));
        cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
        view.read_with(cx, |v, _| {
            assert!(!v.menu.opening_right_click);
            assert_eq!(v.menu.pane.as_ref().unwrap().target.pane, "inactive");
            assert!(v.pending_navigation.is_none());
            assert_ne!(
                v.live.snapshot.as_ref().unwrap().focused_pane_id.as_deref(),
                Some("inactive")
            );
        });
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                for action in [Action::SplitRight, Action::SplitDown, Action::Zoom] {
                    v.activate_pane_menu(action, window, cx);
                    assert_eq!(v.menu.page, Some(Page::Pane));
                    assert!(v.menu.pane.as_ref().unwrap().error.is_some());
                }
                v.dismiss_menu(window, cx);
                let surface = v.live.surface.clone().unwrap();
                // Retain the presented picture, but never use it to aim a new action.
                assert!(v.presentation.frame(&v.live).is_some());
                v.live.surface = None;
                assert!(v.presentation.frame(&v.live).is_some());
                v.open_pane_menu_at(position, window, cx);
                assert!(v.menu.page.is_none());
                v.live.surface = Some(surface.clone());
                Arc::make_mut(v.live.snapshot.as_mut().unwrap()).revision += 1;
                v.open_pane_menu_at(position, window, cx);
                assert!(v.menu.page.is_none());
                Arc::make_mut(v.live.snapshot.as_mut().unwrap()).revision -= 1;
                v.options.surface_size.cols += 1;
                v.open_pane_menu_at(position, window, cx);
                assert!(v.menu.page.is_none());
                v.options.surface_size.cols -= 1;
                Arc::make_mut(v.live.surface.as_mut().unwrap()).popup =
                    Some(Box::new(herdr_client::protocol::ClientShellPopupSurface {
                        terminal_id: "popup".into(),
                        title: String::new(),
                        width: None,
                        height: None,
                        frame: surface.frame.clone(),
                        mouse_reporting: false,
                        sgr_pixel_mouse: false,
                        pixel_width: 0,
                        pixel_height: 0,
                    }));
                assert!(v.input_ready());
                v.open_pane_menu_at(position, window, cx);
                assert!(v.menu.page.is_none());
                v.live.surface = Some(surface);
                Arc::make_mut(v.live.snapshot.as_mut().unwrap())
                    .panes
                    .truncate(1);
                assert!(v.input_ready());
                v.open_pane_menu_at(position, window, cx);
                assert!(v.menu.page.is_none());
            })
        });
    }

    #[gpui::test]
    fn pane_rename_correlates_responses_and_preserves_other_dialogs(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            view.live.snapshot = Some(Arc::new(snapshot()));
            view
        });
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_pane_menu("inactive", Point::default(), window, cx);
                v.activate_pane_menu(Action::Rename, window, cx);
                v.live.dialog_response = Some(("removal".into(), None));
                for outcome in ["rejected", "daemon-error", "success"] {
                    v.menu.pane.as_mut().unwrap().pending = Some(outcome.into());
                    v.live.pane_rename = Some(crate::state::RenameResult {
                        request: outcome.into(),
                        result: None,
                    });
                    v.live.apply(ClientEvent::Response {
                        request_id: "unrelated".into(),
                        response: json!({"error":"unrelated"}),
                    });
                    v.poll_pane_rename(window, cx);
                    assert!(v.menu.pane.as_ref().unwrap().pending.is_some());
                    if outcome == "rejected" {
                        v.live.apply(ClientEvent::CommandRejected {
                            request_id: Some(outcome.into()),
                            reason: herdr_client::Error::UnsupportedMethod,
                        });
                    } else {
                        v.live.apply(ClientEvent::Response {
                            request_id: outcome.into(),
                            response: if outcome == "success" {
                                json!({"result":{}})
                            } else {
                                json!({"error":{"message":"Invalid label"}})
                            },
                        });
                    }
                    v.live.apply(ClientEvent::Response {
                        request_id: "removal".into(),
                        response: json!({"result":{}}),
                    });
                    // A rename response must survive an unrelated response and invalidated surface.
                    assert!(!v.input_ready());
                    v.poll_pane_rename(window, cx);
                    if outcome == "success" {
                        assert!(v.menu.page.is_none());
                    } else {
                        let pane = v.menu.pane.as_ref().unwrap();
                        assert!(pane.pending.is_none());
                        assert!(
                            pane.error
                                .as_ref()
                                .unwrap()
                                .contains(if outcome == "rejected" {
                                    "method not advertised"
                                } else {
                                    "Invalid label"
                                })
                        );
                    }
                }
                assert!(
                    matches!(&v.live.dialog_response, Some((id, Some(Ok(_)))) if id == "removal")
                );
                for fence in ["selection", "generation", "boot", "membership"] {
                    v.live.snapshot = Some(Arc::new(snapshot()));
                    v.open_pane_menu("inactive", Point::default(), window, cx);
                    v.activate_pane_menu(Action::Rename, window, cx);
                    v.menu.pane.as_mut().unwrap().pending = Some("late".into());
                    v.live.pane_rename = Some(crate::state::RenameResult {
                        request: "late".into(),
                        result: Some(Ok(())),
                    });
                    match fence {
                        "selection" => v.selection_epoch += 1,
                        "generation" => v.endpoints[0].generation += 1,
                        "boot" => Arc::make_mut(v.live.snapshot.as_mut().unwrap())
                            .boot_id
                            .push('x'),
                        _ => Arc::make_mut(v.live.snapshot.as_mut().unwrap())
                            .panes
                            .truncate(1),
                    }
                    v.poll_pane_rename(window, cx);
                    assert_eq!(v.menu.page, Some(Page::RenamePane));
                    assert!(v.menu.pane.as_ref().unwrap().error.is_some());
                    assert!(v.menu.pane.as_ref().unwrap().pending.is_none());
                    v.dismiss_menu(window, cx);
                }
            })
        });
    }
}

#[derive(Clone)]
struct Target {
    boot: String,
    workspace: String,
    tab: String,
    pane: String,
    label: String,
}

impl Target {
    fn capture(snapshot: &ClientShellSnapshot, id: &str) -> Option<Self> {
        let pane = snapshot.panes.iter().find(|pane| pane.pane_id == id)?;
        let target = Self {
            boot: snapshot.boot_id.clone(),
            workspace: pane.workspace_id.clone(),
            tab: pane.tab_id.clone(),
            pane: pane.pane_id.clone(),
            label: pane.label.clone().unwrap_or_default(),
        };
        target.validate(snapshot).ok()?;
        Some(target)
    }

    fn validate(&self, snapshot: &ClientShellSnapshot) -> crate::Result<()> {
        if snapshot.boot_id != self.boot
            || !snapshot
                .workspaces
                .iter()
                .any(|w| w.workspace_id == self.workspace)
            || !snapshot
                .tabs
                .iter()
                .any(|t| t.tab_id == self.tab && t.workspace_id == self.workspace)
            || !snapshot.panes.iter().any(|p| {
                p.pane_id == self.pane && p.tab_id == self.tab && p.workspace_id == self.workspace
            })
        {
            return Err(crate::Error::StalePane);
        }
        Ok(())
    }

    fn rename_params(&self, label: &str) -> Value {
        json!({"pane_id": self.pane, "label": label.trim()})
    }
}

#[derive(Clone, Copy)]
enum Action {
    Rename,
    SplitRight,
    SplitDown,
    Zoom,
    Close,
}

impl Action {
    fn request(self, target: &Target) -> Option<(Method, Value)> {
        Some(match self {
            Self::SplitRight | Self::SplitDown => (
                Method::PaneSplit,
                json!({
                    "target_pane_id": target.pane,
                    "direction": if matches!(self, Self::SplitRight) { "right" } else { "down" },
                    "focus": true,
                }),
            ),
            Self::Zoom => (
                Method::PaneZoom,
                json!({"pane_id": target.pane, "mode": "toggle"}),
            ),
            Self::Rename | Self::Close => return None,
        })
    }
}

const ACTIONS: [(Action, &str); 5] = [
    (Action::Rename, "Rename"),
    (Action::SplitRight, "Split Right"),
    (Action::SplitDown, "Split Down"),
    (Action::Zoom, "Toggle Zoom"),
    (Action::Close, "Close"),
];

pub(super) struct PaneMenu {
    target: Target,
    selected: Option<usize>,
    input: Option<Entity<SearchInput>>,
    pending: Option<String>,
    error: Option<String>,
}

impl HerdrWindow {
    pub(super) fn open_pane_menu_at(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pressed_terminal_link = None;
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        let Some(surface) = &self.live.surface else {
            return;
        };
        let Some(id) = crate::terminal::pane_at(
            surface,
            self.bounds,
            position,
            self.cell_width,
            self.config.terminal.line_height(),
        ) else {
            return;
        };
        let id = id.to_owned();
        self.open_pane_menu(&id, position, window, cx);
    }

    fn open_pane_menu(
        &mut self,
        id: &str,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self
            .live
            .snapshot
            .as_ref()
            .and_then(|s| Target::capture(s, id))
        else {
            return;
        };
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Pane);
        self.menu.pane = Some(PaneMenu {
            target,
            selected: None,
            input: None,
            pending: None,
            error: None,
        });
    }

    fn validate_pane_target(&self) -> crate::Result<&Target> {
        if !self.menu_target_current() {
            return Err(crate::Error::StaleConnection);
        }
        let target = &self
            .menu
            .pane
            .as_ref()
            .ok_or(crate::Error::StalePane)?
            .target;
        target.validate(
            self.live
                .snapshot
                .as_ref()
                .ok_or(crate::Error::NotConnected)?,
        )?;
        Ok(target)
    }

    fn pane_error(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        if let Some(pane) = &mut self.menu.pane {
            pane.error = Some(error.to_string());
        }
        cx.notify();
    }

    fn activate_pane_menu(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.validate_pane_target() {
            Ok(target) => target.clone(),
            Err(error) => {
                self.pane_error(error, cx);
                return;
            }
        };
        match action {
            Action::Rename => {
                let input = cx.new(SearchInput::new);
                input.update(cx, |input, cx| {
                    input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
                    input.set_placeholder("Pane name (blank clears)", cx);
                    input.set_text_selected(&target.label, cx);
                    window.focus(&input.focus, cx);
                });
                if let Some(pane) = &mut self.menu.pane {
                    pane.input = Some(input);
                    pane.error = None;
                }
                self.menu.page = Some(Page::RenamePane);
                cx.notify();
            }
            Action::Close => {
                self.menu.close = self
                    .live
                    .snapshot
                    .as_ref()
                    .and_then(|s| CloseConfirmation::capture_pane(s, &target.pane));
                if self.menu.close.is_some() {
                    // Keep the original endpoint fence, rather than reopening the menu.
                    self.menu.page = Some(Page::ConfirmClose);
                    cx.notify();
                }
            }
            action => {
                let result = (|| {
                    let (method, params) = action
                        .request(&target)
                        .ok_or(crate::Error::UnsupportedCommand)?;
                    if !self.input_ready()
                        || self
                            .live
                            .surface
                            .as_ref()
                            .is_some_and(|s| s.popup.is_some())
                    {
                        return Err(crate::Error::ConnectionNotReady);
                    }
                    let handle = self.endpoints[self.selected_endpoint]
                        .connection
                        .handle
                        .as_ref()
                        .ok_or(crate::Error::NotConnected)?;
                    handle.request(&target.boot, method, params)?;
                    Ok::<_, crate::Error>(())
                })();
                match result {
                    Ok(()) => {
                        self.fence_focus_change(None);
                        self.dismiss_menu(window, cx);
                    }
                    Err(error) => self.pane_error(error, cx),
                }
            }
        }
    }

    fn submit_pane_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = &self.menu.pane else { return };
        let Some(input) = &pane.input else { return };
        if pane.pending.is_some() || input.read(cx).is_composing() {
            return;
        }
        let result = (|| {
            let target = self.validate_pane_target()?;
            if !self.input_ready()
                || self
                    .live
                    .surface
                    .as_ref()
                    .is_some_and(|s| s.popup.is_some())
            {
                return Err(crate::Error::ConnectionNotReady);
            }
            let endpoint = &self.endpoints[self.selected_endpoint];
            let handle = endpoint
                .connection
                .handle
                .as_ref()
                .ok_or(crate::Error::NotConnected)?;
            let mut inbox = endpoint
                .connection
                .inbox
                .try_lock()
                .map_err(|_| crate::Error::ConnectionBusy)?;
            // Register under the reducer's lock so even an immediate reply is retained.
            let request = handle.request(
                &target.boot,
                Method::PaneRename,
                target.rename_params(input.read(cx).text()),
            )?;
            inbox.pane_rename = Some(crate::state::RenameResult {
                request: request.clone(),
                result: None,
            });
            Ok::<_, crate::Error>(request)
        })();
        match result {
            Ok(request) => {
                if let Some(pane) = &mut self.menu.pane {
                    pane.pending = Some(request);
                    pane.error = None;
                }
                window.focus(&self.menu.focus, cx);
                cx.notify();
            }
            Err(error) => self.pane_error(error, cx),
        }
    }

    pub(super) fn poll_pane_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self
            .menu
            .pane
            .as_ref()
            .and_then(|pane| pane.pending.as_ref())
        else {
            return;
        };
        let result = if let Err(error) = self.validate_pane_target() {
            Some(Err(std::sync::Arc::new(error)))
        } else {
            self.live
                .pane_rename
                .as_ref()
                .filter(|rename| &rename.request == request)
                .and_then(|rename| rename.result.clone())
        };
        match result {
            Some(Ok(())) => self.dismiss_menu(window, cx),
            Some(Err(error)) => {
                if let Some(pane) = &mut self.menu.pane {
                    pane.pending = None;
                    if let Some(input) = &pane.input {
                        window.focus(&input.read(cx).focus.clone(), cx);
                    }
                }
                self.pane_error(error, cx);
            }
            None => {}
        }
    }

    pub(super) fn pane_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = &mut self.menu.pane else {
            return;
        };
        let key = event.keystroke.key.as_str();
        if self.menu.page == Some(Page::RenamePane)
            && pane.pending.is_none()
            && (pane
                .input
                .as_ref()
                .is_some_and(|input| input.read(cx).is_composing())
                || !matches!(key, "escape" | "enter"))
        {
            return;
        }
        cx.stop_propagation();
        window.prevent_default();
        match key {
            "escape" => self.dismiss_menu(window, cx),
            "enter" if self.menu.page == Some(Page::RenamePane) => {
                self.submit_pane_rename(window, cx)
            }
            "up" | "down" if self.menu.page == Some(Page::Pane) => {
                pane.selected = Some(match (pane.selected, key) {
                    (None, "up") => ACTIONS.len() - 1,
                    (None, _) => 0,
                    (Some(i), "up") => (i + ACTIONS.len() - 1) % ACTIONS.len(),
                    (Some(i), _) => (i + 1) % ACTIONS.len(),
                });
                cx.notify();
            }
            "enter" => {
                if let Some(index) = pane.selected {
                    self.activate_pane_menu(ACTIONS[index].0, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_pane_menu(&self, cx: &mut Context<Self>) -> Div {
        let Some(pane) = &self.menu.pane else {
            return div();
        };
        let mut body = div().flex().flex_col();
        if self.menu.page == Some(Page::Pane) {
            for (index, (action, label)) in ACTIONS.into_iter().enumerate() {
                body = body.child(
                    div()
                        .id(("pane-menu-action", index))
                        .debug_selector(move || format!("pane-menu-{index}"))
                        .min_h(px(self.config.ui.line_height() + 12.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(pane.selected == Some(index), |row| {
                            row.bg(rgb(self.theme.active))
                        })
                        .hover(|row| row.bg(rgb(self.theme.active)))
                        .child(label)
                        .on_hover(cx.listener(move |this, hovered, _, cx| {
                            if *hovered && let Some(pane) = &mut this.menu.pane {
                                pane.selected = Some(index);
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_pane_menu(action, window, cx)
                        })),
                );
            }
        } else {
            body =
                body.p(px(8.))
                    .gap(px(12.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Rename pane"))
                    .child(div().child("Leave blank to clear the custom label."))
                    .when_some(pane.input.clone(), |body, input| {
                        if pane.pending.is_some() {
                            body.child(div().child(input.read(cx).text().to_owned()))
                        } else {
                            body.child(input)
                        }
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("pane-rename-cancel")
                                    .p(px(6.))
                                    .cursor_pointer()
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_menu(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("pane-rename-submit")
                                    .p(px(6.))
                                    .rounded(px(crate::config::corners::CONTROL))
                                    .bg(rgb(self.theme.active))
                                    .cursor_pointer()
                                    .child(if pane.pending.is_some() {
                                        "Renaming..."
                                    } else {
                                        "Rename"
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_pane_rename(window, cx)
                                    })),
                            ),
                    );
        }
        body.when_some(pane.error.clone(), |body, error| {
            body.child(div().p(px(8.)).child(error))
        })
    }
}
