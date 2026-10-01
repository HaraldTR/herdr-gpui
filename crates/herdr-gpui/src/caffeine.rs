//! Keeps the display awake on request, like the Caffeine menu-bar app.
//!
//! The assertion belongs to a `caffeinate` child rather than to IOKit calls
//! made here, so no `unsafe` is needed. `-w` ties the child to this process:
//! a crash or quit releases the display without any cleanup on our side. The
//! state is app-wide, so every window's status bar shows the same cup.

use crate::Result;
use gpui::{App, Global};
use std::process::Child;

/// Only macOS ships `caffeinate`; other platforms do not show the toggle.
pub(crate) const SUPPORTED: bool = cfg!(target_os = "macos");

#[derive(Default)]
struct Caffeine {
    child: Option<Child>,
}

impl Global for Caffeine {}

impl Caffeine {
    /// Forgets a child that exited on its own, e.g. killed from a terminal,
    /// so the cup never claims an assertion nobody holds.
    fn reap(&mut self) {
        if self
            .child
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
        {
            self.child = None;
        }
    }
}

pub(crate) fn active(cx: &App) -> bool {
    cx.try_global::<Caffeine>()
        .is_some_and(|caffeine| caffeine.child.is_some())
}

/// Starts or stops keeping the display awake, and redraws every window.
pub(crate) fn toggle(cx: &mut App) -> Result<()> {
    toggle_with(cx, spawn)
}

fn toggle_with(cx: &mut App, start: impl FnOnce() -> std::io::Result<Child>) -> Result<()> {
    let caffeine = cx.default_global::<Caffeine>();
    caffeine.reap();
    if let Some(mut child) = caffeine.child.take() {
        // Killing never blocks; reaping the zombie does, so it waits elsewhere.
        let _ = child.kill();
        cx.background_executor()
            .spawn(async move {
                let _ = child.wait();
            })
            .detach();
    } else {
        caffeine.child = Some(start().map_err(crate::Error::Caffeine)?);
    }
    cx.refresh_windows();
    Ok(())
}

#[cfg(target_os = "macos")]
fn spawn() -> std::io::Result<Child> {
    use std::process::{Command, Stdio};
    // -d holds off display sleep and the screensaver; -i keeps the Mac awake
    // so agents keep running behind it.
    Command::new("/usr/bin/caffeinate")
        .args(["-d", "-i", "-w"])
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(not(target_os = "macos"))]
fn spawn() -> std::io::Result<Child> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::process::Command;

    fn sleeper() -> std::io::Result<Child> {
        Command::new("sleep").arg("30").spawn()
    }

    #[gpui::test]
    fn toggles_on_and_off_and_kills_the_child(cx: &mut gpui::TestAppContext) {
        assert!(!cx.update(|cx| active(cx)));
        cx.update(|cx| toggle_with(cx, sleeper)).unwrap();
        assert!(cx.update(|cx| active(cx)));
        let pid = cx.read_global::<Caffeine, _>(|c, _| c.child.as_ref().unwrap().id());
        cx.update(|cx| toggle_with(cx, || unreachable!())).unwrap();
        assert!(!cx.update(|cx| active(cx)));
        cx.run_until_parked();
        // The background reap means the pid no longer names our child.
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap();
        assert!(!alive.success());
    }

    #[gpui::test]
    fn failed_start_stays_off_and_keeps_its_source(cx: &mut gpui::TestAppContext) {
        let error = cx
            .update(|cx| toggle_with(cx, || Err(std::io::ErrorKind::NotFound.into())))
            .unwrap_err();
        assert!(
            matches!(&error, crate::Error::Caffeine(source) if source.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!cx.update(|cx| active(cx)));
    }

    #[gpui::test]
    fn a_child_that_exited_on_its_own_counts_as_off(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| toggle_with(cx, || Command::new("true").spawn()))
            .unwrap();
        let mut child = cx.update(|cx| cx.global_mut::<Caffeine>().child.take().unwrap());
        child.wait().unwrap();
        cx.update(|cx| cx.global_mut::<Caffeine>().child = Some(child));
        // The next click starts a fresh assertion instead of "stopping" a dead one.
        cx.update(|cx| toggle_with(cx, sleeper)).unwrap();
        assert!(cx.update(|cx| active(cx)));
        cx.update(|cx| toggle_with(cx, || unreachable!())).unwrap();
        cx.run_until_parked();
    }
}
