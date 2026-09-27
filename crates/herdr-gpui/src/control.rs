//! Lets processes on this machine, such as an agent in a Herdr pane, ask the
//! running app to show a web page. Herdr itself has no way to reach a client,
//! so the app listens on a socket of its own and `herdr-gpui browser open`
//! talks to it. Only this user's local processes can reach it: an agent on an
//! SSH host has no path to this machine's socket.

mod protocol;
#[cfg(unix)]
mod socket;

#[cfg(unix)]
use crate::HerdrWindow;
use crate::{browser::WebUrl, cli::BrowserCommand};
use gpui::App;
pub use protocol::ErrorCode;
use protocol::{BrowserOpen, OpenedIn, Request, Response};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The instructions an agent needs to use browser tabs, as shipped with the
/// app. `herdr-gpui browser skill` prints it; the palette installs it.
const SKILL: &str = include_str!("../../../skills/herdr-gpui-browser/SKILL.md");
const SKILL_NAME: &str = "herdr-gpui-browser";

/// The skill, naming `executable` in its commands. A macOS app bundle puts
/// nothing on `PATH`, so agents are told exactly which file to run.
pub(crate) fn skill(executable: Option<&Path>) -> String {
    let Some(path) = executable.and_then(Path::to_str) else {
        return SKILL.to_owned();
    };
    let command = if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    };
    SKILL.replace("herdr-gpui browser", &format!("{command} browser"))
}

/// Exit status when no app is listening, so a caller can fall back to
/// printing the address instead.
const EXIT_NOT_RUNNING: u8 = 3;

#[cfg(unix)]
fn socket_path() -> Option<PathBuf> {
    crate::preferences::state_dir().map(|dir| dir.join("control.sock"))
}

/// What a request asks the windows to find: a workspace, optionally of one
/// particular local daemon.
#[cfg(unix)]
pub(crate) struct Target<'a> {
    pub daemon: Option<&'a Path>,
    pub workspace: Option<&'a str>,
}

/// Where a window opened a requested tab.
#[cfg(unix)]
pub(crate) enum Placed {
    Opened { workspace_id: String },
    Full,
}

/// Identifiers a request carries are compared, never interpreted, but they
/// are echoed in answers, so they stay short and printable.
#[cfg(any(unix, test))]
fn plain(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

#[cfg(unix)]
fn open_browser(request: &BrowserOpen, cx: &mut App) -> Response {
    if !request
        .workspace_id
        .as_deref()
        .is_none_or(|id| plain(id, 256))
        || !request
            .daemon_socket
            .as_deref()
            .is_none_or(|path| plain(path, 4096))
    {
        return Response::error(ErrorCode::InvalidRequest, "Invalid workspace or daemon");
    }
    let Ok(url) = WebUrl::try_from(request.url.as_str()) else {
        return Response::error(
            ErrorCode::InvalidUrl,
            crate::Error::InvalidBrowserUrl.to_string(),
        );
    };
    if !crate::browser::EMBEDDED {
        cx.open_url(url.as_str());
        return Response::Opened {
            opened_in: OpenedIn::SystemBrowser,
            workspace_id: None,
        };
    }
    let active = cx.active_window();
    let mut windows: Vec<_> = cx
        .windows()
        .into_iter()
        .filter_map(|handle| handle.downcast::<HerdrWindow>())
        .collect();
    if windows.is_empty() {
        return Response::error(ErrorCode::NoWindow, "Herdr GPUI has no window open");
    }
    // The frontmost window wins a workspace that several windows show.
    windows.sort_by_key(|handle| Some(handle.window_id()) != active.map(|a| a.window_id()));
    let target = Target {
        daemon: request.daemon_socket.as_deref().map(Path::new),
        workspace: request.workspace_id.as_deref(),
    };
    // First the window connected to the caller's own daemon; then, when the
    // socket paths are spelled differently, any window showing its workspace.
    for strict in [true, false] {
        for handle in &windows {
            let placed = handle.update(cx, |view, window, cx| {
                view.open_requested_browser_tab(&target, strict, &url, request.focus, window, cx)
            });
            match placed {
                Ok(Some(Placed::Opened { workspace_id })) => {
                    return Response::Opened {
                        opened_in: OpenedIn::Tab,
                        workspace_id: Some(workspace_id),
                    };
                }
                Ok(Some(Placed::Full)) => {
                    return Response::error(ErrorCode::TabLimit, "Too many browser tabs are open");
                }
                Ok(None) | Err(_) => {}
            }
        }
    }
    Response::error(
        ErrorCode::WorkspaceNotFound,
        match &request.workspace_id {
            Some(id) => format!("No Herdr GPUI window shows workspace {id}"),
            None => "No Herdr GPUI window shows a workspace".into(),
        },
    )
}

/// Starts answering requests. A second app instance leaves the first one's
/// socket alone and simply does not listen.
#[cfg(unix)]
pub(crate) fn install(cx: &mut App) {
    let Some(path) = socket_path() else {
        return;
    };
    let server = match socket::Server::bind(&path) {
        Ok(server) => server,
        Err(error) => {
            tracing::warn!(%error, "Browser control socket unavailable");
            return;
        }
    };
    tracing::info!(path = %server.path().display(), "Browser control socket listening");
    cx.on_app_quit(move |cx| {
        let path = path.clone();
        cx.background_executor().spawn(async move {
            let _ = std::fs::remove_file(path);
        })
    })
    .detach();
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        loop {
            timer.timer(std::time::Duration::from_millis(50)).await;
            for incoming in server.drain() {
                let response = cx.update(|cx| match &incoming.request {
                    Request::BrowserOpen(request) => open_browser(request, cx),
                });
                incoming.respond(response);
            }
        }
    })
    .detach();
}

