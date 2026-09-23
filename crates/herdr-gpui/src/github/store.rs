//! Where a GitHub token lives for this build, and the precedence between the
//! environment and that store. Only a signed release build uses the Keychain;
//! every other build says plainly that the token sits unencrypted on disk.

use super::{Profile, Result, credentials, profile, token::Credential};
use crate::Error;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

// All windows share one credential. A refresh token is single-use, and a late
// renewal must not recreate an entry after another window has deleted it.
// Called only by background workers; never hold this lock on the UI thread.
fn transaction<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    static ACCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ACCESS.lock().unwrap_or_else(|error| error.into_inner());
    work()
}

pub(crate) const PLAINTEXT_WARNING: &str = "WARNING: plaintext credential storage is enabled. GitHub tokens are unencrypted on disk; software running as you and backups can read them.";
pub(crate) const DEVELOPMENT_WARNING: &str = "WARNING: this development build does not use the macOS Keychain. GitHub tokens are unencrypted beside the GUI config; software running as you and backups can read them.";
pub(crate) const KEYCHAIN_NOTICE: &str =
    "Credentials are saved in macOS Keychain. macOS may ask you to unlock or approve access.";

#[cfg(target_os = "macos")]
const SERVICE: &str = "dev.herdr.gpui.github";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "github.com";

pub(super) fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic())
}

pub(super) fn resolve_token(
    gh: Option<SecretString>,
    github: Option<SecretString>,
    saved: impl FnOnce() -> Result<Option<SecretString>>,
) -> Result<SecretString> {
    let token = match gh
        .filter(|s| !s.expose_secret().trim().is_empty())
        .or_else(|| github.filter(|s| !s.expose_secret().trim().is_empty()))
    {
        Some(token) => Some(token),
        None => saved()?,
    }
    .ok_or(Error::GitHubAuthentication)?;
    let trimmed = token.expose_secret().trim();
    if !valid_token(trimmed) {
        return Err(Error::GitHubToken);
    }
    if trimmed.len() == token.expose_secret().len() {
        Ok(token)
    } else {
        Ok(trimmed.into())
    }
}

pub(super) fn credential_bytes(bytes: Vec<u8>) -> Result<SecretString> {
    let bytes = Zeroizing::new(bytes);
    // Validate by borrowing so invalid UTF-8 never escapes in FromUtf8Error.
    std::str::from_utf8(&bytes)
        .map(SecretString::from)
        .map_err(Error::GitHubEncoding)
}

pub(super) fn environment_token(name: &str) -> Result<Option<SecretString>> {
    // Own and wipe even non-Unicode environment values. The process environment
    // itself is outside this allocation's lifetime and is not erased here.
    std::env::var_os(name)
        .map(|value| credential_bytes(value.into_encoded_bytes()))
        .transpose()
}

/// Where a saved GitHub token lives for this build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Store {
    /// No persistent store: `GH_TOKEN` / `GITHUB_TOKEN` only.
    #[default]
    Environment,
    /// The app-specific macOS login Keychain entry.
    Keychain,
    /// A private `0600` file beside the GUI config.
    File,
}

// A development build is unsigned and gets a fresh code identity on every rebuild,
// so macOS would re-prompt for the Keychain item's ACL on every run. Only the
// signed release pipeline sets `HERDR_RELEASE_VERSION`, so only it uses Keychain.
pub(super) const KEYCHAIN: bool = cfg!(target_os = "macos") && crate::RELEASE_BUILD;
// The private file relies on POSIX ownership and mode bits, so platforms
// without them keep the environment as their only source of a saved token.
pub(super) const FILE: bool = cfg!(unix);
// Those development builds have no other secure store to fall back on, so the
// private file is their default. Every other platform keeps it an explicit opt-in.
pub(super) const FILE_DEFAULT: bool = cfg!(target_os = "macos") && !KEYCHAIN;
// An unsigned build must never reach the Keychain, whatever else changes here.
const _: () = assert!(!KEYCHAIN || crate::RELEASE_BUILD);

