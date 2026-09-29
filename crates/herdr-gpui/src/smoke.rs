//! Native opt-in smoke driver. No test platform or blocking waits on the UI thread.
use crate::{
    Command, HerdrWindow, LiveState, NavigationTarget, RunCommand, github, open_window, sidebar,
    updater,
};
// The window, menu, and icon checks below need an active desktop, so they and
// everything only they reach are built for macOS alone.
#[cfg(target_os = "macos")]
use crate::{ConnectionStatus, app_icon, endpoint, menu, pull_request};
use anyhow::{Context as _, Result, anyhow, bail};
#[cfg(target_os = "macos")]
use gpui::prelude::*;
use gpui::*;
use herdr_client::{ConnectOptions, ConnectTarget, Method, protocol::*};
use std::{sync::Arc, time::Duration};
use std::{
    sync::atomic::{AtomicU8, Ordering},
    time::Instant,
};

pub static EXIT_CODE: AtomicU8 = AtomicU8::new(0);

#[cfg(target_os = "macos")]
#[path = "smoke_selection.rs"]
mod selection;

#[cfg(target_os = "macos")]
#[path = "smoke_clipboard.rs"]
mod clipboard;

fn banner_height() -> f32 {
    if env!("HERDR_BUILD_WORKTREE") == "1" {
        22.
    } else {
        0.
    }
}

// Baseline viewport sizes already include the existing 34px macOS titlebar.
// Add only the optional banner to preserve the tested content area, not mask clipping.
fn fixture_size(width: f32, height: f32) -> Size<Pixels> {
    size(px(width), px(height + banner_height()))
}

/// Native check that the icon cascade reaches an installed Nerd Font. Prompts
/// draw powerline separators and icons from the Private Use Area, which no text
/// face and no platform default cascade covers, so without the cascade every
/// such cell shapes to the platform's missing-glyph box. Headless shaping
/// cannot show this: the test text system reports no installed families and
/// gives every glyph the same fixed advance.
fn symbol_cascade(window: &mut Window, cx: &mut App) -> Result<&'static str> {
    let detected = crate::config::symbol_fallbacks(cx.text_system().all_font_names());
    if detected.is_empty() {
        // An installed icon font is an external resource, like a daemon binary.
        return Ok("symbol cascade skipped (no Nerd Font installed)");
    }
    let font = crate::config::FontConfig {
        family: "Menlo".into(),
        size: crate::terminal::FONT_SIZE,
        fallbacks: Some(detected.clone()),
    }
    .font();
    // Fallback faces never enter `get_font_for_id`, and a cascade gives the
    // same family a new font id, so coverage is read from the glyphs: anything
    // no font carries shapes to the platform's missing-glyph box, and a covered
    // codepoint must not land on that same glyph.
    let glyphs = |symbol: &str, window: &mut Window| {
        window
            .text_system()
            .shape_line(
                symbol.to_owned().into(),
                px(crate::terminal::FONT_SIZE),
                &[TextRun {
                    len: symbol.len(),
                    font: font.clone(),
                    color: rgb(0xffffff).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.id))
            .collect::<Vec<_>>()
    };
    // Plane 15 is private and unassigned by every shipped font, including the
    // Nerd Font patches, so it names the missing-glyph box for this machine.
    let missing = glyphs("\u{f0000}", window);
    if missing.is_empty() {
        bail!("no missing-glyph baseline to compare icons against");
    }
    // A separator, a branch and a clock: three prompt icons from three ranges.
    for symbol in ["\u{e0b0}", "\u{e0a0}", "\u{f017}"] {
        if glyphs(symbol, window) == missing {
            bail!("icon {symbol:?} stayed a missing-glyph box under cascade {detected:?}");
        }
    }
    if glyphs("A", window) == missing {
        bail!("cascade lost the configured face for plain text");
    }
    Ok("symbol cascade reaches installed Nerd Fonts")
}

