//! What each pane runs, and how to run it again on the destination.

use super::snapshot::{AgentSession, Pane, ProcessInfo, SessionKind};

/// Agents whose sessions Teleport can carry across hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Opencode,
    Pi,
    Omp,
}

impl AgentKind {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "opencode" => Self::Opencode,
            "pi" => Self::Pi,
            "omp" => Self::Omp,
            _ => return None,
        })
    }

    pub(crate) fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
            Self::Omp => "omp",
        }
    }

    /// Whether a reported session reference identifies a session this kind
    /// can resume. Only Herdr's own integrations are trusted to report one.
    fn accepts(self, session: &AgentSession) -> bool {
        session.source == format!("herdr:{}", self.binary())
            && session.agent == self.binary()
            && !session.value.is_empty()
            && !session.value.chars().any(char::is_control)
            && match self {
                Self::Claude | Self::Codex | Self::Opencode => session.kind == SessionKind::Id,
                Self::Pi | Self::Omp => true,
            }
    }

    /// Flags that take a separate value, so the value is kept with its flag.
    fn value_flags(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &[
                "--model",
                "--permission-mode",
                "--add-dir",
                "--allowedTools",
                "--allowed-tools",
                "--disallowedTools",
                "--disallowed-tools",
                "--mcp-config",
                "--settings",
                "--append-system-prompt",
                "--system-prompt",
                "--agent",
                "--effort",
                "--fallback-model",
            ],
            Self::Codex => &[
                "-m",
                "--model",
                "-c",
                "--config",
                "-s",
                "--sandbox",
                "-a",
                "--ask-for-approval",
                "-p",
                "--profile",
                "--add-dir",
            ],
            Self::Opencode => &["-m", "--model", "--agent"],
            Self::Pi | Self::Omp => &["--model", "--provider", "--thinking"],
        }
    }

    /// Flags naming a session, a working directory, or a one-shot mode; the
    /// resume command supplies its own.
    fn replaced_flags(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["--resume", "-r", "--session-id", "--fork-session"],
            Self::Codex => &["-C", "--cd"],
            Self::Opencode => &["-s", "--session", "--port", "--hostname"],
            Self::Pi | Self::Omp => &["--session", "--resume", "-r", "--fork", "--session-dir"],
        }
    }

    fn replaced_switches(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["--continue", "-c", "--print", "-p"],
            Self::Codex => &[],
            Self::Opencode => &["-c", "--continue", "--fork"],
            Self::Pi | Self::Omp => &["-c", "--continue", "--no-session"],
        }
    }

    /// The flags of an agent's original command line worth keeping: model
    /// and permission choices. Positionals are dropped, because they are an
    /// initial prompt or project directory that must not be replayed.
    pub(crate) fn kept_flags(self, argv: &[String]) -> Vec<String> {
        let mut kept = Vec::new();
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
            let inline = arg.contains('=');
            if self.replaced_flags().contains(&name) {
                if !inline {
                    args.next();
                }
            } else if self.replaced_switches().contains(&name) || !arg.starts_with('-') {
                // A switch the resume replaces, or a positional.
            } else if self.value_flags().contains(&name) && !inline {
                if let Some(value) = args.next() {
                    kept.push(arg.clone());
                    kept.push(value.clone());
                }
            } else {
                kept.push(arg.clone());
            }
        }
        kept
    }

    /// The command that reopens `reference` (an id, or the session file's
    /// destination path), with `flags` from the original command line.
    pub(crate) fn resume_argv(self, reference: &str, flags: &[String]) -> Vec<String> {
        let mut argv = vec![self.binary().to_owned()];
        match self {
            Self::Claude => argv.extend(["--resume".to_owned(), reference.to_owned()]),
            Self::Codex => argv.extend(["resume".to_owned(), reference.to_owned()]),
            Self::Opencode | Self::Pi => {
                argv.extend(["--session".to_owned(), reference.to_owned()])
            }
            Self::Omp => argv.push(format!("--resume={reference}")),
        }
        argv.extend(flags.iter().cloned());
        argv
    }
}

/// What a source pane is doing, and so what its destination pane will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Work {
    /// An idle shell prompt; the new pane's shell is enough.
    Shell,
    /// A supported agent with a resumable session.
    Session {
        agent: AgentKind,
        session: AgentSession,
        flags: Vec<String>,
    },
    /// A detected agent without a session Teleport can move. `argv` starts
    /// the same agent afresh; a handoff note carries its context.
    Agent { name: String, argv: Vec<String> },
    /// Any other foreground program, run again with the same arguments.
    Command(Vec<String>),
}