#[cfg(not(unix))]
pub(crate) fn install(_: &mut App) {}

/// The daemon a pane's agent belongs to, from the variables Herdr sets in it.
fn caller_daemon() -> crate::Result<Option<String>> {
    if std::env::var_os("HERDR_SOCKET_PATH").is_none() {
        return Ok(None);
    }
    let path = herdr_client::ConnectTarget::Local.socket_path()?;
    Ok(path.to_str().map(str::to_owned))
}

fn caller_workspace() -> Option<String> {
    std::env::var("HERDR_WORKSPACE_ID")
        .ok()
        .filter(|id| !id.is_empty())
}

#[cfg(unix)]
fn send(request: &Request) -> crate::Result<Response> {
    let path = socket_path().ok_or(crate::Error::MissingStateRoot)?;
    socket::call(&path, request)
}

#[cfg(not(unix))]
fn send(_: &Request) -> crate::Result<Response> {
    Err(crate::Error::ControlUnsupported)
}

fn browser_open(url: &str, workspace: Option<String>, focus: bool) -> crate::Result<String> {
    let url = WebUrl::from_typed(url)?;
    let request = Request::BrowserOpen(BrowserOpen {
        url: url.as_str().to_owned(),
        workspace_id: workspace.or_else(caller_workspace),
        daemon_socket: caller_daemon()?,
        focus,
    });
    match send(&request)? {
        Response::Opened {
            opened_in: OpenedIn::Tab,
            workspace_id,
        } => Ok(match workspace_id {
            Some(id) => format!("Opened {} in a browser tab of workspace {id}", url.as_str()),
            None => format!("Opened {} in a browser tab", url.as_str()),
        }),
        Response::Opened {
            opened_in: OpenedIn::SystemBrowser,
            ..
        } => Ok(format!(
            "Opened {} in the system browser; this platform's build has no browser tabs",
            url.as_str()
        )),
        Response::Error { code, message } => Err(crate::Error::ControlRejected { code, message }),
    }
}

const BROWSER_USAGE: &str = "\
herdr-gpui browser open URL [--workspace ID] [--no-focus]
    Show URL in a browser tab of the running Herdr GPUI. The tab joins the
    caller's own workspace (HERDR_WORKSPACE_ID), or the frontmost one outside
    Herdr. Bare hosts such as localhost:3000 are accepted.
