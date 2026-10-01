//! Persist the device-flow refresh token with its access token and issuing client.
//! Legacy entries contain only an access token; they remain readable.

use super::{Profile, Result, device::TokenResponse, http::oauth, valid_token};
use crate::Error;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use zeroize::Zeroizing;

pub(super) const LIMIT: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credential {
    version: u8,
    pub(super) access_token: SecretString,
    pub(super) refresh_token: Option<SecretString>,
    client_id: String,
    #[serde(default)]
    expires_at: Option<u64>,
}

impl Credential {
    pub(super) fn new(
        access_token: SecretString,
        refresh_token: Option<SecretString>,
        client_id: &str,
    ) -> Result<Self> {
        let credential = Self {
            version: 1,
            access_token,
            refresh_token,
            client_id: client_id.into(),
            expires_at: None,
        };
        credential.validate()?;
        Ok(credential)
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1
            || !valid_token(self.access_token.expose_secret())
            || self.refresh_token.as_ref().is_some_and(|token| {
                !valid_token(token.expose_secret())
                    || self.client_id.is_empty()
                    || self.client_id.len() > 256
                    || !self
                        .client_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            })
        {
            return Err(Error::GitHubToken);
        }
        Ok(())
    }

    pub(super) fn with_expiry(mut self, seconds: Option<u64>, now: std::time::SystemTime) -> Self {
        self.expires_at = seconds.and_then(|seconds| {
            now.duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs()
                .checked_add(seconds)
        });
        self
    }

    fn renewal_due(&self, now: std::time::SystemTime) -> bool {
        self.refresh_token.is_some()
            && self.expires_at.is_some_and(|expires| {
                now.duration_since(std::time::UNIX_EPOCH)
                    .is_ok_and(|now| expires <= now.as_secs().saturating_add(10 * 60))
            })
    }

    pub(super) fn decode(value: &SecretString) -> Result<Self> {
        let text = value.expose_secret().trim();
        if text.len() > LIMIT {
            return Err(Error::GitHubToken);
        }
        if !text.starts_with('{') {
            return Self::new(text.into(), None, "");
        }
        let credential: Self = serde_json::from_str(text).map_err(Error::github_json)?;
        credential.validate()?;
        Ok(credential)
    }

