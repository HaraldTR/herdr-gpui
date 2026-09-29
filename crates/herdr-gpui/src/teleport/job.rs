//! The three blocking phases of a teleport, each run on a background worker:
//! find destinations, review what will move, and move it.

use super::{
    credentials,
    error::{Error, Result, Step},
    git,
    host::Host,
    launch::{
        AgentKind, Work, command_line, handoff_path, handoff_prompt, handoff_resume_prompt, remap,
        remap_argv,
    },
    layout::{Node, tree},
    provision::{self, Arrival},
    remote::{MatchReason, RepoIdentity},
    sessions::{Route, move_session},
    snapshot::{
        HostSnapshot, PaneInfoResult, ProcessInfo, ProcessInfoResult, SnapshotResult, TabCreated,
        TabList, WorkspaceCreated, WorktreeCreated,
    },
};
use herdr_client::shell_quote;
use std::{collections::HashMap, io::Seek, sync::atomic::AtomicBool, time::Duration};

/// How long an agent may take to write its handoff note.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(300);

/// An endpoint as Teleport addresses it.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub(crate) endpoint_id: String,
    pub(crate) label: String,
    pub(crate) host: Host,
}

/// The workspace being moved, as the GUI snapshot describes it.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub(crate) place: Place,
    pub(crate) workspace_id: String,
    /// A label the user chose, carried to the destination workspace.
    pub(crate) custom_label: Option<String>,
    pub(crate) repo_key: String,
    pub(crate) repo_label: String,
    /// The branch the GUI snapshot shows, for finding a checkout it left.
    pub(crate) branch: Option<String>,
    /// Tab labels the user chose, by source tab id.
    pub(crate) tab_labels: HashMap<String, String>,
}

/// A repository open on some host, found through one of its workspaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repository {
    pub(crate) key: String,
    pub(crate) label: String,
    /// A workspace of the repository, preferably its main checkout.
    pub(crate) workspace_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HostRepositories {
    pub(crate) place: Place,
    /// The host's open repositories from its GUI snapshot, or `None` when it
    /// is not connected and they must be read through its CLI.
    pub(crate) repositories: Option<Vec<Repository>>,
    /// Checkouts on this host this client marked as teleported away.
    pub(crate) retired: Vec<Retired>,
}

/// A checkout whose work was teleported away, still open as a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Retired {
    pub(crate) repo_key: String,
    pub(crate) branch: String,
    pub(crate) workspace_id: String,
}

/// Where the worktree lands on a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Destination {
    /// The repository is open there already.
    Open {
        repository: Repository,
        reason: MatchReason,
    },
    /// It is not open: open an existing checkout or clone one first.
    Arrive(Arrival),
    /// The work left this checkout earlier and is coming back to it.
    Reclaim {
        repository: Repository,
        workspace_id: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) place: Place,
    pub(crate) destination: Destination,
    /// `origin`'s URL, for a clone the destination can fetch itself.
    pub(crate) origin: Option<String>,
}

