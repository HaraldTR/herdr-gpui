use super::*;
// Only the macOS-only opt-in tests below use it.
#[cfg(target_os = "macos")]
use anyhow::Context as _;
use flate2::{Compression, write::GzEncoder};

fn archive(root: &Path, entries: &[(&str, u8, &str)]) -> anyhow::Result<PathBuf> {
    let path = root.join("fixture.tar.gz");
    let encoder = GzEncoder::new(File::create(&path)?, Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, kind, content) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::new(*kind));
        let bytes = if *kind == b'0' {
            content.as_bytes()
        } else {
            &[]
        };
        header.set_size(bytes.len() as u64);
        if *kind == b'2' || *kind == b'1' {
            header.set_link_name(content)?;
        }
        header.set_cksum();
        archive.append_data(&mut header, path, bytes)?;
    }
    archive.into_inner()?.finish()?;
    Ok(path)
}

#[test]
fn linux_exact_payload_and_cancel() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let source = archive(root.path(), &[("herdr-gpui-test-target", b'0', "binary")])?;
    let out = private_directory(root.path())?;
    let cancel = AtomicBool::new(false);
    let binary = extract(
        &source,
        out.path(),
        Mode::Linux,
        "herdr-gpui-test-target",
        &cancel,
    )?;
    assert_eq!(fs::read(&binary)?, b"binary");
    assert_eq!(fs::metadata(&binary)?.mode() & 0o7777, 0o755);
    assert_eq!(fs::metadata(out.path())?.mode() & 0o7777, 0o700);
    let out = private_directory(root.path())?;
    assert!(extract(&source, out.path(), Mode::Linux, "wrong", &cancel).is_err());
    cancel.store(true, Ordering::Relaxed);
    assert!(
        extract(
            &source,
            out.path(),
            Mode::Linux,
            "herdr-gpui-test-target",
            &cancel
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn mac_links_and_forbidden_entries() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let cancel = AtomicBool::new(false);
    let source = archive(
        root.path(),
        &[
            ("Herdr.app/file", b'0', "ok"),
            ("Herdr.app/link", b'2', "file"),
        ],
    )?;
    let out = private_directory(root.path())?;
    extract(&source, out.path(), Mode::Mac, "unused", &cancel)?;
    assert_eq!(fs::read(out.path().join("Herdr.app/link"))?, b"ok");
    for entries in [
        vec![("Herdr.app/link", b'2', "../../escape")],
        vec![("Herdr.app/file", b'1', "other")],
        vec![("Herdr.app/fifo", b'6', "")],
        vec![("Herdr.app/device", b'3', "")],
        vec![("Herdr.app/file", b'0', "a"), ("Herdr.app/file", b'0', "b")],
        vec![
            ("Herdr.app/link", b'2', "dir"),
            ("Herdr.app/link/file", b'0', "no"),
        ],
        vec![("other/file", b'0', "no")],
    ] {
        let source = archive(root.path(), &entries)?;
        let out = private_directory(root.path())?;
        assert!(
            extract(&source, out.path(), Mode::Mac, "unused", &cancel).is_err(),
            "{entries:?}"
        );
    }
    Ok(())
}

#[test]
fn bundle_distribution_permissions_are_shared_but_staging_stays_private() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("permissions.tar.gz");
    let encoder = GzEncoder::new(File::create(&source)?, Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, kind, mode, bytes) in [
        (
            "Herdr.app",
            tar::EntryType::Directory,
            0o7777,
            b"".as_slice(),
        ),
        (
            "Herdr.app/empty",
            tar::EntryType::Directory,
            0o700,
            b"".as_slice(),
        ),
        (
            "Herdr.app/Contents/MacOS/Herdr",
            tar::EntryType::Regular,
            0o6777,
            b"executable bytes".as_slice(),
        ),
        (
            "Herdr.app/Contents/Resources/config",
            tar::EntryType::Regular,
            0o6666,
            b"resource bytes".as_slice(),
        ),
        (
            "Herdr.app/Contents/Resources/current",
            tar::EntryType::Symlink,
            0o777,
            b"".as_slice(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(bytes.len() as u64);
        if kind.is_symlink() {
            header.set_link_name("config")?;
        }
        header.set_cksum();
        archive.append_data(&mut header, path, bytes)?;
    }
    archive.into_inner()?.finish()?;
    let stage = private_directory(root.path())?;
    let tree = private_directory(stage.path())?;
    let candidate = extract(
        &source,
        tree.path(),
        Mode::Mac,
        "unused",
        &AtomicBool::new(false),
    )?;
    let installed = root.path().join("Installed.app");
    fs::rename(candidate, &installed)?;
    for path in [
        "",
        "empty",
        "Contents",
        "Contents/MacOS",
        "Contents/Resources",
    ] {
        assert_eq!(
            fs::metadata(installed.join(path))?.mode() & 0o7777,
            0o755,
            "{path}"
        );
    }
    let executable = installed.join("Contents/MacOS/Herdr");
    let resource = installed.join("Contents/Resources/config");
    assert_eq!(fs::metadata(&executable)?.mode() & 0o7777, 0o755);
    assert_eq!(fs::metadata(&resource)?.mode() & 0o7777, 0o644);
    assert_eq!(fs::read(executable)?, b"executable bytes");
    assert_eq!(fs::read(resource)?, b"resource bytes");
    assert_eq!(
        fs::read(installed.join("Contents/Resources/current"))?,
        b"resource bytes"
    );
    for private in [stage.path(), tree.path()] {
        assert_eq!(fs::metadata(private)?.mode() & 0o7777, 0o700);
    }
    Ok(())
}

#[test]
fn traversal_and_link_policy() {
    for path in ["/absolute", "../escape", "Herdr.app/../escape", ""] {
        assert!(!safe_path(Path::new(path)));
    }
    assert!(safe_link(
        Path::new("Herdr.app/dir/link"),
        Path::new("../file")
    ));
    assert!(!safe_link(
        Path::new("Herdr.app/link"),
        Path::new("../outside")
    ));
}

#[test]
fn system_package_locations_are_package_managed() {
    for managed in [
        "/usr/bin/herdr-gpui",
        "/usr/lib/herdr-gpui/herdr-gpui",
        "/nix/store/abc-herdr-gpui/bin/herdr-gpui",
    ] {
        assert!(system_managed(Path::new(managed)), "{managed}");
        assert!(
            matches!(
                linux_location(Path::new(managed), Path::new("/home/user"), 1000, false),
                Err(Error::PackageManaged)
            ),
            "{managed}"
        );
    }
    for unmanaged in [
        "/usr/local/bin/herdr-gpui",
        "/home/user/.local/bin/herdr-gpui",
        "/opt/herdr/bin/herdr-gpui",
        "/usrlocal/herdr-gpui",
    ] {
        assert!(!system_managed(Path::new(unmanaged)), "{unmanaged}");
    }
}

#[test]
fn linux_location_rejects_managed_unsafe_and_outside_home() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let home = root.path().canonicalize()?;
    let bin = home.join("bin");
    fs::create_dir(&bin)?;
    let executable = bin.join("herdr");
    fs::write(&executable, b"binary")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let uid = fs::metadata(&executable)?.uid();
    assert!(linux_location(&executable, &home, uid, false).is_ok());
    assert!(linux_location(&executable, &home, uid, true).is_err());
    assert!(linux_location(&executable, &bin.join("elsewhere"), uid, false).is_err());
    assert!(linux_location(&executable, &home, uid.wrapping_add(1), false).is_err());
    for mode in [0o600, 0o500, 0o4700, 0o2700, 0o722] {
        fs::set_permissions(&executable, fs::Permissions::from_mode(mode))?;
        assert!(
            linux_location(&executable, &home, uid, false).is_err(),
            "{mode:o}"
        );
    }
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o777))?;
    assert!(linux_location(&executable, &home, uid, false).is_err());
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    let link = bin.join("link");
    std::os::unix::fs::symlink(&executable, &link)?;
    assert!(linux_location(&link, &home, uid, false).is_err());
    // A launcher symlink does not disqualify its safe resolved origin.
    assert!(linux_location(&link.canonicalize()?, &home, uid, false).is_ok());
    let parent_link = home.join("linked-bin");
    std::os::unix::fs::symlink(&bin, &parent_link)?;
    assert!(linux_location(&parent_link.join("herdr"), &home, uid, false).is_err());
    let hard_link = bin.join("hard-link");
    fs::hard_link(&executable, &hard_link)?;
    assert!(linux_location(&executable, &home, uid, false).is_err());
    fs::remove_file(hard_link)?;
    let elsewhere = tempfile::tempdir()?;
    assert!(linux_location(&executable, &elsewhere.path().canonicalize()?, uid, false).is_err());
    fs::set_permissions(&home, fs::Permissions::from_mode(0o777))?;
    assert!(linux_location(&executable, &home, uid, false).is_err());
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[test]
fn mac_parent_policy_allows_root_admin_but_not_other_users_or_symlinks() -> anyhow::Result<()> {
    assert!(trusted_mac_parent(0, 0o40775, 501));
    assert!(trusted_mac_parent(501, 0o40775, 501));
    assert!(!trusted_mac_parent(502, 0o40775, 501));
    assert!(!trusted_mac_parent(0, 0o40777, 501));
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let uid = fs::metadata(&parent)?.uid();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o775))?;
    assert!(installation_parent(&parent, Mode::Mac, uid).is_ok());
    assert!(installation_parent(&parent, Mode::Linux, uid).is_err());
    let stage = private_directory(&parent)?;
    assert_eq!(fs::metadata(stage.path())?.mode() & 0o777, 0o700);
    drop(stage);
    let link = parent.join("linked-parent");
    std::os::unix::fs::symlink(&parent, &link)?;
    assert!(installation_parent(&link, Mode::Mac, uid).is_err());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o777))?;
    assert!(installation_parent(&parent, Mode::Mac, uid).is_err());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555))?;
    if uid != 0 {
        assert!(private_directory(&parent).is_err());
    }
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[test]
fn signing_identity_is_pinned_to_herdr() -> anyhow::Result<()> {
    assert_eq!(
        signing_identity("Identifier=so.pen.herdr-gpui\nTeamIdentifier=TEAM123\n")?,
        ("TEAM123".into(), "so.pen.herdr-gpui".into())
    );
    assert!(signing_identity("Identifier=another.signed.app\nTeamIdentifier=TEAM123\n").is_err());
    assert!(signing_identity("Identifier=so.pen.herdr-gpui\nTeamIdentifier=not set\n").is_err());
    Ok(())
}

