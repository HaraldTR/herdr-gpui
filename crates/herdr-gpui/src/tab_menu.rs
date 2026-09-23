use crate::{HerdrWindow, menu::Page, search_input::SearchInput};
use gpui::{prelude::*, *};
use herdr_client::{Method, protocol::ClientShellSnapshot};
use serde_json::{Value, json};

#[derive(Clone)]
struct Target {
    boot: String,
    workspace: String,
    tab: String,
    label: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Arc;

    #[test]
    fn tab_target_retains_membership_and_rejects_stale_snapshots() {
        let original: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        let target = Target::capture(&original, &original.tabs[0].tab_id).unwrap();
        let mut snapshot = original.clone();
        snapshot.focused_tab_id = None;
        snapshot.focused_workspace_id = None;
        assert!(target.validate(&snapshot).is_ok());
        assert_eq!(
            target.rename_params("  \u{4e2d}  ").unwrap(),
            json!({"tab_id": target.tab, "label": "\u{4e2d}"})
        );
        assert!(target.rename_params(" \u{2003}\t").is_err());
        snapshot.boot_id.push_str("-replaced");
        assert!(target.validate(&snapshot).is_err());
        snapshot = original.clone();
        snapshot.tabs[0].workspace_id.push_str("-moved");
        assert!(target.validate(&snapshot).is_err());
        snapshot = original.clone();
        snapshot.workspaces.clear();
        assert!(target.validate(&snapshot).is_err());
        snapshot = original;
        snapshot.tabs.clear();
        assert!(target.validate(&snapshot).is_err());
    }

    #[gpui::test]
    fn inactive_tab_menu_rename_composition_cancel_and_fences(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            view.live.snapshot = Some(Arc::new(
                serde_json::from_str(include_str!(
                    "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
                ))
                .unwrap(),
            ));
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            let mut tab = snapshot.tabs[0].clone();
            tab.tab_id = "inactive".into();
            tab.label = "Original label".into();
            tab.focused = false;
            snapshot.tabs.push(tab);
            view
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let original_focus = view.read_with(cx, |view, _| {
            view.live.snapshot.as_ref().unwrap().focused_tab_id.clone()
        });
        let tab_bounds = cx.debug_bounds("tab-inactive").unwrap();
        cx.simulate_mouse_down(
            tab_bounds.center(),
            MouseButton::Right,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            tab_bounds.center(),
            MouseButton::Right,
            Modifiers::default(),
        );
        view.read_with(cx, |view, _| {
            let tab = view.menu.tab.as_ref().unwrap();
            assert_eq!(tab.target.tab, "inactive");
            assert_eq!(tab.selected, None);
            assert_eq!(
                view.live.snapshot.as_ref().unwrap().focused_tab_id,
                original_focus
            );
            assert!(view.pending_navigation.is_none());
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::Tab)));
        cx.simulate_keystrokes("down enter");
        let input = view.read_with(cx, |v, _| {
            assert!(v.menu.page == Some(Page::RenameTab));
            v.menu.tab.as_ref().unwrap().input.clone().unwrap()
        });
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                assert_eq!(input.text(), "Original label");
                assert_eq!(
                    input.selected_text_range(false, window, cx).unwrap().range,
                    0..14
                );
                input.replace_and_mark_text_in_range(None, "\u{4e2d}", Some(0..1), window, cx);
            })
        });
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::RenameTab)));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.set_text_selected("", cx);
                input.replace_and_mark_text_in_range(None, "\u{4e2d}", Some(0..1), window, cx);
            });
            window.draw(cx).clear();
        });
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::RenameTab)));
        assert!(view.read_with(cx, |v, _| v.menu.tab.as_ref().unwrap().pending.is_none()));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "", window, cx)
            })
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(
            view.read_with(cx, |v, _| v.menu.tab.as_ref().unwrap().error.clone()),
            Some("Enter a tab name.".into())
        );
        cx.simulate_input("new label");
        cx.simulate_keystrokes("cmd-t cmd-w cmd-b");
        assert!(view.read_with(cx, |v, _| v.sidebar_visible
            && v.menu.page == Some(Page::RenameTab)));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| v.menu.tab.is_none()));
        cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_tab_menu("inactive", tab_bounds.center(), window, cx)
            });
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("tab-menu-1").is_none());
        let rename_row = cx.debug_bounds("tab-menu-0").unwrap();
        cx.simulate_mouse_move(rename_row.center(), None, Modifiers::default());
        assert_eq!(
            view.read_with(cx, |v, _| v.menu.tab.as_ref().unwrap().selected),
            Some(0)
        );
        cx.simulate_keystrokes("down enter");
        assert!(view.read_with(cx, |v, _| v.menu.page == Some(Page::RenameTab)));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| v.menu.page.is_none()));

        for generation in [false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.open_tab_menu("inactive", point(px(799.), px(599.)), window, cx);
                    if generation {
                        view.endpoints[0].generation += 1;
                    } else {
                        view.selection_epoch += 1;
                    }
                    assert!(!view.menu_target_current());
                    for (action, _) in ACTIONS {
                        view.activate_tab_menu(action, window, cx);
                        assert!(view.menu.page == Some(Page::Tab));
                        assert!(view.menu.tab.as_ref().unwrap().error.is_some());
                    }
                })
            });
            cx.simulate_keystrokes("escape");
        }

        for button in [MouseButton::Left, MouseButton::Right] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.open_tab_menu("inactive", point(px(799.), px(599.)), window, cx)
                });
                window.draw(cx).clear();
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert_eq!(panel.left(), (px(788.) - panel.size.width).round());
            assert_eq!(panel.top(), (px(588.) - panel.size.height).round());
            assert!(panel.right() <= px(800.) && panel.bottom() <= px(600.));
            assert!(panel.left() >= px(12.) && panel.top() >= px(12.));
            cx.simulate_mouse_down(point(px(5.), px(5.)), button, Modifiers::default());
            assert!(view.read_with(cx, |v, _| v.menu.page.is_none()));
        }
    }

    #[gpui::test]
    fn rename_response_is_correlated_and_survives_surface_invalidation(cx: &mut TestAppContext) {
        use crate::state::RenameResult;
        use herdr_client::ClientEvent;
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
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
                view.open_tab_menu(&id, Point::default(), window, cx);
                view.activate_tab_menu(Action::Rename, window, cx);
                view.menu.tab.as_mut().unwrap().pending = Some("rename-1".into());
                view.live.tab_rename = Some(RenameResult {
                    request: "rename-1".into(),
                    result: None,
                });
                view.live.apply(ClientEvent::Response {
                    request_id: "unrelated".into(),
                    response: json!({"error":"unrelated failure"}),
                });
                view.poll_tab_rename(window, cx);
                assert!(view.menu.tab.as_ref().unwrap().pending.is_some());
                view.live.apply(ClientEvent::CommandRejected {
                    request_id: Some("rename-1".into()),
                    reason: herdr_client::Error::UnsupportedMethod,
                });
                view.poll_tab_rename(window, cx);
                assert_eq!(
                    view.menu.tab.as_ref().unwrap().error.as_deref(),
                    Some("method not advertised by endpoint")
                );
                assert!(view.menu.tab.as_ref().unwrap().pending.is_none());
                view.menu.tab.as_mut().unwrap().pending = Some("rename-error".into());
                view.live.tab_rename = Some(RenameResult {
                    request: "rename-error".into(),
                    result: None,
                });
                view.live.apply(ClientEvent::Response {
                    request_id: "rename-error".into(),
                    response: json!({"error":{"message":"Invalid label"}}),
                });
                // A later, unrelated response must not overwrite the modal's result.
                view.live.apply(ClientEvent::Response {
                    request_id: "other".into(),
                    response: json!({"result":{}}),
                });
                view.poll_tab_rename(window, cx);
                assert!(
                    view.menu
                        .tab
                        .as_ref()
                        .unwrap()
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("Invalid label")
                );
                view.menu.tab.as_mut().unwrap().pending = Some("rename-2".into());
                view.live.tab_rename = Some(RenameResult {
                    request: "rename-2".into(),
                    result: None,
                });
                view.live.apply(ClientEvent::Response {
                    request_id: "rename-2".into(),
                    response: json!({"result":{}}),
                });
                assert!(!view.input_ready());
                view.poll_tab_rename(window, cx);
                assert!(view.menu.page.is_none());
            })
        });
    }
}

