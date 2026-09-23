//! Homebrew-cask delegation. A cask-managed installation is owned by Homebrew:
//! replacing the bundle in place would leave `brew list --cask --versions`
//! reporting a version that is no longer installed, and the next `brew upgrade`
//! would overwrite the self-installed app. So the app runs Homebrew instead.
//!
//! Homebrew verifies the cask's own SHA-256 and updates its receipts, but this
//! path deliberately does not run the signed-manifest or designated-requirement
//! checks in `install`: trust moves to Homebrew and the tap. Detection therefore
//! has to prove the cask really owns this exact bundle before delegating.
//!
//! Homebrew trashes the running bundle while upgrading it, so bundle resources
//! may be gone until the user restarts. Restart is offered as soon as the
//! upgrade lands, and the upgrade itself is never interrupted: killing Homebrew
//! mid-move can leave no installed app at all.

use super::error::{Result, UpdateError as Error};
use super::release;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "herdr-gpui";
const BUNDLE: &str = "Herdr.app";
/// Homebrew's two standard prefixes. `PATH` is not consulted: a writable
/// directory earlier in a user's `PATH` must not decide what the app executes.
const PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];
const UPGRADE: Duration = Duration::from_secs(30 * 60);
const QUERY: Duration = Duration::from_secs(60);
const RELAUNCH: Duration = Duration::from_secs(30);
const DETAIL: usize = 120;
/// Homebrew's output is unbounded; the diagnostics built from it are not.
const TAIL: usize = 8;
/// How long to keep draining the pipes after the process exits.
const DRAIN: Duration = Duration::from_secs(5);

pub(super) struct Cask {
    brew: PathBuf,
    prefix: PathBuf,
    bundle: PathBuf,
    home: OsString,
}

/// Executables and their directories must not be writable by anyone but their
/// owner, who must be this user or root.
fn trusted(path: &Path, uid: u32, directory: bool) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path).map_err(Error::Io)?;
    if meta.file_type().is_symlink()
        || meta.is_dir() != directory
        || (meta.uid() != uid && meta.uid() != 0)
        || meta.mode() & 0o022 != 0
    {
        return Err(Error::UnsafeBrew(path.to_owned()));
    }
    Ok(meta)
}

/// Some(cask) only when Homebrew's own records point at this exact bundle.
/// A missing or unrelated Homebrew is not an error: the app is then standalone.
pub(super) fn detect(bundle: &Path, uid: u32) -> Option<Cask> {
    let home = env::var_os("HOME")?;
    let prefixes = env::var_os("HOMEBREW_PREFIX")
        .map(PathBuf::from)
        .into_iter()
        .chain(PREFIXES.iter().map(PathBuf::from));
    locate(bundle, uid, home, prefixes)
}

/// Prefixes and HOME are arguments, not environment reads, so this stays
/// testable without mutating process-wide state.
fn locate(
    bundle: &Path,
    uid: u32,
    home: OsString,
    prefixes: impl IntoIterator<Item = PathBuf>,
) -> Option<Cask> {
    for prefix in prefixes {
        let brew = prefix.join("bin/brew");
        let owns = trusted(&prefix, uid, true).is_ok()
            && trusted(&brew, uid, false).is_ok_and(|meta| meta.mode() & 0o111 != 0)
            && owned_bundle(&prefix, bundle);
        if owns {
            return Some(Cask {
                brew,
                prefix,
                bundle: bundle.to_owned(),
                home,
            });
        }
    }
    None
}

/// Homebrew moves an `app` artifact to its target and leaves a symlink to it in
/// the Caskroom. That link, not a version string, is what proves ownership.
fn owned_bundle(prefix: &Path, bundle: &Path) -> bool {
    let caskroom = prefix.join("Caskroom").join(TOKEN);
    let Ok(entries) = fs::read_dir(&caskroom) else {
        return false;
    };
    entries.filter_map(std::result::Result::ok).any(|entry| {
        let link = entry.path().join(BUNDLE);
        fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink())
            && fs::read_link(&link).is_ok_and(|target| target == bundle)
    })
}

