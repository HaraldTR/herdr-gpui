//! Opening Teleport from a workspace row: what moves, and the repositories
//! every other connected host has open, both read from the GUI snapshots.

use super::Page;
use crate::{
    HerdrWindow,
    teleport::{
        Follow, HostRepositories, Mark, Place, Repository, Retired, Source, Teleport, host_for,
    },
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

    /// The teleported mark on the menu's workspace, if its work moved away.
    pub(super) fn teleport_mark(&self) -> Option<&Mark> {
        let target = self.menu.target.as_ref()?;
        self.teleport_marks.find(
            &self.endpoints[self.selected_endpoint].id,
            &target.worktree.as_ref()?.key,
            target.branch.as_deref()?,
        )
    }

    /// Where this copy's work was teleported from, when that host is still
    /// one this window can send it back to.
    pub(super) fn teleport_origin(&self) -> Option<&Mark> {
        let target = self.menu.target.as_ref()?;
        let mark = self.teleport_marks.arrived_at(
            &self.endpoints[self.selected_endpoint].id,
            &target.worktree.as_ref()?.key,
            target.branch.as_deref()?,
            &target.id,
        )?;
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.id == mark.endpoint && endpoint.enabled)
            .then_some(mark)
    }

    /// Teleport, already aimed at the host the work came from.
    pub(super) fn teleport_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.teleport_origin().map(|mark| mark.endpoint.clone()) else {
            return;
        };
        self.open_teleport(window, cx);
        if let Some(teleport) = &mut self.teleport {
            teleport.review_host(&origin);
        }
        cx.notify();
    }

    pub(super) fn go_to_teleported(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mark) = self.teleport_mark().cloned() else {
            return;
        };
        let destination = &mark.destination;
        // A restarted daemon renumbers workspaces; find it by repository and
        // branch when that host's snapshot is at hand.
        let workspace = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == destination.endpoint)
            .and_then(|endpoint| endpoint.live.snapshot.as_ref())
            .and_then(|snapshot| {
                snapshot.workspaces.iter().find(|w| {
                    w.branch.as_deref() == Some(mark.branch.as_str())
                        && w.worktree
                            .as_ref()
                            .is_some_and(|t| t.key == destination.repo_key)
                })
            })
            .map_or_else(
                || destination.workspace_id.clone(),
                |w| w.workspace_id.clone(),
            );
        self.dismiss_menu(window, cx);
        self.teleport_follow = Some(Follow::new(destination.endpoint.clone(), workspace));
        cx.notify();
    }

    pub(super) fn clear_teleport_mark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mark) = self.teleport_mark().cloned() {
            self.teleport_marks
                .remove(&mark.endpoint, &mark.repo_key, &mark.branch);
        }
        self.dismiss_menu(window, cx);
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
            branch: target.branch.clone(),
            tab_labels,
        };
        // Every enabled host, connected or not: Teleport reaches each over its
        // own SSH. A connected host's snapshot saves a round trip; the others
        // are read through their CLI. The same machine listed twice is one host.
        let mut hosts: Vec<HostRepositories> = Vec::new();
        for (index, endpoint) in self.endpoints.iter().enumerate() {
            if index == self.selected_endpoint || !endpoint.enabled {
                continue;
            }
            let Ok(host) = host_for(&endpoint.connection.target) else {
                continue;
            };
            if host == source.place.host || hosts.iter().any(|known| known.place.host == host) {
                continue;
            }
            let snapshot = endpoint
                .live
                .snapshot
                .as_ref()
                .filter(|_| endpoint.live.status.is_connected());
            let repositories = snapshot.map(|snapshot| repositories(snapshot));
            // Checkouts this client teleported away from, still open there.
            let retired = snapshot
                .map(|snapshot| {
                    self.teleport_marks
                        .on(&endpoint.id)
                        .filter_map(|mark| {
                            let workspace = snapshot.workspaces.iter().find(|w| {
                                w.branch.as_deref() == Some(mark.branch.as_str())
                                    && w.worktree.as_ref().is_some_and(|t| t.key == mark.repo_key)
                            })?;
                            Some(Retired {
                                repo_key: mark.repo_key.clone(),
                                branch: mark.branch.clone(),
                                workspace_id: workspace.workspace_id.clone(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            hosts.push(HostRepositories {
                place: Place {
                    endpoint_id: endpoint.id.clone(),
                    label: endpoint.label.clone(),
                    host,
                },
                repositories,
                retired,
            });
        }
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
    fn hosts_are_listed_at_once_with_none_chosen_for_the_user(cx: &mut gpui::TestAppContext) {
        use super::super::WorkspaceMenuAction;
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
                // Not connected, and unroutable: listing it must not wait on it.
                view.endpoints.push(crate::endpoint::Endpoint::new(
                    "ssh:box".into(),
                    "Box".into(),
                    herdr_client::ConnectTarget::Ssh {
                        target: "nobody@invalid.invalid".into(),
                        session: "default".into(),
                    },
                    true,
                ));
                view.open_workspace_menu("w4", Default::default(), window, cx);
                view.activate_workspace_menu(WorkspaceMenuAction::Teleport, window, cx);
                assert_eq!(view.menu.page, Some(Page::Teleport));
            })
        });
        // The first frame already lists the host.
        cx.run_until_parked();
        let panel = cx.debug_bounds("menu-panel").unwrap();
        let row = cx.debug_bounds("teleport-host-0").unwrap();
        let dot = cx.debug_bounds("teleport-host-dot-0").unwrap();
        assert!(
            row.contains(&dot.center()),
            "the online dot sits in its row"
        );
        let submit = cx.debug_bounds("teleport-submit").unwrap();
        assert!(panel.contains(&row.origin) && panel.contains(&submit.origin));
        // No host is chosen for the user: Enter does nothing until one is.
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert!(!view.read(cx).teleport.as_ref().unwrap().reviewing());
        });
        cx.simulate_keystrokes("down enter");
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

    fn actions(view: &HerdrWindow) -> Vec<super::super::WorkspaceMenuAction> {
        view.workspace_items()
            .into_iter()
            .map(|(action, _)| action)
            .collect()
    }

    #[gpui::test]
    fn a_teleported_worktree_offers_its_copy_and_can_be_cleared(cx: &mut gpui::TestAppContext) {
        use super::super::WorkspaceMenuAction;
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.live.supports_surface = true;
                view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
                view.endpoints[0].live = view.live.clone();
                view.endpoints.push(crate::endpoint::Endpoint::new(
                    "ssh:box".into(),
                    "Box".into(),
                    herdr_client::ConnectTarget::Ssh {
                        target: "me@box".into(),
                        session: "default".into(),
                    },
                    true,
                ));
                // w4 is the linked worktree on `worktree/sidebar-child`.
                view.teleport_marks.add(Mark {
                    endpoint: crate::endpoint::LOCAL.into(),
                    repo_key: REPO_KEY.into(),
                    branch: "worktree/sidebar-child".into(),
                    destination: crate::teleport::MarkDestination {
                        endpoint: "ssh:box".into(),
                        label: "Box".into(),
                        repo_key: "/home/me/agent-launcher/.git".into(),
                        workspace_id: "w9".into(),
                    },
                });

                view.open_workspace_menu("w4", Default::default(), window, cx);
                let offered = actions(view);
                assert!(offered.contains(&WorkspaceMenuAction::GoToTeleported));
                assert!(offered.contains(&WorkspaceMenuAction::ClearTeleported));
                assert!(!offered.contains(&WorkspaceMenuAction::Teleport));

                view.activate_workspace_menu(WorkspaceMenuAction::GoToTeleported, window, cx);
                assert_eq!(view.menu.page, None);
                assert!(view.teleport_follow.is_some());
                view.poll_teleport(window, cx);
                assert_eq!(view.endpoints[view.selected_endpoint].id, "ssh:box");

                // Back on the local host, clearing the mark offers Teleport again.
                view.teleport_follow = None;
                assert!(view.navigate_endpoint(
                    crate::endpoint::LOCAL,
                    crate::NavigationTarget::Workspace("w4"),
                    cx,
                ));
                view.live.status = crate::state::ConnectionStatus::Connected;
                let snapshot =
                    std::sync::Arc::make_mut(view.live.snapshot.get_or_insert_with(|| {
                        std::sync::Arc::new(crate::sidebar::layout_tests::snapshot(7))
                    }));
                snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
                view.open_workspace_menu("w4", Default::default(), window, cx);
                view.activate_workspace_menu(WorkspaceMenuAction::ClearTeleported, window, cx);
                assert!(
                    view.teleport_marks
                        .find(crate::endpoint::LOCAL, REPO_KEY, "worktree/sidebar-child")
                        .is_none()
                );
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert!(
                    actions(view).contains(&WorkspaceMenuAction::Teleport)
                        == cfg!(any(target_os = "linux", target_os = "macos"))
                );
            })
        });
    }

    #[gpui::test]
    fn a_teleported_copy_offers_to_go_back_where_it_came_from(cx: &mut gpui::TestAppContext) {
        use super::super::WorkspaceMenuAction;
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.endpoints[0].connection.target = herdr_client::ConnectTarget::Local;
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = crate::sidebar::layout_tests::snapshot(7).workspaces;
                // w4's work arrived here from the box.
                view.teleport_marks.add(Mark {
                    endpoint: "ssh:box".into(),
                    repo_key: "/home/me/agent-launcher/.git".into(),
                    branch: "worktree/sidebar-child".into(),
                    destination: crate::teleport::MarkDestination {
                        endpoint: crate::endpoint::LOCAL.into(),
                        label: "Local".into(),
                        repo_key: REPO_KEY.into(),
                        workspace_id: "w4".into(),
                    },
                });
                // Without the box among this window's hosts there is nowhere to go back to.
                view.open_workspace_menu("w4", Default::default(), window, cx);
                assert!(!actions(view).contains(&WorkspaceMenuAction::TeleportBack));
                view.dismiss_menu(window, cx);

                view.endpoints.push(crate::endpoint::Endpoint::new(
                    "ssh:box".into(),
                    "Box".into(),
                    herdr_client::ConnectTarget::Ssh {
                        target: "nobody@invalid.invalid".into(),
                        session: "default".into(),
                    },
                    true,
                ));
                view.open_workspace_menu("w4", Default::default(), window, cx);
                let offered = actions(view);
                if !cfg!(any(target_os = "linux", target_os = "macos")) {
                    assert!(!offered.contains(&WorkspaceMenuAction::TeleportBack));
                    return;
                }
                assert!(offered.contains(&WorkspaceMenuAction::TeleportBack));
                assert!(offered.contains(&WorkspaceMenuAction::Teleport));
                view.activate_workspace_menu(WorkspaceMenuAction::TeleportBack, window, cx);
                assert_eq!(view.menu.page, Some(Page::Teleport));
                assert!(
                    view.teleport.as_ref().unwrap().reviewing(),
                    "goes straight to the review"
                );
            })
        });
    }
}