herdr-gpui browser skill
    Print the agent skill that explains this command.
Exit status: 0 opened, 1 refused, 2 invalid arguments, 3 Herdr GPUI not running.";

/// Runs a control subcommand without starting GPUI.
pub(crate) fn run(command: BrowserCommand) -> ExitCode {
    match command {
        BrowserCommand::Help => {
            println!("{BROWSER_USAGE}");
            ExitCode::SUCCESS
        }
        BrowserCommand::Skill => {
            print!("{}", skill(std::env::current_exe().ok().as_deref()));
            ExitCode::SUCCESS
        }
        BrowserCommand::Open {
            url,
            workspace,
            focus,
        } => match browser_open(&url, workspace, focus) {
            Ok(message) => {
                println!("{}", crate::notifications::safe_text(&message, 4096));
                ExitCode::SUCCESS
            }
            Err(error) => {
                // The running app's answer reaches the caller's terminal.
                eprintln!(
                    "{}",
                    crate::notifications::safe_text(&error.to_string(), 4096)
                );
                ExitCode::from(match error {
                    crate::Error::InvalidBrowserUrl => 2,
                    crate::Error::ControlUnavailable { .. } => EXIT_NOT_RUNNING,
                    _ => 1,
                })
            }
        },
    }
}

/// Writes the skill into each agent configuration directory that exists
/// under `home`, returning where it went. Blocking; run off the UI thread.
pub(crate) fn install_skill(home: &Path, text: &str) -> crate::Result<Vec<PathBuf>> {
    let mut installed = Vec::new();
    for root in [".claude", ".agents"] {
        let root = home.join(root);
        if !root.is_dir() {
            continue;
        }
        let dir = root.join("skills").join(SKILL_NAME);
        let path = dir.join("SKILL.md");
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|source| crate::Error::SkillInstall {
                path: path.clone(),
                source,
            })?;
        installed.push(path);
    }
    if installed.is_empty() {
        return Err(crate::Error::SkillInstall {
            path: home.join(".claude"),
            source: std::io::ErrorKind::NotFound.into(),
        });
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn echoed_identifiers_are_short_and_printable() {
        assert!(plain("w_1", 256));
        for invalid in ["", "w 1", "w\u{1b}[2J", "w\n1"] {
            assert!(!plain(invalid, 256), "{invalid:?}");
        }
        assert!(!plain(&"w".repeat(257), 256));
    }

    #[test]
    fn the_skill_names_the_command_it_teaches() {
        assert!(SKILL.starts_with("---\nname: herdr-gpui-browser\n"));
        assert!(SKILL.contains("herdr-gpui browser open"));
        assert_eq!(skill(None), SKILL);
        let bundled = skill(Some(Path::new(
            "/Applications/Herdr.app/Contents/MacOS/Herdr",
        )));
        assert!(bundled.contains("/Applications/Herdr.app/Contents/MacOS/Herdr browser open"));
        assert!(!bundled.contains("herdr-gpui browser"));
        let spaced = skill(Some(Path::new("/Users/a b/it's/Herdr")));
        assert!(spaced.contains(r"'/Users/a b/it'\''s/Herdr' browser open"));
    }

    #[test]
    fn the_skill_installs_only_into_existing_agent_directories() {
        let home = tempfile::tempdir().unwrap();
        assert!(matches!(
            install_skill(home.path(), SKILL),
            Err(crate::Error::SkillInstall { .. })
        ));
        std::fs::create_dir(home.path().join(".agents")).unwrap();
        let installed = install_skill(home.path(), SKILL).unwrap();
        assert_eq!(
            installed,
            [home
                .path()
                .join(".agents/skills/herdr-gpui-browser/SKILL.md")]
        );
        assert_eq!(std::fs::read_to_string(&installed[0]).unwrap(), SKILL);
        assert!(!home.path().join(".claude").exists());
    }
}