#[test]
fn raw_malformed_archives_fail_before_payload_writes() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let cancel = AtomicBool::new(false);
    for (name, size) in [
        ("../escape", 0),
        ("/absolute", 0),
        ("Herdr.app/large", LIMIT + 1),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o700);
        header.set_size(size);
        header.set_entry_type(tar::EntryType::Regular);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        let source = root.path().join("bad.tar.gz");
        let mut encoder = GzEncoder::new(File::create(&source)?, Compression::fast());
        encoder.write_all(header.as_bytes())?;
        encoder.finish()?;
        let out = private_directory(root.path())?;
        assert!(extract(&source, out.path(), Mode::Mac, "unused", &cancel).is_err());
        assert_eq!(fs::read_dir(out.path())?.count(), 0);
    }
    Ok(())
}

#[test]
fn digest_is_checked_before_extraction_and_staging_is_private() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let stage = private_directory(root.path())?;
    assert_eq!(fs::metadata(stage.path())?.mode() & 0o777, 0o700);
    fs::write(stage.path().join("archive.tar.gz"), b"bad")?;
    let installation = Installation {
        mode: Mode::Linux,
        destination: root.path().join("app"),
        executable: root.path().join("app"),
        uid: fs::metadata(stage.path())?.uid(),
    };
    let asset = release::Asset {
        target: "x86_64-unknown-linux-gnu".into(),
        name: "unused".into(),
        size: 3,
        sha256: "00".repeat(32),
    };
    let offer = release::Offer {
        manifest: release::Manifest {
            schema: 1,
            version: "20260920.2".into(),
            assets: vec![asset.clone()],
        },
        asset,
        manifest_bytes: vec![],
        signature: vec![],
    };
    assert!(candidate(stage.path(), &installation, &offer, &AtomicBool::new(false)).is_err());
    assert_eq!(fs::read_dir(stage.path())?.count(), 1);
    Ok(())
}

