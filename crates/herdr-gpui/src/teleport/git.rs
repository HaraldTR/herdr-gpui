//! Git on both ends of a teleport.
//!
//! Uncommitted work travels as two throwaway commits: one holding the staged
//! tree, and a child holding the whole working tree including untracked (not
//! ignored) files. They are built through a temporary index, so the source
//! checkout's real index, branch, and files are never modified. A bundle
//! carries the branch and those commits, minus everything the destination
//! already has; the destination fetches it, and `read-tree` restores the
//! working tree and then the staged index exactly, binary files included.

use super::{
    error::{Error, Result, Step},
    host::{Host, TRANSFER_IDLE, TRANSFER_OUTPUT},
    remote::{RemoteId, parse_remotes},
};
use herdr_client::{ScriptLimits, shell_quote};
use std::{collections::HashMap, fs::File, io, sync::atomic::AtomicBool};

const SEPARATOR: char = '\u{1e}';
/// Destination tips offered as bundle prerequisites; more add no precision.
const MAX_TIPS: usize = 20_000;

const TRANSFER: ScriptLimits = ScriptLimits {
    output: TRANSFER_OUTPUT,
    idle: TRANSFER_IDLE,
};

/// Fetch remotes for each repository, keyed by its Git common directory.
pub(crate) fn remotes(
    host: &Host,
    keys: &[String],
    cancelled: &AtomicBool,
) -> Result<HashMap<String, Vec<(String, RemoteId)>>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let body = keys
        .iter()
        .map(|key| {
            format!(
                "printf '\\036%s\\n' {key}\ngit --git-dir {key} remote -v 2>/dev/null || :\n",
                key = shell_quote(key)
            )
        })
        .collect::<String>();
    let output = host.query(Step::Discover, &body, &[], cancelled)?;
    let output = String::from_utf8_lossy(&output);
    Ok(output
        .split(SEPARATOR)
        .filter_map(|section| {
            let (key, rest) = section.split_once('\n')?;
            keys.iter()
                .any(|k| k == key)
                .then(|| (key.to_owned(), parse_remotes(rest)))
        })
        .collect())
}

/// What the source checkout holds, for the review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceState {
    pub(crate) branch: Option<String>,
    pub(crate) head: String,
    /// Commits reachable from HEAD but from no remote-tracking ref.
    pub(crate) unpushed: u32,
    pub(crate) changed: u32,
    pub(crate) untracked: u32,
}

pub(crate) fn source_state(
    host: &Host,
    checkout: &str,
    cancelled: &AtomicBool,
) -> Result<SourceState> {
    let body = format!(
        r#"cd -- {checkout}
branch=$(git symbolic-ref -q --short HEAD || :)
head=$(git rev-parse HEAD)
unpushed=$(git rev-list --count HEAD --not --remotes 2>/dev/null || echo 0)
git status --porcelain=v1 --untracked-files=all > "${{TMPDIR:-/tmp}}/herdr-teleport-status.$$"
changed=$(wc -l < "${{TMPDIR:-/tmp}}/herdr-teleport-status.$$")
untracked=$(grep -c '^??' "${{TMPDIR:-/tmp}}/herdr-teleport-status.$$" || :)
rm -f "${{TMPDIR:-/tmp}}/herdr-teleport-status.$$"
printf '%s\n%s\n%s\n%s\n%s\n' "$branch" "$head" "$unpushed" "$changed" "$untracked"
"#,
        checkout = shell_quote(checkout)
    );
    let output = host.query(Step::Review, &body, &[], cancelled)?;
    Ok(parse_source_state(&String::from_utf8_lossy(&output)))
}

fn parse_source_state(output: &str) -> SourceState {
    let mut lines = output.lines().map(str::trim);
    let mut next = || lines.next().unwrap_or_default().to_owned();
    let branch = Some(next()).filter(|b| !b.is_empty());
    let head = next();
    let mut number = || next().parse().unwrap_or(0);
    SourceState {
        branch,
        head,
        unpushed: number(),
        changed: number(),
        untracked: number(),
    }
}

/// The destination repository's view of the branch being moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DestinationBranch {
    /// The branch's commit, when the destination already has the branch.
    pub(crate) tip: Option<String>,
    /// A destination checkout that has the branch checked out.
    pub(crate) checked_out: Option<String>,
    /// Commits the destination has, offered as bundle prerequisites.
    pub(crate) tips: Vec<String>,
}

