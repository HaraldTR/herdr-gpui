//! Opening Teleport from a workspace row: what moves, and the repositories
//! every other connected host has open, both read from the GUI snapshots.

use super::Page;
use crate::{
    HerdrWindow,
    teleport::{HostRepositories, Place, Repository, Source, Teleport, host_for},
};
use gpui::{Context, Window};
use herdr_client::protocol::ClientShellSnapshot;
use std::collections::HashMap;

/// The repositories open in `snapshot`, one per Git common directory, each
/// reached through its main checkout's workspace when that is open.
fn repositories(snapshot: &ClientShellSnapshot) -> Vec<Repository> {
    let mut found: Vec<Repository> = Vec::new();
    for workspace in &snapshot.workspaces {
        let Some(tree) = &workspace.worktree else {
            continue;
        };
        match found.iter_mut().find(|repo| repo.key == tree.key) {
            Some(repo) if !tree.is_linked_worktree => {
                repo.workspace_id.clone_from(&workspace.workspace_id);
            }
            Some(_) => {}
            None => found.push(Repository {
                key: tree.key.clone(),
                label: tree.label.clone(),
                workspace_id: workspace.workspace_id.clone(),
            }),
        }
    }
    found
}

impl HerdrWindow {
    /// Whether the menu's workspace can be teleported: a linked worktree on
    /// a host Teleport can script. It is offered even with no other host
    /// connected, so the dialog can say why there is nowhere to go.
    pub(super) fn can_teleport(&self) -> bool {
        let Some(target) = &self.menu.target else {
            return false;
        };
        // Host scripts need a POSIX client; see `herdr_client::run_script`.
        cfg!(any(target_os = "linux", target_os = "macos"))
            && target.can_delete()
            && self
                .teleport
                .as_ref()
                .is_none_or(|teleport| !teleport.moving())
            && host_for(&self.endpoints[self.selected_endpoint].connection.target).is_ok()
    }