#[test]
fn replacement_retains_backup_and_rolls_back_on_spawn_failure() -> anyhow::Result<()> {
    for mode in [Mode::Linux, Mode::Mac] {
        for fail in [false, true] {
            let root = tempfile::tempdir()?;
            let destination = root.path().join("installed");
            let candidate = root.path().join("candidate");
            let backup = root.path().join("backup");
            fs::write(&destination, b"old")?;
            fs::write(&candidate, b"new")?;
            let result = replace(&destination, &candidate, &backup, mode, || {
                if fail {
                    Err(io(std::io::Error::other("spawn failed")))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), fail);
            assert_eq!(fs::read(&destination)?, if fail { b"old" } else { b"new" });
            if !fail {
                assert_eq!(fs::read(backup)?, b"old");
            }
        }
    }
    Ok(())
}

#[test]
fn replacement_rename_failure_restores_old() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let destination = root.path().join("installed");
    fs::write(&destination, b"old")?;
    let mut launched = false;
    assert!(
        replace(
            &destination,
            &root.path().join("missing"),
            &root.path().join("backup"),
            Mode::Mac,
            || {
                launched = true;
                Ok(())
            }
        )
        .is_err()
    );
    assert!(!launched, "must not launch");
    assert_eq!(fs::read(destination)?, b"old");
    Ok(())
}

