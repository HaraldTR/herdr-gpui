//! Blocking installer. Only `RestartGuard::commit` and its drop are UI-safe.
//! The service must retain a committed guard until application teardown.
//! After a committed attempt, `install-result.txt` in the retained private stage
//! records the outcome and recovery instructions. `archive.tar.gz` and any
//! `previous-installation` backup are retained; never blindly overwrite an
//! existing installation with that backup. No startup health ACK is assumed.

use super::error::{Result, UpdateError as Error};
use super::release;
use Error::Io as io;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitCode, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const HELPER: &str = "--herdr-apply-update";
// codesign and csreq read a bare `-R` argument as the path of a compiled
// requirement file; the leading `=` is what marks the rest as source text.
// Without it every verification exits 1 with "invalid requirement
// specification", which fails closed but blocks all updates.
const REQUIREMENT: &str = "=anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and identifier \"so.pen.herdr-gpui\"";
const LIMIT: u64 = 1024 * 1024 * 1024;
const REQUEST_LIMIT: u64 = 256 * 1024;
const WAIT: Duration = Duration::from_secs(120);

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Linux,
    Mac,
}

struct Installation {
    mode: Mode,
    destination: PathBuf,
    executable: PathBuf,
    uid: u32,
}

pub(super) struct Prepared {
    stage: TempDir,
    lease: File,
    installation: Installation,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    current_version: String,
    version: String,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    args: Vec<Vec<u8>>,
    cwd: Vec<u8>,
    token: Vec<u8>,
}

fn private_directory(parent: &Path) -> Result<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(".herdr-update-")
        .tempdir_in(parent)
        .map_err(io)?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).map_err(io)?;
    Ok(dir)
}

// Drain both pipes concurrently, with a hard memory cap and a finite deadline.
fn output(command: &mut Command, cancel: &AtomicBool) -> Result<String> {
    check(cancel)?;
    command
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(io)?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill(&mut child);
        return Err(Error::MissingValidationPipes);
    };
    let (sender, receiver) = mpsc::sync_channel(2);
    let pipes: [Box<dyn Read + Send>; 2] = [Box::new(stdout), Box::new(stderr)];
    for pipe in pipes {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.take(65537).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
    }
    drop(sender);
    let start = Instant::now();
    let mut bytes = Vec::new();
    let mut received = 0;
    let result: Result<()> = (|| {
        loop {
            check(cancel)?;
            if start.elapsed() > Duration::from_secs(30) {
                break Err(Error::ValidationTimeout);
            }
            while let Ok(result) = receiver.try_recv() {
                let part = result.map_err(io)?;
                if bytes.len() + part.len() > 65536 {
                    return Err(Error::ValidationOutputLimit);
                }
                bytes.extend_from_slice(&part);
                received += 1;
            }
            match child.try_wait().map_err(io)? {
                Some(status) if !status.success() => {
                    break Err(Error::ValidationFailed(status));
                }
                Some(_) if received == 2 => break Ok(()),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result?;
    String::from_utf8(bytes).map_err(Error::ValidationEncoding)
}

fn owned(path: &Path, uid: u32, directory: bool) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if meta.file_type().is_symlink()
        || meta.is_dir() != directory
        || (!directory && !meta.is_file())
        || meta.uid() != uid
        || meta.mode() & 0o6022 != 0
        || meta.permissions().readonly()
    {
        return Err(Error::UnsafeInstallation(path.to_owned()));
    }
    Ok(meta)
}

fn no_links(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::RelativeInstallation);
    }
    let mut prefix = PathBuf::new();
    for part in path.components() {
        if matches!(part, Component::ParentDir | Component::CurDir) {
            return Err(Error::NoncanonicalInstallation);
        }
        prefix.push(part);
        if fs::symlink_metadata(&prefix)
            .map_err(io)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::SymlinkedInstallation);
        }
    }
    Ok(())
}

