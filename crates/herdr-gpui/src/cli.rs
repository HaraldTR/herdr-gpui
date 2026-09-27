use herdr_client::ConnectTarget;
use std::ffi::OsString;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LaunchMode {
    #[default]
    Normal,
    Help,
    BuildInfo,
    /// Talk to the running app, then exit without starting GPUI.
    Browser(BrowserCommand),
    #[cfg(feature = "integration-test")]
    Integration,
    #[cfg(feature = "integration-test")]
    Sidebar,
    #[cfg(feature = "integration-test")]
    Performance,
}

/// `herdr-gpui browser ...`, answered by the running app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserCommand {
    Open {
        url: String,
        workspace: Option<String>,
        focus: bool,
    },
    Skill,
    Help,
}

#[derive(Debug)]
pub struct LaunchOptions {
    pub target: ConnectTarget,
    pub mode: LaunchMode,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("--socket requires a path")]
    MissingSocketPath,
    #[error("--session requires a name")]
    MissingSessionName,
    #[error("--socket may only be specified once")]
    DuplicateSocket,
    #[error("--session may only be specified once")]
    DuplicateSession,
    #[error("--session requires a UTF-8 name")]
    InvalidSessionEncoding(OsString),
    #[error("Unknown option: {}", .0.to_string_lossy())]
    UnknownOption(OsString),
    #[error("--socket cannot be combined with --session or --dev")]
    ConflictingConnectionOptions,
    #[error("browser requires a command: open URL, skill, or --help")]
    MissingBrowserCommand,
    #[error("Unknown browser command: {}", .0.to_string_lossy())]
    UnknownBrowserCommand(OsString),
    #[error("browser open requires one URL")]
    MissingUrl,
    #[error("browser open accepts one URL; unexpected {}", .0.to_string_lossy())]
    UnexpectedArgument(OsString),
    #[error("--workspace requires an ID")]
    MissingWorkspace,
    #[error("browser arguments must be UTF-8")]
    InvalidBrowserEncoding(OsString),
    #[cfg(feature = "integration-test")]
    #[error("native test modes are mutually exclusive and may only be specified once")]
    ConflictingTestModes,
    #[cfg(feature = "integration-test")]
    #[error("fixture tests cannot be combined with connection options or --integration-test")]
    ConflictingFixtureOptions,
    #[cfg(feature = "integration-test")]
    #[error("--integration-test requires an explicit --socket")]
    MissingIntegrationSocket,
    #[cfg(all(feature = "integration-test", not(target_os = "macos")))]
    #[error("--performance-test currently requires macOS native event delivery")]
    UnsupportedPerformancePlatform,
}

// The framed record is also read without execution by cross-platform packaging.
pub fn build_info() -> &'static str {
    const PREFIX_LEN: usize = "\0HERDR_BUILD_IDENTITY_V1\n".len();
    const RECORD: &str = concat!(
        "\0HERDR_BUILD_IDENTITY_V1\n",
        "worktree=",
        env!("HERDR_BUILD_WORKTREE"),
        "\n",
        "branch=",
        env!("HERDR_BUILD_BRANCH"),
        "\n",
        "pr=",
        env!("HERDR_BUILD_PR"),
        "\n\0"
    );
    let record = std::hint::black_box(RECORD);
    &record[PREFIX_LEN..record.len() - 1]
}

fn utf8(value: OsString) -> Result<String, CliError> {
    value
        .into_string()
        .map_err(CliError::InvalidBrowserEncoding)
}

fn parse_browser(mut args: impl Iterator<Item = OsString>) -> Result<BrowserCommand, CliError> {
    let command = args.next().ok_or(CliError::MissingBrowserCommand)?;
    match command.to_str() {
        Some("--help" | "-h") => return Ok(BrowserCommand::Help),
        Some("skill") => {
            return match args.next() {
                None => Ok(BrowserCommand::Skill),
                Some(extra) => Err(CliError::UnexpectedArgument(extra)),
            };
        }
        Some("open") => {}
        _ => return Err(CliError::UnknownBrowserCommand(command)),
    }
    let (mut url, mut workspace, mut focus) = (None, None, true);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(BrowserCommand::Help),
            Some("--no-focus") => focus = false,
            Some("--workspace") => {
                let value = args
                    .next()
                    .filter(|value| {
                        !value.is_empty() && !value.as_encoded_bytes().starts_with(b"-")
                    })
                    .ok_or(CliError::MissingWorkspace)?;
                workspace = Some(utf8(value)?);
            }
            _ if url.is_none() && !arg.as_encoded_bytes().starts_with(b"-") => {
                url = Some(utf8(arg)?);
            }
            _ => return Err(CliError::UnexpectedArgument(arg)),
        }
    }
    Ok(BrowserCommand::Open {
        url: url.ok_or(CliError::MissingUrl)?,
        workspace,
        focus,
    })
}