pub fn start_sidebar(handle: WindowHandle<HerdrWindow>, cx: &mut App) {
    if std::env::var_os("HERDR_TEST_NOTIFICATIONS_ONLY").is_some() {
        start_notifications(handle, cx);
        return;
    }
    EXIT_CODE.store(1, Ordering::SeqCst);
    // AppKit may exit(0) inside cx.quit(), bypassing main's ExitCode. These
    // daemon-free fixtures exit explicitly, like the performance driver.
    #[cfg(target_os = "macos")]
    if let Err(error) = app_icon::verify_native().and_then(|()| crate::app_badge::verify_native()) {
        eprintln!("ICON native FAIL: {error:#}");
        std::process::exit(1);
    }
    cx.set_global(sidebar::layout_tests::PaintedProbes::default());
    cx.set_global(sidebar::layout_tests::VerifyChildGeometry(true));
    if std::env::var_os("HERDR_TEST_SIDEBAR_PROBE_FAILURE").is_some() {
        let _ = handle.update(cx, |view, _, cx| {
            // Exercise the real native paint failure path with a column that
            // intentionally violates the fixed-width initial fixture.
            view.sidebar_width = Some(180.);
            cx.notify();
        });
    }
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        // Shaping first: the cascade is independent of every layout probe below.
        let cascade = match handle.update(cx, |_, window, cx| symbol_cascade(window, cx)) {
            Ok(Ok(summary)) => summary,
            other => {
                eprintln!("SIDEBAR native symbol cascade FAIL: {other:?}");
                std::process::exit(1);
            }
        };
        eprintln!("SIDEBAR native symbol cascade: {cascade}");
        for frame in 0..72 {
            timer.timer(Duration::from_millis(100)).await;
            let result = AnyWindowHandle::from(handle).update(
                cx,
                |root, window, cx| -> Result<()> {
                    use crate::sidebar::layout_tests::PaintedProbes;
                    let (w, h) =
                        [(1200., 780.), (640., 400.), (1000., 650.), (800., 600.)][(frame % 12) / 3];
                    // Retain the glyph fixture's list viewport while reserving
                    // the fixed device footer below both scrollable sections.
                    use crate::config::{Density, LayoutMode, Style};
                    let density = [Density::Comfortable, Density::Normal, Density::Compact][frame / 12 % 3];
                    let style = [Style::Flat, Style::Rounded][frame / 36];
                    // Rounded rows are taller; grow both sections so the probed
                    // rows stay inside their lists at the smallest size too.
                    let h = h
                        + sidebar::DEVICE_FOOTER_HEIGHT
                        + if style == Style::Rounded { 160. } else { 0. };
                    let mode = LayoutMode::new(density, style);
                    let compact = density == Density::Compact;
                    if frame % 3 == 0 {
                        window.resize(fixture_size(w, h));
                    } else if window.viewport_size() != fixture_size(w, h) {
                        bail!(
                            "native resize did not settle: {:?}",
                            window.viewport_size()
                        );
                    }
                    cx.default_global::<PaintedProbes>().0.clear();
                    root.downcast::<HerdrWindow>()
                        .map_err(|_| anyhow!("unexpected root"))?
                        .update(cx, |view, cx| {
                            view.config.layout.mode = mode;
                            cx.notify();
                        });
                    window.refresh();
                    window.draw(cx).clear(cx);
                    cx.default_global::<PaintedProbes>().check()?;
                    let probes = &cx.global::<PaintedProbes>().0;
                    let mut failed = false;
                    for input in [
                        "herdr",
                        "main",
                        "Claude Code",
                        "agent",
                        "1256789",
                        "herdr-gpui-sidebar-rendering-regression-investigation",
                        "fix/sidebar-label-width-and-overflow-regression",
                    ] {
                        if compact && matches!(input, "main" | "1256789" | "fix/sidebar-label-width-and-overflow-regression") {
                            if probes.contains_key(input) {
                                bail!("compact layout painted branch: {input}");
                            }
                            continue;
                        }
                        let p = probes
                            .get(input)
                            .with_context(|| format!("missing paint: {input}"))?;
                        if frame == 0 {
                            eprintln!("SIDEBAR frame={frame} input={input:?} {p:?}");
                        }
                        let expected_short = input.len() < 20;
                        let title_icon = matches!(input, "herdr" | "herdr-gpui-sidebar-rendering-regression-investigation");
                        // Rounded rows give up the highlight's inset, the
                        // density's gap, on both edges.
                        let extra_width = match density {
                            Density::Comfortable => 0.,
                            Density::Normal => 10.,
                            Density::Compact => 16.,
                        } - match (style, density) {
                            (Style::Flat, _) => 0.,
                            (Style::Rounded, Density::Comfortable) => 16.,
                            (Style::Rounded, Density::Normal) => 12.,
                            (Style::Rounded, Density::Compact) => 8.,
                        };
                        let icon_reserve = if title_icon {
                            sidebar::ICON_RESERVE
                        } else if matches!(input, "Claude Code" | "agent") {
                            16. // 12px agent mark and 4px gap before its name.
                        } else {
                            0.
                        };
                        let expected_width = px(sidebar::LABEL_WIDTH + extra_width - icon_reserve);
                        if p.glyph_text != p.cached
                            || (expected_short && p.glyph_text != input)
                            || (!expected_short
                                && (p.width < px(150.) || !p.glyph_text.ends_with('\u{2026}')))
                            || p.clipped
                            || p.bounds.size.width != expected_width
                            || p.mask.size.width != expected_width
                            || p.width > p.bounds.size.width
                            || p.bounds.size.height != px(16.)
                        {
                            eprintln!("SIDEBAR bad paint frame={frame} input={input:?} {p:?}");
                            failed = true;
                        }
                    }
                    if failed {
                        bail!("incomplete/cropped native glyph output");
                    }
                    eprintln!(
                        "SIDEBAR verified frame={frame} mode={mode:?} viewport={:?} clipped=0",
                        window.viewport_size()
                    );
                    Ok(())
                },
            );
            if !matches!(result, Ok(Ok(()))) {
                eprintln!("SIDEBAR native FAIL: {result:?}");
                std::process::exit(1);
            }
        }
        // The other row layouts place text by their own geometry, so they are
        // held to what every layout owes: shaped glyphs match the text they
        // were given, and none is cropped or wider than its box.
        {
            use crate::config::LayoutMode;
            cx.update(|cx| cx.set_global(sidebar::layout_tests::VerifyChildGeometry(false)));
            for mode in [LayoutMode::Superset, LayoutMode::Orca, LayoutMode::Minimal] {
                        timer.timer(Duration::from_millis(100)).await;
                        let result = AnyWindowHandle::from(handle).update(
                            cx,
                            |root, window, cx| -> Result<()> {
                                use crate::sidebar::layout_tests::PaintedProbes;
                                cx.default_global::<PaintedProbes>().0.clear();
                                root.downcast::<HerdrWindow>()
                                    .map_err(|_| anyhow!("unexpected root"))?
                                    .update(cx, |view, cx| {
                                        view.config.layout.mode = mode;
                                        cx.notify();
                                    });
                                window.refresh();
                                window.draw(cx).clear(cx);
                                cx.default_global::<PaintedProbes>().check()?;
                                let probes = &cx.global::<PaintedProbes>().0;
                                // The long name must be cut short in every
                                // layout, which proves ellipsizing natively.
                                const LONG: &str =
                                    "herdr-gpui-sidebar-rendering-regression-investigation";
                                if !probes.get(LONG).is_some_and(|p| p.glyph_text.ends_with('\u{2026}')) {
                                    bail!("{mode}: {LONG:?} was not ellipsized");
                                }
                                for input in ["herdr", "Claude Code", LONG] {
                                    let p = probes
                                        .get(input)
                                        .with_context(|| format!("missing paint: {input}"))?;
                                    // Orca shares one line between an agent and
                                    // its place, so a name may end in an ellipsis.
                                    let shown = p.glyph_text == input
                                        || p.glyph_text.strip_suffix('\u{2026}').is_some_and(
                                            |kept| input.starts_with(kept.trim_end()),
                                        );
                                    if p.glyph_text != p.cached
                                        || !shown
                                        || p.clipped
                                        || p.width > p.bounds.size.width
                                    {
                                        bail!("{mode}: bad paint {input:?} {p:?}");
                                    }
                                }
                                eprintln!("SIDEBAR verified layout={mode}");
                                Ok(())
                            },
                        );
                        if !matches!(result, Ok(Ok(()))) {
                            eprintln!("SIDEBAR native FAIL: {result:?}");
                            std::process::exit(1);
                        }
            }
        }
        let _ = handle.update(cx, |view, _, cx| {
            // Later fixtures add PR badges and dialogs that change label budgets.
            cx.set_global(sidebar::layout_tests::VerifyChildGeometry(false));
            view.config.layout.mode = crate::config::LayoutMode::from(crate::config::Density::Comfortable);
            cx.notify();
        });
        #[cfg(target_os = "macos")]
        for step in 0..4 {
            let point = AnyWindowHandle::from(handle).update(cx, |root, window, cx| {
                let view = root
                    .downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected root"))?;
                view.update(cx, |view, _| {
                    if step == 0 {
                        if let Some(snapshot) = view.live.snapshot.as_mut() {
                            let snapshot = Arc::make_mut(snapshot);
                            snapshot.focused_workspace_id = Some("w4".into());
                            for workspace in &mut snapshot.workspaces {
                                workspace.focused = workspace.workspace_id == "w4";
                            }
                        }
                        view.marked = "preserve active child".into();
                    }
                });
                window.refresh();
                cx.default_global::<sidebar::layout_tests::PaintedProbes>()
                    .0
                    .clear();
                window.draw(cx).clear(cx);
                let label = match step {
                    0 => "\u{25be}",
                    1 => "\u{25b8}",
                    _ => "menu",
                };
                cx.global::<sidebar::layout_tests::PaintedProbes>()
                    .0
                    .get(label)
                    .map(|probe| probe.bounds.center())
                    .context("missing native click target")
                    .and_then(|point| Ok((sidebar::native_tests::Target::acquire(window)?, point)))
            });
            let result = point
                .context("updating native click target")
                .and_then(|point| point.context("native target"))
                .and_then(|(target, point)| target.click(point.x.to_f64(), point.y.to_f64()));
            timer.timer(Duration::from_millis(50)).await;
            let result = if step == 3 {
                result.and_then(|()| {
                    let target = AnyWindowHandle::from(handle)
                        .update(cx, |_, window, _| sidebar::native_tests::Target::acquire(window))
                        .context("acquiring outside-click target")??;
                    target.click(700., 500. + f64::from(banner_height()))
                })
            } else {
                result
            };
            let verified = AnyWindowHandle::from(handle).update(
                cx,
                |root, window, cx| -> Result<()> {
                    result?;
                    let view = root
                        .downcast::<HerdrWindow>()
                        .map_err(|_| anyhow!("unexpected root"))?;
                    let state = view.read(cx);
                    if step < 2 {
                        if state
                            .collapsed_repos
                            .contains("/fixture/agent-launcher/.git")
                            != (step == 0)
                            || state.marked != "preserve active child"
                            || state.live.snapshot.as_ref().is_none_or(|s| {
                                s.workspaces.len() != 40
                                    || s.focused_workspace_id.as_deref() != Some("w4")
                            })
                        {
                            bail!("collapse navigated, removed rows, or lost selection");
                        }
                    } else if step == 2 {
                        if state.menu.page != Some(menu::Page::Menu) {
                            bail!("native footer click did not open menu");
                        }
                        let before = state.input_probe;
                        window.dispatch_action(Box::new(RunCommand { command: Command::Tab }), cx);
                        for key in ["down", "down", "enter", "x", "escape"] {
                            window.dispatch_keystroke(
                                Keystroke {
                                    key: key.into(),
                                    ..Default::default()
                                },
                                cx,
                            );
                        }
                        let state = view.read(cx);
                        if state.menu.page.is_some()
                            || state.input_probe.text != before.text
                            || state.input_probe.keys != before.keys
                            || state.input_probe.actions != before.actions
                        {
                            bail!("menu keyboard handling leaked terminal input");
                        }
                    } else if state.menu.page.is_some() {
                        bail!("outside click did not dismiss native menu");
                    }
                    Ok(())
                },
            );
            if !matches!(verified, Ok(Ok(()))) {
                eprintln!("SIDEBAR native interaction FAIL: {verified:?}");
                std::process::exit(1);
            }
        }
        for show_agents in [false, true] {
            let result = AnyWindowHandle::from(handle).update(cx, |root, window, cx| {
                let view = root.downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected root"))?;
                view.update(cx, |view, cx| {
                    view.config.show_agents = show_agents;
                    cx.notify();
                });
                cx.default_global::<sidebar::layout_tests::PaintedProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear(cx);
                let probes = &cx.global::<sidebar::layout_tests::PaintedProbes>().0;
                if probes.contains_key("Claude Code") != show_agents
                    || !probes.contains_key("herdr")
                    || !probes.contains_key("menu")
                {
                    bail!("native Agents visibility did not follow configuration");
                }
                Ok(())
            });
            if !matches!(result, Ok(Ok(()))) {
                eprintln!("SIDEBAR native visibility FAIL: {result:?}");
                std::process::exit(1);
            }
        }
        #[cfg(target_os = "macos")]
        for (width, height) in [(640., 400.), (1200., 780.)] {
            let _ = handle.update(cx, |_, window, _| window.resize(size(px(width), px(height))));
            timer.timer(Duration::from_millis(100)).await;
            for (keys, action) in [
                ("down enter", menu::WorkspaceAction::Rename),
                ("down down enter", menu::WorkspaceAction::Close),
                ("down down down enter", menu::WorkspaceAction::NewWorktree),
                ("down down down enter", menu::WorkspaceAction::DeleteWorktree),
            ] {
                let point = handle.update(cx, |view, window, cx| {
                    view.live.status = ConnectionStatus::Connected;
                    // Keep the clicked row inside the scroll viewport below the native titlebar.
                    view.sidebar_scroll[0].set_offset(point(px(0.), px(if action == menu::WorkspaceAction::DeleteWorktree { -140. } else { -80. })));
                    cx.default_global::<sidebar::layout_tests::PaintedProbes>().0.clear();
                    cx.notify();
                    window.refresh();
                });
                if let Err(error) = point {
                    eprintln!("SIDEBAR native dialog setup FAIL: {error}");
                    std::process::exit(1);
                }
                let point = AnyWindowHandle::from(handle).update(cx, |_, window, cx| {
                    window.draw(cx).clear(cx);
                    cx.global::<sidebar::layout_tests::PaintedProbes>().0.get(if action == menu::WorkspaceAction::DeleteWorktree { "sidebar-child" } else { "agent-launcher" }).map(|probe| {
                        eprintln!("DIALOG native {action:?} viewport={:?}", window.viewport_size());
                         probe.bounds.center()
                    })
                });
                let target = handle.update(cx, |_, window, _| sidebar::native_tests::Target::acquire(window));
                let clicked = match (point, target) {
                    (Ok(Some(point)), Ok(Ok(target))) => target.right_click(point.x.to_f64(), point.y.to_f64()),
                    _ => Err(anyhow!("missing native workspace")),
                };
                timer.timer(Duration::from_millis(50)).await;
                let verified = AnyWindowHandle::from(handle).update(cx, |root, window, cx| -> Result<()> {
                    clicked?;
                    let view = root.downcast::<HerdrWindow>().map_err(|_| anyhow!("unexpected root"))?;
                    if view.read(cx).menu.page != Some(menu::Page::Workspace) { bail!("right click did not open workspace menu"); }
                    view.update(cx, |view, _| -> Result<()> {
                        let mut pr = pull_request::fixture()?;
                        pr.head_ref_name = "feature/a-deliberately-long-branch-name-for-the-compact-popover".repeat(3);
                        view.workspace_pr_fixture(pr)?;
                        Ok(())
                    })?;
                    cx.default_global::<sidebar::layout_tests::PaintedProbes>().0.clear();
                    window.draw(cx).clear(cx);
                    let probes = &cx.global::<sidebar::layout_tests::PaintedProbes>().0;
                    for (text, probe) in probes.iter().filter(|(text, _)| text.starts_with("#8 ") || matches!(text.as_str(), "+1730" | "-31")) {
                        if probe.glyph_text != probe.cached || probe.clipped || probe.glyph_text.is_empty() {
                            bail!("native PR glyphs clipped: {text} {probe:?}");
                        }
                        if text.starts_with("#8 ") && (!probe.glyph_text.starts_with("#8 Improve") || !probe.glyph_text.ends_with('\u{2026}')) {
                            bail!("native PR title did not truncate correctly: {probe:?}");
                        }
                    }
                    if !probes.contains_key("+1730") || !probes.contains_key("-31") || !probes.keys().any(|text| text.starts_with("#8 ")) {
                        bail!("native PR summary was not painted");
                    }
                    let title = probes.iter().find(|(text, _)| text.starts_with("#8 ")).map(|(_, probe)| probe).context("missing PR title")?;
                    let additions = probes.get("+1730").context("missing PR additions")?;
                    if additions.bounds.bottom() - title.bounds.top() > px(160.)
                        || additions.bounds.bottom() > window.viewport_size().height
                        || title.bounds.right() > window.viewport_size().width
                    {
                        bail!("native PR summary is oversized or outside the viewport");
                    }
                    let before = view.read(cx).input_probe;
                    for key in keys.split(' ') {
                        window.dispatch_keystroke(Keystroke::parse(key)?, cx);
                    }
                    window.draw(cx).clear(cx);
                    if view.read(cx).menu.page != Some(menu::Page::Dialog(action)) { bail!("workspace menu opened wrong dialog"); }
                    window.dispatch_action(Box::new(RunCommand { command: Command::Tab }), cx);
                    // Only the editable dialogs carry a text field; confirmations do not.
                    if matches!(action, menu::WorkspaceAction::Rename | menu::WorkspaceAction::NewWorktree) {
                        window.dispatch_keystroke(Keystroke::parse("cmd-a")?, cx);
                        for ch in "long-label-\u{65e5}\u{672c}-\u{1f600}".repeat(4).chars() {
                            window.dispatch_keystroke(Keystroke::parse(&ch.to_string())?, cx);
                        }
                        window.draw(cx).clear(cx);
                        view.update(cx, |view, cx| -> Result<()> {
                            let input = view.menu.input.as_ref().context("missing native editor")?;
                            if input.text != "long-label-\u{65e5}\u{672c}-\u{1f600}".repeat(4) { bail!("native editor lost Unicode text"); }
                            let end = input.text.encode_utf16().count();
                            let field = input.bounds;
                            let caret = view.bounds_for_range(end..end, Bounds::default(), window, cx).context("missing native IME bounds")?;
                            if !field.contains(&caret.origin) || field.right() > window.viewport_size().width || field.bottom() > window.viewport_size().height { bail!("native dialog caret/field out of bounds"); }
                            Ok(())
                        })?;
                    }
                    window.dispatch_keystroke(Keystroke::parse("escape")?, cx);
                    let state = view.read(cx);
                    if state.menu.page.is_some() || state.input_probe.text != before.text || state.input_probe.keys != before.keys || state.input_probe.actions != before.actions || !state.focus.is_focused(window) { bail!("workspace dialog leaked input or lost focus"); }
                    Ok(())
                });
                if !matches!(verified, Ok(Ok(()))) {
                    eprintln!("SIDEBAR native dialog FAIL: {verified:?}");
                    std::process::exit(1);
                }
            }
        }
        for (width, height) in [(640., 400.), (1200., 780.)] {
            let _ = handle.update(cx, |_, window, _| window.resize(size(px(width), px(height))));
            timer.timer(Duration::from_millis(100)).await;
            for state in 0..5 {
                let result = AnyWindowHandle::from(handle).update(cx, |root, window, cx| -> Result<()> {
                    let view = root.downcast::<HerdrWindow>().map_err(|_| anyhow!("unexpected root"))?;
                    view.update(cx, |view, cx| {
                        view.github_fixture(state == 1, window, cx);
                        if state == 2 {
                            view.menu.github.failed = true;
                            view.menu.github.message = Some("GitHub code expired. Sign in again. ".repeat(40));
                        } else if state == 3 {
                            view.menu.github = github::Auth::connected_fixture();
                        } else if state == 4 {
                            view.menu.github = github::Auth::requesting_fixture();
                        }
                    });
                    cx.default_global::<sidebar::layout_tests::PaintedProbes>().0.clear();
                    window.draw(cx).clear(cx);
                    if state == 1 {
                        let probe = cx.global::<sidebar::layout_tests::PaintedProbes>().0.get("ABCD-1234").context("GitHub device code not painted")?;
                        if probe.clipped || probe.glyph_text != "ABCD-1234" { bail!("GitHub device code clipped"); }
                    }
                    let probes = &cx.global::<sidebar::layout_tests::PaintedProbes>().0;
                    if probes.contains_key("Cancel (Esc)") || probes.contains_key("Close (Esc)") { bail!("redundant GitHub footer close"); }
                    let close = probes.get("Close").context("missing GitHub header close")?;
                    if close.clipped || close.glyph_text != "Close" || close.bounds.bottom() > window.viewport_size().height { bail!("GitHub header close clipped"); }
                    if state == 3 {
                        let signout = probes.get("Sign out (D)").context("missing signout action")?;
                        if signout.bounds.bottom() - close.bounds.top() > px(230.) { bail!("connected GitHub panel is oversized"); }
                    }
                    let before = view.read(cx).input_probe;
                    window.dispatch_keystroke(Keystroke::parse("c")?, cx);
                    window.dispatch_keystroke(Keystroke::parse("escape")?, cx);
                    let state = view.read(cx);
                    if state.menu.page.is_some() || state.input_probe.text != before.text || state.input_probe.keys != before.keys || !state.focus.is_focused(window) { bail!("GitHub sign-in leaked input or lost focus"); }
                    Ok(())
                });
                if !matches!(result, Ok(Ok(()))) {
                    eprintln!("SIDEBAR native GitHub auth FAIL: {result:?}");
                    std::process::exit(1);
                }
            }
        }
        #[cfg(target_os = "macos")]
        if let Err(error) = sidebar_hosts(handle, cx).await {
            eprintln!("SIDEBAR native hosts FAIL: {error:#}");
            std::process::exit(1);
        }
        let probes = cx.update(|cx| cx.default_global::<sidebar::layout_tests::PaintedProbes>().check());
        if !matches!(probes, Ok(())) {
            eprintln!("SIDEBAR native paint FAIL: {probes:?}");
            std::process::exit(1);
        }
        eprintln!("SIDEBAR native PASS: {cascade}; 24 Menlo draws, flat and rounded normal/compact/comfortable layouts at 4 sizes, collapse/expand, menu isolation, PR title/stats glyphs, GitHub auth fixtures, right-click dialogs and Unicode fields at 2 sizes; host routing, disabled selection, scoped repositories, resized host/agent glyphs, independent scroll and decoy key window");
        std::process::exit(0);
    })
    .detach();
}