fn command(cask: &Cask) -> Command {
    let mut command = Command::new(&cask.brew);
    // Homebrew needs a usable environment, so it cannot be cleared entirely.
    // Pass only what it requires, and never inherit the user's PATH.
    command
        .env_clear()
        .env("HOME", &cask.home)
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                cask.prefix.join("bin").display()
            ),
        )
        .env("LC_ALL", "C")
        .env("HOMEBREW_NO_ANALYTICS", "1")
        .env("HOMEBREW_NO_COLOR", "1")
        .env("HOMEBREW_NO_EMOJI", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        // No terminal is attached, so anything that wants an answer must fail
        // rather than wait for one that can never arrive.
        .stdin(Stdio::null());
    command
}

/// One progress line: bounded, and stripped of control bytes because it is
/// rendered directly. Homebrew output is data, never markup or escapes.
fn detail(line: &str) -> Option<String> {
    let text: String = line
        .chars()
        .filter(|c| !c.is_control())
        .take(DETAIL)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Keep the last few lines for diagnostics, reporting each one exactly once.
fn record(tail: &mut Vec<String>, text: String, progress: &mut impl FnMut(String)) {
    if tail.len() == TAIL {
        tail.remove(0);
    }
    tail.push(text.clone());
    progress(text);
}

/// Run Homebrew, reporting progress as it goes.
///
/// `install::output` cannot serve here: it clears the environment, caps at 30
/// seconds, and only reads a pipe once it closes, so it can neither run an
/// upgrade nor show that one is progressing.
fn run(
    mut command: Command,
    deadline: Duration,
    cancel: Option<&AtomicBool>,
    mut progress: impl FnMut(String),
) -> Result<Vec<String>> {
    if deadline.is_zero() {
        return Err(Error::BrewTimeout);
    }
    let start = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::Io)?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::MissingValidationPipes);
    };
    let (sender, receiver) = mpsc::channel();
    for pipe in [
        Box::new(stdout) as Box<dyn std::io::Read + Send>,
        Box::new(stderr),
    ] {
        let sender = sender.clone();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                // A reader that stops early would block Homebrew on a full pipe.
                if sender.send(line.ok()).is_err() {
                    return;
                }
            }
        });
    }
    drop(sender);
    // Only the tail is retained: Homebrew output is unbounded, diagnostics are not.
    let mut tail: Vec<String> = Vec::new();
    // What ended the run, not yet why: a failure's detail is the last line the
    // process produced, which the drain below may not have recovered yet.
    let ended = loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            break Ended::Error(Error::Cancelled);
        }
        if start.elapsed() > deadline {
            break Ended::Error(Error::BrewTimeout);
        }
        while let Ok(line) = receiver.try_recv() {
            let Some(text) = line.as_deref().and_then(detail) else {
                continue;
            };
            record(&mut tail, text, &mut progress);
        }
        match child.try_wait().map_err(Error::Io)? {
            Some(status) if !status.success() => break Ended::Failed(status),
            Some(_) => break Ended::Exited,
            None => thread::sleep(Duration::from_millis(50)),
        }
    };
    if !matches!(ended, Ended::Exited) {
        let _ = child.kill();
    }
    let _ = child.wait();
    // The child exits before its readers have necessarily forwarded everything,
    // so waiting on the readers is what drains the pipes: try_recv sees an empty
    // channel and discards output still in flight, which loses the version line
    // installed() parses and the lines these diagnostics are built from. Both
    // readers hold a sender and the original was dropped above, so the channel
    // disconnects once they reach EOF. Bounded, because a grandchild that
    // inherited the pipes can hold them open after Homebrew itself exits, and a
    // lost line must not become a hung update.
    //
    // These lines are reported like any other. A short command can exit with
    // its whole output still in flight, and whether a line reaches the caller
    // must not depend on which side of the exit it was read on.
    let drain = Instant::now() + DRAIN;
    loop {
        let remaining = drain.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(line) = receiver.recv_timeout(remaining) else {
            break;
        };
        let Some(text) = line.as_deref().and_then(detail) else {
            continue;
        };
        record(&mut tail, text, &mut progress);
    }
    match ended {
        Ended::Exited => Ok(tail),
        // A command can exit with the line that says why still in the pipe, so
        // the detail is taken from the drained output, not from the race.
        Ended::Failed(status) => Err(Error::BrewFailed {
            status,
            detail: tail.last().cloned().unwrap_or_default(),
        }),
        Ended::Error(error) => Err(error),
    }
}