impl Work {
    pub(crate) fn classify(pane: &Pane, process: &ProcessInfo) -> Self {
        let job = process.foreground_job();
        let argv = job.map(|job| job.argv.clone()).unwrap_or_default();
        if let Some(session) = &pane.agent_session
            && let Some(agent) = AgentKind::parse(&session.agent)
            && agent.accepts(session)
        {
            return Self::Session {
                agent,
                session: session.clone(),
                flags: agent.kept_flags(&argv),
            };
        }
        if let Some(name) = pane.agent.as_ref().filter(|name| !name.is_empty()) {
            let argv = if argv.is_empty() {
                vec![name.clone()]
            } else {
                argv
            };
            return Self::Agent {
                name: name.clone(),
                argv,
            };
        }
        if argv.is_empty() {
            Self::Shell
        } else {
            Self::Command(argv)
        }
    }

    /// The program the destination must have, if any.
    pub(crate) fn program(&self) -> Option<&str> {
        match self {
            Self::Shell => None,
            Self::Session { agent, .. } => Some(agent.binary()),
            Self::Agent { argv, .. } | Self::Command(argv) => argv.first().map(|p| basename(p)),
        }
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `path` moved from under `from` to under `to`, if it lies under `from`.
pub(crate) fn remap(path: &str, from: &str, to: &str) -> Option<String> {
    let from = from.trim_end_matches('/');
    let rest = path.strip_prefix(from)?;
    (rest.is_empty() || rest.starts_with('/'))
        .then(|| format!("{}{rest}", to.trim_end_matches('/')))
}

/// Replace every occurrence of the checkout path `from` in `text`, where the
/// match ends at a path boundary (so `/w/feat` never rewrites `/w/feature`).
pub(crate) fn rewrite_paths(text: &[u8], from: &str, to: &str) -> Vec<u8> {
    let from = from.trim_end_matches('/').as_bytes();
    let to = to.trim_end_matches('/').as_bytes();
    if from.is_empty() || from == to {
        return text.to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index..].starts_with(from) {
            let next = text.get(index + from.len()).copied();
            let boundary = !next.is_some_and(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
            if boundary {
                out.extend_from_slice(to);
                index += from.len();
                continue;
            }
        }
        out.push(text[index]);
        index += 1;
    }
    out
}

/// A command line re-targeted at the destination: paths under the source
/// checkout move with it, and an absolute program outside the checkout is
/// found on the destination's `PATH` instead.
pub(crate) fn remap_argv(argv: &[String], from: &str, to: &str) -> Vec<String> {
    argv.iter()
        .enumerate()
        .map(|(index, arg)| {
            let moved =
                String::from_utf8_lossy(&rewrite_paths(arg.as_bytes(), from, to)).into_owned();
            if index == 0 && moved.starts_with('/') && remap(arg, from, to).is_none() {
                basename(&moved).to_owned()
            } else {
                moved
            }
        })
        .collect()
}

/// One POSIX shell command line for `argv`.
pub(crate) fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if !arg.is_empty()
                && arg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
            {
                arg.clone()
            } else {
                herdr_client::shell_quote(arg)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Claude Code's project directory name for `cwd`: every character that is
/// not an ASCII letter or digit becomes `-`, and names over 200 characters are
/// cut and suffixed with a base-36 hash of the full path.
pub(crate) fn claude_project_dir(cwd: &str) -> String {
    const LIMIT: usize = 200;
    let name: String = cwd
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    if name.len() <= LIMIT {
        return name;
    }
    // Java-style string hash over UTF-16 units, as JavaScript computes it.
    let hash = cwd.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit))
    });
    format!(
        "{}-{}",
        &name[..LIMIT],
        base36(i64::from(hash).unsigned_abs())
    )
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

/// pi's session directory name for `cwd`.
pub(crate) fn pi_session_dir(cwd: &str) -> String {
    let trimmed = cwd.strip_prefix(['/', '\\']).unwrap_or(cwd);
    format!("--{}--", trimmed.replace(['/', '\\', ':'], "-"))
}

/// Where the agent in the `index`th handed-off pane writes its note, relative
/// to the checkout. The note is an untracked file, so it travels with the
/// uncommitted changes.
pub(crate) fn handoff_path(index: usize) -> String {
    format!(".herdr/teleport/handoff-{}.md", index + 1)
}

