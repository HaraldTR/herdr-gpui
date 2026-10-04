//! The working tree's changes as rows to review: `git diff HEAD` parsed into
//! files, hunks and numbered lines, plus untracked files shown as wholly
//! added. File contents are untrusted: every row is stripped of control and
//! direction-override characters and bounded before it is drawn or quoted.
use crate::{notifications::safe_text, pull_request::Input};
use std::{
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Diff {
    /// Changed files, relative to the checkout.
    pub files: Vec<String>,
    pub rows: Vec<Row>,
    /// Rows were left out to stay within bounds.
    pub truncated: bool,
}

/// Which version of a file a line number counts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        })
    }

    /// The row a note with `anchor` belongs on, to mark it there.
    pub(crate) fn row_of(&self, anchor: &Anchor) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.anchor(*index).as_ref() == Some(anchor))
    }
}

/// The changes in a checkout, and where it is.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub checkout: String,
    pub diff: Diff,
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

/// Reads the focused checkout's uncommitted changes. Blocking: it runs Git
/// and reads files, so it belongs on a background thread.
pub(crate) fn load(input: &Input) -> crate::Result<Loaded> {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    let never = || false;
    let checkout = crate::pull_request::local_checkout(input, deadline, &never)?;
    // Explicit prefixes and no external tools, whatever the user configured.
    let text = crate::git::git(
        &checkout,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "HEAD",
            "--",
        ],
        "read working tree changes",
        deadline,
        &never,
    )?;
    let mut diff = Diff::parse(&text);
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
    Ok(Loaded { checkout, diff })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const SAMPLE: &str = "diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,4 +10,5 @@ fn main() {
     let a = 1;
-    let b = 2;
+    let b = 3;
+\tlet c = \u{1b}[31m4;
     done();
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone
\\ No newline at end of file
diff --git a/logo.png b/logo.png
new file mode 100644
Binary files /dev/null and b/logo.png differ
";

    #[test]
    fn rows_are_numbered_per_side_and_cleaned() {
        let diff = Diff::parse(SAMPLE);
        assert_eq!(diff.files, ["src/main.rs", "old.txt", "logo.png"]);
        assert!(!diff.truncated);
        let numbered: Vec<_> = diff
            .rows
            .iter()
            .filter(|row| row.file == 0)
            .map(|row| (row.kind, row.old, row.new, row.text.as_str()))
            .collect();
        assert_eq!(
            numbered,
            [
                (Kind::File, None, None, ""),
                (Kind::Hunk, None, None, "@@ -10,4 +10,5 @@ fn main() {"),
                (Kind::Context, Some(10), Some(10), "    let a = 1;"),
                (Kind::Removed, Some(11), None, "    let b = 2;"),
                (Kind::Added, None, Some(11), "    let b = 3;"),
                // Tabs are spaced and escape sequences lose their control.
                (Kind::Added, None, Some(12), "    let c = [31m4;"),
                (Kind::Context, Some(12), Some(13), "    done();"),
            ]
        );
        let status: Vec<_> = diff
            .rows
            .iter()
            .filter(|row| row.kind == Kind::File)
            .map(|row| row.text.as_str())
            .collect();
        assert_eq!(status, ["", "deleted", "new"]);
        assert!(diff.rows.iter().any(|row| row.kind == Kind::Meta
            && row.file == 1
            && row.text == "\\ No newline at end of file"));
        assert!(
            diff.rows
                .iter()
                .any(|row| row.file == 2 && row.text == "Binary file not shown")
        );
    }

    #[test]
    fn notes_anchor_to_a_line_on_its_own_side_or_a_whole_file() {
        let diff = Diff::parse(SAMPLE);
        let anchor = |text: &str| {
            let index = diff.rows.iter().position(|row| row.text == text).unwrap();
            diff.anchor(index)
        };
        assert_eq!(
            anchor("    let b = 2;"),
            Some(Anchor::Line {
                path: "src/main.rs".into(),
                side: Side::Removed,
                number: 11,
                code: "    let b = 2;".into(),
            })
        );
        assert_eq!(
            anchor("    done();"),
            Some(Anchor::Line {
                path: "src/main.rs".into(),
                side: Side::Unchanged,
                number: 13,
                code: "    done();".into(),
            })
        );
        assert_eq!(anchor("@@ -10,4 +10,5 @@ fn main() {"), None);
        assert_eq!(
            diff.anchor(0),
            Some(Anchor::File {
                path: "src/main.rs".into()
            })
        );
        let line = diff.anchor(4).unwrap();
        assert_eq!(diff.row_of(&line), Some(4));
        assert_eq!(diff.anchor(diff.rows.len()), None);
    }

    #[test]
    fn untracked_files_are_wholly_added_and_rows_are_bounded() {
        let mut diff = Diff::default();
        diff.add_untracked("notes/todo.md", Some("one\ntwo\n"));
        diff.add_untracked("blob.bin", None);
        assert_eq!(diff.files, ["notes/todo.md", "blob.bin"]);
        let rows: Vec<_> = diff
            .rows
            .iter()
            .map(|row| (row.kind, row.new, row.text.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (Kind::File, None, "untracked"),
                (Kind::Hunk, None, "@@ -0,0 +1,2 @@"),
                (Kind::Added, Some(1), "one"),
                (Kind::Added, Some(2), "two"),
                (Kind::File, None, "untracked"),
                (Kind::Meta, None, "Binary or large file not shown"),
            ]
        );

        let long = "x".repeat(MAX_ROW_CHARS * 2);
        let huge = format!("{long}\n").repeat(MAX_ROWS + 10);
        let mut diff = Diff::default();
        diff.add_untracked("big.txt", Some(&huge));
        assert!(diff.truncated);
        assert_eq!(diff.rows.len(), MAX_ROWS);
        assert_eq!(diff.rows[2].text.chars().count(), MAX_ROW_CHARS + 1);
    }

    #[test]
    fn only_plain_relative_untracked_names_are_read() {
        assert!(inside("src/a.rs"));
        for refused in ["", "../secret", "/etc/passwd", "a/../../b"] {
            assert!(!inside(refused), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn untracked_links_and_binaries_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("b.bin"), b"a\0b").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("link")).unwrap();
        assert_eq!(untracked_text(&dir.path().join("a.txt")).unwrap(), "hello");
        assert!(untracked_text(&dir.path().join("b.bin")).is_none());
        assert!(untracked_text(&dir.path().join("link")).is_none());
        assert!(untracked_text(&dir.path().join("missing")).is_none());
    }
}
