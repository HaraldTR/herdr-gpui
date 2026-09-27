//! The app's browser tabs: which page each one shows and the workspace it
//! belongs to. Herdr has no browser panes, so these live only in this client;
//! every window shows the same tabs for a workspace, each with its own page.
use super::WebUrl;
use crate::state_file;
use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_TABS: usize = 256;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TITLE_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct TabId(u64);

#[cfg(all(test, any(target_os = "macos", windows)))]
impl TabId {
    pub(crate) fn test(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The daemon a workspace ID belongs to. IDs are only unique within one
/// daemon, so a local session and a saved host can both have a `w_1`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Scope(String);

impl Scope {
    /// A local daemon, named by the client socket the window connects to.
    pub(crate) fn local(client_socket: &std::path::Path) -> Self {
        Self(format!("local:{}", client_socket.display()))
    }

    /// A saved SSH host, named by its catalog ID.
    pub(crate) fn endpoint(id: &str) -> Self {
        Self(id.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tab {
    pub id: TabId,
    pub scope: Scope,
    pub workspace_id: String,
    /// `None` for a new tab still waiting for an address.
    pub url: Option<WebUrl>,
    /// The page's own title once it reports one; the host until then.
    pub title: String,
}

impl Tab {
    fn valid(&self) -> bool {
        !self.workspace_id.is_empty()
            && self.workspace_id.len() <= 256
            && self.title.chars().count() <= MAX_TITLE_CHARS
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    tabs: Vec<Tab>,
}

#[derive(Default)]
pub(crate) struct Store {
    tabs: Vec<Tab>,
    next: u64,
    writer: Option<state_file::Writer<Saved>>,
    quitting: bool,
}

impl Global for Store {}

/// Page titles are untrusted: one line, bounded, and free of control and
/// bidirectional formatting characters that could disguise a tab.
fn clean_title(title: &str) -> String {
    crate::notifications::safe_text(title, MAX_TITLE_CHARS * 4)
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn parse(bytes: &[u8]) -> crate::Result<Vec<Tab>> {
    let saved: Saved = serde_json::from_slice(bytes)?;
    if saved.tabs.len() > MAX_TABS || !saved.tabs.iter().all(Tab::valid) {
        return Err(crate::Error::InvalidBrowserTabs);
    }
    Ok(saved.tabs)
}

impl Store {
    fn path() -> Option<PathBuf> {
        crate::preferences::state_dir().map(|dir| dir.join("browser-tabs.json"))
    }

    /// Called before starting GPUI, like the window state. A missing or
    /// damaged file starts with no tabs.
    pub(crate) fn load() -> Self {
        let path = Self::path();
        let tabs = path
            .as_deref()
            .map(|path| {
                state_file::read(path, MAX_FILE_BYTES)
                    .and_then(|bytes| bytes.as_deref().map_or(Ok(Vec::new()), parse))
            })
            .transpose()
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "Cannot restore browser tabs");
                None
            })
            .unwrap_or_default();
        let writer = path.and_then(
            |path| match state_file::Writer::start("browser-tabs", path) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    tracing::warn!(%error, "Cannot start browser-tab worker");
                    None
                }
            },
        );
        Self::with_tabs(tabs, writer)
    }

    fn with_tabs(tabs: Vec<Tab>, writer: Option<state_file::Writer<Saved>>) -> Self {
        let next = tabs.iter().map(|tab| tab.id.0 + 1).max().unwrap_or(0);
        Self {
            tabs,
            next,
            writer,
            quitting: false,
        }
    }

    pub(crate) fn install(self, cx: &mut App) {
        cx.set_global(self);
        cx.on_app_quit(|cx| {
            let store = cx.global_mut::<Self>();
            store.quitting = true;
            let writer = store.writer.take();
            cx.background_executor().spawn(async move {
                if let Some(writer) = writer {
                    writer.finish();
                }
            })
        })
        .detach();
    }

    /// Runs `f` against the app's store, creating an unsaved one for
    /// fixtures that never installed it.
    pub(crate) fn update<R>(cx: &mut App, f: impl FnOnce(&mut Self) -> R) -> R {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::default());
        }
        f(cx.global_mut::<Self>())
    }

    fn save(&self) {
        if self.quitting {
            return;
        }
        if let Some(writer) = &self.writer {
            writer.save(Saved {
                tabs: self.tabs.clone(),
            });
        }
    }

