use super::*;

/// The label column of the first workspace row, after a full draw.
fn name_left(cx: &mut gpui::VisualTestContext, selector: &'static str) -> Pixels {
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .left()
}

fn remote_endpoint(view: &HerdrWindow) -> crate::endpoint::Endpoint {
    let mut remote = crate::endpoint::Endpoint::new(
        "ssh:test".into(),
        "Remote".into(),
        ConnectTarget::Ssh {
            target: "unused".into(),
            session: "default".into(),
        },
        true,
    );
    remote.live.snapshot = view.live.snapshot.clone();
    let snapshot = Arc::make_mut(remote.live.snapshot.as_mut().unwrap());
    snapshot.workspaces[0].label = "remote workspace".into();
    remote
}

/// With host headers on screen, every workspace row steps in under its host;
/// a single-host sidebar has no header to nest under and keeps its column.
#[gpui::test]
fn workspaces_nest_under_their_host_in_every_layout(cx: &mut gpui::TestAppContext) {
    for mode in crate::config::LayoutMode::ALL {
        let (_single, cx_single) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = fixture_window(window, cx);
                view.config.layout.mode = mode;
                view
            });
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            SidebarFixture(view)
        });
        let alone = name_left(cx_single, "name-herdr");

        let (_multi, cx_multi) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = fixture_window(window, cx);
                view.config.layout.mode = mode;
                let remote = remote_endpoint(&view);
                view.endpoints.push(remote);
                view
            });
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            SidebarFixture(view)
        });
        let local = name_left(cx_multi, "name-herdr");
        let remote = name_left(cx_multi, "name-remote workspace");
        assert!(
            local > alone,
            "{mode}: nested {local:?} should sit right of unnested {alone:?}"
        );
        assert_eq!(local, remote, "{mode}: every host nests its rows alike");
    }
}
