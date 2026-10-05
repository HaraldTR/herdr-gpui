use super::*;

#[gpui::test]
fn close_from_the_pane_menu_honors_disabled_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
        view.live.snapshot = Some(Arc::new(snapshot()));
        view
    });
    for confirm in [true, false] {
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.config.confirm_close_pane = confirm;
                v.open_pane_menu("inactive", Point::default(), window, cx);
                v.activate_pane_menu(Action::Close, window, cx);
                assert_eq!(v.menu.page, Some(Page::ConfirmClose));
                // A waiting dialog still equals a fresh capture. Without
                // confirmation the disconnected fixture attempts the close at
                // once and refuses it, as the keyboard path does, so the
                // dialog carries the refusal instead.
                let fresh =
                    CloseConfirmation::capture_pane(v.live.snapshot.as_ref().unwrap(), "inactive");
                assert_eq!(v.menu.close == fresh, confirm);
                v.dismiss_menu(window, cx);
            })
        });
    }
}
