//! The working tree's changes as rows to review: `git diff` against HEAD, or
//! against where the branch left its base, parsed into files, hunks and
//! numbered lines, plus untracked files shown as wholly added. File contents are untrusted: every row is stripped of control and
//! direction-override characters and bounded before it is drawn or quoted.
use crate::{notifications::safe_text, pull_request::Input};

mod budget;
use std::{
    collections::HashMap,
    io::Read as _,
    path::{Component, Path},
    time::{Duration, Instant},
};

/// Rows kept at most; a larger change is cut short and says so.
const MAX_ROWS: usize = 20_000;
/// Characters kept of one row.
const MAX_ROW_CHARS: usize = 400;
/// Untracked files read at most, and how much of each.
const MAX_UNTRACKED: usize = 64;
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024;
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A file's header; its text says how the file changed, if not edited.
    File,
    Hunk,
    Context,
    Added,
    Removed,
    /// Git's remarks, such as a binary file or a missing final newline.
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub kind: Kind,
    /// Index into [`Diff::files`].
    pub file: usize,
    /// The line's number before the change, for context and removed lines.
    pub old: Option<u32>,
    /// The line's number after the change, for context and added lines.
    pub new: Option<u32>,
    pub text: String,
    /// Syntax colouring of `text`, once worked out; empty otherwise.
    pub spans: Vec<super::highlight::Span>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Diff {
    /// Changed files, relative to the checkout.
    pub files: Vec<String>,
    pub rows: Vec<Row>,
    /// Rows were left out to stay within bounds.
    pub truncated: bool,
    /// The revision removed lines are numbered in, as a prompt names it.
    pub before: String,
}

/// One line of the side-by-side view, as indices into [`Diff::rows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitRow {
    /// A file header, hunk header or remark, across both sides.
    Across(usize),
    /// A removed line beside the added line that replaced it; an unchanged
    /// line is the same row on both sides.
    Sides {
        left: Option<usize>,
        right: Option<usize>,
    },
}

/// Which changes a review shows. Either way the diff ends at the working
/// tree, so added and unchanged lines carry the numbers the files have now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    /// What is not committed yet: against HEAD.
    #[default]
    Uncommitted,
    /// Everything the branch adds, as its pull request will: against the
    /// merge base with the base branch, uncommitted work included.
    Branch,
}

/// Which version of a file a line number counts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Added,
    Removed,
    Unchanged,
}

/// What a review note is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    File {
        path: String,
    },
    Line {
        path: String,
        side: Side,
        number: u32,
        code: String,
        /// For a removed line, the revision its number counts in.
        before: Option<String>,
    },
}

/// One row of untrusted text: tabs become spaces, controls go, and it is
/// bounded.
fn clean(text: &str) -> String {
    let spaced = text.replace('\t', "    ");
    let text = safe_text(&spaced, MAX_ROW_CHARS * 4);
    match text.char_indices().nth(MAX_ROW_CHARS) {
        Some((end, _)) => format!("{}\u{2026}", &text[..end]),
        None => text,
    }
}

/// A path from a `---`/`+++` line or the `diff --git` header, without its
/// `a/`/`b/` prefix or Git's quoting.
fn path(text: &str, prefix: &str) -> String {
    let text = text.trim_end_matches('\r');
    let text = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text);
    clean(text.strip_prefix(prefix).unwrap_or(text))
}

/// The starts of `@@ -a,b +c,d @@`.
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.strip_prefix("@@ ")?.split(' ');
    let start = |part: Option<&str>, sign: char| -> Option<u32> {
        part?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    Some((start(parts.next(), '-')?, start(parts.next(), '+')?))
}

impl Diff {
    fn push(&mut self, kind: Kind, old: Option<u32>, new: Option<u32>, text: &str) -> bool {
        if self.rows.len() >= MAX_ROWS {
            self.truncated = true;
            return false;
        }
        self.rows.push(Row {
            kind,
            file: self.files.len().saturating_sub(1),
            old,
            new,
            text: clean(text),
            spans: Vec::new(),
        });
        true
    }

    fn start_file(&mut self, name: String) -> bool {
        if self.rows.len() >= MAX_ROWS {
            self.truncated = true;
            return false;
        }
        self.files.push(name);
        self.push(Kind::File, None, None, "")
    }

