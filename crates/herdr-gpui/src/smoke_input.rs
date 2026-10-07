//! Native input-handler shutdown regression.
use super::*;

/// Leave the real platform input handler installed until normal app teardown.
/// Unlike the paint-only fixtures, process::exit would hide leaked entities here.
pub(super) fn start(handle: WindowHandle<HerdrWindow>, field: String, cx: &mut App) {
    EXIT_CODE.store(1, Ordering::SeqCst);
    cx.spawn(async move |cx| {
        let result = handle.update(cx, |view, window, cx| -> Result<()> {
            let mut snapshot = sidebar::layout_tests::snapshot(3);
            snapshot.focused_workspace_id = Some("w0".into());
            view.live.snapshot = Some(Arc::new(snapshot));
            view.live.status = crate::state::ConnectionStatus::Connected;
            match field.as_str() {
                "terminal" => window.focus(&view.focus, cx),
                "dialog" => {
                    view.open_focused_workspace_dialog(
                        crate::menu::WorkspaceAction::Rename,
                        window,
                        cx,
                    );
                    anyhow::ensure!(view.menu.input.is_some(), "dialog did not open");
                    anyhow::ensure!(view.menu.focus.is_focused(window), "dialog lacks focus");
                }
                "search" => {
                    view.open_theme_picker(window, cx);
                    let picker = view.menu.themes.as_ref().context("search did not open")?;
                    anyhow::ensure!(
                        picker.search.read(cx).focus.is_focused(window),
                        "search lacks focus"
                    );
                }
                _ => bail!("unknown input shutdown fixture: {field}"),
            }
            cx.notify();
            Ok(())
        });
        let result = result.and_then(|result| result).and_then(|()| {
            AnyWindowHandle::from(handle).update(cx, |_, window, cx| {
                // Synchronously paint and install the handler before requesting quit.
                window.refresh();
                window.draw(cx).clear(cx);
            })
        });
        match result {
            Ok(()) => {
                eprintln!("INPUT shutdown ready: {field}");
                EXIT_CODE.store(0, Ordering::SeqCst);
            }
            Err(error) => eprintln!("INPUT shutdown FAIL: {error:#}"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}
