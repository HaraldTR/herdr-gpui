//! How a trusted script reaches a visible pane.
//!
//! The endpoint API has no method that starts a command in a pane, so the
//! script runs in a new tab's shell: `tab.create` gives that shell the script
//! and its paths as environment variables, and one fixed line is typed to run
//! it. The typed line holds no repository content, so nothing from the file
//! is ever parsed by the user's interactive shell, whatever shell that is.

use super::ScriptKind;
use serde_json::{Value, json};

/// The variable the typed line runs. Internal plumbing, not a stable API.
pub(crate) const SCRIPT_ENV: &str = "HERDR_WORKTREE_SCRIPT";
/// The repository's main checkout, as Conductor's `CONDUCTOR_ROOT_PATH`.
pub(crate) const ROOT_ENV: &str = "HERDR_ROOT_PATH";
/// The checkout the script runs in, also its working directory.
pub(crate) const WORKTREE_ENV: &str = "HERDR_WORKTREE_PATH";

/// The line typed into the new tab's shell.
///
/// Single-quoted with no quote or backslash inside, so POSIX shells, fish,
/// and nushell all pass it to `sh` unchanged. `-e` stops at the first failing
/// command. An archive removes its checkout through the pane's own `herdr`
/// (Herdr sets `HERDR_BIN_PATH` and `HERDR_WORKSPACE_ID` in every pane) only
/// after the script succeeds; a failure leaves the tab open on its output and
/// the checkout in place.
pub(crate) fn command_line(kind: ScriptKind, force: bool) -> &'static str {
    match (kind, force) {
        (ScriptKind::Setup | ScriptKind::Run, _) => r#"sh -ec 'eval "$HERDR_WORKTREE_SCRIPT"'"#,
        (ScriptKind::Archive, false) => {
            r#"sh -ec 'eval "$HERDR_WORKTREE_SCRIPT"; exec "${HERDR_BIN_PATH:-herdr}" worktree remove --workspace "$HERDR_WORKSPACE_ID"'"#
        }
        (ScriptKind::Archive, true) => {
            r#"sh -ec 'eval "$HERDR_WORKTREE_SCRIPT"; exec "${HERDR_BIN_PATH:-herdr}" worktree remove --workspace "$HERDR_WORKSPACE_ID" --force'"#
        }
    }
}

/// Where a script runs: the checkout, and the repository's main checkout
/// when the daemon named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkout {
    pub(crate) path: String,
    pub(crate) root: Option<String>,
}

/// The `tab.create` a script runs in, labelled after its kind.
pub(crate) fn tab_params(
    workspace: &str,
    checkout: &Checkout,
    kind: ScriptKind,
    script: &str,
) -> Value {
    let mut env = serde_json::Map::new();
    env.insert(SCRIPT_ENV.into(), script.into());
    env.insert(WORKTREE_ENV.into(), checkout.path.as_str().into());
    if let Some(root) = &checkout.root {
        env.insert(ROOT_ENV.into(), root.as_str().into());
    }
    json!({
        "workspace_id": workspace,
        "cwd": checkout.path,
        "label": kind.name(),
        "focus": true,
        "env": env,
    })
}

/// The checkout of `workspace`, and its repository's main checkout, from a
/// `worktree.list` response.
pub(crate) fn locate(response: &Value, workspace: &str) -> crate::Result<Checkout> {
    let result = response_result(response)?;
    let entries = (result["type"] == "worktree_list")
        .then(|| result["worktrees"].as_array())
        .flatten()
        .ok_or(crate::Error::WorktreeScriptsCheckout)?;
    let mut own = entries
        .iter()
        .filter(|entry| entry["open_workspace_id"] == workspace);
    let path = match (own.next(), own.next()) {
        (Some(entry), None) => entry["path"].as_str().filter(|path| !path.is_empty()),
        _ => None,
    }
    .ok_or(crate::Error::WorktreeScriptsCheckout)?;
    Ok(Checkout {
        path: path.to_owned(),
        root: main_checkout(result),
    })
}