impl Target {
    fn capture(snapshot: &ClientShellSnapshot, id: &str) -> Option<Self> {
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == id)?;
        Some(Self {
            boot: snapshot.boot_id.clone(),
            workspace: tab.workspace_id.clone(),
            tab: tab.tab_id.clone(),
            label: tab.label.clone(),
        })
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
        {
            return Err(crate::Error::StaleTab);
        }
        Ok(())
    }

    fn rename_params(&self, label: &str) -> crate::Result<Value> {
        let label = label.trim();
        if label.is_empty() {
            return Err(crate::Error::EmptyTabName);
        }
        Ok(json!({"tab_id": self.tab, "label": label}))
    }
}

#[derive(Clone, Copy)]
enum Action {
    Rename,
}

const ACTIONS: [(Action, &str); 1] = [(Action::Rename, "Rename")];

pub(super) struct TabMenu {
    target: Target,
    selected: Option<usize>,
    input: Option<Entity<SearchInput>>,
    pending: Option<String>,
    error: Option<String>,
}

impl HerdrWindow {
    pub(super) fn open_tab_menu(
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
        self.menu.page = Some(Page::Tab);
        self.menu.tab = Some(TabMenu {
            target,
            selected: None,
            input: None,
            pending: None,
            error: None,
        });
    }

    fn validate_tab_target(&self) -> crate::Result<&Target> {
        if !self.menu_target_current() {
            return Err(crate::Error::StaleConnection);
        }
        let target = &self.menu.tab.as_ref().ok_or(crate::Error::NoTab)?.target;
        target.validate(
            self.live
                .snapshot
                .as_ref()
                .ok_or(crate::Error::NotConnected)?,
        )?;
        Ok(target)
    }

    fn tab_error(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        if let Some(tab) = &mut self.menu.tab {
            tab.error = Some(error.to_string());
        }
        cx.notify();
    }