#[test]
fn linux_failed_rename_removes_our_link_and_remains_eligible_for_retry() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let home = root.path().canonicalize()?;
    let destination = home.join("installed");
    let backup = home.join("backup");
    fs::write(&destination, b"old")?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
    let original = fs::metadata(&destination)?;
    for _ in 0..2 {
        let mut launched = false;
        assert!(
            replace(
                &destination,
                &home.join("missing"),
                &backup,
                Mode::Linux,
                || {
                    launched = true;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!launched, "must not launch");
        let current = fs::metadata(&destination)?;
        assert_eq!(current.ino(), original.ino());
        assert_eq!(current.nlink(), 1);
        assert!(!backup.exists());
        linux_location(&destination, &home, original.uid(), false)?;
    }
    let candidate = home.join("candidate");
    fs::write(&candidate, b"new")?;
    replace(&destination, &candidate, &backup, Mode::Linux, || Ok(()))?;
    assert_eq!(fs::read(destination)?, b"new");
    assert_eq!(fs::read(backup)?, b"old");
    Ok(())
}

#[test]
fn failed_linux_backup_cleanup_preserves_changed_paths() -> anyhow::Result<()> {
    for change in ["destination", "backup", "destination-link", "backup-link"] {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("installed");
        let backup = root.path().join("backup");
        fs::write(&destination, b"old")?;
        let original = fs::metadata(&destination)?;
        fs::hard_link(&destination, &backup)?;
        let changed = if change.starts_with("destination") {
            &destination
        } else {
            &backup
        };
        if change.ends_with("-link") {
            fs::remove_file(changed)?;
            let other = if changed == &destination {
                &backup
            } else {
                &destination
            };
            std::os::unix::fs::symlink(other, changed)?;
        } else {
            let different = root.path().join("different");
            fs::write(&different, b"changed")?;
            fs::rename(different, changed)?;
        }
        let saved = fs::symlink_metadata(&backup)?;
        assert!(remove_failed_linux_backup(&destination, &backup, &original).is_err());
        assert_eq!(fs::symlink_metadata(&backup)?.ino(), saved.ino());
    }
    Ok(())
}

#[test]
fn mac_replaces_entire_bundle_and_preserves_recovery() -> anyhow::Result<()> {
    for fail in [false, true] {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("Herdr.app");
        let candidate = root.path().join("candidate.app");
        let backup = root.path().join("previous.app");
        fs::create_dir(&destination)?;
        fs::create_dir(&candidate)?;
        fs::write(destination.join("old-only"), b"old")?;
        fs::write(candidate.join("new-only"), b"new")?;
        assert_eq!(
            replace(&destination, &candidate, &backup, Mode::Mac, || if fail {
                Err(io(std::io::Error::other("launch failed")))
            } else {
                Ok(())
            })
            .is_err(),
            fail
        );
        assert_eq!(destination.join("old-only").exists(), fail);
        assert_eq!(destination.join("new-only").exists(), !fail);
        if !fail {
            assert!(backup.join("old-only").exists());
        }
    }
    Ok(())
}

// A requirement that does not compile makes `codesign --verify -R` exit 1
// for every bundle, so the installed app can never be authenticated.
#[cfg(target_os = "macos")]
#[test]
fn designated_requirement_compiles_as_source_text() -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let directory = tempfile::tempdir()?;
    let compiled = directory.path().join("requirement");
    output(
        Command::new("/usr/bin/csreq")
            .args(["-r", REQUIREMENT, "-b"])
            .arg(&compiled),
        &cancel,
    )?;
    assert!(fs::metadata(&compiled)?.len() > 0);
    assert!(matches!(
        output(
            Command::new("/usr/bin/csreq")
                .args(["-r", &REQUIREMENT[1..], "-b"])
                .arg(directory.path().join("unmarked")),
            &cancel,
        ),
        Err(Error::ValidationFailed(_))
    ));
    Ok(())
}

