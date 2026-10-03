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
mod tests;