/// How a run ended, before its output has finished arriving.
enum Ended {
    Exited,
    Failed(std::process::ExitStatus),
    Error(Error),
}

/// The version Homebrew currently records as installed for the cask.
fn installed(cask: &Cask, timeout: Duration) -> Result<String> {
    let mut command = command(cask);
    command.args(["list", "--cask", "--versions", TOKEN]);
    let lines = run(command, timeout.min(QUERY), None, |_| ())?;
    lines
        .iter()
        .rev()
        .find_map(|line| {
            let (name, version) = line.split_once(char::is_whitespace)?;
            (name == TOKEN)
                .then(|| version.split_whitespace().next_back())
                .flatten()
                .map(str::to_owned)
        })
        .ok_or(Error::BrewVersion)
}

/// Upgrade through Homebrew and confirm it reached at least the offered release
/// and is newer than the running app. Refresh stale metadata and retry once.
///
/// Homebrew is not interrupted once it starts, so cancellation is only honoured
/// before the first mutation is spawned. All attempts share one time budget.
pub(super) fn upgrade(
    cask: &Cask,
    current: &str,
    expected: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
) -> Result<String> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let old = release::parse_version(current).ok_or(Error::CurrentVersion)?;
    let offered = release::parse_version(expected).ok_or(Error::ReleaseVersion)?;
    let start = Instant::now();
    let remaining = || UPGRADE.saturating_sub(start.elapsed());
    let mut refreshed = false;
    loop {
        progress(if refreshed {
            "Retrying Homebrew cask upgrade after refresh...".to_owned()
        } else {
            "Asking Homebrew to upgrade the cask...".to_owned()
        });
        if !refreshed && cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let mut upgrade = command(cask);
        // Keep the initial auto-update enabled. On retry the explicit refresh
        // has already run, so do not ask Homebrew to refresh a second time.
        if refreshed {
            upgrade.env("HOMEBREW_NO_AUTO_UPDATE", "1");
        }
        upgrade.args(["upgrade", "--cask", TOKEN]);
        run(upgrade, remaining(), None, &mut progress)?;
        progress("Checking the installed Homebrew cask version...".to_owned());
        let installed = installed(cask, remaining())?;
        let new = release::parse_version(&installed).ok_or(Error::BrewVersion)?;
        if new > old && new >= offered {
            return Ok(installed);
        }
        if refreshed {
            return Err(Error::BrewStale {
                installed,
                current: current.to_owned(),
                expected: expected.to_owned(),
            });
        }
        // A successful upgrade can still use cached metadata or a tap that has
        // not published the offer yet. Command failures never reach this retry.
        progress("Refreshing Homebrew metadata with brew update...".to_owned());
        let mut update = command(cask);
        update.arg("update");
        run(update, remaining(), None, &mut progress)?;
        refreshed = true;
    }
}