// csreq proves the text parses; this proves codesign accepts the exact
// argument shape `identity()` builds. Apple signs its own platform
// binaries, so `anchor apple` matches /bin/ls whenever codesign reads the
// argument as source text instead of a requirement file path.
#[cfg(target_os = "macos")]
#[test]
fn codesign_accepts_the_inline_requirement_form() -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let verify = |requirement: String| {
        output(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "-R"])
                .arg(requirement)
                .arg("/bin/ls"),
            &cancel,
        )
    };
    verify(format!("{}anchor apple", &REQUIREMENT[..1]))?;
    assert!(matches!(
        verify("anchor apple".to_owned()),
        Err(Error::ValidationFailed(_))
    ));
    Ok(())
}

// End-to-end proof against a real Developer ID bundle: no fixture can
// satisfy the production requirement, so the installation is named
// explicitly and never discovered.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an explicit HERDR_TEST_BUNDLE installed Herdr.app"]
fn installed_bundle_satisfies_the_designated_requirement() -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let bundle = PathBuf::from(
        env::var_os("HERDR_TEST_BUNDLE")
            .context("set HERDR_TEST_BUNDLE to an explicit absolute Herdr.app")?,
    );
    let version = output(
        Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .arg(bundle.join("Contents/Info.plist")),
        &cancel,
    )?;
    let (team, identifier) = identity(&bundle, version.trim(), &cancel)?;
    assert_eq!(identifier, "so.pen.herdr-gpui");
    assert!(!team.is_empty());
    assert!(matches!(
        identity(&bundle, "0.0.0", &cancel),
        Err(Error::BundleVersion)
    ));
    Ok(())
}

#[test]
fn process_output_and_cancellation_are_bounded() {
    let cancel = AtomicBool::new(false);
    assert!(matches!(
        output(&mut Command::new("/usr/bin/yes"), &cancel),
        Err(Error::ValidationOutputLimit)
    ));
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        output(Command::new("/bin/sleep").arg("30"), &cancel),
        Err(Error::Cancelled)
    ));
    assert!(run_helper(&[HELPER.into()]).is_some());
    assert!(run_helper(&["--help".into()]).is_none());
}

#[test]
fn lease_and_ownership_checks() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let executable = root.path().canonicalize()?.join("app");
    fs::write(&executable, b"old")?;
    let uid = fs::metadata(&executable)?.uid();
    let installation = Installation {
        mode: Mode::Linux,
        destination: executable.clone(),
        executable: executable.clone(),
        uid,
    };
    let lease = lock(&installation)?;
    assert!(matches!(lock(&installation), Err(Error::LockContended)));
    // Explicit unlock avoids a concurrently spawning test's brief fork/exec
    // window retaining an inherited descriptor after this thread drops it.
    lease.unlock()?;
    drop(lease);
    let next = lock(&installation);
    assert!(next.is_ok(), "{next:?}");
    next?.unlock()?;
    let handoff = lock_file(&installation, ".update-handoff")?;
    assert!(lock(&installation).is_err());
    let helper_lease = lock_file(&installation, ".update-lock")?;
    helper_lease.unlock()?;
    handoff.unlock()?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o4777))?;
    assert!(owned(&executable, uid, false).is_err());
    Ok(())
}