fn trusted_mac_parent(owner: u32, mode: u32, uid: u32) -> bool {
    (owner == uid || owner == 0) && mode & 0o6002 == 0
}

fn installation_parent(path: &Path, mode: Mode, uid: u32) -> Result<()> {
    no_links(path)?;
    if mode == Mode::Linux {
        owned(path, uid, true)?;
    } else {
        let meta = fs::symlink_metadata(path).map_err(io)?;
        if !meta.is_dir() || !trusted_mac_parent(meta.uid(), meta.mode(), uid) {
            return Err(Error::UnsafeMacParent);
        }
        // /Applications is normally root:admin 0775. Do not equate ownership
        // with access: creating our private stage must succeed without elevation.
    }
    Ok(())
}

/// Distribution packages install under `/usr` (never `/usr/local`, which is
/// the administrator's), and Nix into its read-only store. Neither may be
/// overwritten, and naming the owner is clearer than "outside HOME".
fn system_managed(executable: &Path) -> bool {
    (executable.starts_with("/usr") && !executable.starts_with("/usr/local"))
        || executable.starts_with("/nix/store")
}

fn linux_location(executable: &Path, home: &Path, uid: u32, packaged: bool) -> Result<()> {
    if packaged || system_managed(executable) {
        return Err(Error::PackageManaged);
    }
    no_links(executable)?;
    owned(home, uid, true)?;
    if !executable.starts_with(home) {
        return Err(Error::OutsideHome);
    }
    for ancestor in executable
        .parent()
        .ok_or(Error::MissingExecutableParent)?
        .ancestors()
        .take_while(|p| p.starts_with(home))
    {
        owned(ancestor, uid, true)?;
    }
    let metadata = owned(executable, uid, false)?;
    if metadata.mode() & 0o111 == 0 || metadata.nlink() != 1 {
        return Err(Error::NotStandalone);
    }
    Ok(())
}

/// The effective UID of this process, as the system reports it.
pub(super) fn effective_uid() -> Result<u32> {
    uid(&AtomicBool::new(false))
}

fn uid(cancel: &AtomicBool) -> Result<u32> {
    output(Command::new("/usr/bin/id").arg("-u"), cancel)?
        .trim()
        .parse()
        .map_err(Error::EffectiveUid)
}

/// The bundle root of a running macOS installation. Defined once: brew
/// delegation and standalone installation must agree on what "this app" is.
pub(super) fn mac_bundle(executable: &Path) -> Result<PathBuf> {
    let root = executable.ancestors().nth(3).ok_or(Error::NotHerdrBundle)?;
    if root.file_name() != Some("Herdr.app".as_ref())
        || executable != root.join("Contents/MacOS/Herdr")
    {
        return Err(Error::NotHerdrBundle);
    }
    Ok(root.to_owned())
}

fn detect(cancel: &AtomicBool) -> Result<Installation> {
    if release::parse_version(crate::APP_VERSION).is_none()
        || option_env!("HERDR_UPDATE_PUBLIC_KEY").is_none()
    {
        return Err(Error::LocalBuild);
    }
    let uid = uid(cancel)?;
    if uid == 0 {
        return Err(Error::RootUser);
    }
    // Linux current_exe identifies the loaded executable, not a symlink launcher.
    // Eligibility applies to that resolved origin and its ancestors under HOME.
    let executable = env::current_exe().map_err(io)?;
    no_links(&executable)?;
    let mode = if cfg!(target_os = "macos") {
        Mode::Mac
    } else if cfg!(target_os = "linux") {
        Mode::Linux
    } else {
        return Err(Error::UnsupportedPlatform);
    };
    let destination = match mode {
        Mode::Mac => mac_bundle(&executable)?,
        Mode::Linux => {
            let packaged = ["APPIMAGE", "SNAP", "FLATPAK_ID"]
                .iter()
                .any(|key| env::var_os(key).is_some());
            let home =
                fs::canonicalize(env::var_os("HOME").ok_or(Error::MissingHome)?).map_err(io)?;
            linux_location(&executable, &home, uid, packaged)?;
            executable.clone()
        }
    };
    owned(&destination, uid, mode == Mode::Mac)?;
    installation_parent(
        destination
            .parent()
            .ok_or(Error::MissingInstallationParent)?,
        mode,
        uid,
    )?;
    if mode == Mode::Mac {
        for ancestor in executable
            .parent()
            .ok_or(Error::MissingExecutableParent)?
            .ancestors()
            .take_while(|path| path.starts_with(&destination))
        {
            owned(ancestor, uid, true)?;
        }
    }
    if owned(&executable, uid, false)?.mode() & 0o111 == 0 {
        return Err(Error::NotExecutable);
    }
    if mode == Mode::Mac {
        identity(&destination, crate::APP_VERSION, cancel)?;
    }
    Ok(Installation {
        mode,
        destination,
        executable,
        uid,
    })
}