fn start_notifications(handle: WindowHandle<HerdrWindow>, cx: &mut App) {
    EXIT_CODE.store(1, Ordering::SeqCst);
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        for (width, height) in [(360., 240.), (1200., 780.)] {
            let _ = handle.update(cx, |_, window, _| window.resize(size(px(width), px(height))));
            timer.timer(Duration::from_millis(100)).await;
            for kind in [
                SemanticNotificationKind::NeedsAttention,
                SemanticNotificationKind::Finished,
                SemanticNotificationKind::UpdateInstalled,
                SemanticNotificationKind::Custom,
            ] {
                let result = handle.update(cx, |view, window, cx| -> Result<()> {
                    view.menu.reset();
                    view.config.notifications.enabled = false;
                    view.config.notifications.delay_seconds = 3600;
                    window.focus(&view.focus, cx);
                    let selected = view.selected_endpoint;
                    let snapshot = view.live.snapshot.clone();
                    view.show_toast_preview(kind, cx);
                    let notice = &view.endpoints[selected].toasts.entries.back().context("missing toast preview")?.1;
                    if !notice.visible || notice.kind != kind || view.endpoints.iter().any(|e| e.connection.handle.is_some()) {
                        bail!("preview did not bypass policy offline");
                    }
                    view.command(Command::OpenNotificationTarget, window, cx);
                    if view.selected_endpoint != selected || view.live.snapshot != snapshot || view.pending_navigation.is_some() || !view.focus.is_focused(window) {
                        bail!("offline notification command navigated or stole focus");
                    }
                    Ok(())
                });
                let drawn = AnyWindowHandle::from(handle).update(cx, |_, window, cx| window.draw(cx).clear(cx));
                if !matches!(result, Ok(Ok(()))) || drawn.is_err() {
                    eprintln!("NOTIFICATIONS native FAIL: {result:?} {drawn:?}");
                    std::process::exit(1);
                }
            }
        }
        eprintln!("NOTIFICATIONS native PASS: all four offline previews drawn at narrow/wide sizes; disabled/delayed policy bypass and inert offline command verified");
        std::process::exit(0);
    })
    .detach();
}

