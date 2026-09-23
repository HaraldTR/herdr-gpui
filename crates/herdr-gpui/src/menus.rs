//! The native menu bar. Every item dispatches the same `Command` the palette
//! and the keymap use, so a command exists in one place only.

use crate::{CheckForUpdates, Quit, RunCommand, ShowLogs, controls::Command};
#[cfg(feature = "qa-menu")]
use crate::{
    PlaySound, ShowHerdrNotDetected, ShowUpdatePreview,
    actions::{ShowToastPreview, ShowUpdateDownloadPreview, ShowUpdateHomebrewPreview},
};
use gpui::{Menu, MenuItem};
#[cfg(feature = "qa-menu")]
use herdr_client::protocol::SemanticNotificationKind;

pub(crate) fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Herdr".into(),
            items: vec![
                MenuItem::action(
                    "About Herdr",
                    RunCommand {
                        command: Command::About,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Command Palette",
                    RunCommand {
                        command: Command::Palette,
                    },
                ),
                MenuItem::action(
                    "Settings",
                    RunCommand {
                        command: Command::Settings,
                    },
                ),
                MenuItem::action(
                    "Keybinds",
                    RunCommand {
                        command: Command::Keybinds,
                    },
                ),
                MenuItem::action("Check for Updates...", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Quit Herdr", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action(
                    "New Workspace",
                    RunCommand {
                        command: Command::Workspace,
                    },
                ),
                MenuItem::action(
                    "New Tab",
                    RunCommand {
                        command: Command::Tab,
                    },
                ),
                MenuItem::action(
                    "Switch Workspace",
                    RunCommand {
                        command: Command::WorkspacePicker,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Close Pane...",
                    RunCommand {
                        command: Command::ClosePane,
                    },
                ),
                MenuItem::action(
                    "Close Tab...",
                    RunCommand {
                        command: Command::CloseTab,
                    },
                ),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action(
                    "Increase Font Size",
                    RunCommand {
                        command: Command::IncreaseFontSize,
                    },
                ),
                MenuItem::action(
                    "Decrease Font Size",
                    RunCommand {
                        command: Command::DecreaseFontSize,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Reset Font Size",
                    RunCommand {
                        command: Command::ResetFontSize,
                    },
                ),
            ],
        },
        Menu {
            name: "Terminal".into(),
            items: vec![
                MenuItem::action(
                    "Split Vertically (Right)",
                    RunCommand {
                        command: Command::SplitRight,
                    },
                ),
                MenuItem::action(
                    "Split Horizontally (Down)",
                    RunCommand {
                        command: Command::SplitDown,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Tab",
                    RunCommand {
                        command: Command::NextTab,
                    },
                ),
                MenuItem::action(
                    "Previous Tab",
                    RunCommand {
                        command: Command::PreviousTab,
                    },
                ),
                MenuItem::action(
                    "Toggle Pane Zoom",
                    RunCommand {
                        command: Command::Zoom,
                    },
                ),
                MenuItem::action(
                    "Open Notification Target",
                    RunCommand {
                        command: Command::OpenNotificationTarget,
                    },
                ),
                MenuItem::action(
                    "Toggle Sidebar",
                    RunCommand {
                        command: Command::ToggleSidebar,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Reconnect",
                    RunCommand {
                        command: Command::Reconnect,
                    },
                ),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action(
                    "New Window",
                    RunCommand {
                        command: Command::NewWindow,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action("GPUI Logs", ShowLogs),
            ],
        },
        #[cfg(feature = "qa-menu")]
        Menu {
            name: "QA".into(),
            items: vec![
                MenuItem::action("Show herdr non-detected modal", ShowHerdrNotDetected),
                MenuItem::action("Show app update available", ShowUpdatePreview),
                MenuItem::action(
                    "Show update download progress (50%)",
                    ShowUpdateDownloadPreview,
                ),
                MenuItem::action("Show Homebrew update progress", ShowUpdateHomebrewPreview),
                MenuItem::action("Play Sound", PlaySound),
                #[cfg(target_os = "macos")]
                MenuItem::action(
                    "Enable badge",
                    crate::actions::SetBadgePreview { enabled: true },
                ),
                #[cfg(target_os = "macos")]
                MenuItem::action(
                    "Disable badge preview",
                    crate::actions::SetBadgePreview { enabled: false },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Show NeedsAttention toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::NeedsAttention,
                    },
                ),
                MenuItem::action(
                    "Show Finished toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::Finished,
                    },
                ),
                MenuItem::action(
                    "Show UpdateInstalled toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::UpdateInstalled,
                    },
                ),
                MenuItem::action(
                    "Show Custom toast",
                    ShowToastPreview {
                        kind: SemanticNotificationKind::Custom,
                    },
                ),
            ],
        },
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "qa-menu")]
    fn badge_preview_is_available_only_in_the_macos_qa_menu() {
        let menus = menus();
        let qa = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "QA")
            .unwrap();
        for (label, enabled) in [("Enable badge", true), ("Disable badge preview", false)] {
            let action = qa.items.iter().find_map(|item| match item {
                MenuItem::Action { name, action, .. } if name.as_ref() == label => Some(action),
                _ => None,
            });
            if cfg!(target_os = "macos") {
                assert!(
                    action
                        .unwrap()
                        .partial_eq(&crate::actions::SetBadgePreview { enabled })
                );
            } else {
                assert!(action.is_none());
            }
        }
    }

    #[test]
    #[cfg(feature = "qa-menu")]
    fn qa_menu_carries_update_progress_previews() {
        let menus = menus();
        let qa = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "QA")
            .unwrap();
        for (label, expected) in [
            (
                "Show update download progress (50%)",
                Box::new(ShowUpdateDownloadPreview) as Box<dyn gpui::Action>,
            ),
            (
                "Show Homebrew update progress",
                Box::new(ShowUpdateHomebrewPreview),
            ),
        ] {
            assert!(
                qa.items.iter().any(|item| matches!(item,
                    MenuItem::Action { name, action, .. }
                        if name.as_ref() == label && action.partial_eq(expected.as_ref())
                )),
                "{label}"
            );
        }
    }

    #[test]
    fn qa_menu_requires_explicit_feature() {
        let menus = menus();
        let names: Vec<_> = menus.iter().map(|menu| menu.name.as_ref()).collect();
        let mut expected = vec!["Herdr", "File", "View", "Terminal", "Window"];
        if cfg!(feature = "qa-menu") {
            expected.push("QA");
        }
        assert_eq!(names, expected);
    }

    /// The font size items are the only way to reach these commands from the
    /// macOS menu bar, and each must dispatch the catalog command rather than
    /// an action of its own.
    #[test]
    fn view_menu_carries_the_font_size_commands() {
        let menus = menus();
        let view = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "View")
            .unwrap();
        let actions: Vec<_> = view
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, action, .. } => Some((name.as_ref(), action)),
                _ => None,
            })
            .collect();
        let expected = [
            ("Increase Font Size", Command::IncreaseFontSize),
            ("Decrease Font Size", Command::DecreaseFontSize),
            ("Reset Font Size", Command::ResetFontSize),
        ];
        assert_eq!(actions.len(), expected.len());
        for ((name, action), (label, command)) in actions.iter().zip(expected) {
            assert_eq!(*name, label);
            assert!(action.partial_eq(&RunCommand { command }), "{label}");
        }
        // Reset is a different kind of act from stepping, so it sits apart.
        assert!(matches!(view.items[2], MenuItem::Separator));
    }
}