    /// The last file header's text, which says how the file changed.
    fn mark_file(&mut self, status: &str) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .rev()
            .find(|row| row.kind == Kind::File)
        {
            row.text = status.to_owned();
        }
    }

    /// Parses `git diff` output made with `a/` and `b/` prefixes.
    pub(crate) fn parse(text: &str) -> Self {
        let mut diff = Self::default();
        let mut hunk: Option<(u32, u32)> = None;
        for raw in text.split('\n') {
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if diff.truncated {
                break;
            }
            // Inside a hunk every content line starts with a space, `+`,
            // `-` or `\`, so a header line can only start a new file.
            if let Some(header) = raw.strip_prefix("diff --git ") {
                hunk = None;
                let name = header
                    .rfind(" b/")
                    .map_or(header, |index| &header[index + 1..]);
                diff.start_file(path(name, "b/"));
                continue;
            }
            if diff.files.is_empty() {
                continue;
            }
            if raw.starts_with("@@ ") {
                hunk = hunk_starts(raw);
                diff.push(Kind::Hunk, None, None, raw);
                continue;
            }
            let Some((old, new)) = hunk.as_mut() else {
                if let Some(name) = raw.strip_prefix("+++ ") {
                    if name != "/dev/null"
                        && let Some(file) = diff.files.last_mut()
                    {
                        *file = path(name, "b/");
                    }
                } else if raw.starts_with("new file mode") {
                    diff.mark_file("new");
                } else if raw.starts_with("deleted file mode") {
                    diff.mark_file("deleted");
                } else if raw.starts_with("rename from") {
                    diff.mark_file("renamed");
                } else if raw.starts_with("Binary files") {
                    diff.push(Kind::Meta, None, None, "Binary file not shown");
                }
                continue;
            };
            let (kind, text) = match raw.chars().next() {
                Some(' ') => (Kind::Context, &raw[1..]),
                Some('+') => (Kind::Added, &raw[1..]),
                Some('-') => (Kind::Removed, &raw[1..]),
                Some('\\') => (Kind::Meta, raw),
                _ => continue,
            };
            let numbers = match kind {
                Kind::Context => (Some(*old), Some(*new)),
                Kind::Added => (None, Some(*new)),
                Kind::Removed => (Some(*old), None),
                _ => (None, None),
            };
            if diff.push(kind, numbers.0, numbers.1, text) {
                if numbers.0.is_some() {
                    *old = old.saturating_add(1);
                }
                if numbers.1.is_some() {
                    *new = new.saturating_add(1);
                }
            }
        }
        diff
    }

    /// Adds a file Git does not track yet, every line of it added. `None`
    /// contents stand for a file that cannot be shown as text.
    pub(crate) fn add_untracked(&mut self, name: &str, contents: Option<&str>) {
        if !self.start_file(clean(name)) {
            return;
        }
        self.mark_file("untracked");
        let Some(contents) = contents else {
            self.push(Kind::Meta, None, None, "Binary or large file not shown");
            return;
        };
        let lines: Vec<&str> = contents.lines().collect();
        self.push(
            Kind::Hunk,
            None,
            None,
            &format!("@@ -0,0 +1,{} @@", lines.len()),
        );
        for (index, line) in lines.into_iter().enumerate() {
            let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            if !self.push(Kind::Added, None, Some(number), line) {
                return;
            }
        }
    }

    /// Adds a changed file left out of the diff for its size, so it is still
    /// listed and can take a note as a whole.
    pub(crate) fn add_skipped(&mut self, name: &str, added: Option<u64>, deleted: Option<u64>) {
        if !self.start_file(clean(name)) {
            return;
        }
        self.mark_file("too large to show");
        let remark = match (added, deleted) {
            (Some(added), Some(deleted)) => {
                format!("Large change not shown: +{added} \u{2212}{deleted} lines")
            }
            _ => "Large change not shown".to_owned(),
        };
        self.push(Kind::Meta, None, None, &remark);
    }

    /// What a note on row `index` is about; hunks and remarks take none.
    pub(crate) fn anchor(&self, index: usize) -> Option<Anchor> {
        let row = self.rows.get(index)?;
        let path = self.files.get(row.file)?.clone();
        let (side, number) = match row.kind {
            Kind::File => return Some(Anchor::File { path }),
            Kind::Added => (Side::Added, row.new?),
            Kind::Removed => (Side::Removed, row.old?),
            Kind::Context => (Side::Unchanged, row.new?),
            Kind::Hunk | Kind::Meta => return None,
        };
        Some(Anchor::Line {
            path,
            side,
            number,
            code: row.text.clone(),
            before: (side == Side::Removed).then(|| self.before.clone()),
        })
    }

    /// The rows side by side: each run of removed lines is paired, in order,
    /// with the added lines that follow it, and the longer side runs on alone.
    pub(crate) fn split_rows(&self) -> Vec<SplitRow> {
        let mut split = Vec::with_capacity(self.rows.len());
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        let flush =
            |split: &mut Vec<SplitRow>, removed: &mut Vec<usize>, added: &mut Vec<usize>| {
                for index in 0..removed.len().max(added.len()) {
                    split.push(SplitRow::Sides {
                        left: removed.get(index).copied(),
                        right: added.get(index).copied(),
                    });
                }
                removed.clear();
                added.clear();
            };
        for (index, row) in self.rows.iter().enumerate() {
            match row.kind {
                // Removals after additions start a new pairing.
                Kind::Removed if !added.is_empty() => {
                    flush(&mut split, &mut removed, &mut added);
                    removed.push(index);
                }
                Kind::Removed => removed.push(index),
                Kind::Added => added.push(index),
                Kind::Context => {
                    flush(&mut split, &mut removed, &mut added);
                    split.push(SplitRow::Sides {
                        left: Some(index),
                        right: Some(index),
                    });
                }
                Kind::File | Kind::Hunk | Kind::Meta => {
                    flush(&mut split, &mut removed, &mut added);
                    split.push(SplitRow::Across(index));
                }
            }
        }
        flush(&mut split, &mut removed, &mut added);
        split
    }

    /// The row a note with `anchor` belongs on, to mark it there.
    /// Built once per load: marking a review's notes is then a lookup
    /// per note rather than a pass over every row.
    pub(crate) fn index(&self) -> RowIndex {
        let mut index = RowIndex::default();
        for (number, name) in self.files.iter().enumerate() {
            index.files.entry(name.clone()).or_insert(number);
        }
        for (row, line) in self.rows.iter().enumerate() {
            let key = match line.kind {
                Kind::File => {
                    index.headers.entry(line.file).or_insert(row);
                    continue;
                }
                Kind::Added => line.new.map(|number| (Side::Added, number)),
                Kind::Removed => line.old.map(|number| (Side::Removed, number)),
                Kind::Context => line.new.map(|number| (Side::Unchanged, number)),
                Kind::Hunk | Kind::Meta => None,
            };
            if let Some((side, number)) = key {
                index.lines.entry((line.file, side, number)).or_insert(row);
            }
        }
        index
    }

    /// The row a note with `anchor` belongs on, to mark it there: the one
    /// `index` names, if it still quotes the same line.
    pub(crate) fn row_of(&self, index: &RowIndex, anchor: &Anchor) -> Option<usize> {
        let row = match anchor {
            Anchor::File { path } => *index.headers.get(index.files.get(path)?)?,
            Anchor::Line {
                path, side, number, ..
            } => *index
                .lines
                .get(&(*index.files.get(path)?, *side, *number))?,
        };
        (self.anchor(row).as_ref() == Some(anchor)).then_some(row)
    }
}