#[cfg(target_os = "macos")]
async fn sidebar_hosts(handle: WindowHandle<HerdrWindow>, cx: &mut AsyncApp) -> Result<()> {
    use sidebar::{layout_tests::PaintedProbes, native_tests::Target};
    const REMOTE: &str = "Synthetic host with a deliberately long label";
    let decoy = cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| {
                    HerdrWindow::new(
                        ConnectTarget::Socket("/unused-decoy.sock".into()),
                        window,
                        cx,
                        true,
                    )
                })
            })
        })
        .context("opening decoy window")?;
    handle
        .update(cx, |view, window, cx| {
            view.endpoints.clear();
            for (index, label) in ["Local", REMOTE, "Disabled"].into_iter().enumerate() {
                let mut endpoint = endpoint::Endpoint::new(
                    if index == 0 {
                        endpoint::LOCAL.into()
                    } else {
                        format!("fixture-{index}")
                    },
                    label.into(),
                    ConnectTarget::Socket(format!("/unused-sidebar-{index}.sock").into()),
                    index != 2,
                );
                let mut snapshot = sidebar::layout_tests::snapshot(6);
                snapshot.workspaces.drain(0..3);
                snapshot.workspaces[0].label = format!("repository-{index}");
                snapshot.workspaces[1].custom_label = true;
                snapshot.workspaces[1].label = format!("child-{index}");
                snapshot.workspaces.truncate(2);
                snapshot.agents.truncate(1);
                // This fixture tests the name fallback, not the shared fixture's
                // higher-priority "Claude Code" display name.
                snapshot.agents[0].display_agent = None;
                snapshot.agents[0].name =
                    Some(format!("agent-{index}-with-a-deliberately-long-label"));
                endpoint.live.snapshot = Some(Arc::new(snapshot));
                if let Ok(mut inbox) = endpoint.connection.inbox.lock() {
                    *inbox = endpoint.live.clone();
                }
                view.endpoints.push(endpoint);
            }
            view.selected_endpoint = 0;
            view.live = view.endpoints[0].live.clone();
            view.collapsed_repos.clear();
            view.marked = "preserve collapse composition".into();
            window.resize(fixture_size(480., 780.));
            cx.notify();
        })
        .context("preparing sidebar host fixtures")?;
    cx.update(|cx| cx.activate(true));
    decoy
        .update(cx, |_, window, _| window.activate_window())
        .context("activating decoy window")?;
    // GPUI schedules AppKit activation. Wait for its result, not a guessed delay
    // followed by a synchronous makeKeyWindow call on a possibly unordered window.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let target = decoy
            .update(cx, |_, window, _| Target::acquire(window))
            .context("acquiring decoy target")??;
        let key = target.is_key();
        drop(target);
        let resized = handle
            .update(cx, |_, window, _| {
                window.viewport_size() == fixture_size(480., 780.)
            })
            .context("checking sidebar resize")?;
        if key && resized {
            break;
        }
        if Instant::now() >= deadline {
            let mtm = objc2::MainThreadMarker::new()
                .context("fixture activation requires main thread")?;
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            bail!(
                "decoy activation/resize deadline: key={key}, resized={resized}, app_active={}, app_hidden={}",
                app.isActive(),
                app.isHidden()
            );
        }
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    // The decoy is deliberately key. Neither acquisition nor delivery may use it.
    let target = decoy
        .update(cx, |_, window, _| Target::acquire(window))
        .context("acquiring decoy target")??;
    if !target.is_key() {
        bail!("decoy did not become key");
    }
    drop(target);
    let viewport = handle
        .update(cx, |_, window, _| window.viewport_size())
        .context("reading sidebar viewport")?;
    if viewport != fixture_size(480., 780.) {
        bail!("narrow resize not settled: {viewport:?}");
    }
    for (step, label, expected) in [
        (0, REMOTE, 0), // collapse unselected host
        (1, REMOTE, 0), // expand
        (2, REMOTE, 1),
        (3, "Local", 0),
        (4, "Disabled", 0),
        (5, "child-1", 1),
        (6, "child-0", 0),
        (7, "agent-1-with-a-deliberately-long-label", 1),
        (8, "agent-0-with-a-deliberately-long-label", 0),
        (9, "repository-1", 0),
    ] {
        let decoy_target = decoy
            .update(cx, |_, window, _| Target::acquire(window))
            .context("checking decoy target")??;
        if !decoy_target.is_key() {
            bail!("decoy lost key status at step {step}");
        }
        drop(decoy_target);
        let (target, point) = AnyWindowHandle::from(handle)
            .update(cx, |_, window, cx| -> Result<_> {
                cx.default_global::<PaintedProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear(cx);
                let probes = &cx.global::<PaintedProbes>().0;
                for (name, prefix) in [
                    (REMOTE, "Synthetic host"),
                    ("agent-1-with-a-deliberately-long-label", "agent-1-with-a"),
                ] {
                    let p = probes
                        .get(name)
                        .with_context(|| format!("missing {name}"))?;
                    if p.clipped
                        || p.glyph_text != p.cached
                        || !p.glyph_text.ends_with('\u{2026}')
                        || !p.glyph_text.starts_with(prefix)
                        || p.bounds.size.width < px(110.)
                        || p.width > p.bounds.size.width
                    {
                        bail!("narrow native label: {name}: {p:?}");
                    }
                }
                let p = probes
                    .get(label)
                    .with_context(|| format!("missing target {label}"))?;
                let mut point = p.bounds.center();
                if step < 2 {
                    point.x =
                        p.bounds.left() - px(sidebar::HOST_GAP + sidebar::HOST_ARROW_WIDTH / 2.);
                }
                if step == 9 {
                    // The title ends at the label column's edge, even with an avatar.
                    point.x =
                        p.bounds.right() + px((sidebar::LABEL_GAP + sidebar::ARROW_RESERVE) / 2.);
                }
                Ok((Target::acquire(window)?, point))
            })
            .context("locating sidebar host click target")??;
        target.click(point.x.to_f64(), point.y.to_f64())?;
        drop(target);
        AnyWindowHandle::from(handle)
            .update(cx, |root, window, cx| -> Result<()> {
                cx.default_global::<PaintedProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear(cx);
                let entity = root
                    .downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected root"))?;
                let view = entity.read(cx);
                if view.selected_endpoint != expected {
                    bail!(
                        "step {step}: selected {} expected {expected}",
                        view.selected_endpoint
                    );
                }
                if step < 2
                    && (view.endpoints[1].collapsed != (step == 0)
                        || view.marked != "preserve collapse composition")
                {
                    bail!("host collapse changed selection/composition");
                }
                if (5..=8).contains(&step) {
                    let expected = if step < 7 {
                        NavigationTarget::Workspace("w4".to_owned())
                    } else {
                        NavigationTarget::Pane("p0".to_owned())
                    };
                    if view.pending_navigation.as_ref() != Some(&expected) {
                        bail!("wrong duplicate-ID route at step {step}");
                    }
                }
                if step == 9
                    && (!view.endpoints[1]
                        .collapsed_repos
                        .contains("/fixture/agent-launcher/.git")
                        || !view.collapsed_repos.is_empty())
                {
                    bail!("repository collapse escaped endpoint scope");
                }
                let probes = &cx.global::<PaintedProbes>().0;
                if step == 0
                    && (probes.contains_key("child-1")
                        || !probes.contains_key("agent-1-with-a-deliberately-long-label"))
                {
                    bail!("host collapse hid agents or left workspace visible");
                }
                if step == 9 && (probes.contains_key("child-1") || !probes.contains_key("child-0"))
                {
                    bail!("repository visibility not scoped");
                }
                Ok(())
            })
            .context("verifying sidebar host interaction")??;
        eprintln!("SIDEBAR native host step={step} verified");
    }
    // Exercise wider, narrower, then restored native allocations. This catches
    // stale truncated font runs as well as host labels left at the old 116px.
    for (window_width, preferred, host_width, agent_width, host_prefix, agent_prefix) in [
        (
            800.,
            Some(400.),
            284.,
            343.,
            "Synthetic host",
            "agent-1-with-a",
        ),
        (360., None, 77., 63., "Synthetic", "agent-1"),
        (
            800.,
            Some(160.),
            117.,
            103.,
            "Synthetic host",
            "agent-1-with-",
        ),
        (
            800.,
            Some(480.),
            364.,
            423.,
            REMOTE,
            "agent-1-with-a-deliberately-long-label",
        ),
        (480., None, 116., 175., "Synthetic host", "agent-1-with-a"),
    ] {
        handle
            .update(cx, |view, window, cx| {
                view.sidebar_width = preferred;
                window.resize(fixture_size(window_width, 780.));
                cx.notify();
            })
            .context("resizing sidebar fixture")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let settled = AnyWindowHandle::from(handle)
            .update(cx, |_, window, cx| -> Result<bool> {
                if window.viewport_size() != fixture_size(window_width, 780.) {
                    return Ok(false);
                }
                cx.default_global::<PaintedProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear(cx);
                for (name, prefix, expected_width) in [
                    (REMOTE, host_prefix, host_width),
                    ("agent-1-with-a-deliberately-long-label", agent_prefix, agent_width),
                ] {
                    let p = cx
                        .global::<PaintedProbes>()
                        .0
                        .get(name)
                        .context("missing narrow label")?;
                    if p.clipped
                        || p.glyph_text != p.cached
                        || (p.glyph_text != name && !p.glyph_text.ends_with('\u{2026}'))
                        || !p.glyph_text.starts_with(prefix)
                        || p.bounds.size.width != px(expected_width)
                        || p.mask.size.width != px(expected_width)
                        || p.width > p.bounds.size.width
                    {
                        bail!("resized native label window={window_width} preferred={preferred:?}: {p:?}");
                    }
                }
                Ok(true)
            })
            .context("verifying resized sidebar labels")??;
            if settled {
                break;
            }
            if Instant::now() >= deadline {
                bail!("{window_width}px resize timed out");
            }
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
        }
        eprintln!(
            "SIDEBAR native resized labels verified: window={window_width} preferred={preferred:?} host={host_width} agent={agent_width}"
        );
    }
    for font_size in [16., 20., 12.] {
        AnyWindowHandle::from(handle)
            .update(cx, |root, window, cx| -> Result<()> {
                let entity = root
                    .downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected root"))?;
                entity.update(cx, |view, _| -> Result<()> {
                    view.config.sidebar.size = font_size;
                    view.config.theme = if font_size == 12. { "Default" } else { "Nord" }.into();
                    view.theme = view.config.theme()?;
                    Ok(())
                })?;
                cx.default_global::<PaintedProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear(cx);
                for name in [REMOTE, "agent-1-with-a-deliberately-long-label"] {
                    let probe = cx
                        .global::<PaintedProbes>()
                        .0
                        .get(name)
                        .context("missing scaled label")?;
                    if probe.clipped
                        || probe.glyph_text != probe.cached
                        || !probe.glyph_text.ends_with('\u{2026}')
                        || probe.width > probe.bounds.size.width
                        // Native layout snaps fractional line heights to device pixels.
                        || (probe.bounds.size.height - px(font_size * 4. / 3.)).abs()
                            > px(1. / window.scale_factor())
                    {
                        bail!("scaled native label size={font_size}: {probe:?}");
                    }
                }
                eprintln!("SIDEBAR native themed font labels verified: size={font_size}");
                Ok(())
            })
            .context("verifying scaled sidebar labels")??;
    }
    handle
        .update(cx, |view, _, cx| -> Result<()> {
            let snapshot = Arc::make_mut(
                view.live
                    .snapshot
                    .as_mut()
                    .context("missing scroll snapshot")?,
            );
            let agent = snapshot.agents[0].clone();
            let workspace = snapshot.workspaces[0].clone();
            for index in 0..40 {
                let mut agent = agent.clone();
                agent.pane_id = format!("scroll-p{index}");
                snapshot.agents.push(agent);
                let mut workspace = workspace.clone();
                workspace.workspace_id = format!("scroll-w{index}");
                workspace.worktree = None;
                snapshot.workspaces.push(workspace);
            }
            cx.notify();
            Ok(())
        })
        .context("preparing sidebar scroll fixture")??;
    for list in 0..2 {
        AnyWindowHandle::from(handle)
            .update(cx, |root, window, cx| -> Result<()> {
                window.refresh();
                window.draw(cx).clear(cx);
                let entity = root
                    .downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected root"))?;
                let scroll = entity.read(cx).sidebar_scroll.clone();
                let other = scroll[1 - list].offset();
                scroll[list].set_offset(point(px(0.), px(-80.)));
                window.refresh();
                window.draw(cx).clear(cx);
                if scroll[list].offset().y != px(-80.) || scroll[1 - list].offset() != other {
                    bail!("scroll handles are not independent: list={list}");
                }
                Ok(())
            })
            .context("verifying independent sidebar scrolling")??;
    }
    decoy
        .update(cx, |view, window, _| -> Result<()> {
            if view.selected_endpoint != 0
                || !view.collapsed_repos.is_empty()
                || view.menu.page.is_some()
                || view.pending_navigation.is_some()
                || view.selection_epoch != 0
            {
                bail!("fixture click affected decoy");
            }
            window.remove_window();
            Ok(())
        })
        .context("verifying and closing decoy window")??;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InputProbe {
    pub actions: u64,
    pub keys: u64,
    pub text: u64,
}