    fn activate_tab_menu(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.validate_tab_target() {
            Ok(target) => target.clone(),
            Err(error) => {
                self.tab_error(error, cx);
                return;
            }
        };
        match action {
            Action::Rename => {
                let input = cx.new(SearchInput::new);
                input.update(cx, |input, cx| {
                    input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
                    input.set_placeholder("Tab name", cx);
                    input.set_text_selected(&target.label, cx);
                    window.focus(&input.focus);
                });
                if let Some(tab) = &mut self.menu.tab {
                    tab.input = Some(input);
                    tab.error = None;
                }
                self.menu.page = Some(Page::RenameTab);
                cx.notify();
            }
        }
    }

    fn submit_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = &self.menu.tab else { return };
        let Some(input) = &tab.input else { return };
        if tab.pending.is_some() || input.read(cx).is_composing() {
            return;
        }
        let result = (|| {
            let target = self.validate_tab_target()?;
            let params = target.rename_params(input.read(cx).text())?;
            if !self.input_ready() {
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
            // Install correlation under the same lock used by the event reducer.
            let request = handle.request(&target.boot, Method::TabRename, params)?;
            inbox.tab_rename = Some(crate::state::RenameResult {
                request: request.clone(),
                result: None,
            });
            Ok::<_, crate::Error>(request)
        })();
        match result {
            Ok(request) => {
                if let Some(tab) = &mut self.menu.tab {
                    tab.pending = Some(request);
                    tab.error = None;
                }
                window.focus(&self.menu.focus);
                cx.notify();
            }
            Err(error) => self.tab_error(error, cx),
        }
    }

    pub(super) fn poll_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.menu.tab.as_ref().and_then(|tab| tab.pending.as_ref()) else {
            return;
        };
        let result = if let Err(error) = self.validate_tab_target() {
            Some(Err(std::sync::Arc::new(error)))
        } else {
            self.live
                .tab_rename
                .as_ref()
                .filter(|r| &r.request == request)
                .and_then(|r| r.result.clone())
        };
        match result {
            Some(Ok(())) => self.dismiss_menu(window, cx),
            Some(Err(error)) => {
                if let Some(tab) = &mut self.menu.tab {
                    tab.pending = None;
                    if let Some(input) = &tab.input {
                        window.focus(&input.read(cx).focus);
                    }
                }
                self.tab_error(error, cx);
            }
            None => {}
        }
    }

    pub(super) fn tab_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = &mut self.menu.tab else {
            return;
        };
        let key = event.keystroke.key.as_str();
        if self.menu.page == Some(Page::RenameTab)
            && tab.pending.is_none()
            && (tab
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
            "enter" if self.menu.page == Some(Page::RenameTab) => {
                self.submit_tab_rename(window, cx)
            }
            "up" | "down" if self.menu.page == Some(Page::Tab) => {
                tab.selected = Some(match (tab.selected, key) {
                    (None, "up") => ACTIONS.len() - 1,
                    (None, _) => 0,
                    (Some(i), "up") => (i + ACTIONS.len() - 1) % ACTIONS.len(),
                    (Some(i), _) => (i + 1) % ACTIONS.len(),
                });
                cx.notify();
            }
            "enter" => {
                if let Some(index) = tab.selected {
                    self.activate_tab_menu(ACTIONS[index].0, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_tab_menu(&self, cx: &mut Context<Self>) -> Div {
        let Some(tab) = &self.menu.tab else {
            return div();
        };
        let theme = &self.theme;
        let mut body = div().flex().flex_col();
        if self.menu.page == Some(Page::Tab) {
            for (index, (action, label)) in ACTIONS.into_iter().enumerate() {
                body = body.child(
                    div()
                        .id(("tab-menu-action", index))
                        .debug_selector(move || format!("tab-menu-{index}"))
                        .min_h(px(self.config.ui.line_height() + 12.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(tab.selected == Some(index), |row| row.bg(rgb(theme.active)))
                        .hover(|row| row.bg(rgb(theme.active)))
                        .child(label)
                        .on_hover(cx.listener(move |this, hovered, _, cx| {
                            if *hovered && let Some(tab) = &mut this.menu.tab {
                                tab.selected = Some(index);
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_tab_menu(action, window, cx)
                        })),
                );
            }
        } else {
            body =
                body.p(px(8.))
                    .gap(px(12.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Rename tab"))
                    .when_some(tab.input.clone(), |body, input| {
                        if tab.pending.is_some() {
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
                                    .id("tab-rename-cancel")
                                    .p(px(6.))
                                    .cursor_pointer()
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_menu(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("tab-rename-submit")
                                    .p(px(6.))
                                    .rounded(px(4.))
                                    .bg(rgb(theme.active))
                                    .cursor_pointer()
                                    .child(if tab.pending.is_some() {
                                        "Renaming..."
                                    } else {
                                        "Rename"
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_tab_rename(window, cx)
                                    })),
                            ),
                    );
        }
        body.when_some(tab.error.clone(), |body, error| {
            body.child(div().p(px(8.)).child(error))
        })
    }
}
