//! GPUI actions the app registers and the keystrokes bound to them. Every
//! shortcut comes from the single `controls::COMMANDS` table, so the palette,
//! the menu bar, and the keymap cannot drift apart.

use crate::controls::{self, Command};
use gpui::{Action, App, KeyBinding, actions};

actions!(
    herdr,
    [
        Quit,
        PlaySound,
        ShowHerdrNotDetected,
        ShowLogs,
        CheckForUpdates,
        ShowUpdatePreview,
        ShowUpdateDownloadPreview,
        ShowUpdateHomebrewPreview
    ]
);

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct RunCommand {
    pub(crate) command: Command,
}

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct ShowToastPreview {
    pub(crate) kind: herdr_client::protocol::SemanticNotificationKind,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
pub(crate) struct SetBadgePreview {
    pub(crate) enabled: bool,
}

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    // Reaching for `+` is the more natural way to ask for larger text, and the
    // platforms report it as the shifted character with the shift dropped
    // rather than as shift-`=`, so `cmd-+` is its own binding. The catalog
    // carries one shortcut per command, so the alias is bound by hand here.
    cx.bind_keys([KeyBinding::new(
        "cmd-+",
        RunCommand {
            command: Command::IncreaseFontSize,
        },
        None,
    )]);
    cx.bind_keys(
        controls::COMMANDS
            .iter()
            .filter(|info| !info.shortcut.is_empty() && info.command != Command::Quit)
            .map(|info| {
                KeyBinding::new(
                    info.shortcut,
                    RunCommand {
                        command: info.command,
                    },
                    None,
                )
            }),
    );
}