const STEPS: &[&str] = &[
    "initial painted surface",
    "Cmd-T new tab",
    "Cmd-D right split",
    "Cmd-Shift-D below split",
    "previous tab",
    "next tab",
    "Cmd-Shift-N workspace",
    "workspace navigation",
    "return to full-width tab",
    "text commit + Enter output",
    "native resize",
    "reconnect persisted state",
    "input after reconnect",
    "external workspace pushed to idle GUI",
];

const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(3);

struct ExternalWorkspace {
    // Keep the independent connection alive until the GUI has observed the change.
    _client: herdr_client::Client,
    id: String,
    sent: Instant,
    responded: Instant,
}

fn create_external_workspace(
    target: ConnectTarget,
    options: ConnectOptions,
    boot: String,
) -> Result<ExternalWorkspace> {
    use herdr_client::ClientEvent;
    let client =
        herdr_client::connect(target, options).context("connecting external workspace client")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut request = None;
    loop {
        let event = client
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("external workspace request")?;
        match event {
            ClientEvent::Snapshot(snapshot) if request.is_none() => {
                if snapshot.boot_id != boot {
                    bail!("external client connected to a different daemon boot");
                }
                let sent = Instant::now();
                let id = client
                    .handle
                    .request(
                        &boot,
                        Method::WorkspaceCreate,
                        serde_json::json!({
                            "focus": false, "label": "external-gui-smoke"
                        }),
                    )
                    .context("queueing external workspace request")?;
                request = Some((id, sent));
            }
            ClientEvent::Response {
                request_id,
                response,
            } => {
                let responded = Instant::now();
                let (expected, sent) = request.as_ref().context("unsolicited external response")?;
                if &request_id != expected || response.get("error").is_some() {
                    bail!("external workspace response: {response}");
                }
                let id = response["result"]["workspace"]["workspace_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("external response missing workspace ID")?
                    .to_owned();
                return Ok(ExternalWorkspace {
                    id,
                    sent: *sent,
                    responded,
                    _client: client,
                });
            }
            ClientEvent::Disconnected { reason } => {
                bail!(reason);
            }
            ClientEvent::CommandRejected { reason, .. } => {
                bail!(reason);
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("external workspace request timed out");
        }
    }
}

pub fn start(handle: WindowHandle<HerdrWindow>, cx: &mut App) {
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let mut step = 0;
        let mut since = Instant::now();
        let mut boot = String::new();
        let mut workspace = String::new();
        let mut first_tab = String::new();
        let mut second_tab = String::new();
        let mut split_pane = String::new();
        let mut old_size = ClientSurfaceSize { cols: 0, rows: 0 };
        let marker = format!("HERDR_GUI_{}_OK", std::process::id());
        let reconnected_marker = format!("{marker}_RECONNECTED");
        let mut frames = 0_u64;
        let mut external_rx = None;
        let mut external = None;
        let mut baseline = None;
        let mut switched = None;
        let mut completed = false;
        loop {
            timer.timer(Duration::from_millis(100)).await;
            let result = AnyWindowHandle::from(handle).update(cx, |root, window, cx| -> Result<bool> {
                let view = root.downcast::<HerdrWindow>().map_err(|_| anyhow!("unexpected window root"))?;
                // Observe only: no focus, draw, refresh, request, or reconnect can help
                // deliver this snapshot. The normal GUI event consumer must do it.
                if step == 13 {
                    let view = view.read(cx);
                    let (before, inbox) = baseline.as_ref().context("missing external baseline")?;
                    if !Arc::ptr_eq(inbox, &view.endpoints[view.selected_endpoint].connection.inbox) || !view.live.status.is_connected()
                        || view.endpoints[view.selected_endpoint].connection.handle.as_ref().is_none_or(|h| h.is_disconnected())
                        || view.local_error.is_some() || view.live.error.is_some() {
                        bail!("GUI connection changed or failed during external creation");
                    }
                    if external.is_none() {
                        let rx: &std::sync::mpsc::Receiver<Result<ExternalWorkspace>> = external_rx.as_ref().context("missing external receiver")?;
                        match rx.try_recv() {
                            Ok(result) => external = Some(result?),
                            Err(std::sync::mpsc::TryRecvError::Empty) => {},
                            Err(error) => return Err(error).context("external worker"),
                        }
                    }
                    if since.elapsed() > Duration::from_secs(12) {
                        bail!("external workspace worker/GUI timed out");
                    }
                    let Some(created) = &external else { return Ok(false) };
                    let elapsed = created.sent.elapsed();
                    if elapsed > EXTERNAL_TIMEOUT {
                        bail!("external workspace not consumed by GUI within {EXTERNAL_TIMEOUT:?}: elapsed={elapsed:?} id={} snapshot={:?}", created.id, view.live.snapshot);
                    }
                    let Some(snapshot) = &view.live.snapshot else { return Ok(false) };
                    let before: &Arc<ClientShellSnapshot> = before;
                    if snapshot.boot_id != before.boot_id || snapshot.focused_workspace_id != before.focused_workspace_id
                        || snapshot.focused_tab_id != before.focused_tab_id || snapshot.focused_pane_id != before.focused_pane_id {
                        bail!("external unfocused creation changed GUI boot/focus");
                    }
                    if !snapshot.workspaces.iter().any(|w| w.workspace_id == created.id && w.label == "external-gui-smoke") { return Ok(false); }
                    if snapshot.revision <= before.revision || snapshot.workspaces.len() != before.workspaces.len() + 1
                        || snapshot.tabs.len() != before.tabs.len() + 1 {
                        bail!("incorrect external workspace snapshot: {snapshot:?}");
                    }
                    eprintln!("GUI external workspace push verified: id={} revision={} -> {} command_to_observed_ms={} response_to_observed_ms={} bound_ms={} observation_poll_ms=100 unchanged_connection=true unchanged_focus=true no_refresh=true",
                        created.id, before.revision, snapshot.revision, elapsed.as_millis(), created.responded.elapsed().as_millis(), EXTERNAL_TIMEOUT.as_millis());
                    return Ok(true);
                }
                if frames == 0 {
                    // Exercise the regression: no foreground app or pre-existing input focus.
                    cx.hide();
                    window.blur(cx);
                }
                // on_next_frame runs BEFORE draw, and hidden windows may not receive it.
                // Build the real native window's dispatch tree and input handler synchronously,
                // without activating the app or relying on desktop/OS keyboard focus.
                window.focus(&view.read(cx).focus.clone(), cx);
                window.refresh();
                window.draw(cx).clear(cx);
                frames += 1;
                let focused = view.read(cx).focus.is_focused(window);
                let active = window.is_window_active();
                let actions_ready = window.is_action_available(&RunCommand { command: Command::Tab }, cx);
                let probe = view.read(cx).input_probe;
                let (live, local_error, options, last_queued_options, bounds) = {
                    let view = view.read(cx);
                    (view.live.clone(), view.local_error.clone(), view.options, view.last_queued_options, view.bounds)
                };
                let diagnostic = || format!(
                    "step={step} ({}) elapsed={:?} frames={frames} focus={focused} actions_ready={actions_ready} active={} probe={probe:?} status={:.160} local_error={:.240} live.error={:.240} connected={} snapshot={:?} surface={:?} size={:?} last_queued_options={last_queued_options:?}",
                    STEPS[step], since.elapsed(), active, live.status,
                    local_error.as_deref().unwrap_or("none"), live.error.as_deref().unwrap_or("none"), live.status.is_connected(),
                    live.snapshot.as_ref().map(|s| (s.revision, s.workspaces.len(), s.tabs.len())),
                    live.surface.as_ref().map(|s| (s.projection_revision, s.panes.len(), s.frame.width, s.frame.height)), options.surface_size
                );
                if local_error.is_some() || live.error.is_some() {
                    bail!(diagnostic());
                }
                if since.elapsed() > Duration::from_secs(20) {
                    bail!("timeout: {}", diagnostic());
                }
                if !focused || !actions_ready { return Ok(false); }
                let (Some(snapshot), Some(surface)) = (&live.snapshot, &live.surface) else { return Ok(false) };
                if !live.status.is_connected() || snapshot.boot_id != surface.boot_id || snapshot.revision != surface.projection_revision {
                    return Ok(false);
                }
                surface.frame.validate().with_context(|| format!("invalid frame; {}", diagnostic()))?;
                if let Some(error) = &snapshot.config_diagnostic {
                    bail!("config diagnostic: {error:?}; {}", diagnostic());
                }
                let focused_tab = snapshot.focused_tab_id.as_deref().unwrap_or_default();
                let focused_workspace = snapshot.focused_workspace_id.as_deref().unwrap_or_default();
                let key = |name: &str, window: &mut Window, cx: &mut App| -> Result<()> {
                    let before = view.read(cx).input_probe;
                    let key = Keystroke::parse(name).with_context(|| format!("parsing keystroke {name}"))?;
                    if !window.dispatch_keystroke(key, cx) { bail!("unhandled keystroke {name}"); }
                    let after = view.read(cx).input_probe;
                    let delivered = if name.starts_with("cmd-") {
                        after.actions == before.actions + 1
                    } else {
                        after.keys == before.keys + 1
                    };
                    if !delivered { bail!("keystroke {name} missed intended handler: before={before:?} after={after:?}; {}", diagnostic()); }
                    Ok(())
                };
                match step {
                    0 if !focused_tab.is_empty() && surface.panes.len() == 1 && bounds.size.width > px(0.) => {
                        boot = snapshot.boot_id.clone();
                        workspace = focused_workspace.into();
                        first_tab = focused_tab.into();
                        key("cmd-t", window, cx)?;
                    }
                    1 if snapshot.tabs.len() == 2 && focused_tab != first_tab && surface.panes.len() == 1 => {
                        second_tab = focused_tab.into();
                        split_pane = snapshot.focused_pane_id.clone().unwrap_or_default();
                        key("cmd-d", window, cx)?;
                    }
                    2 if surface.panes.len() == 2 => {
                        let old = surface.panes.iter().find(|p| p.pane_id == split_pane).context("original split pane missing")?;
                        let new = surface.panes.iter().find(|p| Some(&p.pane_id) == snapshot.focused_pane_id.as_ref()).context("focused split missing")?;
                        if new.rect.x <= old.rect.x || new.rect.y != old.rect.y { bail!("right split geometry: {}", diagnostic()); }
                        split_pane = new.pane_id.clone();
                        key("cmd-shift-d", window, cx)?;
                    }
                    3 if surface.panes.len() == 3 => {
                        let old = surface.panes.iter().find(|p| p.pane_id == split_pane).context("original split pane missing")?;
                        let new = surface.panes.iter().find(|p| Some(&p.pane_id) == snapshot.focused_pane_id.as_ref()).context("focused split missing")?;
                        if new.rect.y <= old.rect.y || new.rect.x != old.rect.x { bail!("down split geometry: {}", diagnostic()); }
                        window.dispatch_action(Box::new(RunCommand { command: Command::PreviousTab }), cx);
                    }
                    4 if focused_tab == first_tab && surface.panes.len() == 1 => {
                        window.dispatch_action(Box::new(RunCommand { command: Command::NextTab }), cx);
                    }
                    5 if focused_tab == second_tab && surface.panes.len() == 3 => {
                        key("cmd-shift-n", window, cx)?;
                    }
                    6 if snapshot.workspaces.len() == 2 && focused_workspace != workspace && surface.panes.len() == 1 => {
                        let before = view.read(cx).presentation.probe;
                        view.update(cx, |view, cx| { view.navigate(NavigationTarget::Workspace(&workspace), cx); window.focus(&view.focus, cx); });
                        // Draw the frame that follows the focus change immediately: the client
                        // has just dropped its surface and the next projection is a round trip
                        // away, which is precisely when the terminal area used to blank.
                        window.refresh();
                        window.draw(cx).clear(cx);
                        let after = view.read(cx).presentation.probe;
                        if after.blank > before.blank {
                            bail!("space switch blanked the terminal area: {} empty frame(s); {}", after.blank - before.blank, diagnostic());
                        }
                        switched = Some((Instant::now(), after));
                    }
                    7 if focused_workspace == workspace && focused_tab == second_tab && surface.panes.len() == 3 => {
                        let (started, before) = switched.take().context("missing space switch probe")?;
                        let probe = view.read(cx).presentation.probe;
                        if probe.blank > before.blank {
                            bail!("space switch blanked the terminal area: {} empty frame(s); {}", probe.blank - before.blank, diagnostic());
                        }
                        eprintln!("GUI space switch verified: blank_frames=0 retained_paints={} gap_observed_ms={} observation_poll_ms=100",
                            probe.retained - before.retained, started.elapsed().as_millis());
                        // Use the full-width tab so the exact output row cannot wrap in a split.
                        window.dispatch_action(Box::new(RunCommand { command: Command::PreviousTab }), cx);
                    }
                    8 if focused_tab == first_tab && surface.panes.len() == 1 => {
                        let command = format!("echo HERDR_GUI_{}\"_OK\"", std::process::id());
                        type_text(&command, &view, window, cx)?;
                        key("enter", window, cx)?;
                    }
                    9 if has_output(&surface.frame, &marker) => {
                        eprintln!("GUI shell output verified (not command echo): {marker}");
                        old_size = options.surface_size;
                        window.resize(fixture_size(1000., 650.));
                    }
                    10 if options.surface_size != old_size && last_queued_options == Some(options)
                        && surface.frame.width == options.surface_size.cols && surface.frame.height == options.surface_size.rows => {
                        eprintln!("GUI native resize verified: {:?} -> {:?}", old_size, options.surface_size);
                        view.update(cx, |view, cx| { view.reconnect(); window.focus(&view.focus, cx); cx.notify(); });
                    }
                    11 if snapshot.boot_id == boot && snapshot.workspaces.len() == 2 && snapshot.tabs.len() == 3
                        && focused_workspace == workspace && focused_tab == first_tab && has_output(&surface.frame, &marker) => {
                        type_text(&format!("echo HERDR_GUI_{}\"_OK_RECONNECTED\"", std::process::id()), &view, window, cx)?;
                        key("enter", window, cx)?;
                    }
                    12 if has_output(&surface.frame, &reconnected_marker) => {
                        eprintln!("GUI input pipeline verified: frames={frames} focus={focused} active={active} probe={probe:?}");
                        eprintln!("GUI fresh input after reconnect verified: {reconnected_marker}");
                        let target = view.read(cx).endpoints[view.read(cx).selected_endpoint].connection.target.clone();
                        if !matches!(&target, ConnectTarget::Socket(_)) {
                            bail!("external smoke requires an explicit isolated socket");
                        }
                        baseline = Some((snapshot.clone(), view.read(cx).endpoints[view.read(cx).selected_endpoint].connection.inbox.clone()));
                        let boot = boot.clone();
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::Builder::new().name("external-workspace-smoke".into()).spawn(move || {
                            let _ = tx.send(create_external_workspace(target, options, boot));
                        }).context("spawning external workspace worker")?;
                        external_rx = Some(rx);
                    }
                    _ => return Ok(false),
                }
                eprintln!("GUI step {step} ({}) verified; waiting for {}", STEPS[step], STEPS[step + 1]);
                step += 1;
                since = Instant::now();
                Ok(false)
            });
            match result {
                Ok(Ok(true)) => {
                    completed = true;
                    break;
                }
                Ok(Ok(false)) => {},
                error => {
                    EXIT_CODE.store(1, Ordering::SeqCst);
                    eprintln!("GUI integration FAIL: {error:?}");
                    cx.update(|cx| cx.quit());
                    break;
                }
            }
        }
        if completed {
            #[cfg(target_os = "macos")]
            if let Err(error) = clipboard::verify(handle, cx).await {
                EXIT_CODE.store(1, Ordering::SeqCst);
                eprintln!("GUI clipboard FAIL: {error:#}");
                cx.update(|cx| cx.quit());
                return;
            }
            #[cfg(target_os = "macos")]
            if let Err(error) = selection::verify(handle, cx).await {
                EXIT_CODE.store(1, Ordering::SeqCst);
                eprintln!("GUI selection FAIL: {error:#}");
                cx.update(|cx| cx.quit());
                return;
            }
            match second_window(handle, cx).await {
                Ok(()) => {
                    #[cfg(target_os = "macos")]
                    if let Err(error) = clipboard::verify_remote(cx).await {
                        EXIT_CODE.store(1, Ordering::SeqCst);
                        eprintln!("GUI remote clipboard FAIL: {error:#}");
                        cx.update(|cx| cx.quit());
                        return;
                    }
                    eprintln!("GUI integration PASS: same boot={boot}, 3 workspaces / 4 tabs, persisted shell output after reconnect, external workspace pushed to idle GUI, second window on its own space");
                    EXIT_CODE.store(0, Ordering::SeqCst);
                }
                Err(error) => {
                    EXIT_CODE.store(1, Ordering::SeqCst);
                    eprintln!("GUI second window FAIL: {error:#}");
                }
            }
            cx.update(|cx| cx.quit());
        }
    }).detach();
}

