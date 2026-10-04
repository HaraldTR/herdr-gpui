//! Switching a review between uncommitted changes and the whole branch.
use super::{State, changes, note, window};
use crate::review::diff::Scope;
use std::sync::Arc;

#[gpui::test]
fn switching_scope_reloads_and_keeps_queued_notes(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(changes(), cx)));
    // The removed line, then the added one below it.
    note(&view, cx, 3, "Why remove this?");
    note(&view, cx, 4, "Implement this");
    cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
    let switched = cx.update(|window, cx| {
        let switched = view.update(cx, |view, cx| {
            let request = view.menu.review.as_ref().unwrap().request;
            view.set_review_scope(Scope::Branch, cx);
            let review = view.menu.review.as_ref().unwrap();
            assert_eq!(review.scope, Scope::Branch);
            assert!(matches!(review.state, State::Loading));
            assert_eq!(review.notes.len(), 2, "notes survive the switch");
            // Choosing the scope already shown reads nothing again.
            let reloaded = review.request;
            view.set_review_scope(Scope::Branch, cx);
            assert_eq!(view.menu.review.as_ref().unwrap().request, reloaded);
            reloaded > request
        });
        crate::sidebar::layout_tests::full_draw(window, cx).clear(cx);
        switched
    });
    assert!(switched);

    // The branch diff numbers removed lines in its base, so a note on a
    // removed line made against HEAD marks nothing there; added lines keep
    // the working tree's numbers and stay marked.
    cx.update(|_, cx| {
        view.update(cx, |view, _| {
            let mut branch = changes();
            branch.scope = Scope::Branch;
            branch.base = Some("origin/main".into());
            branch.diff.before = "origin/main at 1a2b3c4".into();
            let review = view.menu.review.as_mut().unwrap();
            let request = review.request;
            review.state = State::Loaded(Arc::new(branch));
            review.refresh_marks();
            assert_eq!(review.marks.get(&3), None);
            assert_eq!(review.marks.get(&4), Some(&2));
            assert_eq!(review.request, request);
        });
    });
}
