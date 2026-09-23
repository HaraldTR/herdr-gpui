//! Opt-in Unix plaintext storage. All callers run on a background worker.
#![forbid(unsafe_code)]

use super::Result;
use crate::Error;
use secrecy::SecretString;
use std::path::Path;

#[cfg(unix)]
use super::token::{Credential, LIMIT};
#[cfg(unix)]
use rustix::fs::{AtFlags, Mode, OFlags, open, openat, renameat, unlinkat};
#[cfg(unix)]
use rustix::process::geteuid;
#[cfg(unix)]
use secrecy::ExposeSecret;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::{
    fs::File,
    io::{Read, Write},
};
#[cfg(unix)]
use zeroize::Zeroizing;

/// Keeping a token private on disk here depends on `openat`, POSIX ownership,
/// and mode bits. Off POSIX there is no equivalent this crate can rely on, so
/// `Store::File` is never selected and only removal, which has nothing to
/// remove, succeeds.
#[cfg(not(unix))]
pub(super) fn read(_path: &Path) -> Result<Option<SecretString>> {
    Ok(None)
}

#[cfg(not(unix))]
pub(super) fn store(_path: &Path, token: Option<&SecretString>, _plaintext: bool) -> Result<()> {
    if token.is_some() {
        return Err(Error::CredentialUnsupported);
    }
    Ok(())
}

#[cfg(unix)]
const NAME: &std::ffi::CStr = c"github-credentials";
#[cfg(unix)]
fn directory(path: &Path) -> Result<File> {
    let dir = File::from(
        open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| Error::CredentialIo(error.into()))?,
    );
    let metadata = dir.metadata().map_err(Error::CredentialIo)?;
    if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(Error::CredentialPermissions);
    }
    Ok(dir)
}

#[cfg(unix)]
fn existing(dir: &File) -> Result<Option<File>> {
    // Keep access relative to the validated directory, even if its path is replaced.
    let file = match openat(
        dir,
        NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(Error::CredentialIo(error.into())),
    };
    let metadata = file.metadata().map_err(Error::CredentialIo)?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > LIMIT as u64
    {
        return Err(Error::CredentialPermissions);
    }
    Ok(Some(file))
}

#[cfg(unix)]
pub(super) fn read(path: &Path) -> Result<Option<SecretString>> {
    let dir = directory(path)?;
    let Some(file) = existing(&dir)? else {
        return Ok(None);
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT + 1));
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(Error::CredentialIo)?;
    let text = std::str::from_utf8(&bytes).map_err(Error::GitHubEncoding)?;
    let value = SecretString::from(text);
    Credential::decode(&value)?;
    Ok(Some(value))
}

#[cfg(unix)]
pub(super) fn store(path: &Path, token: Option<&SecretString>, plaintext: bool) -> Result<()> {
    if token.is_some() && !plaintext {
        return Err(Error::CredentialPolicy);
    }
    // Explicit removal is allowed even after opting out of plaintext storage.
    write(path, token)
}