pub(crate) fn destination_branch(
    host: &Host,
    key: &str,
    branch: &str,
    cancelled: &AtomicBool,
) -> Result<DestinationBranch> {
    let body = format!(
        r#"g() {{ git --git-dir {key} "$@"; }}
g rev-parse -q --verify {reference} || :
printf '\036\n'
g worktree list --porcelain
printf '\036\n'
g for-each-ref --format='%(objectname)' refs/heads refs/remotes refs/tags | sort -u | head -n {MAX_TIPS}
"#,
        key = shell_quote(key),
        reference = shell_quote(&format!("refs/heads/{branch}^{{commit}}")),
    );
    let output = host.query(Step::Review, &body, &[], cancelled)?;
    Ok(parse_destination_branch(
        &String::from_utf8_lossy(&output),
        branch,
    ))
}

fn parse_destination_branch(output: &str, branch: &str) -> DestinationBranch {
    let mut sections = output.split(SEPARATOR);
    let tip = sections
        .next()
        .map(str::trim)
        .filter(|tip| is_object_id(tip))
        .map(str::to_owned);
    let worktrees = sections.next().unwrap_or_default();
    let wanted = format!("branch refs/heads/{branch}");
    let mut path = None;
    let mut checked_out = None;
    for line in worktrees.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_owned());
        } else if line == wanted {
            checked_out.clone_from(&path);
        }
    }
    let tips = sections
        .next()
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|tip| is_object_id(tip))
        .map(str::to_owned)
        .collect();
    DestinationBranch {
        tip,
        checked_out,
        tips,
    }
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `commit` is HEAD or one of its ancestors on the source, so the
/// destination's branch can fast-forward to the source's.
pub(crate) fn source_contains(
    host: &Host,
    checkout: &str,
    commit: &str,
    cancelled: &AtomicBool,
) -> Result<bool> {
    let body = format!(
        r#"cd -- {checkout}
if git cat-file -e {object} 2>/dev/null && git merge-base --is-ancestor {commit} HEAD; then echo yes; else echo no; fi
"#,
        checkout = shell_quote(checkout),
        object = shell_quote(&format!("{commit}^{{commit}}")),
        commit = shell_quote(commit),
    );
    let output = host.query(Step::Review, &body, &[], cancelled)?;
    Ok(output.starts_with(b"yes"))
}

/// Check the destination can take `branch`, per the review's findings.
pub(crate) fn check_destination(
    source: &Host,
    checkout: &str,
    branch: &str,
    destination: &DestinationBranch,
    cancelled: &AtomicBool,
) -> Result<()> {
    if let Some(path) = &destination.checked_out {
        return Err(Error::BranchCheckedOut {
            branch: branch.to_owned(),
            path: path.clone(),
        });
    }
    if let Some(tip) = &destination.tip
        && !source_contains(source, checkout, tip, cancelled)?
    {
        return Err(Error::BranchDiverged {
            branch: branch.to_owned(),
        });
    }
    Ok(())
}

/// Bundle `branch` and the uncommitted work (as `reference`) into `bundle`,
/// excluding commits among `tips` that the source also has.
pub(crate) fn capture(
    host: &Host,
    checkout: &str,
    branch: &str,
    reference: &str,
    tips: &[String],
    bundle: &mut File,
    cancelled: &AtomicBool,
) -> Result<()> {
    let body = format!(
        r#"cd -- {checkout}
t=$(mktemp -d "${{TMPDIR:-/tmp}}/herdr-teleport.XXXXXXXXXX")
trap 'git update-ref -d {reference} 2>/dev/null || :; rm -rf "$t"' EXIT
cat > "$t/tips"
staged=$(git write-tree)
cp "$(git rev-parse --git-path index)" "$t/index"
GIT_INDEX_FILE="$t/index" git add -A
# Handoff notes travel even where `.herdr` is ignored.
if [ -d .herdr/teleport ]; then GIT_INDEX_FILE="$t/index" git add -f -- .herdr/teleport; fi
all=$(GIT_INDEX_FILE="$t/index" git write-tree)
GIT_AUTHOR_NAME=Herdr GIT_AUTHOR_EMAIL=teleport@herdr.invalid
GIT_COMMITTER_NAME=Herdr GIT_COMMITTER_EMAIL=teleport@herdr.invalid
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
index=$(git commit-tree --no-gpg-sign "$staged" -p HEAD -m 'herdr teleport: staged changes')
work=$(git commit-tree --no-gpg-sign "$all" -p "$index" -m 'herdr teleport: working tree')
git update-ref {reference} "$work"
git cat-file --batch-check='%(objectname) %(objecttype)' < "$t/tips" |
    awk '$2 == "commit" {{ print "^" $1 }}' > "$t/revs"
printf '%s\n' {branch_ref} {reference} >> "$t/revs"
git bundle create -q "$t/bundle" --stdin < "$t/revs" >&2
cat "$t/bundle"
"#,
        checkout = shell_quote(checkout),
        reference = shell_quote(reference),
        branch_ref = shell_quote(&format!("refs/heads/{branch}")),
    );
    let tips = tips.join("\n") + "\n";
    host.run(
        Step::Capture,
        &body,
        tips.as_bytes(),
        bundle,
        TRANSFER,
        cancelled,
    )
    .map(drop)
}