/// Where each file header and numbered line of a [`Diff`] is.
#[derive(Debug, Default)]
pub(crate) struct RowIndex {
    files: HashMap<String, usize>,
    headers: HashMap<usize, usize>,
    lines: HashMap<(usize, Side, u32), usize>,
}

/// The changes in a checkout, where it is, and what they were taken against.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub checkout: String,
    pub scope: Scope,
    /// The ref the branch is compared with, for `Scope::Branch`.
    pub base: Option<String>,
    pub diff: Diff,
}

/// A branch name a ref can be built from: nothing Git would read as an
/// option, a range, or a pattern. Pull request data is remote text.
fn plain_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(['-', '/', '.'])
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\@{".contains(c))
}

/// Refs the base may be, most specific first: the pull request's base, the
/// remote's default branch, then the usual names. Full ref names, so none can
/// be taken for an option.
fn base_candidates(hint: Option<&str>, remote_head: Option<&str>) -> Vec<String> {
    let mut names: Vec<&str> = hint.into_iter().filter(|name| plain_branch(name)).collect();
    if let Some(head) = remote_head
        .and_then(|head| head.strip_prefix("refs/remotes/origin/"))
        .filter(|name| plain_branch(name))
    {
        names.push(head);
    }
    names.extend(["main", "master"]);
    let mut refs = Vec::new();
    for name in names {
        for candidate in [
            format!("refs/remotes/origin/{name}"),
            format!("refs/heads/{name}"),
        ] {
            if !refs.contains(&candidate) {
                refs.push(candidate);
            }
        }
    }
    refs
}