fn lock(installation: &Installation) -> Result<File> {
    let lease = lock_file(installation, ".update-lock")?;
    // A helper holds the handoff lease before READY until replacement finishes.
    // This closes the primary-lock handoff gap without inheriting raw FDs.
    match lock_file(installation, ".update-handoff") {
        Ok(probe) => probe.unlock().map_err(io)?,
        Err(error) => {
            let _ = lease.unlock();
            return Err(error);
        }
    }
    Ok(lease)
}

fn lock_file(installation: &Installation, suffix: &str) -> Result<File> {
    installation_parent(
        installation
            .destination
            .parent()
            .ok_or(Error::MissingInstallationParent)?,
        installation.mode,
        installation.uid,
    )?;
    let name = installation
        .destination
        .file_name()
        .ok_or(Error::MissingInstallationName)?;
    let mut lock_name = OsString::from(".");
    lock_name.push(name);
    lock_name.push(suffix);
    let path = installation.destination.with_file_name(lock_name);
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            owned(&path, installation.uid, false)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(io)?
        }
        Err(error) => return Err(io(error)),
    };
    let meta = owned(&path, installation.uid, false)?;
    let opened = file.metadata().map_err(io)?;
    if meta.ino() != opened.ino()
        || meta.dev() != opened.dev()
        || opened.nlink() != 1
        || !opened.is_file()
        || opened.uid() != installation.uid
        || opened.mode() & 0o6022 != 0
    {
        return Err(Error::UnsafeLock);
    }
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => Error::LockContended,
        fs::TryLockError::Error(source) => Error::Io(source),
    })?;
    Ok(file)
}

fn identity(bundle: &Path, version: &str, cancel: &AtomicBool) -> Result<(String, String)> {
    output(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "-R", REQUIREMENT])
            .arg(bundle),
        cancel,
    )?;
    let text = output(
        Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(bundle),
        cancel,
    )?;
    let identity = signing_identity(&text)?;
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        let actual = output(
            Command::new("/usr/bin/plutil")
                .args(["-extract", key, "raw", "-o", "-"])
                .arg(bundle.join("Contents/Info.plist")),
            cancel,
        )?;
        if actual.trim() != version {
            return Err(Error::BundleVersion);
        }
    }
    Ok(identity)
}

fn signing_identity(text: &str) -> Result<(String, String)> {
    let field = |prefix: &'static str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .filter(|value| !value.is_empty() && *value != "not set")
            .map(str::to_owned)
            .ok_or(Error::MissingSignatureField(prefix))
    };
    let team = field("TeamIdentifier=")?;
    let identifier = field("Identifier=")?;
    if identifier != "so.pen.herdr-gpui" {
        return Err(Error::BundleIdentifier);
    }
    Ok((team, identifier))
}