    pub(super) fn encode(&self) -> Result<SecretString> {
        self.validate()?;
        // Keep non-expiring OAuth tokens in the legacy format. Explicitly expose
        // the pair only into a preallocated, wiped serialization buffer.
        if self.refresh_token.is_none() {
            return Ok(self.access_token.expose_secret().into());
        }
        #[derive(Serialize)]
        struct Record<'a> {
            version: u8,
            access_token: &'a str,
            refresh_token: Option<&'a str>,
            client_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            expires_at: Option<u64>,
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT));
        serde_json::to_writer(
            &mut *bytes,
            &Record {
                version: self.version,
                access_token: self.access_token.expose_secret(),
                refresh_token: self.refresh_token.as_ref().map(ExposeSecret::expose_secret),
                client_id: &self.client_id,
                expires_at: self.expires_at,
            },
        )
        .map_err(Error::github_json)?;
        let text = std::str::from_utf8(&bytes).map_err(Error::GitHubEncoding)?;
        Ok(text.into())
    }

    pub(super) fn refresh(&self) -> Result<Self> {
        let refresh = self
            .refresh_token
            .as_ref()
            .ok_or(Error::GitHubAuthentication)?;
        tracing::info!(category = "github_refresh", "Renewing saved GitHub sign-in");
        let reply: TokenResponse = oauth(
            "oauth/access_token",
            &[
                ("client_id", &self.client_id),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.expose_secret()),
            ],
            Duration::from_secs(15),
        )?;
        match super::device::token_reply(reply, &self.client_id)? {
            super::Reply::Token(credential) => Ok(credential),
            _ => Err(Error::GitHubToken),
        }
    }

    /// Renew shortly before expiry, or on rejection for older saved records
    /// without expiry metadata. Persist rotations before using the new token.
    pub(super) fn profile_with(
        self,
        mut profile: impl FnMut(Arc<SecretString>) -> Result<Profile>,
        refresh: impl FnOnce(&Self) -> Result<Self>,
        persist: impl FnOnce(&SecretString) -> Result<()>,
    ) -> Result<Profile> {
        if self.renewal_due(std::time::SystemTime::now()) {
            let renewed = refresh(&self)?;
            persist(&renewed.encode()?)?;
            return profile(Arc::new(renewed.access_token));
        }
        match profile(Arc::new(self.access_token.expose_secret().into())) {
            Err(Error::GitHubAuthentication) if self.refresh_token.is_some() => {
                let renewed = refresh(&self)?;
                // GitHub invalidates the old pair on rotation. Persist the new
                // pair before any further HTTP request, even if profile fails.
                persist(&renewed.encode()?)?;
                profile(Arc::new(renewed.access_token))
            }
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::github::{Reply, device::token_reply};

    fn credential() -> Credential {
        Credential::new("old-access".into(), Some("old-refresh".into()), "client").unwrap()
    }

    fn renewed() -> Credential {
        Credential::new("new-access".into(), Some("new-refresh".into()), "client").unwrap()
    }

    fn profile(token: Arc<SecretString>) -> Result<Profile> {
        if token.expose_secret() == "old-access" {
            return Err(Error::GitHubAuthentication);
        }
        Ok(Profile {
            login: "fixture".into(),
            avatar: None,
            token,
            avatar_updates: None,
        })
    }

    #[test]
    fn expiry_survives_storage_and_renews_before_the_first_profile_request() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let value = credential().with_expiry(Some(3600), now);
        let restored = Credential::decode(&value.encode().unwrap()).unwrap();
        assert!(!restored.renewal_due(now + Duration::from_secs(2999)));
        assert!(restored.renewal_due(now + Duration::from_secs(3000)));
        assert!(restored.renewal_due(now + Duration::from_secs(3601)));

        let persisted = std::cell::Cell::new(false);
        let loaded = restored
            .profile_with(
                |token| {
                    assert!(persisted.get(), "persist before any authenticated request");
                    assert_eq!(token.expose_secret(), "new-access");
                    profile(token)
                },
                |_| Ok(renewed()),
                |_| {
                    persisted.set(true);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(loaded.token.expose_secret(), "new-access");
    }

    #[test]
    fn oauth_expiry_is_preserved_and_missing_or_overflowing_expiry_is_safe() {
        let reply: TokenResponse = serde_json::from_str(
            r#"{"access_token":"access","refresh_token":"refresh","token_type":"bearer","expires_in":28800}"#,
        )
        .unwrap();
        let Reply::Token(value) = token_reply(reply, "client").unwrap() else {
            panic!("expected credential");
        };
        assert!(value.expires_at.is_some());
        assert_eq!(
            Credential::decode(&value.encode().unwrap())
                .unwrap()
                .expires_at,
            value.expires_at
        );
        assert!(!value.renewal_due(std::time::SystemTime::now()));
        assert!(!credential().renewal_due(std::time::SystemTime::now()));
        assert!(
            credential()
                .with_expiry(Some(u64::MAX), std::time::SystemTime::now())
                .expires_at
                .is_none()
        );
    }

    #[test]
    fn restart_renews_expired_saved_token_and_persists_the_rotated_pair() {
        let disk = std::cell::RefCell::new(credential().encode().unwrap());
        let restored = Credential::decode(&disk.borrow()).unwrap();
        let loaded = restored
            .profile_with(
                |token| {
                    if token.expose_secret() == "new-access" {
                        assert_eq!(
                            Credential::decode(&disk.borrow())
                                .unwrap()
                                .access_token
                                .expose_secret(),
                            "new-access"
                        );
                    }
                    profile(token)
                },
                |old| {
                    assert_eq!(
                        old.refresh_token.as_ref().unwrap().expose_secret(),
                        "old-refresh"
                    );
                    assert_eq!(old.client_id, "client");
                    Ok(renewed())
                },
                |value| {
                    *disk.borrow_mut() = value.expose_secret().into();
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(loaded.token.expose_secret(), "new-access");
        let saved = Credential::decode(&disk.borrow()).unwrap();
        assert_eq!(
            saved.refresh_token.as_ref().unwrap().expose_secret(),
            "new-refresh"
        );
        saved
            .profile_with(
                profile,
                |_| panic!("no second refresh"),
                |_| panic!("no rewrite"),
            )
            .unwrap();
    }

    #[test]
    fn legacy_and_nonexpiring_tokens_survive_without_rotation() {
        let old = Credential::decode(&" legacy-token ".into()).unwrap();
        assert_eq!(old.encode().unwrap().expose_secret(), "legacy-token");
        assert!(old.refresh_token.is_none());
        old.profile_with(
            profile,
            |_| panic!("legacy cannot refresh"),
            |_| panic!("legacy cannot write"),
        )
        .unwrap();
        assert!(matches!(
            Credential::decode(&"old-access".into())
                .unwrap()
                .profile_with(profile, |_| panic!(), |_| panic!()),
            Err(Error::GitHubAuthentication)
        ));
    }

    #[test]
    fn refresh_can_replace_an_expiring_pair_with_a_nonexpiring_access_only_token() {
        let saved = std::cell::RefCell::new(credential().encode().unwrap());
        let loaded = credential()
            .profile_with(
                |token| {
                    if token.expose_secret() == "new-access" {
                        assert_eq!(saved.borrow().expose_secret(), "new-access");
                    }
                    profile(token)
                },
                |old| {
                    let response: TokenResponse = serde_json::from_str(
                        r#"{"access_token":"new-access","token_type":"bearer"}"#,
                    )
                    .unwrap();
                    let Reply::Token(renewed) = token_reply(response, &old.client_id)? else {
                        panic!("an access-only refresh response is valid");
                    };
                    assert!(renewed.refresh_token.is_none());
                    Ok(renewed)
                },
                |value| {
                    *saved.borrow_mut() = value.expose_secret().into();
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(loaded.token.expose_secret(), "new-access");
        let restored = Credential::decode(&saved.borrow()).unwrap();
        assert!(restored.refresh_token.is_none());
        assert_eq!(
            restored
                .profile_with(
                    profile,
                    |_| panic!("nonexpiring token must not refresh"),
                    |_| panic!("no rewrite"),
                )
                .unwrap()
                .token
                .expose_secret(),
            "new-access"
        );
    }

    #[test]
    fn transient_failure_never_refreshes_or_deletes_saved_credentials() {
        for error in [
            Error::GitHubRateLimit,
            Error::GitHubStatus(500),
            Error::GitHubForbidden,
        ] {
            let mut error = Some(error);
            assert!(
                credential()
                    .profile_with(
                        |_| Err(error.take().unwrap()),
                        |_| panic!("must not refresh"),
                        |_| panic!("must not write")
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn refresh_and_persistence_failures_are_reported_without_profile_retry() {
        assert!(matches!(
            credential().profile_with(
                profile,
                |_| Err(Error::GitHubDenied),
                |_| panic!("failed rotation must not write")
            ),
            Err(Error::GitHubDenied)
        ));
        assert!(matches!(
            credential().profile_with(
                |token| {
                    assert_eq!(token.expose_secret(), "old-access");
                    Err(Error::GitHubAuthentication)
                },
                |_| Ok(renewed()),
                |_| Err(Error::CredentialPolicy)
            ),
            Err(Error::CredentialPolicy)
        ));
        let wrote = std::cell::Cell::new(false);
        assert!(matches!(
            credential().profile_with(
                |token| {
                    if token.expose_secret() == "old-access" {
                        Err(Error::GitHubAuthentication)
                    } else {
                        assert!(wrote.get());
                        Err(Error::GitHubStatus(503))
                    }
                },
                |_| Ok(renewed()),
                |_| {
                    wrote.set(true);
                    Ok(())
                }
            ),
            Err(Error::GitHubStatus(503))
        ));
        assert!(wrote.get());
    }

    #[test]
    fn stored_records_are_bounded_validated_and_redacted() {
        let value = credential();
        let debug = format!("{value:?}");
        assert!(!debug.contains("old-access"));
        assert!(!debug.contains("old-refresh"));
        for record in [
            r#"{"version":2,"access_token":"secret","refresh_token":"secret","client_id":"client"}"#,
            r#"{"version":1,"access_token":"secret","refresh_token":"","client_id":"client"}"#,
            r#"{"version":1,"access_token":"secret","refresh_token":"secret","client_id":""}"#,
            r#"{"version":1,"access_token":"secret","refresh_token":123,"client_id":"client"}"#,
            r#"{"version":1,"access_token":"secret","refresh_token":"secret","client_id":"client","extra":0}"#,
            "bad\ntoken",
        ] {
            let error = Credential::decode(&record.into()).unwrap_err();
            assert!(!format!("{error:?}").contains("secret"));
            assert!(!error.to_string().contains("secret"));
        }
        assert!(Credential::decode(&"x".repeat(LIMIT + 1).into()).is_err());
        assert!(Credential::new("x".repeat(4097).into(), None, "client").is_err());
    }
}
