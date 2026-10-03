//! The endpoint API methods this client invokes.
//!
//! The daemon's advertised method list stays an open `Vec<String>` on the wire,
//! because a peer may offer methods this client knows nothing about. What is
//! closed is the set this client can *send*, so that set is an enum: a method
//! name reaches the wire through `as_str` in exactly one place, and a typo is a
//! compile error instead of an `UnsupportedMethod` rejection at runtime.

/// An endpoint API method. `as_str` is the wire spelling, and the only place a
/// method name is written out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    ClientShellSurfaceSet,
    CommandInvoke,
    IntegrationList,
    IntegrationInstall,
    LayoutSetSplitRatio,
    PaneClear,
    PaneClose,
    PaneCopyMotion,
    PaneCopySearch,
    PaneEditScrollback,
    PaneFocus,
    PaneFocusDirection,
    PaneInputSet,
    PaneRename,
    PaneScroll,
    PaneSelectionRead,
    PaneSplit,
    PaneZoom,
    ServerReloadConfig,
    TabClose,
    TabCreate,
    TabFocus,
    TabMove,
    TabRename,
    WorkspaceClose,
    WorkspaceCreate,
    WorkspaceFocus,
    WorkspaceGet,
    WorkspaceMoveBlock,
    WorkspaceRename,
    WorktreeCreate,
    WorktreeList,
    WorktreeOpen,
    WorktreeRemove,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientShellSurfaceSet => "client_shell.surface.set",
            Self::CommandInvoke => "command.invoke",
            Self::IntegrationList => "integration.list",
            Self::IntegrationInstall => "integration.install",
            Self::LayoutSetSplitRatio => "layout.set_split_ratio",
            Self::PaneClear => "pane.clear",
            Self::PaneClose => "pane.close",
            Self::PaneCopyMotion => "pane.copy_motion",
            Self::PaneCopySearch => "pane.copy_search",
            Self::PaneEditScrollback => "pane.edit_scrollback",
            Self::PaneFocus => "pane.focus",
            Self::PaneFocusDirection => "pane.focus_direction",
            Self::PaneInputSet => "pane.input.set",
            Self::PaneRename => "pane.rename",
            Self::PaneScroll => "pane.scroll",
            Self::PaneSelectionRead => "pane.selection.read",
            Self::PaneSplit => "pane.split",
            Self::PaneZoom => "pane.zoom",
            Self::ServerReloadConfig => "server.reload_config",
            Self::TabClose => "tab.close",
            Self::TabCreate => "tab.create",
            Self::TabFocus => "tab.focus",
            Self::TabMove => "tab.move",
            Self::TabRename => "tab.rename",
            Self::WorkspaceClose => "workspace.close",
            Self::WorkspaceCreate => "workspace.create",
            Self::WorkspaceFocus => "workspace.focus",
            Self::WorkspaceGet => "workspace.get",
            Self::WorkspaceMoveBlock => "workspace.move_block",
            Self::WorkspaceRename => "workspace.rename",
            Self::WorktreeCreate => "worktree.create",
            Self::WorktreeList => "worktree.list",
            Self::WorktreeOpen => "worktree.open",
            Self::WorktreeRemove => "worktree.remove",
        }
    }

    /// Whether a peer's advertised method list contains this method. The list
    /// is remote data, so it is compared as text rather than parsed.
    pub fn advertised_in(self, methods: &[String]) -> bool {
        methods.iter().any(|method| method == self.as_str())
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_methods_require_exact_advertisement() {
        for (method, name) in [
            (Method::IntegrationList, "integration.list"),
            (Method::IntegrationInstall, "integration.install"),
        ] {
            assert_eq!(method.as_str(), name);
            assert_eq!(method.to_string(), name);
            assert!(!method.advertised_in(&[]));
            assert!(!method.advertised_in(&[format!("{name}.extra")]));
            assert!(method.advertised_in(&["unknown.future".into(), name.into()]));
        }
        assert!(!Method::IntegrationInstall.advertised_in(&["integration.list".into()]));
    }

    #[test]
    fn pane_rename_wire_name_and_advertisement() {
        assert_eq!(Method::PaneRename.as_str(), "pane.rename");
        assert_eq!(Method::PaneRename.to_string(), "pane.rename");
        assert!(Method::PaneRename.advertised_in(&["pane.rename".into()]));
        assert!(!Method::PaneRename.advertised_in(&["tab.rename".into()]));
    }

    #[test]
    fn pane_clear_is_only_advertised_by_newer_daemons() {
        assert_eq!(Method::PaneClear.as_str(), "pane.clear");
        assert!(Method::PaneClear.advertised_in(&["pane.close".into(), "pane.clear".into()]));
        // Advertisement is an exact match: a method sharing the prefix is not clearing.
        assert!(!Method::PaneClear.advertised_in(&["pane.clear_agent_authority".into()]));
    }

    #[test]
    fn pane_input_set_wire_name() {
        assert_eq!(Method::PaneInputSet.as_str(), "pane.input.set");
        assert!(Method::PaneInputSet.advertised_in(&["pane.input.set".into()]));
        assert!(!Method::PaneInputSet.advertised_in(&["pane.input".into()]));
    }

    #[test]
    fn copy_search_wire_name_and_advertisement() {
        for (method, name) in [
            (Method::PaneCopyMotion, "pane.copy_motion"),
            (Method::PaneEditScrollback, "pane.edit_scrollback"),
            (Method::PaneSelectionRead, "pane.selection.read"),
        ] {
            assert_eq!(method.as_str(), name);
            assert!(method.advertised_in(&[name.into()]));
        }
        assert_eq!(Method::PaneCopySearch.as_str(), "pane.copy_search");
        assert!(Method::PaneCopySearch.advertised_in(&["pane.copy_search".into()]));
        assert!(!Method::PaneCopySearch.advertised_in(&["pane.copy_motion".into()]));
    }

    #[test]
    fn split_ratio_wire_name() {
        assert_eq!(
            Method::LayoutSetSplitRatio.as_str(),
            "layout.set_split_ratio"
        );
    }
}