/// The prompt that asks an agent to leave its context behind in the checkout.
pub(crate) fn handoff_prompt(path: &str) -> String {
    format!(
        "This work is moving to another machine. Write a handoff note to {path} \
         (create the directory) so another agent can continue seamlessly: the goal, what is \
         done, the current state including uncommitted changes, decisions and constraints, \
         open questions, and the exact next steps. Do not change anything else, then stop."
    )
}

pub(crate) fn handoff_resume_prompt(path: &str) -> String {
    format!("Read {path} and continue the work it describes.")
}

impl AgentKind {
    /// The command that starts this agent afresh, optionally with a first prompt.
    pub(crate) fn start_argv(self, prompt: Option<&str>) -> Vec<String> {
        let mut argv = vec![self.binary().to_owned()];
        if let Some(prompt) = prompt {
            if self == Self::Opencode {
                argv.push("--prompt".to_owned());
            }
            argv.push(prompt.to_owned());
        }
        argv
    }

    /// Agents a handoff may fall back to, most capable first.
    pub(crate) const FALLBACKS: [Self; 4] = [Self::Claude, Self::Codex, Self::Opencode, Self::Pi];
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::snapshot::Process;
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn session(agent: &str, kind: SessionKind, value: &str) -> AgentSession {
        AgentSession {
            agent: agent.into(),
            kind,
            source: format!("herdr:{agent}"),
            value: value.into(),
        }
    }

    fn pane(agent: Option<&str>, session: Option<AgentSession>) -> Pane {
        Pane {
            pane_id: "p".into(),
            tab_id: "t".into(),
            cwd: Some("/w".into()),
            foreground_cwd: None,
            agent: agent.map(Into::into),
            agent_session: session,
        }
    }

    fn running(argv: &[&str]) -> ProcessInfo {
        ProcessInfo {
            foreground_process_group_id: Some(9),
            shell_pid: Some(1),
            foreground_processes: vec![Process {
                pid: 9,
                argv: strings(argv),
                cwd: None,
            }],
        }
    }

    #[test]
    fn classification_prefers_sessions_then_agents_then_commands() {
        let claude = pane(
            Some("claude"),
            Some(session("claude", SessionKind::Id, "abc")),
        );
        assert_eq!(
            Work::classify(
                &claude,
                &running(&[
                    "/Users/me/.local/bin/claude",
                    "--dangerously-skip-permissions",
                    "--model",
                    "opus",
                    "fix it",
                    "--resume",
                    "old"
                ])
            ),
            Work::Session {
                agent: AgentKind::Claude,
                session: session("claude", SessionKind::Id, "abc"),
                flags: strings(&["--dangerously-skip-permissions", "--model", "opus"]),
            }
        );
        // A session reported by anything but Herdr's integration is not trusted.
        let mut forged = session("claude", SessionKind::Id, "abc");
        forged.source = "plugin:x".into();
        assert!(matches!(
            Work::classify(&pane(Some("claude"), Some(forged)), &running(&["claude"])),
            Work::Agent { .. }
        ));
        // Claude sessions are ids; a path cannot be resumed.
        assert!(matches!(
            Work::classify(
                &pane(
                    Some("claude"),
                    Some(session("claude", SessionKind::Path, "/x.jsonl"))
                ),
                &running(&["claude"])
            ),
            Work::Agent { .. }
        ));
        assert_eq!(
            Work::classify(&pane(Some("gemini"), None), &ProcessInfo::default()),
            Work::Agent {
                name: "gemini".into(),
                argv: strings(&["gemini"])
            }
        );
        assert_eq!(
            Work::classify(&pane(None, None), &running(&["npm", "run", "dev"])),
            Work::Command(strings(&["npm", "run", "dev"]))
        );
        assert_eq!(
            Work::classify(&pane(None, None), &ProcessInfo::default()),
            Work::Shell
        );
    }

    #[test]
    fn resume_commands_match_each_agent() {
        let flags = strings(&["--model", "x"]);
        assert_eq!(
            AgentKind::Claude.resume_argv("id", &flags),
            strings(&["claude", "--resume", "id", "--model", "x"])
        );
        assert_eq!(
            AgentKind::Codex.resume_argv("id", &[]),
            strings(&["codex", "resume", "id"])
        );
        assert_eq!(
            AgentKind::Opencode.resume_argv("ses_1", &[]),
            strings(&["opencode", "--session", "ses_1"])
        );
        assert_eq!(
            AgentKind::Pi.resume_argv("/h/s.jsonl", &[]),
            strings(&["pi", "--session", "/h/s.jsonl"])
        );
        assert_eq!(
            AgentKind::Omp.resume_argv("/h/s.jsonl", &[]),
            strings(&["omp", "--resume=/h/s.jsonl"])
        );
    }