/// Two windows are two clients of one daemon. Each keeps its own focused space,
/// and neither one's navigation may move the other.
async fn second_window(first: WindowHandle<HerdrWindow>, cx: &mut AsyncApp) -> Result<()> {
    // Observe a window without leasing its root: drawing updates that entity.
    fn observe(
        handle: WindowHandle<HerdrWindow>,
        cx: &mut AsyncApp,
        draw: bool,
    ) -> Result<(LiveState, Option<String>)> {
        AnyWindowHandle::from(handle)
            .update(cx, |root, window, cx| -> Result<_> {
                let view = root
                    .downcast::<HerdrWindow>()
                    .map_err(|_| anyhow!("unexpected window root"))?;
                if draw {
                    // A hidden window still needs a draw to publish its geometry.
                    window.refresh();
                    window.draw(cx).clear(cx);
                }
                let view = view.read(cx);
                Ok((view.live.clone(), view.local_error.clone()))
            })
            .context("observing window")?
    }

    fn healthy(live: &LiveState, local_error: Option<&str>) -> Result<Option<(String, String)>> {
        if let Some(error) = local_error.or(live.error.as_deref()) {
            bail!("window error: {error:.240}");
        }
        let (Some(snapshot), Some(surface)) = (&live.snapshot, &live.surface) else {
            return Ok(None);
        };
        if !live.status.is_connected()
            || snapshot.boot_id != surface.boot_id
            || snapshot.revision != surface.projection_revision
        {
            return Ok(None);
        }
        surface.frame.validate().context("invalid surface frame")?;
        let Some(workspace) = snapshot.focused_workspace_id.clone() else {
            return Ok(None);
        };
        Ok(Some((snapshot.boot_id.clone(), workspace)))
    }

    let (target, inbox) = first
        .update(cx, |view, _, _| {
            let endpoint = &view.endpoints[view.selected_endpoint];
            (
                endpoint.connection.target.clone(),
                endpoint.connection.inbox.clone(),
            )
        })
        .context("reading the first window's connection")?;
    let (live, local_error) = observe(first, cx, false)?;
    let (boot, first_workspace) =
        healthy(&live, local_error.as_deref())?.context("first window is not ready")?;
    let elsewhere = live
        .snapshot
        .as_ref()
        .context("missing first snapshot")?
        .workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .find(|id| *id != first_workspace)
        .context("the daemon has only one workspace to show")?;
    let second = cx
        .update(|cx| open_window(target, updater::Updater::secondary(), cx, false))
        .context("opening a second window")?;
    if AnyWindowHandle::from(second) == AnyWindowHandle::from(first) {
        bail!("the second window replaced the first");
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut navigated = false;
    loop {
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
        let (second_live, second_error) = observe(second, cx, true)?;
        let (first_live, first_error) = observe(first, cx, false)?;
        let first_state = healthy(&first_live, first_error.as_deref())?;
        if first_state
            .as_ref()
            .map(|(_, workspace)| workspace.as_str())
            != Some(first_workspace.as_str())
        {
            bail!("the first window lost or changed its space: {first_state:?}");
        }
        if !Arc::ptr_eq(
            &inbox,
            &first
                .update(cx, |view, _, _| {
                    view.endpoints[view.selected_endpoint]
                        .connection
                        .inbox
                        .clone()
                })
                .context("re-reading the first window's inbox")?,
        ) {
            bail!("the first window's connection was replaced");
        }
        if let Some((second_boot, second_workspace)) =
            healthy(&second_live, second_error.as_deref())?
        {
            if second_boot != boot {
                bail!("the second window reached another daemon: {second_boot} != {boot}");
            }
            if !navigated {
                second
                    .update(cx, |view, _, cx| {
                        if view.input_ready() {
                            view.navigate(NavigationTarget::Workspace(&elsewhere), cx);
                            true
                        } else {
                            false
                        }
                    })
                    .context("navigating the second window")?
                    .then(|| navigated = true);
            } else if second_workspace == elsewhere {
                let panes = second_live
                    .surface
                    .as_ref()
                    .map_or(0, |surface| surface.panes.len());
                eprintln!(
                    "GUI second window verified: boot={boot} windows=2 first_space={first_workspace} second_space={second_workspace} second_panes={panes} separate_inbox=true"
                );
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "second window deadline: navigated={navigated} first={first_state:?} second_status={:.160} second_snapshot={:?} second_surface={:?}",
                second_live.status,
                second_live
                    .snapshot
                    .as_ref()
                    .map(|s| (s.revision, s.focused_workspace_id.clone())),
                second_live.surface.as_ref().map(|s| (
                    s.projection_revision,
                    s.frame.width,
                    s.frame.height
                ))
            );
        }
    }
}

fn type_text(
    text: &str,
    view: &Entity<HerdrWindow>,
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    for ch in text.chars() {
        let before = view.read(cx).input_probe.text;
        if !window.dispatch_keystroke(
            Keystroke {
                modifiers: Modifiers::default(),
                key: ch.to_string(),
                key_char: Some(ch.to_string()),
            },
            cx,
        ) {
            bail!("unhandled text keystroke {ch:?}");
        }
        if view.read(cx).input_probe.text != before + 1 {
            bail!("text keystroke {ch:?} missed native input handler");
        }
    }
    Ok(())
}

fn has_output(frame: &FrameData, marker: &str) -> bool {
    frame.width > 0
        && frame.cells.chunks(usize::from(frame.width)).any(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                // The daemon paints a scrollbar after the terminal's last column
                // once this fixture has produced more than a screen of output.
                .trim_end_matches(['▕', '▐'])
                .trim()
                == marker
        })
}

#[cfg(test)]
mod tests {
    use super::create_external_workspace;
    use anyhow::{Context as _, Result};
    use herdr_client::{ConnectOptions, ConnectTarget, protocol::ClientSurfaceSize};

    #[test]
    fn external_workspace_error_retains_client_source() -> Result<()> {
        // Invalid geometry fails before spawning a worker or opening a socket.
        let options = ConnectOptions {
            surface_size: ClientSurfaceSize { cols: 0, rows: 0 },
            ..ConnectOptions::default()
        };
        let error = create_external_workspace(
            ConnectTarget::Socket("/unused-smoke.sock".into()),
            options,
            "fixture-boot".into(),
        )
        .err()
        .context("invalid geometry unexpectedly connected")?;
        assert_eq!(error.to_string(), "connecting external workspace client");
        assert!(matches!(
            error.downcast_ref::<herdr_client::Error>(),
            Some(herdr_client::Error::EmptySurface)
        ));
        assert!(format!("{error:#}").contains("surface dimensions must be nonzero"));
        Ok(())
    }
}