/// The repository's main checkout in a `worktree.list` result: its one
/// entry that is neither linked nor bare.
pub(crate) fn main_checkout(result: &Value) -> Option<String> {
    let mut main = result["worktrees"]
        .as_array()?
        .iter()
        .filter(|entry| entry["is_linked_worktree"] == false && entry["is_bare"] == false);
    match (main.next(), main.next()) {
        (Some(entry), None) => entry["path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

/// The tab and pane a `tab.create` response made.
pub(crate) fn created_tab(response: &Value) -> crate::Result<(String, String)> {
    let result = response_result(response)?;
    let id = |value: &Value| {
        value
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    };
    (result["type"] == "tab_created")
        .then(|| {
            Some((
                id(&result["tab"]["tab_id"])?,
                id(&result["root_pane"]["pane_id"])?,
            ))
        })
        .flatten()
        .ok_or(crate::Error::WorktreeScriptsResponse)
}

fn response_result(response: &Value) -> crate::Result<&Value> {
    match response.get("error").filter(|error| !error.is_null()) {
        Some(error) => Err(crate::Error::DaemonResponse(error.clone())),
        None => Ok(&response["result"]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn typed_lines_hold_no_repository_content_and_survive_any_shell() {
        for (kind, force) in [
            (ScriptKind::Setup, false),
            (ScriptKind::Run, false),
            (ScriptKind::Archive, false),
            (ScriptKind::Archive, true),
        ] {
            let line = command_line(kind, force);
            let quoted = line
                .strip_prefix("sh -ec '")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap();
            assert!(!quoted.contains(['\'', '\\', '\n']), "{line}");
            assert!(quoted.starts_with(r#"eval "$HERDR_WORKTREE_SCRIPT""#));
        }
        assert!(!command_line(ScriptKind::Run, true).contains("remove"));
        assert!(!command_line(ScriptKind::Archive, false).contains("--force"));
        assert!(command_line(ScriptKind::Archive, true).ends_with("--force'"));
    }

    #[test]
    #[cfg(unix)]
    fn archive_line_removes_only_after_the_script_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        // Stands in for the pane's herdr, recording what it was asked.
        let herdr = dir.path().join("herdr");
        std::fs::write(&herdr, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n").unwrap();
        std::fs::set_permissions(&herdr, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let run = |script: &str, force: bool| {
            std::process::Command::new("/bin/sh")
                .args(["-c", command_line(ScriptKind::Archive, force)])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LOG", &log)
                .env("HERDR_BIN_PATH", &herdr)
                .env("HERDR_WORKSPACE_ID", "w7")
                .env(SCRIPT_ENV, script)
                .status()
                .unwrap()
        };
        assert!(!run("echo one >> \"$LOG\"\nfalse\necho two >> \"$LOG\"", false).success());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\n");
        assert!(run("echo 'it''s fine' >> \"$LOG\"", true).success());
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "one\nits fine\nworktree remove --workspace w7 --force\n"
        );
    }

    #[test]
    fn tab_params_carry_the_script_and_paths_as_environment() {
        let checkout = Checkout {
            path: "/w/feat".into(),
            root: Some("/r".into()),
        };
        assert_eq!(
            tab_params("w2", &checkout, ScriptKind::Setup, "npm ci"),
            json!({"workspace_id":"w2","cwd":"/w/feat","label":"setup","focus":true,
                "env":{"HERDR_WORKTREE_SCRIPT":"npm ci","HERDR_WORKTREE_PATH":"/w/feat","HERDR_ROOT_PATH":"/r"}})
        );
        let params = tab_params(
            "w2",
            &Checkout {
                root: None,
                ..checkout
            },
            ScriptKind::Run,
            "make",
        );
        assert_eq!(params["label"], "run");
        assert!(params["env"].get(ROOT_ENV).is_none());
    }

    #[test]
    fn locates_a_workspace_checkout_and_its_main_checkout() {
        let response = json!({"result":{"type":"worktree_list","worktrees":[
            {"path":"/r","is_linked_worktree":false,"is_bare":false,"open_workspace_id":"w1"},
            {"path":"/w/feat","is_linked_worktree":true,"is_bare":false,"open_workspace_id":"w2"},
            {"path":"/w/other","is_linked_worktree":true,"is_bare":false}
        ]}});
        assert_eq!(
            locate(&response, "w2").unwrap(),
            Checkout {
                path: "/w/feat".into(),
                root: Some("/r".into())
            }
        );
        assert_eq!(locate(&response, "w1").unwrap().path, "/r");
        assert!(matches!(
            locate(&response, "w9"),
            Err(crate::Error::WorktreeScriptsCheckout)
        ));
        assert!(matches!(
            locate(&json!({"error":{"code":"x","message":"no"}}), "w2"),
            Err(crate::Error::DaemonResponse(_))
        ));
        // Two entries claiming the workspace identify nothing.
        let ambiguous = json!({"result":{"type":"worktree_list","worktrees":[
            {"path":"/a","is_linked_worktree":true,"is_bare":false,"open_workspace_id":"w2"},
            {"path":"/b","is_linked_worktree":true,"is_bare":false,"open_workspace_id":"w2"}
        ]}});
        assert!(locate(&ambiguous, "w2").is_err());
        // A bare repository has no main checkout to name.
        let bare = json!({"type":"worktree_list","worktrees":[
            {"path":"/r.git","is_linked_worktree":false,"is_bare":true}
        ]});
        assert_eq!(main_checkout(&bare), None);
    }

    #[test]
    fn created_tabs_name_their_pane() {
        let created = json!({"result":{"type":"tab_created","tab":{"tab_id":"w2:t3"},"root_pane":{"pane_id":"w2:p5"}}});
        assert_eq!(
            created_tab(&created).unwrap(),
            ("w2:t3".to_owned(), "w2:p5".to_owned())
        );
        assert!(matches!(
            created_tab(&json!({"result":{"type":"workspace_created"}})),
            Err(crate::Error::WorktreeScriptsResponse)
        ));
        assert!(matches!(
            created_tab(&json!({"error":{"code":"denied","message":"no"}})),
            Err(crate::Error::DaemonResponse(_))
        ));
    }
}