/// Start the upgraded app, leaving the caller to quit once it is up. `open -n`
/// launches the new bundle rather than activating this still-running instance.
pub(super) fn relaunch(cask: &Cask) -> Result<()> {
    let mut command = Command::new("/usr/bin/open");
    command
        .env_clear()
        .env("LC_ALL", "C")
        .arg("-n")
        .arg(&cask.bundle)
        .stdin(Stdio::null());
    run(command, RELAUNCH, None, |_| ()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the macOS-only opt-in tests below use it.
    #[cfg(target_os = "macos")]
    use anyhow::Context as _;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// A Homebrew prefix that owns `target`, returning the artifact link.
    fn caskroom(prefix: &Path, version: &str, target: &Path) -> anyhow::Result<PathBuf> {
        let bin = prefix.join("bin");
        fs::create_dir_all(&bin)?;
        fs::write(bin.join("brew"), b"#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(bin.join("brew"), fs::Permissions::from_mode(0o755))?;
        let versioned = prefix.join("Caskroom").join(TOKEN).join(version);
        fs::create_dir_all(&versioned)?;
        let link = versioned.join(BUNDLE);
        symlink(target, &link)?;
        Ok(link)
    }

    #[test]
    fn detection_requires_homebrew_to_own_this_exact_bundle() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        let prefix = root.join("prefix");
        let bundle = root.join("Applications/Herdr.app");
        let other = root.join("Applications/Other.app");
        fs::create_dir_all(&bundle)?;
        fs::create_dir_all(&other)?;
        let uid = fs::metadata(&root)?.uid();
        let found = |bundle: &Path| {
            locate(bundle, uid, root.clone().into(), [prefix.clone()]).map(|cask| cask.brew)
        };
        assert!(found(&bundle).is_none(), "no Caskroom yet");

        let link = caskroom(&prefix, "20260921.1", &other)?;
        assert!(found(&bundle).is_none(), "cask owns another bundle");
        fs::remove_file(&link)?;
        symlink(&bundle, &link)?;
        assert_eq!(found(&bundle), Some(prefix.join("bin/brew")));

        let brew = prefix.join("bin/brew");
        fs::set_permissions(&brew, fs::Permissions::from_mode(0o777))?;
        assert!(found(&bundle).is_none(), "world-writable brew");
        fs::set_permissions(&brew, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o777))?;
        assert!(found(&bundle).is_none(), "world-writable prefix");
        fs::set_permissions(&prefix, fs::Permissions::from_mode(0o755))?;
        assert!(found(&bundle).is_some());

        fs::remove_file(&brew)?;
        assert!(found(&bundle).is_none(), "no brew executable");
        Ok(())
    }

    fn cask(script: &str, root: &Path) -> anyhow::Result<Cask> {
        let brew = root.join("brew");
        // Write in a single-threaded child: a concurrent test's fork must not
        // inherit a writable handle to this executable (Linux ETXTBSY).
        let status = Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s' \"$1\" > \"$2\"",
                "write-brew-fixture",
                script,
            ])
            .arg(&brew)
            .status()?;
        anyhow::ensure!(status.success(), "writing brew fixture failed: {status}");
        fs::set_permissions(&brew, fs::Permissions::from_mode(0o755))?;
        Ok(Cask {
            brew,
            prefix: root.to_owned(),
            bundle: root.join(BUNDLE),
            home: root.into(),
        })
    }

    // The real Homebrew layout is the contract this module reads; a fixture
    // cannot prove Homebrew still records casks the way detection expects.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an explicit HERDR_TEST_BUNDLE installed from the cask"]
    fn the_real_cask_is_detected_and_reports_its_version() -> anyhow::Result<()> {
        let bundle = PathBuf::from(
            env::var_os("HERDR_TEST_BUNDLE")
                .context("set HERDR_TEST_BUNDLE to an explicit absolute Herdr.app")?,
        );
        let uid = super::super::install::effective_uid()?;
        let cask = detect(&bundle, uid).context("Homebrew does not own this bundle")?;
        let version = installed(&cask, QUERY)?;
        assert!(
            release::parse_version(&version).is_some(),
            "{version} is a release version"
        );
        // A bundle Homebrew did not install must never be treated as managed.
        assert!(detect(&bundle.join("Contents"), uid).is_none());

        // The real upgrade, resolved but not performed: proof that the minimal
        // environment is enough for Homebrew to auto-update its taps and plan
        // the cask, which a fixture shell script cannot show.
        let mut dry = command(&cask);
        dry.args(["upgrade", "--cask", "--dry-run", TOKEN]);
        let lines = run(dry, UPGRADE, None, |line| println!("{line}"))?;
        assert!(
            lines.iter().any(|line| line.contains(TOKEN)),
            "Homebrew resolved the cask: {lines:?}"
        );
        Ok(())
    }

    // Actually upgrades the installed app, so it is opt-in twice over: past
    // `--ignored` and past an explicit request. `just test-update` must not
    // sweep it up; `just test-brew-upgrade` runs it on purpose.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "upgrades the installed app; set HERDR_TEST_BREW_UPGRADE, HERDR_TEST_BUNDLE and HERDR_TEST_BREW_EXPECTED"]
    fn homebrew_really_installs_a_newer_release() -> anyhow::Result<()> {
        let bundle = PathBuf::from(
            env::var_os("HERDR_TEST_BUNDLE")
                .context("set HERDR_TEST_BUNDLE to an explicit absolute Herdr.app")?,
        );
        env::var_os("HERDR_TEST_BREW_UPGRADE")
            .context("set HERDR_TEST_BREW_UPGRADE=1 to really upgrade this installation")?;
        let expected = env::var("HERDR_TEST_BREW_EXPECTED")
            .context("set HERDR_TEST_BREW_EXPECTED to the offered YYYYMMDD.COUNTER release")?;
        let uid = super::super::install::effective_uid()?;
        let cask = detect(&bundle, uid).context("Homebrew does not own this bundle")?;
        let before = installed(&cask, QUERY)?;
        let cancel = AtomicBool::new(false);
        let mut lines = 0;
        let after = upgrade(&cask, &before, &expected, &cancel, |line| {
            lines += 1;
            println!("{line}");
        })?;
        assert!(lines > 1, "the upgrade reported progress");
        assert!(
            release::parse_version(&after) > release::parse_version(&before),
            "{before} -> {after}"
        );
        assert!(release::parse_version(&after) >= release::parse_version(&expected));
        assert_eq!(
            installed(&cask, QUERY)?,
            after,
            "Homebrew records the new version"
        );
        // Homebrew owns the same bundle afterwards, so the next check still
        // delegates instead of falling back to replacing a managed install.
        assert!(detect(&bundle, uid).is_some());
        // Running it again cannot claim a second update.
        assert!(matches!(
            upgrade(&cask, &after, &expected, &cancel, |_| ()),
            Err(Error::BrewStale { installed, .. }) if installed == after
        ));
        Ok(())
    }

    #[test]
    fn progress_is_bounded_and_failures_keep_the_last_line() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let noisy = cask(
            "#!/bin/sh\nfor i in $(seq 1 200); do echo \"line $i\"; done\necho 'boom' >&2\nexit 3\n",
            root.path(),
        )?;
        let mut seen = Vec::new();
        let Err(error) = run(command(&noisy), QUERY, None, |line| seen.push(line)) else {
            anyhow::bail!("a non-zero exit must fail");
        };
        // Every line, whichever side of the child's exit it was read on: a
        // command this short can exit with all of it still in flight.
        let mut expected: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
        expected.push("boom".to_owned());
        seen.sort();
        expected.sort();
        assert_eq!(seen, expected, "every line is reported as progress");
        assert!(matches!(&error, Error::BrewFailed { status, detail }
                if status.code() == Some(3) && !detail.is_empty() && detail.len() <= DETAIL));

        // A line that only reaches the pipe after the process exits is still
        // that process's output: the grandchild keeps the pipe open past the
        // exit, so this line can be read only while draining.
        let late = cask(
            "#!/bin/sh\n( sleep 1; echo 'after exit' ) &\necho 'before exit'\nexit 3\n",
            root.path(),
        )?;
        let mut seen = Vec::new();
        let Err(error) = run(command(&late), QUERY, None, |line| seen.push(line)) else {
            anyhow::bail!("a non-zero exit must fail");
        };
        assert_eq!(seen, ["before exit", "after exit"], "drained lines report");
        assert!(
            matches!(&error, Error::BrewFailed { detail, .. } if detail == "after exit"),
            "the detail is the last line produced, not the last one read before \
             the exit: {error:?}"
        );

        let long = cask(
            &format!("#!/bin/sh\necho '{}'\n", "x".repeat(4096)),
            root.path(),
        )?;
        let lines = run(command(&long), QUERY, None, |_| ())?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), DETAIL, "detail is truncated");

        let control = cask("#!/bin/sh\nprintf '\\033[31mred\\033[0m\\n'\n", root.path())?;
        assert_eq!(
            run(command(&control), QUERY, None, |_| ())?,
            vec!["[31mred[0m".to_owned()],
            "escape bytes are stripped, never rendered"
        );
        Ok(())
    }

    #[test]
    fn timeout_cancellation_and_version_parsing() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let slow = cask("#!/bin/sh\nsleep 30\n", root.path())?;
        assert!(matches!(
            run(command(&slow), Duration::from_millis(200), None, |_| ()),
            Err(Error::BrewTimeout)
        ));
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            run(command(&slow), QUERY, Some(&cancel), |_| ()),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            upgrade(&slow, "20260921.1", "20260921.2", &cancel, |_| ()),
            Err(Error::Cancelled)
        ));

        let listed = cask(
            &format!("#!/bin/sh\necho 'other 1.2'\necho '{TOKEN} 20260921.2'\n"),
            root.path(),
        )?;
        assert_eq!(installed(&listed, QUERY)?, "20260921.2");
        let empty = cask("#!/bin/sh\necho 'nothing here'\n", root.path())?;
        assert!(matches!(installed(&empty, QUERY), Err(Error::BrewVersion)));
        Ok(())
    }

    /// State is private to the fixture HOME; no shell environment is inherited
    /// and no real Homebrew command can be reached.
    fn fake_brew(root: &Path, first: &str, refreshed: &str, fail: &str) -> anyhow::Result<Cask> {
        cask(
            &format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HOME/commands"
[ "$PATH" = "$HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin" ] || exit 90
[ "$LC_ALL" = C ] || exit 91
[ "$HOMEBREW_NO_ANALYTICS" = 1 ] || exit 92
phase="$1"
if [ "$1" = upgrade ]; then
    if [ -f "$HOME/refreshed" ]; then
        phase=retry
        [ "${{HOMEBREW_NO_AUTO_UPDATE:-}}" = 1 ] || exit 93
    else
        [ -z "${{HOMEBREW_NO_AUTO_UPDATE:-}}" ] || exit 94
    fi
fi
if [ "$phase" = '{fail}' ]; then
    printf '%s failed\n' "$phase" >&2
    exit 3
fi
case "$*" in
    'upgrade --cask {TOKEN}') printf 'upgrade output\n' ;;
    update) : > "$HOME/refreshed" ;;
    'list --cask --versions {TOKEN}')
        if [ -f "$HOME/refreshed" ]; then
            printf '{TOKEN} {refreshed}\n'
        else
            printf '{TOKEN} {first}\n'
        fi ;;
    *) exit 95 ;;
