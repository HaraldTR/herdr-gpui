//! Native checks run only inside live_gui's isolated HOME/XDG fixture process.
use super::*;
use crate::sidebar::native_tests::Target;
use anyhow::{Context as _, Result, bail, ensure};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Layout([Option<Bounds<Pixels>>; 7]);
impl Global for Layout {}

pub(super) fn probe(index: usize) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, _, cx| {
            cx.default_global::<Layout>().0[index] = Some(bounds);
        },
    )
    .absolute()
    .size_full()
}

pub(crate) async fn verify_native(
    source: WindowHandle<HerdrWindow>,
    cx: &mut AsyncApp,
) -> Result<()> {
    #[cfg(feature = "mockup")]
    let capture = std::env::var_os("HERDR_TEST_SETTINGS_CAPTURE").map(std::path::PathBuf::from);
    let before = source.update(cx, |view, _, _| view.input_probe)?;
    AnyWindowHandle::from(source).update(cx, |_, window, cx| -> Result<()> {
        window.draw(cx).clear(cx);
        ensure!(
            window.dispatch_keystroke(Keystroke::parse("cmd-,")?, cx),
            "Settings shortcut was not handled"
        );
        Ok(())
    })??;
    let deadline = Instant::now() + Duration::from_secs(3);
    let settings = loop {
        let settings = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<SettingsWindow>())
        });
        if let Some(settings) = settings
            && settings.update(cx, |view, _, _| !view.loading)?
        {
            break settings;
        }
        ensure!(
            Instant::now() < deadline,
            "standalone Settings open/load timed out"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    };
    settings.update(cx, |view, window, _| -> Result<()> {
        ensure!(
            view.error.is_none() && !view.saving && view.focus.is_focused(window),
            "Settings load/focus mismatch: {:?}",
            view.error
        );
        Ok(())
    })??;
    let source_actions = source.update(cx, |view, _, _| view.input_probe.actions)?;

    for expected in [size(px(680.), px(560.)), size(px(960.), px(780.))] {
        settings.update(cx, |_, window, _| window.resize(expected))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !settings.update(cx, |_, window, _| window.viewport_size() == expected)? {
            ensure!(
                Instant::now() < deadline,
                "Settings resize to {expected:?} timed out"
            );
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        }
        for (index, section) in Section::ALL.into_iter().enumerate() {
            let (target, bounds) =
                AnyWindowHandle::from(settings).update(cx, |_, window, cx| -> Result<_> {
                    window.draw(cx).clear(cx);
                    let bounds = cx.global::<Layout>().0[index]
                        .context("missing Settings category paint")?;
                    Ok((Target::acquire(window)?, bounds))
                })??;
            target.click(
                f64::from(f32::from(bounds.center().x)),
                f64::from(f32::from(bounds.center().y)),
            )?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !settings.update(cx, |view, _, _| view.section == section)? {
                ensure!(
                    Instant::now() < deadline,
                    "native category click {section:?} timed out"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
            AnyWindowHandle::from(settings).update(cx, |root, window, cx| -> Result<()> {
                window.draw(cx).clear(cx);
                let view = root
                    .downcast::<SettingsWindow>()
                    .map_err(|_| anyhow::anyhow!("unexpected Settings root"))?;
                let view = view.read(cx);
                let body = view.body_scroll.bounds();
                ensure!(
                    (body.left() - px(184.)).abs() < px(1.)
                        && (body.right() - expected.width).abs() < px(1.),
                    "Settings body width at {expected:?}/{section:?}: {body:?}"
                );
                ensure!(
                    body.top() >= px(crate::titlebar::HEIGHT)
                        && body.bottom() <= expected.height - px(36.)
                        && body.size.height > px(400.),
                    "Settings body height: {body:?}"
                );
                let mut bottom = px(crate::titlebar::HEIGHT);
                for (index, bounds) in cx.global::<Layout>().0.iter().enumerate() {
                    let bounds = bounds.context("unpainted sidebar category")?;
                    ensure!(
                        bounds.left() >= px(0.)
                            && bounds.right() <= body.left()
                            && bounds.top() >= bottom
                            && bounds.bottom() <= body.bottom()
                            && bounds.size.height >= px(35.),
                        "Settings category {index} clipped/overlapped: {bounds:?}"
                    );
                    bottom = bounds.bottom();
                }
                ensure!(!view.saving, "navigation started a settings write");
                Ok(())
            })??;
        }
    }
    settings.update(cx, |view, window, cx| {
        view.select_section(Section::Appearance, window, cx)
    })?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !settings.update(cx, |view, _, cx| {
        !view.busy() && !view.native_theme_search(cx).2
    })? {
        ensure!(
            Instant::now() < deadline,
            "Settings theme catalog did not settle"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    let (unfiltered, saved_theme) = settings.update(cx, |view, window, cx| {
        let (focus, count, _) = view.native_theme_search(cx);
        window.focus(&focus, cx);
        (count, view.config.theme.clone())
    })?;
    ensure!(
        unfiltered > 1,
        "native theme catalog has no searchable choices"
    );
    AnyWindowHandle::from(settings).update(cx, |_, window, cx| -> Result<()> {
        window.draw(cx).clear(cx);
        for key in ["n", "o", "r", "d"] {
            ensure!(
                window.dispatch_keystroke(Keystroke::parse(key)?, cx),
                "theme search key {key} was not handled"
            );
        }
        Ok(())
    })??;
    let deadline = Instant::now() + Duration::from_secs(3);
    let filtered = loop {
        let ready = settings.update(cx, |view, window, cx| -> Result<_> {
            let (focus, count, busy) = view.native_theme_search(cx);
            ensure!(
                focus.is_focused(window),
                "typing lost the theme search focus"
            );
            ensure!(
                view.config.theme == saved_theme && !view.saving,
                "theme search saved a preference"
            );
            Ok((!view.loading && !busy && count > 0 && count < unfiltered).then_some(count))
        })??;
        if let Some(count) = ready {
            break count;
        }
        ensure!(
            Instant::now() < deadline,
            "native Nord search did not narrow the catalog"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    };

    // Config::load has completed its sandbox-only maintenance by this point.
    // Browsing must now leave the exact bytes alone until the close boundary.
    let (config_path, original_bytes) = cx
        .background_executor()
        .spawn(async {
            let path = Config::local_path()?;
            let bytes = std::fs::read(&path).context("read sandbox Settings baseline")?;
            anyhow::Ok((path, bytes))
        })
        .await?;
    let source_config = source.update(cx, |view, _, _| view.config.clone())?;
    for name in ["Nord", "Dracula", "Nord"] {
        let deadline = Instant::now() + Duration::from_secs(3);
        let expected = loop {
            let ready = settings.update(cx, |view, _, cx| -> Result<_> {
                ensure!(
                    !view.saving,
                    "browsing started a save before selecting {name}"
                );
                // A background byte check can yield to the normal file watcher.
                // Select and check synchronously once that unrelated read settles.
                if view.loading {
                    return Ok(None);
                }
                view.accept_theme_choice(
                    themes::Choice {
                        scope: themes::Scope::App,
                        name: name.into(),
                    },
                    cx,
                );
                let expected = Theme::builtin(name)
                    .context("native fixture theme must be built-in")?
                    .with_contrast(view.config.contrast);
                ensure!(
                    view.config.theme == name && view.theme == expected,
                    "{name} did not synchronously update Settings chrome"
                );
                ensure!(
                    view.theme_dirty() && !view.busy() && !view.saving,
                    "{name} started a save instead of remaining an idle draft"
                );
                let appearance = cx.global::<crate::app::InitialAppearance>();
                ensure!(
                    appearance.config.theme == name && appearance.theme == expected,
                    "{name} did not update the appearance used by new windows"
                );
                Ok(Some(expected))
            })??;
            if let Some(expected) = ready {
                break expected;
            }
            ensure!(
                Instant::now() < deadline,
                "Settings load did not settle before selecting {name}"
            );
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        };
        source.update(cx, |view, _, _| -> Result<()> {
            ensure!(
                view.config.theme == name && view.theme == expected,
                "{name} did not synchronously reach the source window"
            );
            for (before, after) in [
                (&source_config.sidebar, &view.config.sidebar),
                (&source_config.tabs, &view.config.tabs),
                (&source_config.terminal, &view.config.terminal),
                (&source_config.ui, &view.config.ui),
            ] {
                ensure!(
                    before.family == after.family
                        && before.size == after.size
                        && before.fallbacks == after.fallbacks,
                    "theme broadcast changed source fonts"
                );
            }
            Ok(())
        })??;
        for window in [AnyWindowHandle::from(source), settings.into()] {
            window.update(cx, |_, window, cx| window.draw(cx).clear(cx))?;
        }
        let path = config_path.clone();
        let bytes = cx
            .background_executor()
            .spawn(async move { std::fs::read(path) })
            .await?;
        ensure!(
            bytes == original_bytes,
            "selecting {name} wrote the sandbox config before close"
        );
    }
    AnyWindowHandle::from(settings).update(cx, |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;

    #[cfg(feature = "mockup")]
    if let Some(path) = capture {
        ensure!(
            path.is_absolute(),
            "HERDR_TEST_SETTINGS_CAPTURE must be an absolute destination"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let image = loop {
            let image =
                AnyWindowHandle::from(settings).update(cx, |root, window, cx| -> Result<_> {
                    let view = root
                        .downcast::<SettingsWindow>()
                        .map_err(|_| anyhow::anyhow!("unexpected Settings root"))?;
                    if view.read(cx).busy() || view.read(cx).native_theme_search(cx).2 {
                        return Ok(None);
                    }
                    // Draw and capture in the same update: a watcher reload between
                    // separate callbacks must not turn the capture into a loading frame.
                    window.draw(cx).clear(cx);
                    Ok(Some(window.render_to_image()?))
                })??;
            if let Some(image) = image {
                break image;
            }
            ensure!(
                Instant::now() < deadline,
                "Settings GPU capture did not settle"
            );
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        };
        let printed_path = path.clone();
        cx.background_executor()
            .spawn(async move {
                let mut png = Vec::new();
                image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
                std::fs::write(&path, png).context("save Settings GPU capture")?;
                anyhow::Ok(())
            })
            .await?;
        eprintln!("SIDEBAR Settings GPU capture: {}", printed_path.display());
    }

    AnyWindowHandle::from(settings).update(cx, |_, window, cx| -> Result<()> {
        window.dispatch_keystroke(Keystroke::parse("cmd-a")?, cx);
        window.dispatch_keystroke(Keystroke::parse("backspace")?, cx);
        Ok(())
    })??;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !settings.update(cx, |view, _, cx| {
        view.native_theme_search(cx).1 == unfiltered
    })? {
        ensure!(
            Instant::now() < deadline,
            "clearing the native theme query did not restore the catalog"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }

    source.update(cx, |view, window, cx| view.open_preferences(window, cx))?;
    // Opening is deferred to leave the source entity's update first.
    cx.background_executor()
        .timer(Duration::from_millis(10))
        .await;
    let windows = cx.update(|cx| cx.windows());
    ensure!(
        windows.len() == 2 && windows.contains(&settings.into()),
        "Settings singleton was not reused"
    );
    let path = config_path.clone();
    let bytes = cx
        .background_executor()
        .spawn(async move { std::fs::read(path) })
        .await?;
    ensure!(
        bytes == original_bytes,
        "theme draft was persisted before the close command"
    );
    settings.update(cx, |view, _, _| -> Result<()> {
        ensure!(
            view.theme_dirty() && !view.saving && view.config.theme == "Nord",
            "final Nord draft was lost before closing Settings"
        );
        Ok(())
    })??;
    AnyWindowHandle::from(settings).update(cx, |_, window, cx| -> Result<()> {
        window.draw(cx).clear(cx);
        ensure!(
            window.dispatch_keystroke(Keystroke::parse("cmd-,")?, cx),
            "Settings Cmd-, was not handled"
        );
        window.dispatch_keystroke(Keystroke::parse("cmd-w")?, cx);
        Ok(())
    })??;
    let deadline = Instant::now() + Duration::from_secs(2);
    while cx.update(|cx| cx.windows()) != vec![source.into()] {
        ensure!(
            Instant::now() < deadline,
            "Cmd-W did not close only Settings"
        );
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    cx.background_executor()
        .spawn(async move {
            let saved_bytes = std::fs::read(config_path).context("read closed Settings config")?;
            let mut expected: toml::Table = toml::from_str(std::str::from_utf8(&original_bytes)?)?;
            let actual: toml::Table = toml::from_str(std::str::from_utf8(&saved_bytes)?)?;
            ensure!(
                actual.get("theme").and_then(toml::Value::as_str) == Some("Nord"),
                "closing Settings did not persist the final Nord choice"
            );
            expected.insert("theme".into(), toml::Value::String("Nord".into()));
            ensure!(
                actual == expected,
                "theme close changed unrelated sandbox config fields"
            );
            anyhow::Ok(())
        })
        .await?;
    source.update(cx, |view, window, _| -> Result<()> {
        let after = view.input_probe;
        if view.menu.page.is_some()
            || !view.focus.is_focused(window)
            || before.text != after.text
            || before.keys != after.keys
            || source_actions != after.actions
            || view.native_settings_save_in_flight()
        {
            bail!("Settings affected source input/focus or started a save");
        }
        Ok(())
    })??;
    eprintln!(
        "SIDEBAR native standalone settings PASS: all 7 category clicks and bounds at 680x560/960x780, native Nord search {filtered}/{unfiltered} and clear, singleton Cmd-comma, isolated Cmd-W; theme draft live across windows; final theme persisted on close in sandbox"
    );
    Ok(())
}
