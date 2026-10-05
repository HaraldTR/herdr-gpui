//! A review lives in a tab: opened once per checkout, restored with the
//! other tabs, and gone when its tab closes.
use super::window;
use crate::browser::{Location, ReviewCheckout, Store, scope};

fn checkout() -> ReviewCheckout {
    ReviewCheckout {
        repo_key: "/work/repo/.git".into(),
        branch: "feature".into(),
        checkout: Some("/work/repo".into()),
    }
}

#[gpui::test]
fn a_restored_review_tab_gets_its_state_and_a_closed_one_loses_it(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    // A review tab as the last run saved it: in the store, with no state.
    let id = cx.update(|_, cx| {
        let tab_scope = scope(&view.read(cx).endpoints[0]);
        Store::update(cx, |store| {
            store.open(
                tab_scope,
                "w0",
                Some(Location::Review {
                    checkout: checkout(),
                }),
                None,
            )
        })
        .unwrap()
    });
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            assert!(view.reviews.is_empty());
            view.poll_reviews(cx);
            // Made and reading; the next tick does not make another.
            assert_eq!(view.reviews.len(), 1);
            let request = view.reviews[&id].request;
            assert!(request > 0, "a read started");
            view.poll_reviews(cx);
            assert_eq!(view.reviews[&id].request, request);
        })
    });
    cx.update(|_, cx| {
        Store::update(cx, |store| store.close(id));
        view.update(cx, |view, cx| {
            view.poll_reviews(cx);
            assert!(view.reviews.is_empty());
        })
    });
}

#[test]
fn saved_review_tabs_are_checked_when_read() {
    let good =
        r#"{"kind":"review","checkout":{"repo_key":"/r/.git","branch":"main","checkout":"/r"}}"#;
    let location: Location = serde_json::from_str(good).unwrap();
    assert!(!location.is_page());
    assert_eq!(location.default_title(), "Review \u{00b7} main");
    for bad in [
        r#"{"kind":"review","checkout":{"repo_key":"relative","branch":"main","checkout":null}}"#,
        r#"{"kind":"review","checkout":{"repo_key":"/r/.git","branch":"","checkout":null}}"#,
        r#"{"kind":"review","checkout":{"repo_key":"/r/.git","branch":"a\nb","checkout":null}}"#,
        r#"{"kind":"review","checkout":{"repo_key":"/r/.git","branch":"main","checkout":"r"}}"#,
    ] {
        assert!(serde_json::from_str::<Location>(bad).is_err(), "{bad}");
    }
}
