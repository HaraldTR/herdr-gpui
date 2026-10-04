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