esac
"#
            ),
            root,
        )
    }

    const FIRST_ATTEMPT: &str = "upgrade --cask herdr-gpui\nlist --cask --versions herdr-gpui\n";
    const RECOVERY: &str = "upgrade --cask herdr-gpui\nlist --cask --versions herdr-gpui\nupdate\nupgrade --cask herdr-gpui\nlist --cask --versions herdr-gpui\n";

    #[test]
    fn reaching_or_exceeding_the_offer_needs_no_refresh() -> anyhow::Result<()> {
        for version in ["20260921.3", "20260921.10", "20260922.1"] {
            let root = tempfile::tempdir()?;
            let cask = fake_brew(root.path(), version, "unused", "")?;
            assert_eq!(
                upgrade(
                    &cask,
                    "20260921.1",
                    "20260921.3",
                    &AtomicBool::new(false),
                    |_| ()
                )?,
                version
            );
            assert_eq!(
                fs::read_to_string(root.path().join("commands"))?,
                FIRST_ATTEMPT
            );
        }
        Ok(())
    }

    #[test]
    fn stale_or_intermediate_versions_refresh_and_retry_once() -> anyhow::Result<()> {
        for first in ["20260921.1", "20260921.2"] {
            let root = tempfile::tempdir()?;
            let cask = fake_brew(root.path(), first, "20260921.3", "")?;
            let mut progress = Vec::new();
            assert_eq!(
                upgrade(
                    &cask,
                    "20260921.1",
                    "20260921.3",
                    &AtomicBool::new(false),
                    |line| progress.push(line)
                )?,
                "20260921.3"
            );
            assert_eq!(fs::read_to_string(root.path().join("commands"))?, RECOVERY);
            assert_eq!(
                progress,
                [
                    "Asking Homebrew to upgrade the cask...",
                    "upgrade output",
                    "Checking the installed Homebrew cask version...",
                    "Refreshing Homebrew metadata with brew update...",
                    "Retrying Homebrew cask upgrade after refresh...",
                    "upgrade output",
                    "Checking the installed Homebrew cask version...",
                ]
            );
        }
        Ok(())
    }

    #[test]
    fn persistent_staleness_reports_the_final_version_and_offer() -> anyhow::Result<()> {
        for (current, first, final_version, expected) in [
            ("20260921.1", "20260921.1", "20260921.1", "20260921.3"),
            ("20260921.1", "20260921.1", "20260921.2", "20260921.3"),
            ("20260921.3", "20260921.3", "20260921.3", "20260921.3"),
        ] {
            let root = tempfile::tempdir()?;
            let cask = fake_brew(root.path(), first, final_version, "")?;
            let result = upgrade(&cask, current, expected, &AtomicBool::new(false), |_| ());
            assert!(
                matches!(result, Err(Error::BrewStale { installed, current: old, expected: offer })
                if installed == final_version && old == current && offer == expected)
            );
            assert_eq!(fs::read_to_string(root.path().join("commands"))?, RECOVERY);
        }
        Ok(())
    }

    #[test]
    fn command_failures_never_trigger_recovery_or_another_retry() -> anyhow::Result<()> {
        for (fail, commands) in [
            ("upgrade", "upgrade --cask herdr-gpui\n".to_owned()),
            ("list", FIRST_ATTEMPT.to_owned()),
            ("update", format!("{FIRST_ATTEMPT}update\n")),
            (
                "retry",
                format!("{FIRST_ATTEMPT}update\nupgrade --cask herdr-gpui\n"),
            ),
        ] {
            let root = tempfile::tempdir()?;
            let cask = fake_brew(root.path(), "20260921.1", "20260921.3", fail)?;
            let result = upgrade(
                &cask,
                "20260921.1",
                "20260921.3",
                &AtomicBool::new(false),
                |_| (),
            );
            assert!(
                matches!(&result, Err(Error::BrewFailed { status, detail })
                if status.code() == Some(3) && detail == &format!("{fail} failed")),
                "{fail}: {result:?}"
            );
            assert_eq!(fs::read_to_string(root.path().join("commands"))?, commands);
        }
        Ok(())
    }

    #[test]
    fn cancellation_before_spawn_runs_nothing_but_mutation_is_not_interrupted() -> anyhow::Result<()>
    {
        for already_cancelled in [true, false] {
            let root = tempfile::tempdir()?;
            let cask = fake_brew(root.path(), "20260921.1", "20260921.3", "")?;
            let cancel = AtomicBool::new(already_cancelled);
            let result = upgrade(&cask, "20260921.1", "20260921.3", &cancel, |_| {
                cancel.store(true, Ordering::Release);
            });
            assert!(matches!(result, Err(Error::Cancelled)));
            assert!(!root.path().join("commands").exists());
        }

        let root = tempfile::tempdir()?;
        let cask = fake_brew(root.path(), "20260921.1", "20260921.3", "")?;
        let cancel = AtomicBool::new(false);
        assert_eq!(
            upgrade(&cask, "20260921.1", "20260921.3", &cancel, |line| {
                if line == "upgrade output" {
                    cancel.store(true, Ordering::Release);
                }
            })?,
            "20260921.3"
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(fs::read_to_string(root.path().join("commands"))?, RECOVERY);
        Ok(())
    }

    #[test]
    fn exhausted_budget_does_not_spawn_another_command() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let cask = fake_brew(root.path(), "20260921.1", "20260921.3", "")?;
        assert!(matches!(
            run(command(&cask), Duration::ZERO, None, |_| ()),
            Err(Error::BrewTimeout)
        ));
        assert!(matches!(
            installed(&cask, Duration::ZERO),
            Err(Error::BrewTimeout)
        ));
        assert!(!root.path().join("commands").exists());
        Ok(())
    }
}
