//! Drawing the review unified or side by side.
use super::{changes, note, window};
use crate::review::view::Layout;

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
}

#[gpui::test]
fn side_by_side_pairs_lines_and_notes_either_side(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(changes(), cx)));
    draw(cx);
    // Unified: the removed line sits above the one that replaced it.
    let removed = cx.debug_bounds("review-row-3").unwrap();
    let added = cx.debug_bounds("review-row-4").unwrap();
    assert!(removed.top() < added.top());

    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.set_review_layout(Layout::Split, cx));
    });
    draw(cx);
    assert!(cx.debug_bounds("review-row-3").is_none());
    let left = cx.debug_bounds("review-left-3").unwrap();
    let right = cx.debug_bounds("review-right-4").unwrap();
    assert_eq!(left.top(), right.top(), "one row, two sides");
    assert!(left.right() <= right.left());
    // Equal halves, give or take the right half's divider.
    assert!((left.size.width - right.size.width).abs() <= gpui::px(1.));
    // An unchanged line shows on both sides, a header across both.
    assert_eq!(
        cx.debug_bounds("review-left-2").unwrap().top(),
        cx.debug_bounds("review-right-2").unwrap().top()
    );
    let header = cx.debug_bounds("review-row-0").unwrap();
    assert!(header.size.width >= left.size.width + right.size.width);

    // Notes work from either side and are marked on the row they name.
    note(&view, cx, 3, "Keep this");
    note(&view, cx, 4, "Implement this");
    draw(cx);
    view.read_with(cx, |view, _| {
        let review = view.menu.review.as_ref().unwrap();
        assert_eq!(review.notes.len(), 2);
        assert_eq!(review.marks.get(&3), Some(&1));
        assert_eq!(review.marks.get(&4), Some(&2));
    });

    // Back to one column, with the notes still queued.
    cx.update(|_, cx| {
        view.update(cx, |view, cx| view.set_review_layout(Layout::Unified, cx));
    });
    draw(cx);
    assert!(cx.debug_bounds("review-row-3").is_some());
    assert!(cx.debug_bounds("review-left-3").is_none());
    view.read_with(cx, |view, _| {
        assert_eq!(view.menu.review.as_ref().unwrap().notes.len(), 2);
    });
}