    #[test]
    fn kept_flags_drop_sessions_positionals_and_directories() {
        assert_eq!(
            AgentKind::Codex.kept_flags(&strings(&[
                "codex",
                "-C",
                "/src",
                "--cd=/src",
                "-m",
                "o3",
                "resume",
                "--yolo",
                "hi"
            ])),
            strings(&["-m", "o3", "--yolo"])
        );
        assert_eq!(
            AgentKind::Opencode.kept_flags(&strings(&[
                "opencode",
                "/src",
                "--session",
                "s",
                "-c",
                "--model=a/b"
            ])),
            strings(&["--model=a/b"])
        );
        assert!(
            AgentKind::Claude
                .kept_flags(&strings(&["claude", "--model"]))
                .is_empty()
        );
    }

    #[test]
    fn paths_move_only_at_boundaries() {
        assert_eq!(
            remap("/w/feat/src", "/w/feat", "/h/feat").as_deref(),
            Some("/h/feat/src")
        );
        assert_eq!(
            remap("/w/feat", "/w/feat/", "/h/feat").as_deref(),
            Some("/h/feat")
        );
        assert_eq!(remap("/w/feature", "/w/feat", "/h/feat"), None);
        let text =
            br#"{"cwd":"/w/feat","file":"/w/feat/a.rs","other":"/w/feature","x":"/w/feat.bak"}"#;
        assert_eq!(
            rewrite_paths(text, "/w/feat", "/home/u/feat"),
            br#"{"cwd":"/home/u/feat","file":"/home/u/feat/a.rs","other":"/w/feature","x":"/w/feat.bak"}"#
        );
    }

    #[test]
    fn remapped_commands_find_programs_on_the_destination() {
        assert_eq!(
            remap_argv(
                &strings(&[
                    "/opt/homebrew/bin/cargo",
                    "run",
                    "--manifest-path=/w/feat/Cargo.toml"
                ]),
                "/w/feat",
                "/h/f"
            ),
            strings(&["cargo", "run", "--manifest-path=/h/f/Cargo.toml"])
        );
        assert_eq!(
            remap_argv(&strings(&["/w/feat/scripts/dev.sh"]), "/w/feat", "/h/f"),
            strings(&["/h/f/scripts/dev.sh"])
        );
        assert_eq!(
            command_line(&strings(&["git", "commit", "-m", "it's done", ""])),
            "git commit -m 'it'\\''s done' ''"
        );
    }

    #[test]
    fn claude_project_dirs_match_claude_code() {
        assert_eq!(
            claude_project_dir(
                "/Users/penso/.herdr/worktrees/herdr-gpui/worktree-calm-forest-9099"
            ),
            "-Users-penso--herdr-worktrees-herdr-gpui-worktree-calm-forest-9099"
        );
        // Computed by Claude Code's own function under JavaScript: over 200
        // characters, with UTF-16 surrogates each becoming a dash.
        assert_eq!(
            claude_project_dir(
                "/Users/penso/.herdr/worktrees/moltis/agent-1255-bug-agentend-messagesending-and-messagesent-hooks-are-declared-but-never-dispatched-by-the-gateway-runtime-fbd6e3d9-extra-long-suffix/\u{e9}/sub/and/some/more/\u{1f600}/deeper"
            ),
            "-Users-penso--herdr-worktrees-moltis-agent-1255-bug-agentend-messagesending-and-messagesent-hooks-are-declared-but-never-dispatched-by-the-gateway-runtime-fbd6e3d9-extra-long-suffix---sub-and-some-mor-brbx8l"
        );
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn fresh_starts_carry_the_handoff_prompt() {
        let prompt = handoff_resume_prompt(&handoff_path(0));
        assert_eq!(
            prompt,
            "Read .herdr/teleport/handoff-1.md and continue the work it describes."
        );
        assert_eq!(
            AgentKind::Claude.start_argv(Some("go")),
            strings(&["claude", "go"])
        );
        assert_eq!(
            AgentKind::Opencode.start_argv(Some("go")),
            strings(&["opencode", "--prompt", "go"])
        );
        assert_eq!(AgentKind::Codex.start_argv(None), strings(&["codex"]));
    }

    #[test]
    fn pi_session_dirs_match_pi() {
        assert_eq!(
            pi_session_dir("/Users/penso/.herdr/worktrees/moltis/moltis-ui"),
            "--Users-penso-.herdr-worktrees-moltis-moltis-ui--"
        );
    }
}
