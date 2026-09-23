//! Diagnostics for a failing GitHub exchange. Records go to the process-local
//! log window only, and carry public metadata: status, response headers that
//! explain an enterprise rejection, and the connection's own diagnosis. Never a
//! token, a device code, a request field, or a response body.

use crate::Error;

/// A short, public response header, or `""` when GitHub did not send it.
pub(super) fn header<'a>(headers: &'a ureq::http::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The SSO challenge without its one-time authorization request identifier, so
/// the log names the organization that must approve the token and nothing more.
pub(super) fn public_sso(challenge: &str) -> &str {
    challenge.split('?').next().unwrap_or(challenge)
}

/// Why a GitHub call failed, from public headers only. Enterprise sign-ins fail
/// on exactly these (SSO, scopes, org policy), and the request id is what GitHub
/// support asks for.
pub(super) fn http(context: &'static str, status: u16, headers: &ureq::http::HeaderMap) {
    tracing::warn!(
        category = "github_http",
        context,
        status,
        request_id = header(headers, "x-github-request-id"),
        scopes = header(headers, "x-oauth-scopes"),
        accepted_scopes = header(headers, "x-accepted-oauth-scopes"),
        sso = public_sso(header(headers, "x-github-sso")),
        "GitHub request failed"
    );
}

/// Transport failures carry the connection's own diagnosis, not the exchange:
/// a proxy, TLS interception, or DNS failure is invisible otherwise.
pub(super) fn network(context: &'static str) -> impl FnOnce(ureq::Error) -> Error {
    move |error| {
        tracing::warn!(
            category = "github_network",
            context,
            transport = %error,
            "GitHub request did not complete"
        );
        Error::GitHubNetwork(error)
    }
}

/// Stable label for a failure, so the log window can be filtered by cause
/// without matching on user-facing sentences.
pub(super) fn kind(error: &Error) -> &'static str {
    match error {
        Error::GitHubAuthentication => "authentication",
        Error::GitHubForbidden => "forbidden",
        Error::GitHubRateLimit => "rate_limit",
        Error::GitHubStatus(_) => "status",
        Error::GitHubRead(_) => "read",
        Error::GitHubSize => "size",
        Error::GitHubJson(_) => "json",
        Error::GitHubToken => "token",
        Error::GitHubTokenType => "token_type",
        Error::GitHubEncoding(_) => "encoding",
        Error::GitHubNetwork(_) => "network",
        Error::GitHubHeader(_) => "header",
        Error::GitHubDevice => "device",
        Error::GitHubExpired => "expired",
        Error::GitHubDenied => "denied",
        Error::GitHubAuthorization => "authorization",
        Error::GitHubQuery => "query",
        Error::GitHubWorker(_) => "worker",
        #[cfg(target_os = "macos")]
        Error::KeychainRead(_) => "keychain_read",
        #[cfg(target_os = "macos")]
        Error::KeychainWrite(_) => "keychain_write",
        Error::CredentialDirectory => "credential_directory",
        Error::CredentialPermissions => "credential_permissions",
        Error::CredentialIo(_) => "credential_io",
        Error::CredentialPolicy => "credential_policy",
        Error::CredentialUnsupported => "credential_unsupported",
        _ => "other",
    }
}

/// Records the failed step next to the message the menu shows.
pub(super) fn failure(context: &'static str, error: &Error) {
    let transport = match error {
        Error::GitHubNetwork(source) => source.to_string(),
        _ => String::new(),
    };
    tracing::warn!(
        category = "github_failure",
        context,
        kind = kind(error),
        transport = transport.as_str(),
        detail = %error,
        "GitHub operation failed"
    );
}