    pub(crate) fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The tabs of one workspace, in the order they were opened.
    pub(crate) fn in_workspace<'a>(
        &'a self,
        scope: &'a Scope,
        workspace_id: &'a str,
    ) -> impl Iterator<Item = &'a Tab> + 'a {
        self.tabs
            .iter()
            .filter(move |tab| &tab.scope == scope && tab.workspace_id == workspace_id)
    }

    /// Opens a tab, or returns `None` when the app already holds the most
    /// it keeps.
    pub(crate) fn open(
        &mut self,
        scope: Scope,
        workspace_id: &str,
        url: Option<WebUrl>,
    ) -> Option<TabId> {
        if self.tabs.len() >= MAX_TABS {
            return None;
        }
        let id = TabId(self.next);
        self.next += 1;
        self.tabs.push(Tab {
            id,
            scope,
            workspace_id: workspace_id.to_owned(),
            title: url.as_ref().map_or("New Tab", WebUrl::host).to_owned(),
            url,
        });
        self.save();
        Some(id)
    }

    pub(crate) fn close(&mut self, id: TabId) -> Option<Tab> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let tab = self.tabs.remove(index);
        self.save();
        Some(tab)
    }

    /// Records where a page went and what it calls itself. Returns whether
    /// anything changed.
    pub(crate) fn visited(&mut self, id: TabId, url: Option<WebUrl>, title: Option<&str>) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return false;
        };
        let mut changed = false;
        if let Some(url) = url
            && tab.url.as_ref() != Some(&url)
        {
            tab.url = Some(url);
            changed = true;
        }
        if let Some(title) = title.map(clean_title).filter(|title| !title.is_empty())
            && tab.title != title
        {
            tab.title = title;
            changed = true;
        }
        if changed {
            self.save();
        }
        changed
    }

    /// Drops the tabs of workspaces the daemon closed.
    pub(crate) fn forget_workspaces(&mut self, scope: &Scope, closed: &[String]) -> bool {
        let before = self.tabs.len();
        self.tabs
            .retain(|tab| &tab.scope != scope || !closed.contains(&tab.workspace_id));
        let changed = self.tabs.len() != before;
        if changed {
            self.save();
        }
        changed
    }

    /// Whether any tab belongs to one of `closed`, without claiming the
    /// store for writing, which would redraw every window.
    pub(crate) fn has_workspaces(&self, scope: &Scope, closed: &[String]) -> bool {
        self.tabs
            .iter()
            .any(|tab| &tab.scope == scope && closed.contains(&tab.workspace_id))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn url(value: &str) -> Option<WebUrl> {
        Some(WebUrl::try_from(value).unwrap())
    }

    #[test]
    fn tabs_belong_to_one_workspace_of_one_daemon() {
        let mut store = Store::default();
        let local = Scope::local("/tmp/herdr-client.sock".as_ref());
        let remote = Scope::endpoint("ssh:box");
        let a = store
            .open(local.clone(), "w_1", url("http://localhost:3000"))
            .unwrap();
        let b = store
            .open(remote.clone(), "w_1", url("https://example.com/"))
            .unwrap();
        assert_ne!(a, b);
        let ids = |scope, workspace| {
            store
                .in_workspace(scope, workspace)
                .map(|tab| tab.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&local, "w_1"), [a]);
        assert_eq!(ids(&remote, "w_1"), [b]);
        assert!(ids(&local, "w_2").is_empty());
        assert_eq!(store.get(a).unwrap().title, "localhost");
        let blank = store.open(local, "w_1", None).unwrap();
        assert_eq!(store.get(blank).unwrap().title, "New Tab");
    }

    #[test]
    fn visits_update_address_and_a_clean_title() {
        let mut store = Store::default();
        let scope = Scope::endpoint("local");
        let id = store.open(scope, "w_1", url("https://a.test/")).unwrap();
        assert!(store.visited(id, url("https://b.test/x"), Some(" Docs\u{7}\n ")));
        let tab = store.get(id).unwrap();
        assert_eq!(tab.url, url("https://b.test/x"));
        assert_eq!(tab.title, "Docs");
        assert!(!store.visited(id, None, Some("")));
        assert!(!store.visited(id, None, Some("Docs")));
        assert!(store.visited(id, None, Some("\u{202e}txt.exe")));
        assert_eq!(store.get(id).unwrap().title, "txt.exe");
        let long = "x".repeat(MAX_TITLE_CHARS * 2);
        assert!(store.visited(id, None, Some(&long)));
        assert_eq!(store.get(id).unwrap().title.len(), MAX_TITLE_CHARS);
    }

    #[test]
    fn closing_a_workspace_only_touches_the_named_daemon() {
        let mut store = Store::default();
        let (one, two) = (Scope::endpoint("local"), Scope::endpoint("ssh:x"));
        let gone = store
            .open(one.clone(), "w_gone", url("https://a.test/"))
            .unwrap();
        let kept = store
            .open(one.clone(), "w_1", url("https://a.test/"))
            .unwrap();
        let other = store.open(two, "w_gone", url("https://a.test/")).unwrap();
        let closed = ["w_gone".to_owned()];
        assert!(store.has_workspaces(&one, &closed));
        assert!(store.forget_workspaces(&one, &closed));
        assert!(!store.has_workspaces(&one, &closed));
        assert!(!store.forget_workspaces(&one, &closed));
        assert!(store.get(gone).is_none());
        assert!(store.get(kept).is_some() && store.get(other).is_some());
        assert!(store.close(kept).is_some());
        assert!(store.close(kept).is_none());
    }

    #[test]
    fn saved_tabs_round_trip_and_invalid_files_are_rejected() {
        let mut store = Store::default();
        let id = store
            .open(Scope::endpoint("local"), "w_1", url("https://a.test/"))
            .unwrap();
        let bytes = serde_json::to_vec(&Saved {
            tabs: store.tabs.clone(),
        })
        .unwrap();
        let restored = Store::with_tabs(parse(&bytes).unwrap(), None);
        assert_eq!(restored.tabs, store.tabs);
        // New tabs never reuse a restored ID.
        assert!(restored.next > id.0);
        for invalid in [
            r#"{"tabs":[{"id":0,"scope":"local","workspace_id":"w","url":"file:///etc/passwd","title":""}]}"#,
            r#"{"tabs":[{"id":0,"scope":"local","workspace_id":"","url":"https://a.test/","title":""}]}"#,
            r#"{"tabs":"#,
        ] {
            assert!(parse(invalid.as_bytes()).is_err(), "{invalid}");
        }
    }

    #[test]
    fn the_store_is_bounded() {
        let mut store = Store::default();
        for _ in 0..MAX_TABS {
            assert!(
                store
                    .open(Scope::endpoint("local"), "w", url("https://a.test/"))
                    .is_some()
            );
        }
        assert!(
            store
                .open(Scope::endpoint("local"), "w", url("https://a.test/"))
                .is_none()
        );
    }
}
