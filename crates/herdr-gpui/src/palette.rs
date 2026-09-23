use crate::{
    Error, HerdrWindow, NavigationTarget, Result,
    controls::{COMMANDS, Command},
    menu::Page,
    search_input::{Changed, SearchInput},
};
use gpui::{prelude::*, *};
use herdr_client::{
    Method,
    protocol::{ClientShellCommandAction, ClientShellSnapshot},
};
use serde_json::{Value, json};

#[derive(Clone)]
enum Action {
    Native(Command),
    Workspace(String),
    Configured(String, ClientShellCommandAction),
}

#[derive(Clone)]
struct Entry {
    label: String,
    detail: String,
    badge: &'static str,
    action: Action,
}

#[derive(Clone)]
struct Target {
    boot: String,
    workspace: Option<String>,
    tab: Option<String>,
    pane: Option<String>,
}

impl Target {
    fn capture(snapshot: &ClientShellSnapshot) -> Self {
        Self {
            boot: snapshot.boot_id.clone(),
            workspace: snapshot.focused_workspace_id.clone(),
            tab: snapshot.focused_tab_id.clone(),
            pane: snapshot.focused_pane_id.clone(),
        }
    }

    fn validate_boot(&self, snapshot: &ClientShellSnapshot) -> Result<()> {
        if self.boot.is_empty() || self.boot != snapshot.boot_id {
            return Err(Error::PaletteSessionChanged);
        }
        Ok(())
    }

    fn workspace_exists(&self, snapshot: &ClientShellSnapshot, id: &str) -> Result<()> {
        self.validate_boot(snapshot)?;
        if !snapshot.workspaces.iter().any(|w| w.workspace_id == id) {
            return Err(Error::PaletteWorkspaceRemoved);
        }
        Ok(())
    }

    fn invocation(
        &self,
        snapshot: &ClientShellSnapshot,
        id: &str,
        action: ClientShellCommandAction,
    ) -> Result<Value> {
        self.validate_boot(snapshot)?;
        if action == ClientShellCommandAction::Unknown {
            return Err(Error::UnsupportedCommand);
        }
        if !snapshot
            .commands
            .iter()
            .any(|c| c.command_id == id && c.action == action)
        {
            return Err(Error::PaletteCommandChanged);
        }
        if let Some(id) = &self.workspace {
            self.workspace_exists(snapshot, id)?;
        }
        if let Some(id) = &self.tab
            && !snapshot
                .tabs
                .iter()
                .any(|t| t.tab_id == *id && self.workspace.as_ref() == Some(&t.workspace_id))
        {
            return Err(Error::PaletteTabRemoved);
        }
        if let Some(id) = &self.pane
            && !snapshot.panes.iter().any(|p| {
                p.pane_id == *id
                    && self.workspace.as_ref() == Some(&p.workspace_id)
                    && self.tab.as_ref() == Some(&p.tab_id)
            })
        {
            return Err(Error::PalettePaneRemoved);
        }
        let mut params = json!({"command_id": id});
        for (key, value) in [
            ("workspace_id", &self.workspace),
            ("tab_id", &self.tab),
            ("pane_id", &self.pane),
        ] {
            if let Some(value) = value {
                params[key] = json!(value);
            }
        }
        Ok(params)
    }
}

fn matches_query(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|token| text.contains(&token.to_lowercase()))
}

pub(super) struct Palette {
    pub search: Entity<SearchInput>,
    entries: Vec<Entry>,
    filtered: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
    target: Option<Target>,
    workspaces_only: bool,
    error: Option<String>,
    _subscription: Subscription,
}

impl Palette {
    fn filter(&mut self, query: &str) {
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let id = match &entry.action {
                    Action::Configured(id, _) | Action::Workspace(id) => id.as_str(),
                    Action::Native(_) => "",
                };
                matches_query(
                    &format!("{} {} {} {id}", entry.label, entry.detail, entry.badge),
                    query,
                )
                .then_some(index)
            })
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }
}