impl LaunchOptions {
    pub fn parse(args: impl IntoIterator<Item = impl Into<OsString>>) -> Result<Self, CliError> {
        let mut args = args.into_iter().map(Into::into).peekable();
        if args.peek().is_some_and(|arg| arg == "browser") {
            args.next();
            return Ok(Self {
                target: ConnectTarget::Local,
                mode: LaunchMode::Browser(parse_browser(args)?),
            });
        }
        let mut socket = None;
        let mut session = None;
        let mut development = false;
        #[cfg(feature = "integration-test")]
        let mut mode = LaunchMode::Normal;
        #[cfg(not(feature = "integration-test"))]
        let mode = LaunchMode::Normal;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h" | "--build-info") => {
                    return Ok(Self {
                        target: ConnectTarget::Local,
                        mode: if arg == "--build-info" {
                            LaunchMode::BuildInfo
                        } else {
                            LaunchMode::Help
                        },
                    });
                }
                Some("--socket" | "--session") => {
                    let is_socket = arg == "--socket";
                    let missing = if is_socket {
                        CliError::MissingSocketPath
                    } else {
                        CliError::MissingSessionName
                    };
                    let value = args
                        .next()
                        .filter(|value| {
                            !value.is_empty() && !value.as_encoded_bytes().starts_with(b"-")
                        })
                        .ok_or(missing)?;
                    if is_socket {
                        if socket.replace(value).is_some() {
                            return Err(CliError::DuplicateSocket);
                        }
                    } else {
                        let value = value
                            .into_string()
                            .map_err(CliError::InvalidSessionEncoding)?;
                        if session.replace(value).is_some() {
                            return Err(CliError::DuplicateSession);
                        }
                    }
                }
                Some("--dev") => development = true,
                #[cfg(feature = "integration-test")]
                Some(flag @ ("--integration-test" | "--sidebar-test" | "--performance-test")) => {
                    let next = match flag {
                        "--integration-test" => LaunchMode::Integration,
                        "--sidebar-test" => LaunchMode::Sidebar,
                        _ => LaunchMode::Performance,
                    };
                    if mode != LaunchMode::Normal {
                        return Err(CliError::ConflictingTestModes);
                    }
                    mode = next;
                }
                _ => {
                    return Err(CliError::UnknownOption(arg));
                }
            }
        }
        if socket.is_some() && (session.is_some() || development) {
            return Err(CliError::ConflictingConnectionOptions);
        }
        #[cfg(feature = "integration-test")]
        {
            if matches!(mode, LaunchMode::Sidebar | LaunchMode::Performance)
                && (socket.is_some() || session.is_some() || development)
            {
                return Err(CliError::ConflictingFixtureOptions);
            }
            if mode == LaunchMode::Integration && socket.is_none() {
                return Err(CliError::MissingIntegrationSocket);
            }
            #[cfg(not(target_os = "macos"))]
            if mode == LaunchMode::Performance {
                return Err(CliError::UnsupportedPerformancePlatform);
            }
        }
        let target = match (socket, session) {
            (Some(path), _) => ConnectTarget::Socket(path.into()),
            (_, Some(name)) => ConnectTarget::Session { name, development },
            _ if development => ConnectTarget::Session {
                name: "default".into(),
                development,
            },
            _ => ConnectTarget::Local,
        };
        Ok(Self { target, mode })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn build_info_is_an_informational_mode() {
        assert_eq!(
            LaunchOptions::parse(["--build-info"]).unwrap().mode,
            LaunchMode::BuildInfo
        );
        assert_eq!(
            build_info(),
            concat!(
                "worktree=",
                env!("HERDR_BUILD_WORKTREE"),
                "\nbranch=",
                env!("HERDR_BUILD_BRANCH"),
                "\npr=",
                env!("HERDR_BUILD_PR"),
                "\n"
            )
        );
    }

    #[test]
    fn browser_commands_never_start_the_gui() {
        let mode = |args: &[&str]| LaunchOptions::parse(args.iter().copied()).map(|o| o.mode);
        assert_eq!(
            mode(&["browser", "open", "localhost:3000"]).unwrap(),
            LaunchMode::Browser(BrowserCommand::Open {
                url: "localhost:3000".into(),
                workspace: None,
                focus: true,
            })
        );
        assert_eq!(
            mode(&[
                "browser",
                "open",
                "--no-focus",
                "--workspace",
                "w_2",
                "https://a.test"
            ])
            .unwrap(),
            LaunchMode::Browser(BrowserCommand::Open {
                url: "https://a.test".into(),
                workspace: Some("w_2".into()),
                focus: false,
            })
        );
        assert_eq!(
            mode(&["browser", "skill"]).unwrap(),
            LaunchMode::Browser(BrowserCommand::Skill)
        );
        for help in [&["browser", "--help"][..], &["browser", "open", "-h"]] {
            assert_eq!(
                mode(help).unwrap(),
                LaunchMode::Browser(BrowserCommand::Help)
            );
        }
        for (args, expected) in [
            (&["browser"][..], CliError::MissingBrowserCommand),
            (
                &["browser", "eval"],
                CliError::UnknownBrowserCommand("eval".into()),
            ),
            (&["browser", "open"], CliError::MissingUrl),
            (
                &["browser", "open", "--workspace"],
                CliError::MissingWorkspace,
            ),
            (
                &["browser", "open", "a", "b"],
                CliError::UnexpectedArgument("b".into()),
            ),
            (
                &["browser", "open", "--new"],
                CliError::UnexpectedArgument("--new".into()),
            ),
            (
                &["browser", "skill", "x"],
                CliError::UnexpectedArgument("x".into()),
            ),
        ] {
            assert_eq!(mode(args).unwrap_err(), expected, "{args:?}");
        }
        // Only a leading "browser" is the subcommand.
        assert_eq!(
            mode(&["--dev", "browser"]).unwrap_err(),
            CliError::UnknownOption("browser".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn browser_arguments_must_be_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let invalid = OsString::from_vec(b"https://a.test/\xff".to_vec());
        let error =
            LaunchOptions::parse([OsString::from("browser"), "open".into(), invalid.clone()])
                .unwrap_err();
        assert_eq!(error, CliError::InvalidBrowserEncoding(invalid));
    }

    #[test]
    fn connection_selection() {
        assert!(
            matches!(LaunchOptions::parse(["--dev"]).unwrap().target, ConnectTarget::Session { name, development: true } if name == "default")
        );
        assert!(
            matches!(LaunchOptions::parse(["--session", "test"]).unwrap().target, ConnectTarget::Session { name, development: false } if name == "test")
        );
        assert!(matches!(
            LaunchOptions::parse(std::iter::empty::<OsString>())
                .unwrap()
                .target,
            ConnectTarget::Local
        ));
    }

    #[test]
    fn rejects_missing_and_duplicate_values() {
        for (args, expected, message) in [
            (
                vec!["--socket"],
                CliError::MissingSocketPath,
                "--socket requires a path",
            ),
            (
                vec!["--session"],
                CliError::MissingSessionName,
                "--session requires a name",
            ),
            (
                vec!["--socket", "--help"],
                CliError::MissingSocketPath,
                "--socket requires a path",
            ),
            (
                vec!["--session", "--dev"],
                CliError::MissingSessionName,
                "--session requires a name",
            ),
            (
                vec!["--socket", ""],
                CliError::MissingSocketPath,
                "--socket requires a path",
            ),
            (
                vec!["--socket", "a", "--socket", "b"],
                CliError::DuplicateSocket,
                "--socket may only be specified once",
            ),
            (
                vec!["--session", "a", "--session", "b"],
                CliError::DuplicateSession,
                "--session may only be specified once",
            ),
            (
                vec!["--socket", "a", "--dev"],
                CliError::ConflictingConnectionOptions,
                "--socket cannot be combined with --session or --dev",
            ),
            (
                vec!["--socket", "a", "--session", "b"],
                CliError::ConflictingConnectionOptions,
                "--socket cannot be combined with --session or --dev",
            ),
            (
                vec!["--unknown"],
                CliError::UnknownOption("--unknown".into()),
                "Unknown option: --unknown",
            ),
        ] {
            let error = LaunchOptions::parse(args).unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(error.to_string(), message);
        }
    }

    // Windows paths are UTF-16 and have no byte form to round-trip here; the
    // parser stays byte-agnostic on both because it never converts to `String`.
    #[cfg(unix)]
    #[test]
    fn socket_paths_need_not_be_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"/tmp/socket-\xff".to_vec());
        let options = LaunchOptions::parse([OsString::from("--socket"), path.clone()]).unwrap();
        assert!(
            matches!(options.target, ConnectTarget::Socket(actual) if actual.as_os_str() == path)
        );
        let error = LaunchOptions::parse([OsString::from("--session"), path.clone()]).unwrap_err();
        assert_eq!(error, CliError::InvalidSessionEncoding(path.clone()));
        assert_eq!(error.to_string(), "--session requires a UTF-8 name");
        let error = LaunchOptions::parse([path.clone()]).unwrap_err();
        assert_eq!(error, CliError::UnknownOption(path.clone()));
        assert_eq!(
            error.to_string(),
            format!("Unknown option: {}", path.to_string_lossy())
        );
    }

    #[cfg(feature = "integration-test")]
    #[test]
    fn test_modes_are_exclusive() {
        for first in ["--integration-test", "--sidebar-test", "--performance-test"] {
            for second in ["--integration-test", "--sidebar-test", "--performance-test"] {
                let error = LaunchOptions::parse([first, second]).unwrap_err();
                assert_eq!(error, CliError::ConflictingTestModes);
                assert_eq!(
                    error.to_string(),
                    "native test modes are mutually exclusive and may only be specified once"
                );
            }
        }
        let error = LaunchOptions::parse(["--integration-test"]).unwrap_err();
        assert_eq!(error, CliError::MissingIntegrationSocket);
        assert_eq!(
            error.to_string(),
            "--integration-test requires an explicit --socket"
        );
        for flag in ["--sidebar-test", "--performance-test"] {
            let error = LaunchOptions::parse([flag, "--dev"]).unwrap_err();
            assert_eq!(error, CliError::ConflictingFixtureOptions);
            assert_eq!(
                error.to_string(),
                "fixture tests cannot be combined with connection options or --integration-test"
            );
        }
    }
}
