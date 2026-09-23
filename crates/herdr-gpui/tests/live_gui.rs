//! Requires an active native desktop and an explicitly selected daemon executable.
//! The daemon and its sandbox are POSIX, so the harness compiles on Unix only.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../test-support/sandbox.rs"]
mod sandbox;

use sandbox::{Sandbox, daemon_binary, stop_children};
use std::{
    ffi::OsString,
    fs,
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

struct Isolated {
    sandbox: Sandbox,
    daemon: Option<Child>,
    gui: Option<Child>,
}

fn gui_command(sandbox: &Sandbox, mut env: impl FnMut(&str) -> Option<OsString>) -> Command {
    let mut command = sandbox.command(env!("CARGO_BIN_EXE_herdr-gpui"), "gui.log");
    // Only the GUI needs desktop transport; keep HOME, XDG and daemon paths private.
    for name in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"] {
        if let Some(mut value) = env(name) {
            if name == "WAYLAND_DISPLAY"
                && !value.is_empty()
                && Path::new(&value).is_relative()
                && let Some(runtime) =
                    env("XDG_RUNTIME_DIR").filter(|dir| Path::new(dir).is_absolute())
            {
                // Relative Wayland sockets belong to the parent desktop, not our runtime dir.
                value = Path::new(&runtime).join(value).into_os_string();
            }
            command.env(name, value);
        }
    }
    command
}

#[test]
#[ignore = "requires active native desktop; GUI-only fixtures, no daemon"]
fn native_sidebar() {
    native_fixture(false);
}

#[test]
#[ignore = "requires active native desktop; notification fixtures, no daemon"]
fn native_notifications() {
    native_fixture(true);
}

fn native_fixture(notifications_only: bool) {
    let mut isolated = Isolated {
        sandbox: Sandbox::new(),
        daemon: None,
        gui: None,
    };
    let mut command = gui_command(&isolated.sandbox, |name| std::env::var_os(name));
    if notifications_only {
        command.env("HERDR_TEST_NOTIFICATIONS_ONLY", "1");
    }
    isolated.gui = Some(command.arg("--sidebar-test").spawn().unwrap());
    let gui = isolated.gui.as_mut().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while gui.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = gui.kill();
            let _ = gui.wait();
            panic!("native sidebar timeout");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let status = gui.wait().unwrap();
    let log = fs::read_to_string(isolated.sandbox.dir.join("gui.log")).unwrap();
    eprintln!("{log}");
    assert!(status.success(), "native sidebar failed");
    assert!(log.contains(if notifications_only {
        "NOTIFICATIONS native PASS:"
    } else {
        "SIDEBAR native PASS:"
    }));
}

impl Drop for Isolated {
    fn drop(&mut self) {
        stop_children([&mut self.gui, &mut self.daemon]);
        for name in ["gui.log", "daemon.log"] {
            if let Ok(log) = fs::read_to_string(self.sandbox.dir.join(name)) {
                eprintln!("{name} (last 40 lines, at most 600 chars each):");
                let lines = log.lines().rev().take(40).collect::<Vec<_>>();
                for line in lines.into_iter().rev() {
                    eprintln!("{}", line.chars().take(600).collect::<String>());
                }
            }
        }
    }
}