impl Store {
    pub(crate) fn select(config: &crate::config::Config) -> Self {
        Self::choose(
            config.github.allow_plaintext_credentials && FILE,
            KEYCHAIN,
            FILE_DEFAULT,
        )
    }

    pub(super) const fn choose(opted_in: bool, keychain: bool, file_default: bool) -> Self {
        if keychain {
            Self::Keychain
        } else if opted_in || file_default {
            Self::File
        } else {
            Self::Environment
        }
    }

    /// Credential handling shown in the GitHub menu, so storage is never implicit.
    pub(crate) fn note(self, connected: bool) -> Option<Note> {
        match self {
            Self::File if FILE_DEFAULT => Some(Note::Warning(DEVELOPMENT_WARNING)),
            Self::File => Some(Note::Warning(PLAINTEXT_WARNING)),
            Self::Keychain if !connected => Some(Note::Info(KEYCHAIN_NOTICE)),
            Self::Keychain | Self::Environment => None,
        }
    }
}

pub(crate) enum Note {
    Warning(&'static str),
    Info(&'static str),
}

#[cfg(target_os = "macos")]
fn keychain_token() -> Result<Option<SecretString>> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => credential_bytes(bytes).map(Some),
        Err(e) if e.code() == -25300 => Ok(None),
        Err(error) => Err(Error::KeychainRead(error)),
    }
}