/// Copy a local file to a fresh private directory on `host`; returns the
/// remote file's path.
pub(crate) fn upload(host: &Host, file: File, cancelled: &AtomicBool) -> Result<String> {
    let body = r#"umask 077
d=$(mktemp -d "${TMPDIR:-/tmp}/herdr-teleport.XXXXXXXXXX")
cat > "$d/payload"
printf '%s' "$d/payload"
"#;
    let mut path = Vec::new();
    host.run(Step::Transfer, body, file, &mut path, TRANSFER, cancelled)?;
    let path = String::from_utf8(path).map_err(|_| Error::Script {
        step: Step::Transfer,
        source: herdr_client::Error::ScriptIo(io::ErrorKind::InvalidData.into()),
    })?;
    Ok(path)
}

/// Remove a path returned by [`upload`], with its private directory.
pub(crate) fn discard_upload(host: &Host, path: &str, cancelled: &AtomicBool) -> Result<()> {
    let body = format!(
        "rm -f -- {path}\nrmdir -- \"$(dirname -- {path})\" 2>/dev/null || :\n",
        path = shell_quote(path)
    );
    host.query(Step::Transfer, &body, &[], cancelled).map(drop)
}

/// Fetch the uploaded bundle into the destination repository.
pub(crate) fn fetch(
    host: &Host,
    key: &str,
    bundle: &str,
    branch: &str,
    reference: &str,
    cancelled: &AtomicBool,
) -> Result<()> {
    let branch_ref = format!("refs/heads/{branch}");
    let body = format!(
        "git --git-dir {key} fetch --no-tags --no-write-fetch-head -q {bundle} {branch_spec} {reference_spec}\n",
        key = shell_quote(key),
        bundle = shell_quote(bundle),
        branch_spec = shell_quote(&format!("{branch_ref}:{branch_ref}")),
        reference_spec = shell_quote(&format!("{reference}:{reference}")),
    );
    let sink = io::sink();
    host.run(Step::Fetch, &body, io::empty(), sink, TRANSFER, cancelled)
        .map(drop)
}

/// Restore the uncommitted work in the new destination checkout, then drop
/// the temporary reference.
pub(crate) fn restore(
    host: &Host,
    checkout: &str,
    reference: &str,
    cancelled: &AtomicBool,
) -> Result<()> {
    let body = format!(
        r#"cd -- {checkout}
git read-tree -m -u HEAD {reference}
git read-tree {staged}
git update-ref -d {reference}
git update-index -q --refresh >/dev/null 2>&1 || :
"#,
        checkout = shell_quote(checkout),
        reference = shell_quote(reference),
        staged = shell_quote(&format!("{reference}^")),
    );
    host.query(Step::Restore, &body, &[], cancelled).map(drop)
}

/// Best-effort removal of the temporary reference after a failed move.
pub(crate) fn drop_reference(host: &Host, key: &str, reference: &str, cancelled: &AtomicBool) {
    let body = format!(
        "git --git-dir {} update-ref -d {} 2>/dev/null || :\n",
        shell_quote(key),
        shell_quote(reference)
    );
    let _ = host.query(Step::Restore, &body, &[], cancelled);
}

// Host scripts need /bin/sh; Teleport is not offered on other clients.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "git_tests.rs"]
mod tests;
