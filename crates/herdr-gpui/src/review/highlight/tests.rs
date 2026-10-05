#![allow(clippy::unwrap_used)]
use super::*;
use core::prelude::v1::test;

fn tokens(diff: &Diff, text: &str) -> Vec<(String, Token)> {
    let row = diff.rows.iter().find(|row| row.text == text).unwrap();
    row.spans
        .iter()
        .map(|span| (row.text[span.start..span.end].to_owned(), span.token))
        .collect()
}

#[test]
fn rust_lines_are_coloured_by_what_they_are_on_their_own_side() {
    let mut diff = Diff::parse(
        "diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 /// Docs.
-fn old() -> u32 { 1 }
+fn new() -> &'static str { \"two\" }
 // done
",
    );
    colour(&mut diff);
    let added = tokens(&diff, "fn new() -> &'static str { \"two\" }");
    assert!(added.contains(&("fn".into(), Token::Keyword)), "{added:?}");
    assert!(
        added.contains(&("new".into(), Token::Function)),
        "{added:?}"
    );
    assert!(
        added
            .iter()
            .any(|(text, token)| text.contains("two") && *token == Token::String),
        "{added:?}"
    );
    let removed = tokens(&diff, "fn old() -> u32 { 1 }");
    assert!(
        removed.contains(&("1".into(), Token::Number)),
        "{removed:?}"
    );
    assert!(
        removed.contains(&("u32".into(), Token::Type)),
        "{removed:?}"
    );
    assert_eq!(
        tokens(&diff, "// done"),
        [("// done".into(), Token::Comment)]
    );
    // Spans never reach past the text, and headers stay plain.
    for row in &diff.rows {
        assert!(
            row.spans
                .iter()
                .all(|span| span.start < span.end && span.end <= row.text.len())
        );
        if matches!(row.kind, Kind::File | Kind::Hunk) {
            assert!(row.spans.is_empty());
        }
    }
}

#[test]
fn a_comment_opened_in_one_hunk_does_not_colour_the_next() {
    let mut diff = Diff::parse(
        "diff --git a/a.c b/a.c
--- a/a.c
+++ b/a.c
@@ -1,1 +1,1 @@
-/* unterminated
@@ -9,1 +9,1 @@
+int x = 1;
",
    );
    colour(&mut diff);
    let next = tokens(&diff, "int x = 1;");
    assert!(next.contains(&("int".into(), Token::Type)), "{next:?}");
    assert!(
        !next.iter().any(|(_, token)| *token == Token::Comment),
        "{next:?}"
    );
}

#[test]
fn files_without_a_known_grammar_stay_plain() {
    let mut diff = Diff::default();
    diff.add_untracked("notes.unknownext", Some("fn main() {}\n"));
    diff.add_untracked("Makefile", Some("all:\n"));
    colour(&mut diff);
    assert!(diff.rows[2].spans.is_empty());
}