#[test]
fn shared_mac_parent_does_not_relax_lock_file_policy() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o775))?;
    let uid = fs::metadata(&parent)?.uid();
    let installation = Installation {
        mode: Mode::Mac,
        destination: parent.join("Herdr.app"),
        executable: parent.join("unused"),
        uid,
    };
    let path = parent.join(".Herdr.app.update-lock");
    let sentinel = parent.join("sentinel");
    fs::write(&sentinel, b"do not modify")?;
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600))?;
    std::os::unix::fs::symlink(&sentinel, &path)?;
    assert!(lock_file(&installation, ".update-lock").is_err());
    fs::remove_file(&path)?;
    fs::hard_link(&sentinel, &path)?;
    assert!(lock_file(&installation, ".update-lock").is_err());
    assert!(owned(&path, uid.wrapping_add(1), false).is_err());
    fs::remove_file(&path)?;
    fs::write(&path, b"unsafe lock")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
    assert!(lock_file(&installation, ".update-lock").is_err());
    assert_eq!(fs::read(&sentinel)?, b"do not modify");
    Ok(())
}

#[test]
fn guard_keeps_committed_stage_and_reaps_before_cancel_cleanup() -> anyhow::Result<()> {
    for commit in [false, true] {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let stage = private_directory(&parent)?;
        let stage_path = stage.path().to_owned();
        fs::write(stage_path.join("archive.tar.gz"), b"retained archive")?;
        let uid = fs::metadata(&parent)?.uid();
        let installation = Installation {
            mode: Mode::Linux,
            destination: parent.join("app"),
            executable: parent.join("app"),
            uid,
        };
        let lease = lock(&installation)?;
        let transcript = parent.join("control-transcript");
        let child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(File::create(&transcript)?)
            .spawn()?;
        let prepared = Prepared {
            stage,
            lease,
            installation,
        };
        let instruction = [b"COMMIT\n".as_slice(), &[42; 32]].concat();
        let (mut guard, owner) = own_helper(prepared, child, instruction.clone());
        assert!(stage_path.join("archive.tar.gz").exists());
        let committed = (|| -> anyhow::Result<()> {
            if commit {
                guard.commit()?;
                guard.commit()?; // Only one instruction may be sent.
                assert!(stage_path.join("archive.tar.gz").exists());
            }
            Ok(())
        })();
        drop(guard);
        owner
            .join()
            .map_err(|_| anyhow::anyhow!("helper owner thread panicked"))?;
        committed?;
        assert_eq!(stage_path.exists(), commit);
        if commit {
            assert_eq!(
                fs::read(stage_path.join("archive.tar.gz"))?,
                b"retained archive"
            );
            assert_eq!(fs::read(transcript)?, instruction);
        }
    }
    Ok(())
}

#[test]
fn result_marker_is_bounded_and_uses_the_preopened_file() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let stage = private_directory(root.path())?;
    let path = stage.path().join("install-result.txt");
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let held_path = stage.path().join("held-result.txt");
    fs::rename(&path, &held_path)?;
    let outside = root.path().join("do-not-touch");
    fs::write(&outside, b"unchanged")?;
    std::os::unix::fs::symlink(&outside, &path)?;
    record_result(
        &mut report,
        &Err(io(std::io::Error::other("\x1b[31m unsafe\n".repeat(4096)))),
    )?;
    let text = fs::read_to_string(&held_path)?;
    assert!(text.len() < 8192);
    assert!(!text.contains('\x1b'));
    assert!(text.contains("previous-installation"));
    assert!(text.contains("manually install a verified signed release"));
    assert_eq!(fs::metadata(&held_path)?.mode() & 0o777, 0o600);
    assert_eq!(fs::read(&outside)?, b"unchanged");
    record_result(&mut report, &Ok(()))?;
    let text = fs::read_to_string(&held_path)?;
    assert!(text.starts_with("Update installed"));
    assert!(!text.contains("Recovery:"));
    Ok(())
}