#[test]
#[ignore = "requires active desktop and explicit HERDR_TEST_BINARY; launches a native GUI and isolated daemon"]
fn native_gui_live() {
    let binary = daemon_binary();
    let mut isolated = Isolated {
        sandbox: Sandbox::new(),
        daemon: None,
        gui: None,
    };
    let socket = isolated.sandbox.socket();
    isolated.daemon = Some(
        isolated
            .sandbox
            .command(binary, "daemon.log")
            .arg("server")
            .spawn()
            .unwrap(),
    );
    isolated
        .sandbox
        .wait_for_daemon(isolated.daemon.as_mut().unwrap(), Duration::from_secs(20));
    let mut gui_command = gui_command(&isolated.sandbox, |name| std::env::var_os(name));
    isolated.gui = Some(
        gui_command
            .arg("--socket")
            .arg(&socket)
            .arg("--integration-test")
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = isolated.gui.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "native GUI timeout (active desktop required)"
        );
        assert!(
            isolated
                .daemon
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "daemon died while GUI was running"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "GUI failed: {status}");
    let log = fs::read_to_string(isolated.sandbox.dir.join("gui.log")).unwrap();
    assert!(
        log.contains("GUI integration PASS:"),
        "GUI exited without completing harness"
    );
    assert!(
        log.contains("GUI input pipeline verified:"),
        "GUI did not verify native action, key, and text delivery"
    );
    assert!(
        log.contains("GUI second window verified:") && log.contains("windows=2 first_space="),
        "GUI did not verify a second window on its own space"
    );
    assert!(
        log.contains("GUI external workspace push verified:")
            && log.contains("bound_ms=3000 observation_poll_ms=100 unchanged_connection=true unchanged_focus=true no_refresh=true")
            && log.contains("3 workspaces / 4 tabs"),
        "GUI did not verify bounded external workspace delivery without refresh/reconnect"
    );
    assert!(
        isolated
            .daemon
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none(),
        "GUI exit killed daemon"
    );
    eprintln!(
        "GUI exited successfully; isolated daemon is still alive; cleaning up only owned children"
    );
}

#[test]
fn gui_desktop_environment_preserves_sandbox_isolation() {
    use std::{collections::BTreeMap, ffi::OsStr, os::unix::ffi::OsStrExt};

    let sandbox = Sandbox::new();
    let daemon = sandbox.command("/usr/bin/env", "daemon.log");
    let base = daemon.get_envs().collect::<BTreeMap<_, _>>();
    assert_eq!(
        base[OsStr::new("XDG_RUNTIME_DIR")],
        Some(sandbox.dir.as_os_str())
    );
    for name in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"] {
        assert!(!base.contains_key(OsStr::new(name)));
    }
    assert_eq!(
        gui_command(&sandbox, |_| None)
            .get_envs()
            .collect::<BTreeMap<_, _>>(),
        base
    );

    for (display, runtime, expected) in [
        (
            Some("wayland-1"),
            Some("/run/user/1000"),
            Some("/run/user/1000/wayland-1"),
        ),
        (
            Some("nested/wayland-1"),
            Some("/run/user/1000"),
            Some("/run/user/1000/nested/wayland-1"),
        ),
        (
            Some("/desktop/wayland-1"),
            Some("/run/user/1000"),
            Some("/desktop/wayland-1"),
        ),
        (Some("/desktop/wayland-1"), None, Some("/desktop/wayland-1")),
        (Some("wayland-1"), None, Some("wayland-1")),
        (Some("wayland-1"), Some("relative"), Some("wayland-1")),
        (Some("wayland-1"), Some(""), Some("wayland-1")),
        (Some(""), Some("/run/user/1000"), Some("")),
        (None, Some("/run/user/1000"), None),
    ] {
        let command = gui_command(&sandbox, |name| match name {
            "DISPLAY" => Some(":42".into()),
            "XAUTHORITY" => Some("/desktop/auth".into()),
            "WAYLAND_DISPLAY" => display.map(Into::into),
            "XDG_RUNTIME_DIR" => runtime.map(Into::into),
            _ => panic!("must not inherit {name}"),
        });
        let mut expected_env = base.clone();
        expected_env.insert(OsStr::new("DISPLAY"), Some(OsStr::new(":42")));
        expected_env.insert(OsStr::new("XAUTHORITY"), Some(OsStr::new("/desktop/auth")));
        if let Some(expected) = expected {
            expected_env.insert(OsStr::new("WAYLAND_DISPLAY"), Some(OsStr::new(expected)));
        }
        assert_eq!(command.get_envs().collect::<BTreeMap<_, _>>(), expected_env);
        assert_eq!(command.get_current_dir(), daemon.get_current_dir());
    }

    let command = gui_command(&sandbox, |name| match name {
        "WAYLAND_DISPLAY" => Some(OsStr::from_bytes(b"wayland-\xff").to_owned()),
        "XDG_RUNTIME_DIR" => Some(OsStr::from_bytes(b"/run/user/\xfe").to_owned()),
        _ => None,
    });
    let env = command.get_envs().collect::<BTreeMap<_, _>>();
    assert_eq!(
        env[OsStr::new("WAYLAND_DISPLAY")],
        Some(OsStr::from_bytes(b"/run/user/\xfe/wayland-\xff"))
    );
}