    pub(super) fn open_teleport(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = &self.menu.target else {
            return;
        };
        let Some(snapshot) = self.live.snapshot.clone() else {
            return;
        };
        let Some(worktree) = target.worktree.clone() else {
            return;
        };
        let selected = &self.endpoints[self.selected_endpoint];
        let Ok(host) = host_for(&selected.connection.target) else {
            return;
        };
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == target.id);
        let tab_labels: HashMap<String, String> = snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == target.id && tab.custom_label)
            .map(|tab| (tab.tab_id.clone(), tab.label.clone()))
            .collect();
        let source = Source {
            place: Place {
                endpoint_id: selected.id.clone(),
                label: selected.label.clone(),
                host,
            },
            workspace_id: target.id.clone(),
            custom_label: workspace
                .filter(|workspace| workspace.custom_label)
                .map(|workspace| workspace.label.clone()),
            repo_key: worktree.key.clone(),
            repo_label: worktree.label.clone(),
            tab_labels,
        };
        let hosts: Vec<HostRepositories> = self
            .endpoints
            .iter()
            .enumerate()
            .filter(|(index, endpoint)| {
                *index != self.selected_endpoint
                    && endpoint.enabled
                    && endpoint.live.status.is_connected()
            })
            .filter_map(|(_, endpoint)| {
                let host = host_for(&endpoint.connection.target).ok()?;
                let snapshot = endpoint.live.snapshot.as_ref()?;
                Some(HostRepositories {
                    place: Place {
                        endpoint_id: endpoint.id.clone(),
                        label: endpoint.label.clone(),
                        host,
                    },
                    repositories: repositories(snapshot),
                })
            })
            .collect();
        let label = target.label.clone();
        self.menu.page = Some(Page::Teleport);
        self.menu.error = None;
        self.teleport = Some(Teleport::start(source, label, hosts));
        window.focus(&self.menu.focus, cx);
        cx.notify();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::sidebar::layout_tests::{REPO_KEY, snapshot};

    #[test]
    fn repositories_prefer_their_main_checkout_workspace() {
        // w3 is the main checkout; w4 and w5 are linked worktrees of it.
        let mut snapshot = snapshot(6);
        snapshot.workspaces.swap(3, 5);
        assert_eq!(
            repositories(&snapshot),
            [Repository {
                key: REPO_KEY.into(),
                label: "agent-launcher".into(),
                workspace_id: "w3".into(),
            }]
        );
        snapshot
            .workspaces
            .retain(|workspace| workspace.workspace_id != "w3");
        assert_eq!(repositories(&snapshot)[0].workspace_id, "w5");
    }

    fn teleport_items(view: &HerdrWindow) -> usize {
        view.workspace_items()
            .iter()
            .filter(|(action, _)| *action == super::super::WorkspaceMenuAction::Teleport)
            .count()
    }

    #[gpui::test]
    fn teleport_is_offered_for_linked_worktrees_on_scriptable_hosts(cx: &mut gpui::TestAppContext) {
        // Host scripts need a POSIX client, so Windows never offers Teleport.
        let offered = usize::from(cfg!(any(target_os = "linux", target_os = "macos")));
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = crate::state::ConnectionStatus::Connected;
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert_eq!(
                    teleport_items(view),
                    0,
                    "a custom socket cannot be scripted"
                );
                view.dismiss_menu(window, cx);
                view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert_eq!(
                    teleport_items(view),
                    offered,
                    "offered even with no other host"
                );
                view.dismiss_menu(window, cx);

                let mut remote = crate::endpoint::Endpoint::new(
                    "ssh:box".into(),
                    "Box".into(),
                    herdr_client::ConnectTarget::Ssh {
                        target: "me@box".into(),
                        session: "default".into(),
                    },
                    true,
                );
                remote.live = view.live.clone();
                view.endpoints.push(remote);
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert_eq!(teleport_items(view), offered);
                view.dismiss_menu(window, cx);
                // A main checkout is not a worktree that can move.
                view.open_workspace_menu("w3", Default::default(), window, cx);
                assert_eq!(teleport_items(view), 0);
                view.dismiss_menu(window, cx);
            })
        });
    }

    #[gpui::test]
    fn the_destination_picker_renders_and_enter_starts_the_review(cx: &mut gpui::TestAppContext) {
        use crate::teleport::{Candidate, MatchReason, host_for};
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let place = |id: &str, target: herdr_client::ConnectTarget| Place {
                    endpoint_id: id.into(),
                    label: format!("host {id}"),
                    host: host_for(&target).unwrap(),
                };
                let source = Source {
                    place: place("local", herdr_client::ConnectTarget::Local),
                    workspace_id: "w4".into(),
                    custom_label: None,
                    repo_key: REPO_KEY.into(),
                    repo_label: "agent-launcher".into(),
                    tab_labels: HashMap::new(),
                };
                let candidate = Candidate {
                    // An unroutable target: the review worker fails fast.
                    place: place(
                        "box",
                        herdr_client::ConnectTarget::Ssh {
                            target: "nobody@invalid.invalid".into(),
                            session: "default".into(),
                        },
                    ),
                    repository: Repository {
                        key: "/r/.git".into(),
                        label: "agent-launcher".into(),
                        workspace_id: "w9".into(),
                    },
                    reason: MatchReason::Name,
                };
                view.menu.page = Some(Page::Teleport);
                view.teleport = Some(Teleport::choosing(source, vec![candidate]));
                window.focus(&view.menu.focus, cx);
                cx.notify();
            })
        });
        cx.run_until_parked();
        let panel = cx.debug_bounds("menu-panel").unwrap();
        let row = cx.debug_bounds("teleport-candidate-0").unwrap();
        let submit = cx.debug_bounds("teleport-submit").unwrap();
        assert!(panel.contains(&row.origin) && panel.contains(&submit.origin));
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert!(view.read(cx).teleport.as_ref().unwrap().reviewing());
        });
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.poll_teleport(window, cx));
            assert!(view.read(cx).teleport.is_none(), "closing cancels a review");
            assert_eq!(view.read(cx).menu.page, None);
        });
    }
}