#[test]
fn real_executable_archive_installs_relaunches_and_rolls_back() -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    for fail in [false, true] {
        let root = tempfile::tempdir()?;
        let parent = root.path().canonicalize()?;
        let destination = parent.join("herdr-gpui");
        fs::copy("/bin/cat", &destination)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
        let old = fs::read(&destination)?;
        let uid = fs::metadata(&destination)?.uid();
        linux_location(&destination, &parent, uid, false)?;
        let installation = Installation {
            mode: Mode::Linux,
            destination: destination.clone(),
            executable: destination.clone(),
            uid,
        };
        let stage = private_directory(&parent)?;
        let payload = parent.join("portable-executable");
        let source = parent.join("fixture.c");
        fs::write(
            &source,
            b"#include <stdio.h>\nint main(int argc, char **argv) {\n    if (argc != 2) return 1;\n    return puts(argv[1]) == EOF;\n}\n",
        )?;
        // Build an ordinary relocatable executable. Apple's system binaries
        // can retain platform restrictions even after ad-hoc re-signing.
        output(
            Command::new("/usr/bin/env")
                .arg("PATH=/usr/bin:/bin")
                .arg("/usr/bin/cc")
                .arg(&source)
                .arg("-o")
                .arg(&payload),
            &AtomicBool::new(false),
        )?;
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o700))?;
        let expected_payload = fs::read(&payload)?;
        let archive_path = stage.path().join("archive.tar.gz");
        let encoder = GzEncoder::new(File::create(&archive_path)?, Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        // Match release packaging, without append_file's platform-dependent
        // GNU sparse detection for linker-created executable files.
        let mut header = tar::Header::new_ustar();
        header.set_size(expected_payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(
            &mut header,
            "herdr-gpui-20260920.2-portable-test",
            expected_payload.as_slice(),
        )?;
        archive.into_inner()?.finish()?;
        let bytes = fs::read(&archive_path)?;
        let asset = release::Asset {
            target: "portable-test".into(),
            name: "fixture.tar.gz".into(),
            size: bytes.len() as u64,
            sha256: Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let offer = release::Offer {
            manifest: release::Manifest {
                schema: 1,
                version: "20260920.2".into(),
                assets: vec![asset.clone()],
            },
            asset,
            manifest_bytes: vec![],
            signature: vec![],
        };
        let cancel = AtomicBool::new(false);
        // No signing-key bypass in production: this fixture enters below
        // manifest authentication to exercise real hash/extract/swap/exec.
        let (_tree, candidate) = candidate(stage.path(), &installation, &offer, &cancel)?;
        let backup = stage.path().join("previous-installation");
        let result = replace(&destination, &candidate, &backup, Mode::Linux, || {
            let mut command = Command::new(&destination);
            command.arg("restarted successfully");
            if fail {
                command.current_dir(parent.join("missing-directory"));
            }
            let text = output(&mut command, &cancel)?;
            assert_eq!(text, "restarted successfully\n");
            Ok(())
        });
        let mut report = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(stage.path().join("install-result.txt"))?;
        record_result(&mut report, &result)?;
        assert_eq!(result.is_err(), fail, "{result:?}");
        assert_eq!(
            fs::read(&destination)?,
            if fail { old.clone() } else { expected_payload }
        );
        assert!(archive_path.exists());
        if !fail {
            assert_eq!(fs::read(backup)?, old);
        } else {
            assert!(
                fs::read_to_string(stage.path().join("install-result.txt"))?
                    .contains("restart failed")
            );
        }
    }
    Ok(())
}

#[test]
fn argument_bytes_and_guard_decision_are_lossless() -> anyhow::Result<()> {
    let raw = vec![b'a', 0xff, b' '];
    let encoded = serde_json::to_vec(&vec![OsString::from_vec(raw.clone()).into_vec()])?;
    let decoded: Vec<Vec<u8>> = serde_json::from_slice(&encoded)?;
    assert_eq!(OsString::from_vec(decoded[0].clone()).into_vec(), raw);
    let (control, receiver) = mpsc::channel();
    drop(RestartGuard {
        control,
        input: None,
        instruction: vec![],
        committed: false,
    });
    assert!(matches!(receiver.recv()?, Control::Close));
    let (control, receiver) = mpsc::channel();
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    let mut guard = RestartGuard {
        control,
        input: child.stdin.take(),
        instruction: b"COMMIT\n".to_vec(),
        committed: false,
    };
    guard.commit()?;
    // COMMIT alone must not release the EOF barrier.
    assert!(child.try_wait()?.is_none());
    drop(guard);
    assert!(matches!(receiver.recv()?, Control::Commit));
    assert!(matches!(receiver.recv()?, Control::Close));
    assert!(child.wait()?.success());
    Ok(())
}