fn authenticate(request: &Request) -> Result<release::Offer> {
    if request.current_version != crate::APP_VERSION
        || release::parse_version(crate::APP_VERSION).is_none()
        || release::parse_version(&request.version) <= release::parse_version(crate::APP_VERSION)
    {
        return Err(Error::StaleRequest);
    }
    let manifest = release::verify_manifest(
        &request.manifest,
        &request.signature,
        option_env!("HERDR_UPDATE_PUBLIC_KEY").ok_or(Error::LocalBuild)?,
        &request.version,
    )?;
    let asset = manifest
        .assets
        .iter()
        .find(|asset| Some(asset.target.as_str()) == release::target())
        .ok_or(Error::MissingPlatformAsset)?
        .clone();
    Ok(release::Offer {
        manifest,
        asset,
        manifest_bytes: request.manifest.clone(),
        signature: request.signature.clone(),
    })
}

fn safe_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.as_os_str().len() <= 4096
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn safe_link(path: &Path, target: &Path) -> bool {
    if target.as_os_str().is_empty() || target.as_os_str().len() > 4096 {
        return false;
    }
    let mut depth = path.components().count() - 1;
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => (),
            Component::ParentDir if depth > 1 => depth -= 1,
            _ => return false,
        }
    }
    depth >= 1
}

fn extract(
    archive: &Path,
    root: &Path,
    mode: Mode,
    name: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let decoder = flate2::read::MultiGzDecoder::new(File::open(archive).map_err(io)?);
    // Bound even tar headers, padding and extension records, not only file data.
    let mut archive = tar::Archive::new(decoder.take(LIMIT + 1));
    let mut seen = HashSet::new();
    let mut distribution_directories = HashSet::new();
    let mut links = Vec::new();
    let mut expanded = 0u64;
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true).mode(0o700);
    for (index, entry) in archive.entries().map_err(io)?.raw(true).enumerate() {
        check(cancel)?;
        if index >= 20000 {
            return Err(Error::ArchiveEntryLimit);
        }
        let mut entry = entry.map_err(io)?;
        let path = entry.path().map_err(io)?.into_owned();
        if !safe_path(&path) || !seen.insert(path.clone()) {
            return Err(Error::ArchivePath);
        }
        for parent in path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            distribution_directories.insert(parent.to_owned());
        }
        let kind = entry.header().entry_type();
        if mode == Mode::Linux {
            if path != Path::new(name) || !kind.is_file() {
                return Err(Error::LinuxPayload);
            }
        } else if !path.starts_with("Herdr.app") {
            return Err(Error::OutsideBundle);
        }
        expanded = expanded
            .checked_add(entry.size())
            .filter(|size| *size <= LIMIT)
            .ok_or(Error::ExpandedArchiveLimit)?;
        let destination = root.join(&path);
        // Symlinks are created only after every regular write has finished.
        if kind.is_symlink() && mode == Mode::Mac {
            let target = entry
                .link_name()
                .map_err(io)?
                .ok_or(Error::MissingLinkTarget)?
                .into_owned();
            if !safe_link(&path, &target) || entry.size() != 0 {
                return Err(Error::ArchiveSymlink);
            }
            links.push((path, target));
        } else if kind.is_dir() && mode == Mode::Mac {
            if entry.size() != 0 {
                return Err(Error::DirectoryData);
            }
            directories.create(&destination).map_err(io)?;
            distribution_directories.insert(path.clone());
        } else if kind.is_file() {
            directories
                .create(destination.parent().ok_or(Error::MissingArchiveParent)?)
                .map_err(io)?;
            let executable = entry.header().mode().map_err(io)? & 0o111 != 0;
            if mode == Mode::Linux && !executable {
                return Err(Error::PayloadNotExecutable);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(if executable { 0o700 } else { 0o600 })
                .open(&destination)
                .map_err(io)?;
            let mut buffer = [0; 65536];
            loop {
                check(cancel)?;
                let count = entry.read(&mut buffer).map_err(io)?;
                if count == 0 {
                    break;
                }
                file.write_all(&buffer[..count]).map_err(io)?;
            }
            // Normalize distribution permissions independently of the process
            // umask, without restoring archive write/special bits or changing data.
            file.set_permissions(fs::Permissions::from_mode(if executable {
                0o755
            } else {
                0o644
            }))
            .map_err(io)?;
            file.sync_all().map_err(io)?;
        } else {
            return Err(Error::ArchiveEntryType);
        }
    }
    let mut reader = archive.into_inner();
    let mut buffer = [0; 65536];
    loop {
        check(cancel)?;
        let count = reader.read(&mut buffer).map_err(io)?;
        if count == 0 {
            break;
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(Error::TrailingArchiveData);
        }
    }
    if reader.limit() == 0 {
        return Err(Error::ExpandedArchiveLimit);
    }
    let link_paths: HashSet<&Path> = links.iter().map(|(path, _)| path.as_path()).collect();
    for path in &seen {
        if path
            .ancestors()
            .skip(1)
            .any(|parent| link_paths.contains(parent))
        {
            return Err(Error::ArchiveSymlinkParent);
        }
    }
    // Include implicit parents and empty directories, but never the private
    // extraction root. Do this before creating any symlinks.
    for path in distribution_directories {
        check(cancel)?;
        let path = root.join(path);
        directories.create(&path).map_err(io)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    for (path, target) in &links {
        let destination = root.join(path);
        directories
            .create(destination.parent().ok_or(Error::MissingSymlinkParent)?)
            .map_err(io)?;
        std::os::unix::fs::symlink(target, destination).map_err(io)?;
    }
    // Also reject escape through a chain of individually relative links.
    for (path, _) in links {
        if !fs::canonicalize(root.join(path))
            .map_err(io)?
            .starts_with(fs::canonicalize(root.join("Herdr.app")).map_err(io)?)
        {
            return Err(Error::SymlinkEscape);
        }
    }
    let candidate = root.join(if mode == Mode::Mac { "Herdr.app" } else { name });
    if seen.is_empty() || !candidate.exists() {
        return Err(Error::EmptyArchive);
    }
    Ok(candidate)
}

