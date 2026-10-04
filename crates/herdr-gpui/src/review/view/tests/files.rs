//! The list of changed files beside the diff.
use super::window;
use crate::review::{
    diff::{Diff, Scope},
    view::{
        Layout, Loaded,
        files::{FileItem, file_items},
    },
};
use gpui::{Modifiers, MouseButton, point, px};

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
}

/// Files in Git's sorted order, each long enough to scroll past.
fn many_files() -> Loaded {
    let mut diff = Diff::parse(
        "diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
 # demo
-old
+new
",
    );
    let long: String = (0..80).map(|line| format!("line {line}\n")).collect();
    for name in ["src/a.rs", "src/b.rs", "tests/c.rs", "z.txt"] {
        diff.add_untracked(name, Some(&long));
    }
    Loaded {
        checkout: "/work/repo".into(),
        scope: Scope::Uncommitted,
        base: None,
        diff,
    }
}

#[test]
fn files_sit_under_their_folders_and_count_their_lines() {
    let loaded = many_files();
    let entries = loaded.diff.file_entries();
    let counts: Vec<_> = entries
        .iter()
        .map(|entry| (entry.added, entry.removed, entry.status.as_str()))
        .collect();
    assert_eq!(
        counts,
        [
            (1, 1, ""),
            (80, 0, "untracked"),
            (80, 0, "untracked"),
            (80, 0, "untracked"),
            (80, 0, "untracked"),
        ]
    );
    assert_eq!(
        file_items(&loaded.diff, &entries),
        [
            FileItem::File(0),
            FileItem::Folder("src".into()),
            FileItem::File(1),
            FileItem::File(2),
            FileItem::Folder("tests".into()),
            FileItem::File(3),
            // A root file after a folder gets the root's own heading.
            FileItem::Folder("/".into()),
            FileItem::File(4),
        ]
    );
}

#[gpui::test]
fn clicking_a_file_brings_it_to_the_top_in_either_layout(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(many_files(), cx)));
    draw(cx);
    draw(cx);
    let top = |cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |view, _| view.menu.review.as_ref().unwrap().top_file())
    };
    assert_eq!(top(cx), Some(0));
    for layout in [Layout::Unified, Layout::Split] {
        cx.update(|_, cx| view.update(cx, |view, cx| view.set_review_layout(layout, cx)));
        draw(cx);
        let file = cx.debug_bounds("review-file-3").unwrap();
        cx.simulate_click(file.center(), Modifiers::default());
        draw(cx);
        draw(cx);
        assert_eq!(top(cx), Some(3), "{layout:?}");
        // The file at the top is marked in the list, and its header shows.
        assert!(cx.debug_bounds("review-row-0").is_none(), "{layout:?}");
        let back = cx.debug_bounds("review-file-0").unwrap();
        cx.simulate_click(back.center(), Modifiers::default());
        draw(cx);
        draw(cx);
        assert_eq!(top(cx), Some(0), "{layout:?}");
    }
}

#[gpui::test]
fn the_file_list_resizes_by_its_right_edge(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(many_files(), cx)));
    draw(cx);
    let list = cx.debug_bounds("review-files").unwrap();
    assert_eq!(list.size.width, px(240.));
    let edge = cx.debug_bounds("review-files-resize").unwrap();
    assert!(
        (edge.right() - list.right()).abs() <= px(1.),
        "{edge:?} {list:?}"
    );
    let start = edge.center();
    let to = point(start.x + px(100.), start.y);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(start.x + px(40.), start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    draw(cx);
    let wider = cx.debug_bounds("review-files").unwrap();
    assert!((wider.size.width - px(340.)).abs() <= px(4.), "{wider:?}");
    view.update(cx, |view, _| {
        assert!(view.review_files_width.chosen().is_some());
        assert!(!view.review_files_width.take_unsaved(), "saved on release");
    });
}
