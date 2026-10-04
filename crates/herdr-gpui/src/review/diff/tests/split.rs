//! Pairing a diff's rows for the side-by-side view.
use super::*;

#[test]
fn removed_lines_sit_beside_the_lines_that_replaced_them() {
    let diff = Diff::parse(
        "diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,6 +1,6 @@
 keep
-old one
-old two
-old three
+new one
+new two
 middle
+only added
-only removed
\\ No newline at end of file
",
    );
    let text = |index: Option<usize>| index.map(|index| diff.rows[index].text.as_str());
    let split: Vec<_> = diff
        .split_rows()
        .into_iter()
        .map(|row| match row {
            SplitRow::Across(index) => (Some(diff.rows[index].text.as_str()), None, true),
            SplitRow::Sides { left, right } => (text(left), text(right), false),
        })
        .collect();
    assert_eq!(
        split,
        [
            (Some(""), None, true),
            (Some("@@ -1,6 +1,6 @@"), None, true),
            (Some("keep"), Some("keep"), false),
            (Some("old one"), Some("new one"), false),
            (Some("old two"), Some("new two"), false),
            (Some("old three"), None, false),
            (Some("middle"), Some("middle"), false),
            // An addition is not paired with a removal that follows it.
            (None, Some("only added"), false),
            (Some("only removed"), None, false),
            (Some("\\ No newline at end of file"), None, true),
        ]
    );
    // Every row appears, and an unchanged line is one row on both sides.
    let mut seen: Vec<usize> = diff
        .split_rows()
        .into_iter()
        .flat_map(|row| match row {
            SplitRow::Across(index) => vec![index],
            SplitRow::Sides { left, right } if left == right => left.into_iter().collect(),
            SplitRow::Sides { left, right } => left.into_iter().chain(right).collect(),
        })
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..diff.rows.len()).collect::<Vec<_>>());
}