fn candidate(
    stage: &Path,
    installation: &Installation,
    offer: &release::Offer,
    cancel: &AtomicBool,
) -> Result<(TempDir, PathBuf)> {
    let archive = stage.join("archive.tar.gz");
    owned(&archive, installation.uid, false)?;
    release::verify_archive(&archive, &offer.asset, cancel)?;
    let tree = private_directory(stage)?;
    let name = format!(
        "herdr-gpui-{}-{}",
        offer.manifest.version, offer.asset.target
    );
    let path = extract(&archive, tree.path(), installation.mode, &name, cancel)?;
    if installation.mode == Mode::Mac {
        if owned(&path.join("Contents/MacOS/Herdr"), installation.uid, false)?.mode() & 0o111 == 0 {
            return Err(Error::BundleNotExecutable);
        }
        let old = identity(&installation.destination, crate::APP_VERSION, cancel)?;
        if identity(&path, &offer.manifest.version, cancel)? != old {
            return Err(Error::SigningIdentityChanged);
        }
    }
    Ok((tree, path))
}

pub(super) fn prepare(
    offer: &release::Offer,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<Prepared> {
    check(cancel)?;
    let installation = detect(cancel)?;
    let lease = lock(&installation)?;
    let stage = private_directory(
        installation
            .destination
            .parent()
            .ok_or(Error::MissingInstallationParent)?,
    )?;
    let mut token = vec![0; 32];
    File::open("/dev/urandom")
        .map_err(io)?
        .read_exact(&mut token)
        .map_err(io)?;
    let request = Request {
        current_version: crate::APP_VERSION.into(),
        version: offer.manifest.version.clone(),
        manifest: offer.manifest_bytes.clone(),
        signature: offer.signature.clone(),
        args: env::args_os().skip(1).map(OsString::into_vec).collect(),
        cwd: env::current_dir().map_err(io)?.into_os_string().into_vec(),
        token,
    };
    let authenticated = authenticate(&request)?;
    if authenticated != *offer {
        return Err(Error::UnauthenticatedOffer);
    }
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() as u64 > REQUEST_LIMIT {
        return Err(Error::RestartArgumentsLimit);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.path().join("request.json"))
        .map_err(io)?;
    file.write_all(&bytes).map_err(io)?;
    file.sync_all().map_err(io)?;
    release::download(
        &authenticated,
        &stage.path().join("archive.tar.gz"),
        cancel,
        progress,
    )?;
    candidate(stage.path(), &installation, &authenticated, cancel)?;
    check(cancel)?;
    Ok(Prepared {
        stage,
        lease,
        installation,
    })
}

fn read_request(stage: &Path, uid: u32) -> Result<Request> {
    let path = stage.join("request.json");
    owned(&path, uid, false)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(REQUEST_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > REQUEST_LIMIT {
        return Err(Error::RequestLimit);
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if request.token.len() != 32
        || request.args.iter().any(|arg| arg.contains(&0))
        || request.cwd.contains(&0)
    {
        return Err(Error::MalformedRequest);
    }
    Ok(request)
}

enum Control {
    Commit,
    Close,
}

/// Drop only closes a pipe and notifies the background owner. The pipe has one
/// writer and receives at most 39 bytes, so commit cannot fill its buffer.
pub(super) struct RestartGuard {
    control: mpsc::Sender<Control>,
    input: Option<ChildStdin>,
    instruction: Vec<u8>,
    committed: bool,
}

impl RestartGuard {
    /// Call only once the UI has decided to quit. Retain the guard until teardown.
    pub(super) fn commit(&mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        self.input
            .as_mut()
            .ok_or(Error::MissingControlPipe)?
            .write_all(&self.instruction)
            .map_err(io)?;
        self.control
            .send(Control::Commit)
            .map_err(|_| Error::HelperOwnerStopped)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for RestartGuard {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.control.send(Control::Close);
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn install_and_restart(prepared: Prepared, cancel: &AtomicBool) -> Result<RestartGuard> {
    check(cancel)?;
    let request = read_request(prepared.stage.path(), prepared.installation.uid)?;
    let mut child = Command::new(&prepared.installation.executable)
        .arg(HELPER)
        .arg(prepared.stage.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io)?;
    let Some(stdout) = child.stdout.take() else {
        kill(&mut child);
        return Err(Error::MissingHelperOutput);
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(6).read_to_end(&mut bytes).map(|_| bytes);
        let _ = ready_tx.send(result);
    });
    let start = Instant::now();
    loop {
        if let Err(error) = check(cancel) {
            kill(&mut child);
            return Err(error);
        }
        if start.elapsed() > WAIT {
            kill(&mut child);
            return Err(Error::HelperTimeout);
        }
        match ready_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(bytes)) if bytes == b"READY\n" => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Ok(Err(source)) => {
                kill(&mut child);
                return Err(Error::Io(source));
            }
            _ => {
                kill(&mut child);
                return Err(Error::HelperNotReady);
            }
        }
    }
    let mut instruction = b"COMMIT\n".to_vec();
    instruction.extend_from_slice(&request.token);
    let (guard, _owner) = own_helper(prepared, child, instruction);
    Ok(guard)
}

fn own_helper(
    mut prepared: Prepared,
    mut child: Child,
    instruction: Vec<u8>,
) -> (RestartGuard, thread::JoinHandle<()>) {
    // Once the helper is armed, unwinding or process teardown must not remove
    // its input. Cancellation explicitly cleans up only after killing/reaping it.
    prepared.stage.disable_cleanup(true);
    let input = child.stdin.take();
    let (control, receiver) = mpsc::channel();
    let owner = thread::spawn(move || {
        let mut committed = false;
        while let Ok(message) = receiver.recv() {
            match message {
                Control::Commit => committed = true,
                Control::Close => break,
            }
        }
        if !committed {
            kill(&mut child);
            let _ = prepared.stage.close();
            return;
        }
        // If the process exits before this worker runs, OS descriptor teardown
        // still closes the barrier and releases the lease; staging survives.
        let _stage = prepared.stage.keep();
        drop(prepared.lease);
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < WAIT => thread::sleep(Duration::from_millis(10)),
                _ => {
                    kill(&mut child);
                    break;
                }
            }
        }
    });
    (
        RestartGuard {
            control,
            input,
            instruction,
            committed: false,
        },
        owner,
    )
}

fn record_result(file: &mut File, result: &Result<()>) -> Result<()> {
    let text = match result {
        Ok(()) => "Update installed and restart process spawned. Startup health is not confirmed.\nKeep previous-installation until the new app is verified working.\n".to_owned(),
        Err(error) => {
            // Bound and escape even filenames/error text; no terminal controls
            // or shell commands from an error are copied into recovery guidance.
            let error: String = error.to_string().chars().flat_map(char::escape_default).take(4096).collect();
            format!("Update installation or restart failed.\nError: {error}\n\nRecovery:\nKeep this private staging directory, archive.tar.gz, and previous-installation (if present).\nClose all Herdr GUI instances; leave the daemon running.\nIf the original installation exists, try launching it manually; rollback may already have restored it.\nIf it is missing, restore previous-installation to the original installation location. Preserve any existing destination before replacing it; do not merge app bundles.\nIf there is no usable backup, manually install a verified signed release.\nNo automatic retry or cleanup will run.\n")
        }
    };
    file.rewind().map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)?;
    file.set_len(text.len() as u64).map_err(io)?;
    file.sync_all().map_err(io)
}