#[cfg(unix)]
fn write(path: &Path, token: Option<&SecretString>) -> Result<()> {
    let dir = directory(path)?;
    let present = existing(&dir)?.is_some();
    let Some(token) = token else {
        if present {
            unlinkat(&dir, NAME, AtFlags::empty())
                .map_err(|error| Error::CredentialIo(error.into()))?;
        }
        return dir.sync_all().map_err(Error::CredentialIo);
    };
    Credential::decode(token)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        ".github-credentials-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut file = File::from(
        openat(
            &dir,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| Error::CredentialIo(error.into()))?,
    );
    let result = (|| {
        file.write_all(token.expose_secret().as_bytes())
            .map_err(Error::CredentialIo)?;
        file.sync_all().map_err(Error::CredentialIo)?;
        existing(&dir)?;
        // Replace the directory entry atomically, never a symlink's target.
        renameat(&dir, name.as_str(), &dir, NAME)
            .map_err(|error| Error::CredentialIo(error.into()))?;
        dir.sync_all().map_err(Error::CredentialIo)
    })();
    let _ = unlinkat(&dir, name.as_str(), AtFlags::empty());
    result
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn refresh_record_roundtrips_maximum_escaped_tokens_in_a_private_file() {
        let path = tempfile::tempdir().unwrap();
        std::fs::set_permissions(path.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let access = "\"\\".repeat(2048);
        let refresh = "\\\"".repeat(2048);
        let client = "c".repeat(256);
        let credential = Credential::new(
            access.as_str().into(),
            Some(refresh.as_str().into()),
            &client,
        )
        .unwrap();
        let encoded = credential.encode().unwrap();
        // Every token character needs JSON escaping; the issuing client is also maximal.
        assert!(encoded.expose_secret().len() > 2 * 4096 * 2);
        assert!(encoded.expose_secret().len() <= LIMIT);
        store(path.path(), Some(&encoded), true).unwrap();
        let file = path.path().join("github-credentials");
        let metadata = std::fs::metadata(&file).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), geteuid().as_raw());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.len(), encoded.expose_secret().len() as u64);
        let saved = read(path.path()).unwrap().unwrap();
        assert_eq!(saved.expose_secret(), encoded.expose_secret());
        let restored = Credential::decode(&saved).unwrap();
        assert_eq!(restored.access_token.expose_secret(), access);
        assert_eq!(
            restored.refresh_token.as_ref().unwrap().expose_secret(),
            refresh
        );
        let record: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        assert_eq!(record["client_id"], client);
        assert_eq!(record["version"], 1);
        store(path.path(), None, false).unwrap();
        assert!(read(path.path()).unwrap().is_none());
        assert!(!file.exists());
    }

    #[test]
    fn private_atomic_roundtrip_and_unsafe_files_rejected() {
        let path =
            std::env::temp_dir().join(format!("herdr-credential-test-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let token: SecretString = "fixture-not-a-real-token".into();
        assert!(read(&path).unwrap().is_none());
        let error = store(&path, Some(&token), false).unwrap_err();
        assert!(!error.to_string().contains(token.expose_secret()));
        assert!(read(&path).unwrap().is_none());
        store(&path, Some(&token), true).unwrap();
        store(&path, None, false).unwrap();
        assert!(!path.join("github-credentials").exists());
        assert!(read(&path).unwrap().is_none());
        write(&path, Some(&token)).unwrap();
        write(&path, Some(&token)).unwrap();
        assert_eq!(
            read(&path).unwrap().unwrap().expose_secret(),
            token.expose_secret()
        );
        assert!(write(&path, Some(&"invalid\ntoken".into())).is_err());
        assert_eq!(
            read(&path).unwrap().unwrap().expose_secret(),
            token.expose_secret()
        );
        let file = path.join("github-credentials");
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path).is_err());
        assert!(write(&path, Some(&token)).is_err());
        std::fs::remove_file(&file).unwrap();
        write(&path, Some(&token)).unwrap();
        let hardlink = path.join("hardlink");
        std::fs::hard_link(&file, &hardlink).unwrap();
        assert!(read(&path).is_err());
        assert!(write(&path, Some(&token)).is_err());
        std::fs::remove_file(&hardlink).unwrap();
        std::fs::write(&file, vec![b'x'; LIMIT + 1]).unwrap();
        assert!(read(&path).is_err());
        assert!(write(&path, Some(&token)).is_err());
        std::fs::remove_file(&file).unwrap();
        let target = path.join("untouched");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&target, &file).unwrap();
        assert!(read(&path).is_err());
        assert!(write(&path, Some(&token)).is_err());
        assert!(write(&path, None).is_err());
        assert!(store(&path, None, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_file(&file).unwrap();
        write(&path, Some(&token)).unwrap();
        write(&path, None).unwrap();
        assert!(read(&path).unwrap().is_none());
        let linked_dir = path.join("linked-directory");
        symlink(&path, &linked_dir).unwrap();
        assert!(read(&linked_dir).is_err());
        assert!(write(&linked_dir, Some(&token)).is_err());
        std::fs::create_dir(&file).unwrap();
        assert!(read(&path).is_err());
        assert!(write(&path, Some(&token)).is_err());
        assert!(write(&path, None).is_err());
        std::fs::remove_dir(&file).unwrap();

        let original = path.join("original");
        let moved = path.join("moved");
        std::fs::create_dir(&original).unwrap();
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
        write(&original, Some(&token)).unwrap();
        let pinned = directory(&original).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
        write(&original, Some(&"replacement-fixture".into())).unwrap();
        let mut bytes = Zeroizing::new(Vec::with_capacity(4097));
        existing(&pinned)
            .unwrap()
            .unwrap()
            .take(4097)
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes.as_slice(), token.expose_secret().as_bytes());
        assert_eq!(
            read(&original).unwrap().unwrap().expose_secret(),
            "replacement-fixture"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(write(&path, Some(&token)).is_err());
    }
}
