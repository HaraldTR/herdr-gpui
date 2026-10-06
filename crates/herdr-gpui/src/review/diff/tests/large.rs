//! Large changes: left out of the diff by name, listed as not shown, and
//! never failing the whole review.
use super::super::budget::{Counted, excluded, parse_numstat, too_large};
use super::*;
use std::process::Command;

fn counted(path: &str, added: u64, deleted: u64) -> Counted {
    Counted {
        path: path.into(),
        added: Some(added),
        deleted: Some(deleted),
    }
}

#[test]
fn numstat_names_renames_by_their_new_path_and_binaries_by_none() {
    let text = concat!(
        "3\t1\tsrc/a.rs\0",
        "-\t-\tlogo.png\0",
        "2\t0\t\0old.rs\0new.rs\0",
    );
    assert_eq!(
        parse_numstat(text),
        [
            counted("src/a.rs", 3, 1),
            Counted {
                path: "logo.png".into(),
                added: None,
                deleted: None,
            },
            counted("new.rs", 2, 0),
        ]
    );
    assert!(parse_numstat("").is_empty());
}

#[test]
fn files_too_large_alone_or_past_the_budget_are_left_out() {
    let files = [
        counted("small.rs", 10, 2),
        counted("generated.rs", 6_000, 0),
        counted("minified.js", 1, 1),
        counted("big-a.rs", 2_500, 2_500),
        counted("big-b.rs", 2_500, 2_500),
        counted("big-c.rs", 2_500, 2_500),
        counted("tail.rs", 1, 0),
    ];
    let size = |path: &str| (path == "minified.js").then_some(4 * 1024 * 1024);
    let skipped: Vec<&str> = too_large(&files, size)
        .into_iter()
        .map(|file| file.path.as_str())
        .collect();
    // 12 + 5000 + 5000 lines fit; big-c would pass 15,000, so it is left
    // out, and the small file after it still fits.
    assert_eq!(skipped, ["generated.rs", "minified.js", "big-c.rs"]);
    assert_eq!(excluded("a b/*.rs"), ":(top,literal,exclude)a b/*.rs");
}

#[test]
fn a_huge_file_is_listed_while_the_rest_of_the_diff_shows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap();
    let checkout = path.to_str().unwrap().to_owned();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(["-C", &checkout, "-c", "user.name=Test"])
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(path.join("small.rs"), "one\n").unwrap();
    std::fs::write(path.join("generated.rs"), "").unwrap();
    std::fs::write(path.join("bundle.js"), "").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    std::fs::write(path.join("small.rs"), "one\ntwo\n").unwrap();
    let generated: String = (0..6_000)
        .map(|line| format!("const X{line}: u32 = {line};\n"))
        .collect();
    std::fs::write(path.join("generated.rs"), generated).unwrap();
    std::fs::write(path.join("bundle.js"), "x".repeat(1_500_000)).unwrap();

    let input = Input {
        checkout: Some(checkout.clone()),
        repo_key: path.join(".git").to_str().unwrap().to_owned(),
        branch: "main".into(),
    };
    let loaded = load(&input, Scope::Uncommitted, None).unwrap();
    let diff = &loaded.diff;
    assert!(
        diff.rows
            .iter()
            .any(|row| row.kind == Kind::Added && row.text == "two")
    );
    for name in ["generated.rs", "bundle.js"] {
        let file = diff.files.iter().position(|file| file == name).unwrap();
        let rows: Vec<_> = diff
            .rows
            .iter()
            .filter(|row| row.file == file)
            .map(|row| (row.kind, row.text.as_str()))
            .collect();
        assert_eq!(rows[0], (Kind::File, "too large to show"), "{name}");
        assert!(rows[1].1.starts_with("Large change not shown"), "{name}");
        assert_eq!(rows.len(), 2, "{name} has no lines");
    }
    assert!(diff.rows.len() < 20, "{}", diff.rows.len());
    // A skipped file still takes a note as a whole.
    let header = diff
        .rows
        .iter()
        .position(|row| row.kind == Kind::File && row.text == "too large to show")
        .unwrap();
    assert!(matches!(diff.anchor(header), Some(Anchor::File { .. })));
}