fn replace(
    destination: &Path,
    candidate: &Path,
    backup: &Path,
    mode: Mode,
    launch: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if backup.try_exists().map_err(io)? {
        return Err(Error::RecoveryExists);
    }
    let original = fs::symlink_metadata(destination).map_err(io)?;
    match mode {
        Mode::Linux => fs::hard_link(destination, backup).map_err(io)?,
        Mode::Mac => fs::rename(destination, backup).map_err(io)?,
    }
    if let Err(error) = fs::rename(candidate, destination) {
        if mode == Mode::Mac {
            if let Err(rollback) = fs::rename(backup, destination) {
                return Err(Error::ReplacementRollback {
                    source: error,
                    rollback,
                    backup: backup.to_owned(),
                });
            }
        } else if let Err(cleanup) = remove_failed_linux_backup(destination, backup, &original) {
            return Err(Error::ReplacementCleanup {
                source: error,
                cleanup: Box::new(cleanup),
                backup: backup.to_owned(),
            });
        }
        return Err(io(error));
    }
    if let Err(error) = launch() {
        // Move aside only the exact candidate we just installed; never delete
        // an arbitrary installation tree. Both copies survive failed recovery.
        if let Err(recovery) =
            fs::rename(destination, candidate).and_then(|()| fs::rename(backup, destination))
        {
            return Err(Error::RestartRecovery {
                source: Box::new(error),
                recovery,
                backup: backup.to_owned(),
            });
        }
        return Err(error);
    }
    File::open(
        destination
            .parent()
            .ok_or(Error::MissingDestinationParent)?,
    )
    .map_err(io)?
    .sync_all()
    .map_err(io)
}

