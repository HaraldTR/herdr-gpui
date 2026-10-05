//! The blocking work behind a fan-out. Every function here runs host scripts
//! and must only be called from a background worker, never the UI thread.

use super::{
    error::{Error, Result, Step, script},
    plan::{DiffStat, Lane, parse_commit, parse_stats, stats_script},
};
use crate::teleport::{AgentKind, Envelope, Host, WorktreeCreated};
use std::sync::atomic::{AtomicBool, Ordering};

/// How long `herdr agent start` may wait for each agent to be ready. Agents
/// starting side by side on one machine are slower than one alone.
const AGENT_START_TIMEOUT_MS: &str = "90000";

/// What a fan-out launches.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) host: Host,
    /// The workspace new worktrees are created through: the main checkout.
    pub(crate) workspace_id: String,
    /// The ref the first worktree branches from. The rest branch from the
    /// commit it resolved to, so every lane starts from the same commit.
    pub(crate) base: String,
    pub(crate) prompt: String,
    pub(crate) lanes: Vec<Lane>,
}

/// A lane's new worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkout {
    pub(crate) workspace_id: String,
    pub(crate) path: String,
    pub(crate) pane_id: String,
}

/// How far one lane has got.
#[derive(Debug)]
pub(crate) enum Progress {
    CreatingWorktree,
    Created(Checkout),
    StartingAgent,
    Prompting,
    /// The agent took the prompt and is working on it.
    Running,
    Failed(Error),
}

#[derive(Debug)]
pub(crate) enum Report {
    /// The commit every lane branches from and is compared against.
    Base(String),
    Lane(usize, Progress),
}

/// Create every lane's worktree one at a time, since concurrent `git
/// worktree add` runs contend for the repository's ref locks, then start the
/// agents and send the prompt to all of them side by side.
pub(crate) fn launch(request: &Request, report: &(impl Fn(Report) + Sync), cancelled: &AtomicBool) {
    let mut base: Option<String> = None;
    let mut created = Vec::new();
    for (index, lane) in request.lanes.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            report(Report::Lane(index, Progress::Failed(Error::Cancelled)));
            continue;
        }
        report(Report::Lane(index, Progress::CreatingWorktree));
        let from = base.as_deref().unwrap_or(&request.base);
        let checkout = match create(request, lane, from, cancelled) {
            Ok(checkout) => checkout,
            Err(error) => {
                report(Report::Lane(index, Progress::Failed(error)));
                continue;
            }
        };
        report(Report::Lane(index, Progress::Created(checkout.clone())));
        if base.is_none() {
            match resolve_base(&request.host, &checkout.path, cancelled) {
                Ok(commit) => {
                    report(Report::Base(commit.clone()));
                    base = Some(commit);
                }
                // Without its commit nothing can be compared; the checkout
                // stays listed so it can still be removed.
                Err(error) => {
                    report(Report::Lane(index, Progress::Failed(error)));
                    continue;
                }
            }
        }
        created.push((index, lane, checkout));
    }
    std::thread::scope(|scope| {
        for (index, lane, checkout) in &created {
            let run = move || {
                let progress = match start(request, *index, lane, checkout, report, cancelled) {
                    Ok(()) => Progress::Running,
                    Err(error) => Progress::Failed(error),
                };
                report(Report::Lane(*index, progress));
            };
            let spawned = std::thread::Builder::new()
                .name("herdr-fan-out-lane".into())
                .spawn_scoped(scope, run);
            if let Err(error) = spawned {
                tracing::warn!(%error, "could not start a fan-out lane worker");
                report(Report::Lane(
                    *index,
                    Progress::Failed(Error::Script {
                        step: Step::StartAgent,
                        source: herdr_client::Error::ScriptSpawn(error),
                    }),
                ));
            }
        }
    });
}

fn create(request: &Request, lane: &Lane, base: &str, cancelled: &AtomicBool) -> Result<Checkout> {
    let line = Host::herdr_line(&[
        "worktree",
        "create",
        "--workspace",
        &request.workspace_id,
        "--branch",
        &lane.branch,
        "--base",
        base,
        "--no-focus",
    ]);
    let output = request
        .host
        .capture(&line, cancelled)
        .map_err(script(Step::CreateWorktree))?;
    let created = serde_json::from_slice::<Envelope<WorktreeCreated>>(&output)
        .map_err(|source| Error::Decode {
            step: Step::CreateWorktree,
            source,
        })?
        .result;
    Ok(Checkout {
        workspace_id: created.workspace.workspace_id,
        path: created.worktree.path,
        pane_id: created.root_pane.pane_id,
    })
}

fn resolve_base(host: &Host, checkout: &str, cancelled: &AtomicBool) -> Result<String> {
    let body = format!(
        "git -C {} rev-parse --verify 'HEAD^{{commit}}'\n",
        herdr_client::shell_quote(checkout)
    );
    let output = host
        .capture(&body, cancelled)
        .map_err(script(Step::ResolveBase))?;
    parse_commit(&String::from_utf8_lossy(&output)).ok_or(Error::NoBase)
}

/// Start the lane's agent in its first pane and, once it is ready, send it
/// the prompt.
fn start(
    request: &Request,
    index: usize,
    lane: &Lane,
    checkout: &Checkout,
    report: &impl Fn(Report),
    cancelled: &AtomicBool,
) -> Result<()> {
    report(Report::Lane(index, Progress::StartingAgent));
    let start = Host::herdr_line(&[
        "agent",
        "start",
        &lane.agent,
        "--kind",
        lane.kind.name(),
        "--pane",
        &checkout.pane_id,
        "--timeout",
        AGENT_START_TIMEOUT_MS,
    ]);
    request
        .host
        .capture(&start, cancelled)
        .map_err(script(Step::StartAgent))?;
    report(Report::Lane(index, Progress::Prompting));
    // The pane is the target: an agent name could match one started elsewhere.
    let prompt = Host::herdr_line(&["agent", "prompt", &checkout.pane_id, &request.prompt]);
    request
        .host
        .capture(&prompt, cancelled)
        .map_err(script(Step::Prompt))?;
    Ok(())
}

/// The agent kinds installed on `host`, in [`AgentKind::ALL`] order.
pub(crate) fn installed(host: &Host, cancelled: &AtomicBool) -> Result<Vec<AgentKind>> {
    let binaries = AgentKind::ALL.map(AgentKind::binary);
    let found = host
        .installed_programs(&binaries, cancelled)
        .map_err(script(Step::Detect))?;
    Ok(AgentKind::ALL
        .into_iter()
        .filter(|kind| found.iter().any(|binary| binary == kind.binary()))
        .collect())
}

/// Each checkout's changes since `base`; `None` for a checkout that is gone.
pub(crate) fn stats(
    host: &Host,
    checkouts: &[String],
    base: &str,
    cancelled: &AtomicBool,
) -> Result<Vec<Option<DiffStat>>> {
    let output = host
        .capture(&stats_script(checkouts, base), cancelled)
        .map_err(script(Step::Compare))?;
    Ok(parse_stats(
        &String::from_utf8_lossy(&output),
        checkouts.len(),
    ))
}

/// Remove a lane's worktree and workspace, discarding uncommitted changes.
/// Herdr keeps the branch, so committed work stays reachable.
pub(crate) fn remove(host: &Host, workspace_id: &str, cancelled: &AtomicBool) -> Result<()> {
    let line = Host::herdr_line(&["worktree", "remove", "--workspace", workspace_id, "--force"]);
    host.capture(&line, cancelled)
        .map(drop)
        .map_err(script(Step::Remove))
}