impl HerdrWindow {
    pub(super) fn open_palette(
        &mut self,
        workspaces_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::Palette);
        let search = cx.new(SearchInput::new);
        let subscription = cx.subscribe(&search, |this, search, _: &Changed, cx| {
            if let Some(palette) = &mut this.menu.palette {
                palette.filter(search.read(cx).text());
                cx.notify();
            }
        });
        let mut entries = Vec::new();
        if !workspaces_only {
            entries.extend(
                COMMANDS
                    .iter()
                    .filter(|info| info.command != Command::Palette)
                    .map(|info| Entry {
                        label: info.label.into(),
                        detail: info.shortcut.into(),
                        badge: "",
                        action: Action::Native(info.command),
                    }),
            );
        }
        let target = self.live.snapshot.as_ref().map(|snapshot| {
            if workspaces_only {
                entries.extend(snapshot.workspaces.iter().map(|workspace| Entry {
                    label: workspace.label.clone(),
                    detail: format!(
                        "#{}  {}",
                        workspace.number,
                        workspace.branch.as_deref().unwrap_or("")
                    ),
                    // Every row here is a workspace; a badge saying so is noise.
                    badge: "",
                    action: Action::Workspace(workspace.workspace_id.clone()),
                }));
            } else {
                entries.extend(snapshot.commands.iter().map(|command| {
                    let mut bindings = command.binding_labels.clone();
                    if !command.binding_label.is_empty()
                        && !bindings.contains(&command.binding_label)
                    {
                        bindings.push(command.binding_label.clone());
                    }
                    bindings.retain(|binding| !binding.is_empty());
                    Entry {
                        label: command
                            .description
                            .as_ref()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(&command.command_id)
                            .clone(),
                        detail: if bindings.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "Daemon bindings: {} (not GUI shortcuts)",
                                bindings.join(", ")
                            )
                        },
                        badge: "Herdr command",
                        action: Action::Configured(command.command_id.clone(), command.action),
                    }
                }));
            }
            Target::capture(snapshot)
        });
        search.update(cx, |input, cx| {
            input.set_placeholder(
                if workspaces_only {
                    "Search workspaces..."
                } else {
                    "Search commands..."
                },
                cx,
            );
            input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
            window.focus(&input.focus);
        });
        let mut palette = Palette {
            search,
            entries,
            filtered: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            target,
            workspaces_only,
            error: None,
            _subscription: subscription,
        };
        palette.filter("");
        self.menu.palette = Some(palette);
        cx.notify();
    }

    fn activate_palette(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if !self.menu_target_current() {
            if let Some(palette) = &mut self.menu.palette {
                palette.error = Some("The selected connection changed. Reopen the palette.".into());
            }
            cx.notify();
            return;
        }
        if let Action::Native(command) = action {
            self.dismiss_menu(window, cx);
            self.command(command, window, cx);
            return;
        }
        let result = (|| {
            if !self.input_ready() {
                return Err(Error::PaletteConnectionNotReady);
            }
            let snapshot = self.live.snapshot.as_ref().ok_or(Error::NoSnapshot)?;
            let target = self
                .menu
                .palette
                .as_ref()
                .and_then(|p| p.target.as_ref())
                .ok_or(Error::NoPaletteSession)?;
            match &action {
                Action::Workspace(id) => target.workspace_exists(snapshot, id).map(|()| None),
                Action::Configured(id, action) => {
                    let params = target.invocation(snapshot, id, *action)?;
                    Ok(Some(params))
                }
                Action::Native(_) => unreachable!(),
            }
        })();
        match result {
            Ok(params) => {
                if let Some(params) = params {
                    self.request_focus_change(
                        Method::CommandInvoke.as_str(),
                        None,
                        |handle, boot| handle.request(boot, Method::CommandInvoke, params),
                    );
                }
                self.dismiss_menu(window, cx);
                if let Action::Workspace(id) = action {
                    self.navigate(NavigationTarget::Workspace(&id), cx);
                }
            }
            Err(error) => {
                if let Some(palette) = &mut self.menu.palette {
                    palette.error = Some(error.to_string());
                }
                cx.notify();
            }
        }
    }

    pub(super) fn palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = &mut self.menu.palette else {
            return;
        };
        if palette.search.read(cx).is_composing() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                window.prevent_default();
                self.dismiss_menu(window, cx);
            }
            "up" | "down" if !palette.filtered.is_empty() => {
                cx.stop_propagation();
                window.prevent_default();
                let count = palette.filtered.len();
                palette.selected = (palette.selected
                    + if event.keystroke.key == "up" {
                        count - 1
                    } else {
                        1
                    })
                    % count;
                palette
                    .scroll
                    .scroll_to_item(palette.selected, ScrollStrategy::Center);
                cx.notify();
            }
            "enter" => {
                cx.stop_propagation();
                window.prevent_default();
                if let Some(index) = palette.filtered.get(palette.selected) {
                    let action = palette.entries[*index].action.clone();
                    self.activate_palette(action, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_palette(&self, cx: &mut Context<Self>) -> Div {
        let Some(palette) = &self.menu.palette else {
            return div();
        };
        let theme = &self.theme;
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .p(px(16.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(self.config.ui.size * 1.35))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if palette.workspaces_only {
                                        "Switch Workspace"
                                    } else {
                                        "Command Palette"
                                    }),
                            )
                            .child(
                                div()
                                    .id("palette-close")
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .rounded(px(4.))
                                    .hover(|s| s.bg(rgb(theme.active)))
                                    .child("Close")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_menu(window, cx)
                                    })),
                            ),
                    )
                    .child(div().pt(px(12.)).child(palette.search.clone()))
                    .child(div().pt(px(8.)).text_color(rgb(theme.muted)).child(format!(
                        "{} of {} results",
                        palette.filtered.len(),
                        palette.entries.len()
                    ))),
            )
            .when_some(palette.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .id("palette-error")
                        .max_h(px(90.))
                        .overflow_y_scroll()
                        .flex_none()
                        .p(px(12.))
                        .text_color(rgb(theme.foreground))
                        .bg(rgb(theme.active))
                        .child(error),
                )
            })
            .when(palette.filtered.is_empty(), |panel| {
                panel.child(
                    div()
                        .flex_1()
                        .p(px(16.))
                        .text_color(rgb(theme.muted))
                        .child("No matching results. Try a shorter search."),
                )
            })
            .when(!palette.filtered.is_empty(), |panel| {
                panel.child(
                    uniform_list(
                        "palette-results",
                        palette.filtered.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            let Some(palette) = &this.menu.palette else {
                                return Vec::new();
                            };
                            range
                                .map(|index| {
                                    let entry = palette.entries[palette.filtered[index]].clone();
                                    div()
                                        .id(index)
                                        .debug_selector(move || format!("palette-row-{index}"))
                                        // Selection and hover read as rows, not as
                                        // labels, so the fill spans the whole list.
                                        .w_full()
                                        .h(px(this.config.ui.line_height() * 2. + 20.))
                                        .px(px(16.))
                                        .flex()
                                        .items_center()
                                        .gap(px(12.))
                                        .cursor_pointer()
                                        .when(index == palette.selected, |row| {
                                            row.bg(rgb(this.theme.active))
                                        })
                                        .hover(|s| s.bg(rgb(this.theme.active)))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .child(div().truncate().child(entry.label))
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_color(rgb(this.theme.muted))
                                                        .child(entry.detail),
                                                ),
                                        )
                                        .when(!entry.badge.is_empty(), |row| {
                                            row.child(
                                                div()
                                                    .flex_none()
                                                    .text_color(rgb(this.theme.muted))
                                                    .child(entry.badge),
                                            )
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.activate_palette(entry.action.clone(), window, cx)
                                        }))
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(palette.scroll.clone())
                    .flex_1()
                    .min_h_0(),
                )
            })
            .child(
                div()
                    .debug_selector(|| "palette-status".into())
                    .flex_none()
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(rgb(theme.active))
                    .text_color(rgb(theme.muted))
                    .child("Up / Down to navigate. Enter or click to select. Esc to cancel."),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui::TestAppContext;
    use herdr_client::protocol::ClientShellCommand;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap()
    }

    #[gpui::test]
    fn notification_command_is_searchable_and_targetless_activation_is_inert(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_toast_preview(
                    herdr_client::protocol::SemanticNotificationKind::Custom,
                    cx,
                );
                let selected = view.selected_endpoint;
                view.open_palette(false, window, cx);
                let palette = view.menu.palette.as_mut().unwrap();
                palette.filter("Open Notification Target");
                assert_eq!(palette.filtered.len(), 1);
                let action = palette.entries[palette.filtered[0]].action.clone();
                assert!(matches!(
                    action,
                    Action::Native(Command::OpenNotificationTarget)
                ));
                view.activate_palette(action, window, cx);
                assert!(view.menu.page.is_none());
                assert!(view.pending_navigation.is_none());
                assert_eq!(view.selected_endpoint, selected);
                assert_eq!(view.endpoints[selected].toasts.entries.len(), 1);
            })
        });
        cx.update(|window, cx| {
            crate::bind_keys(cx);
            window.focus(&view.read(cx).focus);
            window.draw(cx).clear();
            window.dispatch_keystroke(Keystroke::parse("cmd-alt-n").unwrap(), cx);
            assert!(view.read(cx).pending_navigation.is_none());
            assert_eq!(view.read(cx).endpoints[0].toasts.entries.len(), 1);
        });
    }

    #[gpui::test]
    fn only_mixed_palettes_carry_badges(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.commands = vec![ClientShellCommand {
                    command_id: "build".into(),
                    action: ClientShellCommandAction::Shell,
                    description: None,
                    binding_label: String::new(),
                    binding_labels: Vec::new(),
                }];
            })
        });
        for workspaces_only in [true, false] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.open_palette(workspaces_only, window, cx)
                })
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                let entries = &view.menu.palette.as_ref().unwrap().entries;
                assert!(!entries.is_empty());
                // The workspace switcher lists nothing else, so it needs no badges;
                // the command palette still separates daemon commands from native ones.
                assert_eq!(
                    entries.iter().all(|entry| entry.badge.is_empty()),
                    workspaces_only
                );
            });
            cx.update(|window, cx| view.update(cx, |view, cx| view.dismiss_menu(window, cx)));
        }
    }

    #[test]
    fn filtering_matches_all_unicode_tokens_in_any_order() {
        assert!(matches_query("CAF\u{c9} branch 42", " 42\tCAF\u{e9} "));
        assert!(matches_query("\u{391}\u{392} workspace", "\u{3b1}\u{3b2}"));
        assert!(matches_query("anything", " \n "));
        assert!(!matches_query("CAF\u{c9} branch 42", "caf\u{e9} missing"));
    }

    #[test]
    fn invocation_uses_captured_ids_not_current_focus() {
        let mut snapshot = snapshot();
        snapshot.commands = vec![ClientShellCommand {
            command_id: "build".into(),
            action: ClientShellCommandAction::Shell,
            description: None,
            binding_label: String::new(),
            binding_labels: Vec::new(),
        }];
        let target = Target::capture(&snapshot);
        snapshot.focused_workspace_id = None;
        snapshot.focused_tab_id = None;
        snapshot.focused_pane_id = None;
        assert_eq!(
            target
                .invocation(&snapshot, "build", ClientShellCommandAction::Shell)
                .unwrap(),
            json!({
                "command_id": "build", "workspace_id": target.workspace,
                "tab_id": target.tab, "pane_id": target.pane,
            })
        );
        let empty_target = Target::capture(&snapshot);
        assert_eq!(
            empty_target
                .invocation(&snapshot, "build", ClientShellCommandAction::Shell)
                .unwrap(),
            json!({"command_id": "build"})
        );
    }

    #[test]
    fn invocation_rejects_stale_boot_command_action_and_membership() {
        let mut original = snapshot();
        original.commands = vec![ClientShellCommand {
            command_id: "build".into(),
            action: ClientShellCommandAction::Shell,
            description: None,
            binding_label: String::new(),
            binding_labels: Vec::new(),
        }];
        let target = Target::capture(&original);
        for change in 0..7 {
            let mut snapshot = original.clone();
            match change {
                0 => snapshot.boot_id.push_str("-new"),
                1 => snapshot.commands.clear(),
                2 => snapshot.commands[0].action = ClientShellCommandAction::Pane,
                3 => snapshot.workspaces.clear(),
                4 => snapshot.tabs.clear(),
                5 => snapshot.panes.clear(),
                _ => {
                    for pane in &mut snapshot.panes {
                        pane.tab_id = "foreign".into();
                    }
                }
            }
            assert!(
                target
                    .invocation(&snapshot, "build", ClientShellCommandAction::Shell)
                    .is_err()
            );
        }
        original.commands[0].action = ClientShellCommandAction::Unknown;
        assert!(
            target
                .invocation(&original, "build", ClientShellCommandAction::Unknown)
                .is_err()
        );
    }

    #[test]
    fn workspace_selection_rejects_removed_id_and_restarted_daemon() {
        let mut snapshot = snapshot();
        let target = Target::capture(&snapshot);
        let id = snapshot.workspaces[0].workspace_id.clone();
        assert!(target.workspace_exists(&snapshot, &id).is_ok());
        assert!(target.workspace_exists(&snapshot, "missing").is_err());
        snapshot.boot_id.push_str("-new");
        assert!(target.workspace_exists(&snapshot, &id).is_err());
    }
}
