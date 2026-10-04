//! The checkpoints dialog of a workspace row: the checkout's snapshots, newest
//! first, each with what its turn changed, and a restore behind a
//! confirmation that says what restoring replaces.

use super::Page;
use crate::{
    HerdrWindow,
    checkpoint::{Checkout, Listing, age, host_for, summary},
};
use gpui::{prelude::*, *};
use herdr_client::protocol::AgentStatus;

impl HerdrWindow {
    /// The checkout the menu's workspace shows, when checkpoints can reach it.
    pub(super) fn checkpoint_checkout(&self) -> Option<Checkout> {
        let target = self.menu.target.as_ref()?;
        let endpoint = self.endpoints.get(self.selected_endpoint)?;
        let host = host_for(&endpoint.connection.target, &self.live)?;
        Checkout::new(
            host,
            &target.worktree.as_ref()?.key,
            target.branch.as_deref(),
        )
    }

    pub(super) fn open_checkpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(checkout), Some(target)) = (self.checkpoint_checkout(), &self.menu.target) else {
            return;
        };
        let workspace = target.id.clone();
        self.checkpoints
            .open(checkout, target.label.clone(), workspace);
        self.menu.page = Some(Page::Checkpoints);
        self.menu.error = None;
        window.focus(&self.menu.focus, cx);
        cx.notify();
    }

    /// Closing the dialog forgets its list; a restore it started still runs.
    pub(crate) fn close_stale_checkpoints(&mut self) {
        if self.menu.page != Some(Page::Checkpoints) && self.checkpoints.view.is_some() {
            self.checkpoints.close();
        }
    }

    /// Whether an agent in the dialog's workspace is mid-turn, and so may
    /// keep changing the files a restore puts back.
    fn checkpoint_agent_working(&self) -> bool {
        let Some(view) = &self.checkpoints.view else {
            return false;
        };
        self.live.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.agents.iter().any(|agent| {
                agent.workspace_id == view.workspace_id
                    && matches!(
                        agent.agent_status,
                        AgentStatus::Working | AgentStatus::Blocked
                    )
            })
        })
    }

    pub(crate) fn checkpoints_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        let Some(view) = &mut self.checkpoints.view else {
            self.dismiss_menu(window, cx);
            return;
        };
        let count = match &view.listing {
            Listing::Ready(list) => list.len(),
            _ => 0,
        };
        match event.keystroke.key.as_str() {
            "escape" if view.confirming.is_some() && !view.restoring => view.confirming = None,
            "escape" => self.dismiss_menu(window, cx),
            "up" | "down" if view.confirming.is_none() && count > 0 => {
                let up = event.keystroke.key == "up";
                view.selected = Some(match view.selected {
                    None if up => count - 1,
                    None => 0,
                    Some(index) if up => (index + count - 1) % count,
                    Some(index) => (index + 1) % count,
                });
            }
            "enter" if view.confirming.is_some() => self.checkpoints.restore(),
            "enter" => {
                if let Some(checkpoint) = view
                    .selected_checkpoint()
                    .filter(|checkpoint| view.restorable(checkpoint))
                {
                    view.confirming = Some(checkpoint.id.clone());
                }
            }
            _ => return,
        }
        cx.notify();
    }

    pub(crate) fn render_checkpoints(&self, cx: &mut Context<Self>) -> Div {
        let Some(view) = &self.checkpoints.view else {
            return div();
        };
        let theme = &self.theme;
        let font = &self.config.ui;
        let muted = rgb(theme.muted);
        let danger = crate::menu::danger(theme);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
            });
        let line = |text: String| div().min_w_0().child(text);
        let mut body = div().flex().flex_col().gap(px(6.)).px(px(16.)).py(px(12.));
        let confirming = view.confirming_checkpoint();
        if let Some(checkpoint) = confirming {
            body = body
                .child(
                    line(format!(
                        "Restore \u{201c}{}\u{201d} from {}?",
                        checkpoint.label,
                        age(checkpoint.created, now)
                    ))
                    .font_weight(FontWeight::SEMIBOLD),
                )
                .child(line(format!(
                    "Files in this checkout go back to how they were then, and {} moves back to the commit it was on. Files created since are removed; ignored files are left alone.",
                    view.checkout.branch
                )))
                .child(
                    line("The current state is saved as a checkpoint first, so this can be undone.".into())
                        .text_color(muted),
                );
            if self.checkpoint_agent_working() {
                body = body.child(
                    line(
                        "An agent in this workspace is still working and may keep changing files."
                            .into(),
                    )
                    .debug_selector(|| "checkpoints-agent-working".into())
                    .text_color(danger),
                );
            }
        } else {
            match &view.listing {
                Listing::Loading => {
                    body = body.child(line("Loading checkpoints...".into()).text_color(muted));
                }
                Listing::Failed(error) => {
                    body = body.child(
                        line(error.clone())
                            .debug_selector(|| "checkpoints-error".into())
                            .text_color(danger),
                    );
                }
                Listing::Ready(list) if list.is_empty() => {
                    body = body.child(
                        line(if self.config.agent_checkpoints {
                            "No checkpoints yet. One is taken each time an agent here starts or finishes a turn.".into()
                        } else {
                            "No checkpoints. Checkpoint agent turns is off in Settings > General.".into()
                        })
                        .text_color(muted),
                    );
                }
                Listing::Ready(list) => {
                    for (index, checkpoint) in list.iter().enumerate() {
                        let selected = view.selected == Some(index);
                        let restorable = view.restorable(checkpoint);
                        let id = checkpoint.id.clone();
                        body = body.child(
                            div()
                                .id(("checkpoint", index))
                                .debug_selector(move || format!("checkpoint-{index}"))
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(crate::config::corners::CONTROL))
                                .when(selected, |row| row.bg(rgb(theme.active)))
                                .hover(|row| row.bg(rgb(theme.active)))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .truncate()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(crate::sidebar::label_text(
                                                    &checkpoint.label,
                                                )),
                                        )
                                        .child(div().truncate().text_color(muted).child(format!(
                                            "{} \u{b7} {}",
                                            age(checkpoint.created, now),
                                            summary(checkpoint.diff)
                                        ))),
                                )
                                .when(restorable, |row| {
                                    row.child(
                                        div()
                                            .id(("checkpoint-restore", index))
                                            .debug_selector(move || {
                                                format!("checkpoint-restore-{index}")
                                            })
                                            .flex_none()
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(crate::config::corners::CONTROL))
                                            .border_1()
                                            .border_color(rgb(theme.active))
                                            .hover(|button| button.bg(rgb(theme.background)))
                                            .cursor_pointer()
                                            .child("Restore")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                if let Some(view) = &mut this.checkpoints.view {
                                                    view.selected = Some(index);
                                                    view.confirming = Some(id.clone());
                                                }
                                                cx.notify();
                                            })),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(view) = &mut this.checkpoints.view {
                                        view.selected = Some(index);
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                }
            }
        }
        if let Some(error) = &view.error {
            body = body.child(
                line(error.clone())
                    .debug_selector(|| "checkpoints-restore-error".into())
                    .text_color(danger),
            );
        }
        let button = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(12.))
                .py(px(6.))
                .rounded(px(crate::config::corners::CONTROL))
                .border_1()
                .cursor_pointer()
        };
        let mut footer = div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(rgb(theme.active))
            .child(
                button("checkpoints-cancel")
                    .border_color(rgb(theme.active))
                    .hover(|button| button.bg(rgb(theme.active)))
                    .child(if confirming.is_some() {
                        "Cancel"
                    } else {
                        "Close"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        match &mut this.checkpoints.view {
                            Some(view) if view.confirming.is_some() && !view.restoring => {
                                view.confirming = None;
                                cx.notify();
                            }
                            _ => this.dismiss_menu(window, cx),
                        }
                    })),
            );
        if confirming.is_some() {
            let armed = !view.restoring;
            footer = footer.child(
                button("checkpoints-restore")
                    .border_color(danger)
                    .text_color(danger)
                    .child(if armed { "Restore" } else { "Restoring..." })
                    .when(!armed, |button| {
                        button.opacity(0.4).cursor(CursorStyle::OperationNotAllowed)
                    })
                    .when(armed, |button| {
                        button
                            .hover(|button| button.bg(rgb(theme.active)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.checkpoints.restore();
                                cx.notify();
                            }))
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        svg()
                            .path("icons/refresh.svg")
                            .size(px(16.))
                            .flex_none()
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(font.size * 1.35))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Checkpoints"),
                            )
                            .child(
                                div().truncate().text_color(muted).child(format!(
                                    "{} \u{b7} {}",
                                    view.title, view.checkout.branch
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .id("checkpoints-body")
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(footer)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::HerdrWindow;

    /// The fixture's w4 (a linked worktree on `worktree/sidebar-child`) on
    /// this machine's own daemon.
    pub(super) fn local(view: &mut HerdrWindow) {
        view.live.status = crate::state::ConnectionStatus::Connected;
        view.live.local_daemon_peer = true;
        view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
        let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
        snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
        for workspace in &mut snapshot.workspaces {
            if let Some(tree) = &mut workspace.worktree {
                tree.key = "/repo/.git".into();
            }
        }
    }

    fn offered(view: &HerdrWindow) -> bool {
        view.workspace_items()
            .iter()
            .any(|(action, _)| *action == super::super::WorkspaceMenuAction::Checkpoints)
    }

    #[gpui::test]
    fn checkpoints_are_offered_only_where_git_may_run(cx: &mut gpui::TestAppContext) {
        let scriptable = cfg!(any(target_os = "linux", target_os = "macos"));
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                local(view);
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert_eq!(offered(view), scriptable);
                view.dismiss_menu(window, cx);
                // A workspace with no checkout has nothing to snapshot.
                view.open_workspace_menu("w0", Default::default(), window, cx);
                assert!(!offered(view));
                view.dismiss_menu(window, cx);
                // Another user's daemon may name any path; Git never runs there.
                view.live.local_daemon_peer = false;
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert!(!offered(view));
                view.dismiss_menu(window, cx);
            })
        });
    }

    // Host scripts need a POSIX client, so other clients offer no dialog.
    // Fixture windows never poll, so no job queued here reaches a worker.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod dialog {
        use super::local;
        use crate::{
            checkpoint::{Checkpoint, Diff, Listing},
            menu::Page,
        };
        use herdr_client::protocol::AgentStatus;

        fn checkpoint(id: &str, branch: &str) -> Checkpoint {
            Checkpoint {
                id: id.into(),
                created: 0,
                label: format!("turn {id}"),
                branch: Some(branch.into()),
                diff: Diff {
                    files: 2,
                    additions: 7,
                    deletions: 1,
                },
            }
        }

        #[gpui::test]
        fn a_restore_is_confirmed_and_warns_of_working_agents(cx: &mut gpui::TestAppContext) {
            let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    local(view);
                    let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                    snapshot.agents[0].workspace_id = "w4".into();
                    snapshot.agents[0].agent_status = AgentStatus::Working;
                    view.open_workspace_menu("w4", Default::default(), window, cx);
                    view.activate_workspace_menu(
                        crate::menu::page::WorkspaceMenuAction::Checkpoints,
                        window,
                        cx,
                    );
                    assert_eq!(view.menu.page, Some(Page::Checkpoints));
                    let checkpoints = view.checkpoints.view.as_mut().unwrap();
                    assert_eq!(checkpoints.checkout.branch, "worktree/sidebar-child");
                    checkpoints.listing = Listing::Ready(vec![
                        checkpoint("2", "worktree/sidebar-child"),
                        checkpoint("1", "worktree/sidebar-child"),
                    ]);
                })
            });
            cx.run_until_parked();
            assert!(cx.debug_bounds("checkpoint-1").is_some());
            assert!(cx.debug_bounds("checkpoints-restore").is_none());
            cx.simulate_keystrokes("down down enter");
            cx.run_until_parked();
            cx.update(|_, cx| {
                let view = view.read(cx).checkpoints.view.as_ref().unwrap();
                assert_eq!(view.confirming.as_deref(), Some("1"));
                assert!(!view.restoring);
            });
            assert!(cx.debug_bounds("checkpoints-agent-working").is_some());
            let panel = cx.debug_bounds("menu-panel").unwrap();
            let restore = cx.debug_bounds("checkpoints-restore").unwrap();
            assert!(panel.contains(&restore.origin));
            // Escape backs out of the confirmation, not the dialog.
            cx.simulate_keystrokes("escape");
            cx.update(|_, cx| {
                let view = view.read(cx);
                assert_eq!(view.menu.page, Some(Page::Checkpoints));
                assert!(view.checkpoints.view.as_ref().unwrap().confirming.is_none());
            });
            cx.simulate_keystrokes("enter enter");
            cx.update(|_, cx| {
                assert!(view.read(cx).checkpoints.view.as_ref().unwrap().restoring);
            });
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.dismiss_menu(window, cx);
                    view.close_stale_checkpoints();
                    assert!(view.checkpoints.view.is_none());
                })
            });
        }
    }
}
