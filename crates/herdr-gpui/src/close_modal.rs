use crate::{
    Error, HerdrWindow, Result,
    controls::{self, Command},
    menu::Page,
};
use gpui::{prelude::*, *};
use herdr_client::{Method, protocol::ClientShellSnapshot};
use serde_json::{Value, json};

pub(super) struct CloseConfirmation {
    boot: String,
    workspace: String,
    tab: String,
    pane: Option<String>,
    label: String,
    confirm_selected: bool,
    error: Option<String>,
}

impl CloseConfirmation {
    pub(super) fn capture_pane(snapshot: &ClientShellSnapshot, id: &str) -> Option<Self> {
        let pane = snapshot.panes.iter().find(|pane| pane.pane_id == id)?;
        let mut close = Self::capture_tab(snapshot, &pane.tab_id)?;
        if close.workspace != pane.workspace_id {
            return None;
        }
        close.pane = Some(pane.pane_id.clone());
        close.label = pane.label.clone().unwrap_or_else(|| pane.pane_id.clone());
        close.request(snapshot).ok()?;
        Some(close)
    }

    pub(super) fn capture_tab(snapshot: &ClientShellSnapshot, id: &str) -> Option<Self> {
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == id)?;
        Some(Self {
            boot: snapshot.boot_id.clone(),
            workspace: tab.workspace_id.clone(),
            tab: tab.tab_id.clone(),
            pane: None,
            label: tab.label.clone(),
            confirm_selected: false,
            error: None,
        })
    }

    fn capture(command: Command, snapshot: &ClientShellSnapshot) -> Option<Self> {
        if !matches!(command, Command::ClosePane | Command::CloseTab) {
            return None;
        }
        controls::request(command, snapshot)?;
        let tab = snapshot
            .tabs
            .iter()
            .find(|tab| Some(&tab.tab_id) == snapshot.focused_tab_id.as_ref())?;
        let pane = if command == Command::ClosePane {
            Some(
                snapshot
                    .panes
                    .iter()
                    .find(|pane| Some(&pane.pane_id) == snapshot.focused_pane_id.as_ref())?,
            )
        } else {
            None
        };
        Some(Self {
            boot: snapshot.boot_id.clone(),
            workspace: tab.workspace_id.clone(),
            tab: tab.tab_id.clone(),
            pane: pane.map(|pane| pane.pane_id.clone()),
            label: pane
                .map(|pane| pane.label.clone().unwrap_or_else(|| pane.pane_id.clone()))
                .unwrap_or_else(|| tab.label.clone()),
            confirm_selected: false,
            error: None,
        })
    }

    fn request(&self, snapshot: &ClientShellSnapshot) -> Result<(Method, Value)> {
        if snapshot.boot_id != self.boot
            || !snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.workspace_id == self.workspace)
            || !snapshot
                .tabs
                .iter()
                .any(|tab| tab.tab_id == self.tab && tab.workspace_id == self.workspace)
            || self.pane.as_ref().is_some_and(|id| {
                !snapshot.panes.iter().any(|pane| {
                    &pane.pane_id == id
                        && pane.tab_id == self.tab
                        && pane.workspace_id == self.workspace
                })
            })
        {
            return Err(Error::StaleCloseTarget);
        }
        Ok(if let Some(id) = &self.pane {
            (Method::PaneClose, json!({"pane_id": id}))
        } else {
            (Method::TabClose, json!({"tab_id": self.tab}))
        })
    }
}

impl HerdrWindow {
    pub(super) fn open_tab_close(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(close) = self
            .live
            .snapshot
            .as_ref()
            .and_then(|snapshot| CloseConfirmation::capture_tab(snapshot, id))
        else {
            return;
        };
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.close = Some(close);
        self.menu.page = Some(Page::ConfirmClose);
        if !self.config.confirm_close_tab {
            self.confirm_close(window, cx);
        }
    }

    pub(super) fn open_close_confirmation(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(close) = self
            .live
            .snapshot
            .as_ref()
            .and_then(|snapshot| CloseConfirmation::capture(command, snapshot))
        else {
            return;
        };
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.close = Some(close);
        self.menu.page = Some(Page::ConfirmClose);
        if command == Command::CloseTab && !self.config.confirm_close_tab {
            self.confirm_close(window, cx);
        }
    }

    fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(close) = &self.menu.close else {
            return;
        };
        let result = (|| {
            if !self.menu_target_current() || !self.input_ready() {
                return Err(Error::StaleConnection);
            }
            if close.pane.is_some()
                && self
                    .live
                    .surface
                    .as_ref()
                    .is_some_and(|s| s.popup.is_some())
            {
                return Err(Error::ConnectionNotReady);
            }
            let snapshot = self.live.snapshot.as_ref().ok_or(Error::NotConnected)?;
            close.request(snapshot)
        })();
        match result {
            Ok((method, params)) => {
                self.request_focus_change(method.as_str(), None, |handle, boot| {
                    handle.request(boot, method, params)
                });
                self.dismiss_menu(window, cx);
            }
            Err(error) => {
                if let Some(close) = &mut self.menu.close {
                    close.error = Some(error.to_string());
                }
                cx.notify();
            }
        }
    }

    pub(super) fn close_confirmation_key(
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
                if let Some(close) = &mut self.menu.close {
                    close.confirm_selected = !close.confirm_selected;
                }
                cx.notify();
            }
            "enter" => {
                if self
                    .menu
                    .close
                    .as_ref()
                    .is_some_and(|close| close.confirm_selected)
                {
                    self.confirm_close(window, cx);
                } else {
                    self.dismiss_menu(window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_close_confirmation(&self, cx: &mut Context<Self>) -> Div {
        let Some(close) = &self.menu.close else {
            return div();
        };
        let theme = &self.theme;
        let kind = if close.pane.is_some() { "pane" } else { "tab" };
        div().p(px(12.)).flex().flex_col().gap(px(12.))
            .child(div().text_size(px(self.config.ui.size * 1.35)).font_weight(FontWeight::SEMIBOLD).child(format!("Close {kind}?")))
            .child(div().child(close.label.clone()))
            .child(div().text_color(rgb(theme.muted)).child(if close.pane.is_some() {
                "This terminates the pane and its running processes. This cannot be undone."
            } else { "This terminates every pane and running process in this tab. This cannot be undone." }))
            .when_some(close.error.clone(), |panel, error| panel.child(div().bg(rgb(theme.active)).p(px(8.)).child(error)))
            .child(div().flex().justify_end().gap(px(8.))
                .child(div().id("close-cancel").debug_selector(|| "close-cancel".into()).px(px(12.)).py(px(6.)).rounded(px(4.)).border_1()
                    .border_color(rgb(if close.confirm_selected { theme.active } else { theme.foreground }))
                    .cursor_pointer().hover(|s| s.bg(rgb(theme.active))).child("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx))))
                .child(div().id("close-confirm").debug_selector(|| "close-confirm".into()).px(px(12.)).py(px(6.)).rounded(px(4.)).border_1()
                    .border_color(rgb(if close.confirm_selected { theme.foreground } else { theme.active }))
                    .bg(rgb(theme.active)).cursor_pointer().child(format!("Close {kind}"))
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_close(window, cx)))))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Arc;

    #[gpui::test]
    fn skipping_tab_confirmation_keeps_connection_checks_and_pane_prompt(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            view.config.confirm_close_tab = false;
            view.live.snapshot = Some(Arc::new(
                serde_json::from_str(include_str!(
                    "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
                ))
                .unwrap(),
            ));
            view
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let id = view.live.snapshot.as_ref().unwrap().tabs[0].tab_id.clone();
                view.open_tab_close(&id, window, cx);
                // The disconnected fixture must attempt the close immediately but refuse to send it.
                assert!(view.menu.close.as_ref().unwrap().error.is_some());
                assert!(view.pending_navigation.is_none());
                view.dismiss_menu(window, cx);
                view.open_close_confirmation(Command::CloseTab, window, cx);
                assert!(view.menu.close.as_ref().unwrap().error.is_some());
                view.dismiss_menu(window, cx);
                view.open_close_confirmation(Command::ClosePane, window, cx);
                assert!(view.menu.close.as_ref().unwrap().error.is_none());
                view.dismiss_menu(window, cx);
                view.config.confirm_close_tab = true;
                view.open_tab_close(&id, window, cx);
                assert!(view.menu.close.as_ref().unwrap().error.is_none());
            })
        });
    }

    #[gpui::test]
    fn tab_icon_bounds_and_inactive_cross_confirmation(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
                "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
            ))
            .unwrap();
            let mut tab = snapshot.tabs[0].clone();
            tab.tab_id = "inactive".into();
            tab.focused = false;
            snapshot.tabs.push(tab);
            view.live.snapshot = Some(Arc::new(snapshot));
            view
        });
        for width in [800., 360.] {
            cx.simulate_resize(size(px(width), px(600.)));
            cx.update(|window, cx| window.draw(cx).clear());
            let button = cx.debug_bounds("new-tab").unwrap();
            let icon = cx.debug_bounds("new-tab-icon").unwrap();
            assert!(button.size.width >= px(34.));
            assert_eq!(button.size.height, px(24.));
            assert_eq!(icon.size, size(px(14.), px(14.)));
            // Centred within the content box, which the divider insets by a pixel.
            assert!((button.center().x - icon.center().x).abs() <= px(1.));
            assert_eq!(button.center().y, icon.center().y);
            assert!(button.right() <= px(width));
            // The button follows the last tab rather than the window's right edge,
            // within the rounding of the tab's own one-pixel divider.
            let last = cx.debug_bounds("tab-inactive").unwrap();
            assert!((button.left() - last.right()).abs() <= px(1.), "{last:?}");
        }
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| window.draw(cx).clear());
        let original_focus = view.read_with(cx, |view, _| {
            view.live.snapshot.as_ref().unwrap().focused_tab_id.clone()
        });
        let button = cx.debug_bounds("close-tab-inactive").unwrap();
        let icon = cx.debug_bounds("close-tab-icon-inactive").unwrap();
        assert_eq!(button.size, size(px(18.), px(18.)));
        assert_eq!(icon.size, size(px(12.), px(12.)));
        assert_eq!(button.center(), icon.center());
        // Hugging the tab's inner right edge, clear of the label beside it:
        // three pixels of padding inside the one-pixel divider.
        let tab = cx.debug_bounds("tab-inactive").unwrap();
        assert_eq!(tab.right() - button.right(), px(4.));
        assert!(button.left() - tab.left() >= px(24.));
        for fence in ["cancel", "selection", "generation", "boot"] {
            cx.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
            view.read_with(cx, |view, _| {
                assert!(view.menu.page == Some(Page::ConfirmClose));
                let close = view.menu.close.as_ref().unwrap();
                assert_eq!(close.tab, "inactive");
                assert!(!close.confirm_selected);
                assert_eq!(
                    close.request(view.live.snapshot.as_ref().unwrap()).unwrap(),
                    (Method::TabClose, json!({"tab_id": "inactive"}))
                );
                assert_eq!(
                    view.live.snapshot.as_ref().unwrap().focused_tab_id,
                    original_focus
                );
                assert!(view.pending_navigation.is_none());
            });
            if fence != "cancel" {
                cx.update(|window, cx| {
                    view.update(cx, |view, cx| {
                        match fence {
                            "selection" => view.selection_epoch += 1,
                            "generation" => view.endpoints[0].generation += 1,
                            _ => {
                                let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                                snapshot.boot_id.push_str("-new");
                                assert!(
                                    view.menu.close.as_ref().unwrap().request(snapshot).is_err()
                                );
                            }
                        }
                        view.confirm_close(window, cx);
                        assert!(view.menu.close.as_ref().unwrap().error.is_some());
                        assert!(view.pending_navigation.is_none());
                    });
                });
            }
            cx.simulate_keystrokes("enter");
            assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
            cx.update(|window, cx| window.draw(cx).clear());
        }
    }

    #[test]
    fn explicit_pane_close_retains_inactive_target_and_membership() {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        let mut pane = snapshot.panes[0].clone();
        pane.pane_id = "inactive".into();
        snapshot.panes.push(pane);
        let close = CloseConfirmation::capture_pane(&snapshot, "inactive").unwrap();
        assert!(!close.confirm_selected);
        snapshot.focused_pane_id = None;
        assert_eq!(
            close.request(&snapshot).unwrap(),
            (Method::PaneClose, json!({"pane_id":"inactive"}))
        );
        let original = snapshot.clone();
        for case in 0..6 {
            let mut snapshot = original.clone();
            match case {
                0 => snapshot.boot_id.push('x'),
                1 => snapshot.workspaces.clear(),
                2 => snapshot.tabs.clear(),
                3 => snapshot.panes[1].tab_id.push('x'),
                4 => snapshot.panes[1].workspace_id.push('x'),
                _ => snapshot.panes.truncate(1),
            }
            assert!(matches!(
                close.request(&snapshot),
                Err(Error::StaleCloseTarget)
            ));
        }
    }

    #[test]
    fn explicit_tab_close_does_not_follow_focus() -> anyhow::Result<()> {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))?;
        let mut inactive = snapshot.tabs[0].clone();
        inactive.tab_id = "inactive".into();
        inactive.focused = false;
        snapshot.tabs.push(inactive);
        let close = CloseConfirmation::capture_tab(&snapshot, "inactive")
            .ok_or_else(|| anyhow::anyhow!("missing tab"))?;
        assert!(!close.confirm_selected);
        assert_eq!(
            close.request(&snapshot)?,
            (Method::TabClose, json!({"tab_id":"inactive"}))
        );
        snapshot.tabs.retain(|tab| tab.tab_id != "inactive");
        assert!(close.request(&snapshot).is_err());
        Ok(())
    }
    #[test]
    fn close_retains_original_target_and_rejects_replaced_sessions() -> anyhow::Result<()> {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))?;
        for command in [Command::ClosePane, Command::CloseTab] {
            let close = CloseConfirmation::capture(command, &snapshot)
                .ok_or_else(|| anyhow::anyhow!("missing target"))?;
            assert!(!close.confirm_selected, "Cancel is the safe default");
            let expected = close.request(&snapshot)?;
            let original = snapshot.clone();
            snapshot.focused_pane_id = None;
            snapshot.focused_tab_id = None;
            assert_eq!(close.request(&snapshot)?, expected);
            snapshot.boot_id = "new-boot".into();
            assert!(close.request(&snapshot).is_err());
            snapshot = original.clone();
            snapshot.tabs.clear();
            assert!(close.request(&snapshot).is_err());
            snapshot = original;
        }
        Ok(())
    }
}