// `Store::Keychain` is never selected off macOS; the stubs keep the match total.
#[cfg(not(target_os = "macos"))]
fn keychain_token() -> Result<Option<SecretString>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn keychain_save(token: Option<&SecretString>) -> Result<()> {
    use security_framework::passwords::{delete_generic_password, set_generic_password};
    let result = match token {
        Some(token) => set_generic_password(SERVICE, ACCOUNT, token.expose_secret().as_bytes()),
        None => delete_generic_password(SERVICE, ACCOUNT),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if token.is_none() && e.code() == -25300 => Ok(()),
        Err(error) => Err(Error::KeychainWrite(error)),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_save(_: Option<&SecretString>) -> Result<()> {
    Err(Error::CredentialPolicy)
}

pub(super) fn saved_token(store: Store) -> Result<Option<SecretString>> {
    match store {
        Store::Environment => Ok(None),
        Store::Keychain => keychain_token(),
        Store::File => credentials::read(&credential_directory()?),
    }
}

pub(super) fn load_profile(store: Store) -> Result<Option<Profile>> {
    let gh = environment_token("GH_TOKEN")?;
    let github = if gh
        .as_ref()
        .is_none_or(|s| s.expose_secret().trim().is_empty())
    {
        environment_token("GITHUB_TOKEN")?
    } else {
        None
    };
    if gh
        .as_ref()
        .is_none_or(|s| s.expose_secret().trim().is_empty())
        && github
            .as_ref()
            .is_none_or(|s| s.expose_secret().trim().is_empty())
    {
        return transaction(|| {
            let saved = saved_token(store)?;
            tracing::info!(
                category = "github_restore",
                ?store,
                present = saved.is_some(),
                "Checking saved GitHub sign-in"
            );
            saved
                .map(|token| {
                    Credential::decode(&token)?.profile_with(
                        profile,
                        Credential::refresh,
                        |value| save_unlocked(Some(value), store),
                    )
                })
                .transpose()
        });
    }
    tracing::info!(
        category = "github_restore",
        "Checking environment GitHub token"
    );
    let token = resolve_token(gh, github, || Ok(None))?;
    profile(std::sync::Arc::new(token)).map(Some)
}

pub(super) fn credential_directory() -> Result<std::path::PathBuf> {
    crate::config::Config::path()?
        .parent()
        .map(std::path::Path::to_owned)
        .ok_or(Error::CredentialDirectory)
}

pub(super) fn save(token: Option<&SecretString>, store: Store) -> Result<()> {
    transaction(|| save_unlocked(token, store))
}

fn save_unlocked(token: Option<&SecretString>, store: Store) -> Result<()> {
    match store {
        Store::Keychain => keychain_save(token),
        // Removal stays allowed without an opt-in, so a file written under an
        // earlier policy is still cleaned up by an explicit sign-out.
        Store::Environment => credentials::store(&credential_directory()?, token, false),
        Store::File => credentials::store(&credential_directory()?, token, true),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::{
        sync::{Mutex, mpsc},
        thread,
        time::Duration,
    };

    #[test]
    fn competing_restores_load_the_rotated_credential_inside_the_transaction() {
        let saved = Mutex::new(
            Credential::new("old-access".into(), Some("old-refresh".into()), "client")
                .unwrap()
                .encode()
                .unwrap(),
        );
        let (refresh_tx, refresh_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (competing_tx, competing_rx) = mpsc::sync_channel(1);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        thread::scope(|scope| {
            let saved = &saved;
            let first = scope.spawn(move || {
                transaction(|| {
                    let credential = Credential::decode(&saved.lock().unwrap())?;
                    credential.profile_with(
                        |token| {
                            if token.expose_secret() == "old-access" {
                                return Err(Error::GitHubAuthentication);
                            }
                            assert_eq!(token.expose_secret(), "new-access");
                            Ok(Profile {
                                login: "fixture".into(),
                                avatar: None,
                                token,
                                avatar_updates: None,
                            })
                        },
                        |_| {
                            refresh_tx.send(()).unwrap();
                            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                            Credential::new(
                                "new-access".into(),
                                Some("new-refresh".into()),
                                "client",
                            )
                        },
                        |value| {
                            *saved.lock().unwrap() = value.expose_secret().into();
                            Ok(())
                        },
                    )
                })
                .unwrap()
            });
            refresh_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let second = scope.spawn(move || {
                competing_tx.send(()).unwrap();
                transaction(|| {
                    entered_tx.send(()).unwrap();
                    let credential = Credential::decode(&saved.lock().unwrap())?;
                    assert_eq!(credential.access_token.expose_secret(), "new-access");
                    assert_eq!(
                        credential.refresh_token.as_ref().unwrap().expose_secret(),
                        "new-refresh"
                    );
                    credential.profile_with(
                        |token| {
                            Ok(Profile {
                                login: "fixture".into(),
                                avatar: None,
                                token,
                                avatar_updates: None,
                            })
                        },
                        |_| panic!("the competing restore must not reuse the old refresh token"),
                        |_| panic!("the competing restore must not rewrite the credential"),
                    )
                })
                .unwrap()
            });
            competing_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                entered_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            release_tx.send(()).unwrap();
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(first.join().unwrap().token.expose_secret(), "new-access");
            assert_eq!(second.join().unwrap().token.expose_secret(), "new-access");
        });
    }

    #[test]
    fn signout_transaction_waits_for_refresh_and_removes_the_rotated_credential() {
        let saved = Mutex::new(Some(SecretString::from("old-access")));
        let (refresh_tx, refresh_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (signout_tx, signout_rx) = mpsc::sync_channel(1);
        let (deleted_tx, deleted_rx) = mpsc::sync_channel(1);
        thread::scope(|scope| {
            let saved = &saved;
            let refresh = scope.spawn(move || {
                transaction(|| {
                    refresh_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let renewed =
                        Credential::new("new-access".into(), Some("new-refresh".into()), "client")?;
                    *saved.lock().unwrap() = Some(renewed.encode()?);
                    // Different return types must still share the process-wide lock.
                    Ok(renewed)
                })
                .unwrap()
            });
            refresh_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let signout = scope.spawn(move || {
                signout_tx.send(()).unwrap();
                transaction(|| {
                    let removed = saved.lock().unwrap().take().unwrap();
                    deleted_tx.send(()).unwrap();
                    let credential = Credential::decode(&removed)?;
                    assert_eq!(credential.access_token.expose_secret(), "new-access");
                    assert_eq!(
                        credential.refresh_token.as_ref().unwrap().expose_secret(),
                        "new-refresh"
                    );
                    Ok(())
                })
                .unwrap();
            });
            signout_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                deleted_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            release_tx.send(()).unwrap();
            deleted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                refresh.join().unwrap().access_token.expose_secret(),
                "new-access"
            );
            signout.join().unwrap();
        });
        assert!(saved.lock().unwrap().is_none());
    }
}