impl Candidate {
    fn rank(&self) -> (u8, Option<MatchReason>) {
        match &self.destination {
            Destination::Reclaim { .. } => (0, None),
            Destination::Open { reason, .. } => (1, Some(*reason)),
            Destination::Arrive(Arrival::Existing { .. }) => (2, None),
            Destination::Arrive(Arrival::Clone { .. }) => (3, None),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Discovery {
    pub(crate) candidates: Vec<Candidate>,
    /// Hosts that could not be read, with the reason.
    pub(crate) unreachable: Vec<(String, Error)>,
}

/// One repository per Git common directory, reached through its main
/// checkout's workspace when that is open.
pub(crate) fn repositories_of(snapshot: &HostSnapshot) -> Vec<Repository> {
    let mut found: Vec<Repository> = Vec::new();
    for workspace in &snapshot.workspaces {
        let Some(tree) = &workspace.worktree else {
            continue;
        };
        match found.iter_mut().find(|repo| repo.key == tree.repo_key) {
            Some(repo) if !tree.is_linked_worktree => {
                repo.workspace_id.clone_from(&workspace.workspace_id);
            }
            Some(_) => {}
            None => found.push(Repository {
                key: tree.repo_key.clone(),
                label: tree.repo_name.clone(),
                workspace_id: workspace.workspace_id.clone(),
            }),
        }
    }
    found
}

/// Find where the worktree can go on every other host: a matching open
/// repository, else an existing checkout to open or a place to clone to.
pub(crate) fn discover(
    source: &Source,
    hosts: Vec<HostRepositories>,
    cancelled: &AtomicBool,
) -> Result<Discovery> {
    let own = git::remotes(
        &source.place.host,
        std::slice::from_ref(&source.repo_key),
        cancelled,
    )?;
    let identity = RepoIdentity {
        name: source.repo_label.clone(),
        remotes: own.get(&source.repo_key).cloned().unwrap_or_default(),
    };
    let origin = provision::source_repository(
        &source.place.host,
        &source.repo_key,
        &source.repo_label,
        cancelled,
    )?;
    let mut discovery = Discovery::default();
    for host in hosts {
        match discover_host(
            &host,
            &identity,
            &origin,
            source.branch.as_deref(),
            cancelled,
        ) {
            Ok(candidates) => discovery.candidates.extend(candidates),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => discovery
                .unreachable
                .push((host.place.label.clone(), error)),
        }
    }
    discovery.candidates.sort_by(|a, b| {
        a.rank()
            .cmp(&b.rank())
            .then_with(|| a.place.label.cmp(&b.place.label))
    });
    Ok(discovery)
}

fn discover_host(
    host: &HostRepositories,
    identity: &RepoIdentity,
    source: &provision::SourceRepository,
    branch: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Vec<Candidate>> {
    let repositories = match &host.repositories {
        Some(repositories) => repositories.clone(),
        None => {
            let snapshot: SnapshotResult =
                host.place
                    .host
                    .herdr(Step::Discover, &["api", "snapshot"], cancelled)?;
            repositories_of(&snapshot.snapshot)
        }
    };
    let keys: Vec<_> = repositories.iter().map(|r| r.key.clone()).collect();
    let remotes = git::remotes(&host.place.host, &keys, cancelled)?;
    let candidate = |destination| Candidate {
        place: host.place.clone(),
        destination,
        origin: source.origin.clone(),
    };
    let open: Vec<Candidate> = repositories
        .into_iter()
        .filter_map(|repository| {
            let theirs = RepoIdentity {
                name: repository.label.clone(),
                remotes: remotes.get(&repository.key).cloned().unwrap_or_default(),
            };
            let reason = identity.matches(&theirs)?;
            Some(candidate(Destination::Open { repository, reason }))
        })
        .collect();
    // Going back to a checkout the work left: that checkout is the place.
    let back = branch.and_then(|branch| {
        host.retired.iter().find_map(|retired| {
            let repository = open
                .iter()
                .find_map(|candidate| match &candidate.destination {
                    Destination::Open { repository, .. } if repository.key == retired.repo_key => {
                        Some(repository.clone())
                    }
                    _ => None,
                })?;
            (retired.branch == branch).then(|| Destination::Reclaim {
                repository,
                workspace_id: retired.workspace_id.clone(),
            })
        })
    });
    if let Some(back) = back {
        return Ok(vec![candidate(back)]);
    }
    if !open.is_empty() {
        return Ok(open);
    }
    let arrival = provision::probe(&host.place.host, source, identity, cancelled)?;
    Ok(vec![candidate(Destination::Arrive(arrival))])
}

/// What the destination pane will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// An idle shell.
    Shell,
    /// Move the agent's session and resume it.
    Resume(AgentKind),
    /// Ask the source agent for a note, then start `to` with it.
    Handoff { note: String, to: AgentKind },
    /// Start the same program afresh.
    Start(Vec<String>),
    /// Run the same command again.
    Run(Vec<String>),
    /// Nothing on the destination can take this over.
    Missing(String),
}

#[derive(Debug, Clone)]
pub(crate) struct PanePlan {
    pub(crate) pane_id: String,
    /// The directory the source program runs in.
    pub(crate) cwd: Option<String>,
    pub(crate) work: Work,
    pub(crate) action: Action,
}

#[derive(Debug, Clone)]
pub(crate) struct TabPlan {
    pub(crate) label: Option<String>,
    pub(crate) tree: Node,
    pub(crate) panes: Vec<PanePlan>,
}

impl TabPlan {
    fn pane(&self, id: &str) -> Option<&PanePlan> {
        self.panes.iter().find(|pane| pane.pane_id == id)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Review {
    pub(crate) checkout: String,
    pub(crate) branch: String,
    pub(crate) state: git::SourceState,
    pub(crate) tabs: Vec<TabPlan>,
    /// Why an open repository was chosen; `None` when it arrives fresh.
    pub(crate) reason: Option<MatchReason>,
    pub(crate) github: GitHubAccess,
}

/// Whether the destination can pull and push to GitHub by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitHubAccess {
    /// It reaches `origin` already, or `origin` is not on GitHub.
    Direct,
    /// It cannot: this machine's `gh` token is installed for the repository.
    Token,
    /// It cannot, and `gh` here is not signed in to lend a token.
    Unavailable,
}

impl Review {
    pub(crate) fn panes(&self) -> impl Iterator<Item = &PanePlan> {
        self.tabs.iter().flat_map(|tab| tab.panes.iter())
    }
}

fn process_infos(
    host: &Host,
    panes: &[String],
    cancelled: &AtomicBool,
) -> Result<HashMap<String, ProcessInfo>> {
    let body = panes
        .iter()
        .map(|pane| {
            format!(
                "printf '\\036%s\\n' {pane}\nherdr_cli pane process-info --pane {pane} 2>/dev/null || :\n",
                pane = shell_quote(pane)
            )
        })
        .collect::<String>();
    let output = host.query(Step::Review, &body, &[], cancelled)?;
    let output = String::from_utf8_lossy(&output);
    Ok(output
        .split('\u{1e}')
        .filter_map(|section| {
            let (pane, json) = section.split_once('\n')?;
            let info = serde_json::from_str::<super::snapshot::Envelope<ProcessInfoResult>>(json)
                .ok()?
                .result
                .process_info;
            Some((pane.to_owned(), info))
        })
        .collect())
}

/// Choose what each pane becomes on a destination with `installed` programs.
pub(crate) fn plan_action(
    work: &Work,
    installed: &[String],
    note: impl FnOnce() -> String,
) -> Action {
    let has = |program: &str| installed.iter().any(|p| p == program);
    let fallback = || {
        AgentKind::FALLBACKS
            .into_iter()
            .find(|kind| has(kind.binary()))
    };
    match work {
        Work::Shell => Action::Shell,
        Work::Session { agent, .. } if has(agent.binary()) => Action::Resume(*agent),
        Work::Session { agent, .. } => fallback().map_or_else(
            || Action::Missing(agent.binary().to_owned()),
            |to| Action::Handoff { note: note(), to },
        ),
        Work::Agent { name, argv } => match AgentKind::parse(name) {
            Some(kind) if has(kind.binary()) => Action::Handoff {
                note: note(),
                to: kind,
            },
            None if work.program().is_some_and(has) => Action::Start(argv.clone()),
            _ => fallback().map_or_else(
                || Action::Missing(name.clone()),
                |to| Action::Handoff { note: note(), to },
            ),
        },
        Work::Command(argv) => Action::Run(argv.clone()),
    }
}

/// Read the source workspace and the destination, and decide what moves.
pub(crate) fn review(
    source: &Source,
    destination: &Candidate,
    cancelled: &AtomicBool,
) -> Result<Review> {
    let host = &source.place.host;
    let snapshot: SnapshotResult = host.herdr(Step::Review, &["api", "snapshot"], cancelled)?;
    let snapshot = snapshot.snapshot;
    let worktree = snapshot
        .workspace(&source.workspace_id)
        .and_then(|workspace| workspace.worktree.clone())
        .filter(|worktree| worktree.is_linked_worktree)
        .ok_or(Error::WorkspaceGone)?;
    let state = git::source_state(host, &worktree.checkout_path, cancelled)?;
    let branch = state.branch.clone().ok_or(Error::DetachedHead)?;
    // A fresh clone has no branch to collide with; anything already there does.
    let key = match &destination.destination {
        Destination::Open { repository, .. } => Some(repository.key.as_str()),
        Destination::Arrive(Arrival::Existing { key, .. }) => Some(key.as_str()),
        Destination::Arrive(Arrival::Clone { .. }) => None,
        // The checkout there is this branch's, and is backed up before reuse.
        Destination::Reclaim { .. } => None,
    };
    if let Some(key) = key {
        let dest = git::destination_branch(&destination.place.host, key, &branch, cancelled)?;
        git::check_destination(host, &worktree.checkout_path, &branch, &dest, cancelled)?;
    }

    let tabs = snapshot.tabs_of(&source.workspace_id);
    let pane_ids: Vec<String> = snapshot
        .panes
        .iter()
        .filter(|pane| tabs.iter().any(|tab| tab.tab_id == pane.tab_id))
        .map(|pane| pane.pane_id.clone())
        .collect();
    let processes = process_infos(host, &pane_ids, cancelled)?;
    let works: Vec<(String, Work, Option<String>)> = pane_ids
        .iter()
        .filter_map(|id| {
            let pane = snapshot.pane(id)?;
            let process = processes.get(id).cloned().unwrap_or_default();
            let cwd = process
                .foreground_job()
                .and_then(|job| job.cwd.clone())
                .or_else(|| pane.foreground_cwd.clone())
                .or_else(|| pane.cwd.clone());
            Some((id.clone(), Work::classify(pane, &process), cwd))
        })
        .collect();
    let mut programs: Vec<&str> = AgentKind::FALLBACKS.iter().map(|k| k.binary()).collect();
    programs.extend(works.iter().filter_map(|(_, work, _)| work.program()));
    programs.sort_unstable();
    programs.dedup();
    let installed = destination
        .place
        .host
        .installed(Step::Review, &programs, cancelled)?;

    let github = match destination
        .origin
        .as_deref()
        .filter(|origin| credentials::is_github(origin))
    {
        Some(origin) if !credentials::reachable(&destination.place.host, origin, cancelled)? => {
            if credentials::local_token(cancelled)?.is_some() {
                GitHubAccess::Token
            } else {
                GitHubAccess::Unavailable
            }
        }
        _ => GitHubAccess::Direct,
    };

    let mut notes = 0;
    let tabs = tabs
        .iter()
        .map(|tab| plan_tab(&snapshot, tab, source, &works, &installed, &mut notes))
        .collect();
    Ok(Review {
        checkout: worktree.checkout_path,
        branch,
        state,
        tabs,
        reason: match &destination.destination {
            Destination::Open { reason, .. } => Some(*reason),
            Destination::Arrive(_) | Destination::Reclaim { .. } => None,
        },
        github,
    })
}

fn plan_tab(
    snapshot: &HostSnapshot,
    tab: &super::snapshot::Tab,
    source: &Source,
    works: &[(String, Work, Option<String>)],
    installed: &[String],
    notes: &mut usize,
) -> TabPlan {
    let members: Vec<String> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .map(|pane| pane.pane_id.clone())
        .collect();
    let node = snapshot
        .layout(&tab.tab_id)
        .and_then(tree)
        .or_else(|| Node::row(&members))
        .unwrap_or_else(|| Node::Pane(String::new()));
    let panes = node
        .plan()
        .slots
        .into_iter()
        .filter_map(|id| {
            let (_, work, cwd) = works.iter().find(|(pane, ..)| *pane == id)?;
            let action = plan_action(work, installed, || {
                let path = handoff_path(*notes);
                *notes += 1;
                path
            });
            Some(PanePlan {
                pane_id: id,
                cwd: cwd.clone(),
                work: work.clone(),
                action,
            })
        })
        .collect();
    TabPlan {
        label: source.tab_labels.get(&tab.tab_id).cloned(),
        tree: node,
        panes,
    }
}

/// Where the teleported workspace ended up.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub(crate) endpoint_id: String,
    pub(crate) workspace_id: String,
    /// The destination repository, for remembering where the work went.
    pub(crate) repo_key: String,
    pub(crate) branch: String,
    /// Things that did not carry over, for the user to see.
    pub(crate) warnings: Vec<String>,
}

fn reference_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("refs/herdr-teleport/{nanos:x}-{:x}", std::process::id())
}

/// Move the workspace. `report` announces each step as it starts.
pub(crate) fn run(
    source: &Source,
    destination: &Candidate,
    review: &Review,
    mut report: impl FnMut(Step),
    cancelled: &AtomicBool,
) -> Result<Outcome> {
    let from = &source.place.host;
    let to = &destination.place.host;
    let mut warnings = Vec::new();

    // The repository comes first: if it cannot be put on the destination,
    // nothing on the source has been touched.
    let repository = arrive(source, destination, &mut report, cancelled)?;
    let key = &repository.key;

    // Notes are written into the source checkout first, so they travel with
    // the uncommitted changes.
    let handoffs: Vec<&PanePlan> = review
        .panes()
        .filter(|pane| matches!(pane.action, Action::Handoff { .. }))
        .collect();
    if !handoffs.is_empty() {
        report(Step::Handoff);
        let timeout = HANDOFF_TIMEOUT.as_millis().to_string();
        for pane in handoffs {
            let Action::Handoff { note, .. } = &pane.action else {
                continue;
            };
            let asked = from.herdr_ok(
                Step::Handoff,
                &[
                    "agent",
                    "prompt",
                    &pane.pane_id,
                    &handoff_prompt(&format!("{}/{note}", review.checkout)),
                    "--wait",
                    "--timeout",
                    &timeout,
                ],
                cancelled,
            );
            match asked {
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(error) => {
                    warnings.push(format!("No handoff note from {}: {error}", pane.pane_id))
                }
                Ok(()) => {}
            }
        }
    }

    report(Step::Capture);
    let dest = git::destination_branch(to, key, &review.branch, cancelled)?;
    // Coming back, the branch is checked out in the very checkout being
    // reclaimed, which is backed up rather than refused.
    if !matches!(destination.destination, Destination::Reclaim { .. }) {
        git::check_destination(from, &review.checkout, &review.branch, &dest, cancelled)?;
    }
    let reference = reference_name();
    let mut bundle = tempfile::tempfile().map_err(Error::LocalFile)?;
    git::capture(
        from,
        &review.checkout,
        &reference,
        &dest.tips,
        &mut bundle,
        cancelled,
    )?;
    bundle.rewind().map_err(Error::LocalFile)?;

    report(Step::Transfer);
    let uploaded = git::upload(to, bundle, cancelled)?;
    report(Step::Fetch);
    let fetched = git::fetch(to, key, &uploaded, &reference, cancelled);
    let _ = git::discard_upload(to, &uploaded, cancelled);
    fetched?;

    let placed = (|| {
        let landing = land(
            source,
            destination,
            &repository,
            review,
            &reference,
            &mut report,
            cancelled,
        )?;
        let panes = rebuild_tabs(to, &landing, review, &mut report, cancelled)?;
        Ok((landing, panes))
    })();
    let (landing, panes) = match placed {
        Ok(placed) => placed,
        Err(error) => {
            git::drop_reference(to, key, &reference, &AtomicBool::new(false));
            return Err(error);
        }
    };

    // The source's programs stop here, so every session file is complete. The
    // workspace itself stays, for this client to mark as teleported.
    report(Step::Retire);
    retire(from, &source.workspace_id, &review.checkout, cancelled)?;

    report(Step::Sessions);
    let mut launches = Vec::new();
    for (plan, target) in review.panes().zip(&panes) {
        let line = match &plan.action {
            Action::Shell | Action::Missing(_) => None,
            Action::Resume(agent) => {
                let Work::Session { session, flags, .. } = &plan.work else {
                    continue;
                };
                let route = Route {
                    source: from,
                    destination: to,
                    from: &review.checkout,
                    to: &landing.checkout,
                    cwd: &target.cwd,
                };
                Some(match move_session(*agent, session, &route, cancelled) {
                    Ok(reference) => agent.resume_argv(&reference, flags),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(error) => {
                        warnings.push(format!("{} started afresh: {error}", agent.binary()));
                        agent.start_argv(None)
                    }
                })
            }
            Action::Handoff { note, to: agent } => {
                let note = format!("{}/{note}", landing.checkout);
                Some(agent.start_argv(Some(&handoff_resume_prompt(&note))))
            }
            Action::Start(argv) | Action::Run(argv) => {
                Some(remap_argv(argv, &review.checkout, &landing.checkout))
            }
        };
        if let Action::Missing(program) = &plan.action {
            warnings.push(format!(
                "{program} is not installed on {}",
                destination.place.label
            ));
        }
        if let Some(argv) = line {
            launches.push((target, argv));
        }
    }

    if review.github == GitHubAccess::Token {
        report(Step::Credentials);
        // Pull and push matter, but not more than the move: failures warn.
        let installed = credentials::local_token(cancelled).and_then(|token| {
            let token = token.ok_or(Error::InvalidToken)?;
            credentials::install(to, &landing.checkout, &token, cancelled)
        });
        match installed {
            Ok(credentials::Installed::Complete) => {}
            Ok(credentials::Installed::WithoutEnvrc) => warnings.push(
                "The repository tracks .envrc, so GH_TOKEN was not added; git pull and push still work"
                    .to_owned(),
            ),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => warnings.push(format!("GitHub access was not set up: {error}")),
        }
    }

    report(Step::Launch);
    for (target, argv) in launches {
        let mut line = command_line(&argv);
        if target.cwd != target.created_in {
            line = format!("cd -- {} && {line}", shell_quote(&target.cwd));
        }
        to.herdr_ok(
            Step::Launch,
            &["pane", "run", &target.pane_id, &line],
            cancelled,
        )?;
    }

    Ok(Outcome {
        endpoint_id: destination.place.endpoint_id.clone(),
        workspace_id: landing.workspace_id,
        repo_key: repository.key.clone(),
        branch: review.branch.clone(),
        warnings,
    })
}

/// The destination repository, opened as a space: as it is when already
/// open, else after opening an existing checkout or cloning one.
fn arrive(
    source: &Source,
    destination: &Candidate,
    report: &mut impl FnMut(Step),
    cancelled: &AtomicBool,
) -> Result<Repository> {
    let to = &destination.place.host;
    let (path, key) = match &destination.destination {
        Destination::Open { repository, .. } | Destination::Reclaim { repository, .. } => {
            return Ok(repository.clone());
        }
        Destination::Arrive(Arrival::Existing { path, key }) => (path, key.clone()),
        Destination::Arrive(Arrival::Clone { path }) => {
            report(Step::Clone);
            let key = provision::clone(
                &source.place.host,
                &source.repo_key,
                to,
                path,
                destination.origin.as_deref(),
                cancelled,
            )?;
            (path, key)
        }
    };
    report(Step::Open);
    let opened: WorkspaceCreated = to.herdr(
        Step::Open,
        &["workspace", "create", "--cwd", path, "--no-focus"],
        cancelled,
    )?;
    Ok(Repository {
        key,
        label: source.repo_label.clone(),
        workspace_id: opened.workspace.workspace_id,
    })
}

/// A destination pane standing in for a source pane.
#[derive(Debug, Clone)]
struct Target {
    pane_id: String,
    /// Where its program should run, and where its shell started.
    cwd: String,
    created_in: String,
}

/// Where the work lands: a workspace, its checkout, the empty tab the first
/// source tab is rebuilt in, and tabs to close once the rebuild is done.
struct Landing {
    workspace_id: String,
    checkout: String,
    first_tab: String,
    first_pane: String,
    stale_tabs: Vec<String>,
}

/// Put the branch and changes in place: a new worktree, or, coming back, the
/// checkout the work once left (backed up first).
fn land(
    source: &Source,
    destination: &Candidate,
    repository: &Repository,
    review: &Review,
    reference: &str,
    report: &mut impl FnMut(Step),
    cancelled: &AtomicBool,
) -> Result<Landing> {
    let to = &destination.place.host;
    if let Destination::Reclaim { workspace_id, .. } = &destination.destination {
        report(Step::Restore);
        let snapshot: SnapshotResult = to.herdr(Step::Restore, &["api", "snapshot"], cancelled)?;
        let snapshot = snapshot.snapshot;
        let checkout = snapshot
            .workspace(workspace_id)
            .and_then(|workspace| workspace.worktree.as_ref())
            .map(|worktree| worktree.checkout_path.clone())
            .ok_or(Error::WorkspaceGone)?;
        let stale_tabs = snapshot
            .tabs_of(workspace_id)
            .iter()
            .map(|tab| tab.tab_id.clone())
            .collect();
        let backup = reference.replacen("refs/herdr-teleport/", "refs/herdr-teleport/backup/", 1);
        git::reclaim(to, &checkout, reference, &backup, cancelled)?;
        report(Step::Tabs);
        let made: TabCreated = to.herdr(
            Step::Tabs,
            &[
                "tab",
                "create",
                "--workspace",
                workspace_id,
                "--cwd",
                &checkout,
                "--no-focus",
            ],
            cancelled,
        )?;
        return Ok(Landing {
            workspace_id: workspace_id.clone(),
            checkout,
            first_tab: made.tab.tab_id,
            first_pane: made.root_pane.pane_id,
            stale_tabs,
        });
    }
    git::advance_branch(to, &repository.key, &review.branch, reference, cancelled)?;
    report(Step::CreateWorktree);
    let mut args = vec![
        "worktree",
        "create",
        "--workspace",
        &repository.workspace_id,
        "--branch",
        &review.branch,
        "--no-focus",
    ];
    if let Some(label) = &source.custom_label {
        args.extend(["--label", label]);
    }
    let created: WorktreeCreated = to.herdr(Step::CreateWorktree, &args, cancelled)?;
    report(Step::Restore);
    git::restore(to, &created.worktree.path, reference, cancelled)?;
    Ok(Landing {
        workspace_id: created.workspace.workspace_id,
        checkout: created.worktree.path,
        first_tab: created.tab.tab_id,
        first_pane: created.root_pane.pane_id,
        stale_tabs: Vec::new(),
    })
}

/// Stop everything running in the source workspace but keep the workspace:
/// one fresh shell tab replaces its tabs.
fn retire(host: &Host, workspace_id: &str, checkout: &str, cancelled: &AtomicBool) -> Result<()> {
    let tabs: TabList = host.herdr(
        Step::Retire,
        &["tab", "list", "--workspace", workspace_id],
        cancelled,
    )?;
    host.herdr_ok(
        Step::Retire,
        &[
            "tab",
            "create",
            "--workspace",
            workspace_id,
            "--cwd",
            checkout,
            "--label",
            "teleported",
            "--no-focus",
        ],
        cancelled,
    )?;
    for tab in tabs.tabs {
        host.herdr_ok(Step::Retire, &["tab", "close", &tab.tab_id], cancelled)?;
    }
    Ok(())
}

/// Rebuild the source tabs in `landing`, then close the tabs it replaces.
/// Returns one target per pane in `review.panes()` order.
fn rebuild_tabs(
    to: &Host,
    landing: &Landing,
    review: &Review,
    report: &mut impl FnMut(Step),
    cancelled: &AtomicBool,
) -> Result<Vec<Target>> {
    report(Step::Tabs);
    let checkout = &landing.checkout;
    let cwd_of = |plan: Option<&PanePlan>| {
        plan.and_then(|plan| plan.cwd.as_deref())
            .and_then(|cwd| remap(cwd, &review.checkout, checkout))
            .unwrap_or_else(|| checkout.clone())
    };
    let mut targets: HashMap<String, Target> = HashMap::new();
    for (index, tab) in review.tabs.iter().enumerate() {
        let build = tab.tree.plan();
        let first_cwd = cwd_of(build.slots.first().and_then(|id| tab.pane(id)));
        let (tab_id, root, root_cwd) = if index == 0 {
            (
                landing.first_tab.clone(),
                landing.first_pane.clone(),
                checkout.clone(),
            )
        } else {
            let made: TabCreated = to.herdr(
                Step::Tabs,
                &[
                    "tab",
                    "create",
                    "--workspace",
                    &landing.workspace_id,
                    "--cwd",
                    &first_cwd,
                    "--no-focus",
                ],
                cancelled,
            )?;
            (made.tab.tab_id, made.root_pane.pane_id, first_cwd.clone())
        };
        if let Some(label) = &tab.label {
            to.herdr_ok(Step::Tabs, &["tab", "rename", &tab_id, label], cancelled)?;
        }
        let mut slots = vec![(root, root_cwd)];
        for step in &build.steps {
            let cwd = cwd_of(build.slots.get(step.created).and_then(|id| tab.pane(id)));
            let ratio = format!("{:.4}", step.ratio);
            let target = &slots[step.target].0;
            let made: PaneInfoResult = to.herdr(
                Step::Tabs,
                &[
                    "pane",
                    "split",
                    target,
                    "--direction",
                    step.direction.as_str(),
                    "--ratio",
                    &ratio,
                    "--cwd",
                    &cwd,
                    "--no-focus",
                ],
                cancelled,
            )?;
            slots.push((made.pane.pane_id, cwd));
        }
        for (source_pane, (pane_id, created_in)) in build.slots.iter().zip(slots) {
            targets.insert(
                source_pane.clone(),
                Target {
                    pane_id,
                    cwd: cwd_of(tab.pane(source_pane)),
                    created_in,
                },
            );
        }
    }
    for tab in &landing.stale_tabs {
        to.herdr_ok(Step::Tabs, &["tab", "close", tab], cancelled)?;
    }
    review
        .panes()
        .map(|plan| {
            targets
                .get(&plan.pane_id)
                .cloned()
                .ok_or_else(|| Error::Decode {
                    step: Step::Tabs,
                    source: serde::de::Error::custom("a pane was not recreated"),
                })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::teleport::snapshot::{AgentSession, SessionKind};

    fn installed(programs: &[&str]) -> Vec<String> {
        programs.iter().map(|p| (*p).to_owned()).collect()
    }

    fn session(agent: AgentKind) -> Work {
        Work::Session {
            agent,
            session: AgentSession {
                agent: agent.binary().into(),
                kind: SessionKind::Id,
                source: format!("herdr:{}", agent.binary()),
                value: "id".into(),
            },
            flags: vec![],
        }
    }

    #[test]
    fn actions_resume_when_possible_and_hand_off_otherwise() {
        let note = || "note".to_owned();
        assert_eq!(
            plan_action(
                &session(AgentKind::Opencode),
                &installed(&["opencode"]),
                note
            ),
            Action::Resume(AgentKind::Opencode)
        );
        // The destination lacks opencode, so the first available agent takes over.
        assert_eq!(
            plan_action(
                &session(AgentKind::Opencode),
                &installed(&["codex", "pi"]),
                note
            ),
            Action::Handoff {
                note: "note".into(),
                to: AgentKind::Codex
            }
        );
        assert_eq!(
            plan_action(&session(AgentKind::Claude), &installed(&[]), note),
            Action::Missing("claude".into())
        );
        let sessionless = Work::Agent {
            name: "claude".into(),
            argv: vec!["claude".into()],
        };
        assert_eq!(
            plan_action(&sessionless, &installed(&["claude"]), note),
            Action::Handoff {
                note: "note".into(),
                to: AgentKind::Claude
            }
        );
        let unknown = Work::Agent {
            name: "gemini".into(),
            argv: vec!["gemini".into(), "-y".into()],
        };
        assert_eq!(
            plan_action(&unknown, &installed(&["gemini"]), note),
            Action::Start(vec!["gemini".into(), "-y".into()])
        );
        assert_eq!(
            plan_action(&Work::Command(vec!["make".into()]), &installed(&[]), note),
            Action::Run(vec!["make".into()])
        );
        assert_eq!(
            plan_action(&Work::Shell, &installed(&[]), note),
            Action::Shell
        );
    }

    #[test]
    fn notes_are_numbered_only_when_handed_off() {
        let notes = std::cell::Cell::new(0);
        let next = || {
            let path = handoff_path(notes.get());
            notes.set(notes.get() + 1);
            path
        };
        assert_eq!(
            plan_action(&session(AgentKind::Claude), &installed(&["claude"]), next),
            Action::Resume(AgentKind::Claude)
        );
        assert_eq!(notes.get(), 0);
        assert!(matches!(
            plan_action(&session(AgentKind::Pi), &installed(&["claude"]), next),
            Action::Handoff { note, .. } if note == ".herdr/teleport/handoff-1.md"
        ));
        assert_eq!(notes.get(), 1);
    }
}