fn remove_failed_linux_backup(
    destination: &Path,
    backup: &Path,
    original: &fs::Metadata,
) -> Result<()> {
    let current = fs::symlink_metadata(destination).map_err(io)?;
    let saved = fs::symlink_metadata(backup).map_err(io)?;
    // Remove only our extra hardlink, never a changed destination, symlink,
    // or recovery file whose identity no longer matches the original inode.
    if !original.is_file()
        || [&current, &saved].iter().any(|meta| {
            !meta.is_file() || meta.dev() != original.dev() || meta.ino() != original.ino()
        })
    {
        return Err(Error::RecoveryChanged);
    }
    fs::remove_file(backup).map_err(io)
}

fn helper(stage: &Path) -> Result<()> {
    let cancel = AtomicBool::new(false);
    let installation = detect(&cancel)?;
    no_links(stage)?;
    if stage.parent() != installation.destination.parent()
        || !stage
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b".herdr-update-"))
    {
        return Err(Error::StageLocation);
    }
    if owned(stage, installation.uid, true)?.mode() & 0o077 != 0 {
        return Err(Error::StagePermissions);
    }
    let request = read_request(stage, installation.uid)?;
    let offer = authenticate(&request)?;
    let _handoff = lock_file(&installation, ".update-handoff")?;
    candidate(stage, &installation, &offer, &cancel)?;
    // Create once only after stage/request validation. Holding the descriptor
    // avoids following a replaced result path after the GUI has gone away.
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.join("install-result.txt"))
        .map_err(io)?;
    report
        .write_all(b"Helper armed; no committed installation outcome recorded yet.\n")
        .map_err(io)?;
    report.sync_all().map_err(io)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = std::io::stdin()
            .take(40)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    std::io::stdout().write_all(b"READY\n").map_err(io)?;
    std::io::stdout().flush().map_err(io)?;
    let instruction = receiver
        .recv_timeout(WAIT)
        .map_err(Error::CommitBarrier)?
        .map_err(io)?;
    let mut expected = b"COMMIT\n".to_vec();
    expected.extend_from_slice(&request.token);
    if instruction != expected {
        return Err(Error::NotCommitted);
    }
    let result = (|| {
        let start = Instant::now();
        let _lease = loop {
            match lock_file(&installation, ".update-lock") {
                Ok(lease) => break lease,
                Err(Error::LockContended) if start.elapsed() < Duration::from_secs(5) => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error),
            }
        };
        let fresh = detect(&cancel)?;
        if fresh.destination != installation.destination {
            return Err(Error::InstallationChanged);
        }
        // Never install the previously inspected tree. Rebuild from the authenticated
        // archive after the parent closes the barrier, then validate again.
        let (tree, candidate) = candidate(stage, &installation, &offer, &cancel)?;
        let backup = stage.join("previous-installation");
        let args: Vec<OsString> = request.args.into_iter().map(OsString::from_vec).collect();
        let cwd = PathBuf::from(OsString::from_vec(request.cwd));
        let result = replace(
            &installation.destination,
            &candidate,
            &backup,
            installation.mode,
            || {
                Command::new(&installation.executable)
                    .args(args)
                    .current_dir(cwd)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(io)?;
                Ok(())
            },
        );
        // Retain failed candidate as well as old installation for manual recovery.
        let _ = tree.keep();
        result
    })();
    let recorded = record_result(&mut report, &result);
    match (result, recorded) {
        (Err(error), Err(record)) => Err(Error::RecordOutcome {
            source: Box::new(error),
            record: Box::new(record),
        }),
        (Err(error), _) => Err(error),
        (Ok(()), recorded) => recorded,
    }
}

/// Pass arguments excluding argv[0], before initializing GPUI. Recognized but
/// malformed helper invocations fail closed rather than starting the GUI.
pub(super) fn run_helper(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|arg| arg != HELPER) {
        return None;
    }
    Some(if args.len() == 2 && helper(Path::new(&args[1])).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
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
    fn bundle_distribution_permissions_are_shared_but_staging_stays_private() -> anyhow::Result<()>
    {
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
        assert!(
            linux_location(&executable, &elsewhere.path().canonicalize()?, uid, false).is_err()
        );
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
        assert!(
            signing_identity("Identifier=another.signed.app\nTeamIdentifier=TEAM123\n").is_err()
        );
        assert!(
            signing_identity("Identifier=so.pen.herdr-gpui\nTeamIdentifier=not set\n").is_err()
        );
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
}
