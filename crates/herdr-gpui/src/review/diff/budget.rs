//! Keeping a review's diff within bounds before Git writes it. Git's output
//! is read whole and capped, so one generated or vendored file could make
//! the review fail. `git diff --numstat` is small whatever the change, so
//! the files too large to show are known first, left out of the diff, and
//! listed as not shown.

/// Changed lines one file may bring before it is left out.
const MAX_FILE_LINES: u64 = 5_000;
/// A working-tree file larger than this is left out, whatever its line
/// count: one minified line can be megabytes.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Changed lines the whole diff may hold; later files are left out.
const MAX_TOTAL_LINES: u64 = 15_000;
/// Files left out by name at most, so the command line stays bounded.
const MAX_SKIPPED: usize = 500;

/// One file's changed line counts; `None` for a binary file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Counted {
    pub path: String,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

impl Counted {
    fn lines(&self) -> u64 {
        self.added
            .unwrap_or(0)
            .saturating_add(self.deleted.unwrap_or(0))
    }
}

/// Parses `git diff --numstat -z`: `added\tdeleted\tpath\0`, or for a
/// rename `added\tdeleted\t\0old\0new\0`, counted under the new path.
pub(super) fn parse_numstat(text: &str) -> Vec<Counted> {
    let mut counted = Vec::new();
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            // A rename names both sides in the next two fields.
            let _old = fields.next();
            match fields.next() {
                Some(new) => new.to_owned(),
                None => break,
            }
        } else {
            path.to_owned()
        };
        counted.push(Counted {
            path,
            added: added.parse().ok(),
            deleted: deleted.parse().ok(),
        });
    }
    counted
}

/// The files to leave out of the diff, in order: any too large on its own,
/// by lines or by its size on disk (`size`), and every file once the diff
/// holds as many lines as it may.
pub(super) fn too_large(counted: &[Counted], size: impl Fn(&str) -> Option<u64>) -> Vec<&Counted> {
    let mut total = 0_u64;
    let mut skipped = Vec::new();
    for file in counted {
        let lines = file.lines();
        let oversized = lines > MAX_FILE_LINES
            || size(&file.path).is_some_and(|bytes| bytes > MAX_FILE_BYTES)
            || total.saturating_add(lines) > MAX_TOTAL_LINES;
        if oversized {
            if skipped.len() == MAX_SKIPPED {
                break;
            }
            skipped.push(file);
        } else {
            total += lines;
        }
    }
    skipped
}

/// The pathspec leaving `path` out, matched literally from the top of the
/// repository, so no name can act as a pattern or an option.
pub(super) fn excluded(path: &str) -> String {
    format!(":(top,literal,exclude){path}")
}