/// The first candidate that exists, shortened for display, and where HEAD
/// left it. Reads local refs only; nothing is fetched.
fn branch_base(
    checkout: &str,
    hint: Option<&str>,
    deadline: Instant,
) -> crate::Result<(String, String)> {
    let never = || false;
    let remote_head = crate::git::git(
        checkout,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        "read the remote's default branch",
        deadline,
        &never,
    )
    .ok();
    for candidate in base_candidates(hint, remote_head.as_deref()) {
        let merge_base = crate::git::git(
            checkout,
            &["merge-base", "HEAD", &candidate],
            "find where the branch started",
            deadline,
            &never,
        );
        if let Ok(commit) = merge_base {
            let label = candidate
                .strip_prefix("refs/remotes/")
                .or_else(|| candidate.strip_prefix("refs/heads/"))
                .unwrap_or(&candidate)
                .to_owned();
            return Ok((label, commit));
        }
    }
    Err(crate::Error::ReviewNoBase)
}

/// Whether `name`, as `git ls-files` printed it, stays inside the checkout.
fn inside(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// An untracked file's text, or `None` when it is not a small regular text
/// file. A link is never followed out of the checkout.
fn untracked_text(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_UNTRACKED_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_UNTRACKED_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Git's output outgrew what is read: say so in the review's terms.
fn too_large(error: crate::Error) -> crate::Error {
    match error {
        crate::Error::PrSize => crate::Error::ReviewTooLarge,
        error => error,
    }
}

/// Reads the focused checkout's changes in `scope`; `base_hint` names the
/// pull request's base branch when one is known. Blocking: it runs Git and
/// reads files, so it belongs on a background thread.
pub(crate) fn load(input: &Input, scope: Scope, base_hint: Option<&str>) -> crate::Result<Loaded> {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    let never = || false;
    let checkout = crate::pull_request::local_checkout(input, deadline, &never)?;
    let (base, revision, before) = match scope {
        Scope::Uncommitted => (None, "HEAD".to_owned(), "HEAD".to_owned()),
        Scope::Branch => {
            let (label, commit) = branch_base(&checkout, base_hint, deadline)?;
            let short: String = commit.chars().take(7).collect();
            let before = format!("{label} at {short}");
            (Some(label), commit, before)
        }
    };
    // Line counts first: they are small whatever the change, and say which
    // files would make the diff itself too large to read.
    let numstat = crate::git::git(
        &checkout,
        &[
            "diff",
            "--numstat",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            &revision,
            "--",
        ],
        "count working tree changes",
        deadline,
        &never,
    )
    .map_err(too_large)?;
    let counted = budget::parse_numstat(&numstat);
    let root = Path::new(&checkout);
    let skipped = budget::too_large(&counted, |path| {
        inside(path)
            .then(|| std::fs::symlink_metadata(root.join(path)).ok())
            .flatten()
            .filter(std::fs::Metadata::is_file)
            .map(|metadata| metadata.len())
    });
    // Explicit prefixes and no external tools, whatever the user configured.
    let mut args = vec![
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "--src-prefix=a/".to_owned(),
        "--dst-prefix=b/".to_owned(),
        revision,
        "--".to_owned(),
    ];
    if !skipped.is_empty() {
        args.push(":(top)".to_owned());
        args.extend(skipped.iter().map(|file| budget::excluded(&file.path)));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let text = crate::git::git(
        &checkout,
        &args,
        "read working tree changes",
        deadline,
        &never,
    )
    .map_err(too_large)?;
    let mut diff = Diff::parse(&text);
    diff.before = before;
    for file in skipped {
        diff.add_skipped(&file.path, file.added, file.deleted);
    }
    let untracked = crate::git::git(
        &checkout,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "list untracked files",
        deadline,
        &never,
    )?;
    for name in untracked
        .split('\0')
        .filter(|name| inside(name))
        .take(MAX_UNTRACKED)
    {
        let contents = untracked_text(&Path::new(&checkout).join(name));
        diff.add_untracked(name, contents.as_deref());
    }
    Ok(Loaded {
        checkout,
        scope,
        base,
        diff,
    })
}

#[cfg(test)]
mod tests;
