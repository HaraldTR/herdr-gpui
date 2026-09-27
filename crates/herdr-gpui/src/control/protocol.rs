//! The control socket's messages: one JSON request line, one JSON response
//! line, then the connection closes.
use serde::{Deserialize, Serialize};

/// Bounds a request and a response alike; an address is at most 8 KiB.
#[cfg(unix)]
pub(crate) const MAX_MESSAGE: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub(crate) enum Request {
    #[serde(rename = "browser.open")]
    BrowserOpen(BrowserOpen),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserOpen {
    pub url: String,
    /// The Herdr workspace to open the tab in, normally the caller's own
    /// `HERDR_WORKSPACE_ID`. Absent, the tab opens in the workspace the
    /// frontmost window shows.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The client socket of the caller's daemon, which tells two local
    /// sessions' identical workspace IDs apart.
    #[serde(default)]
    pub daemon_socket: Option<String>,
    /// Whether the window switches to the new tab.
    pub focus: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenedIn {
    /// A browser tab inside the window.
    Tab,
    /// The system browser, on a platform whose build cannot show pages.
    SystemBrowser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    InvalidUrl,
    NoWindow,
    WorkspaceNotFound,
    TabLimit,
    Busy,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Response {
    Opened {
        opened_in: OpenedIn,
        workspace_id: Option<String>,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[cfg(any(unix, test))]
impl Response {
    pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn requests_have_a_stable_wire_form() {
        let request = Request::BrowserOpen(BrowserOpen {
            url: "http://localhost:3000/".into(),
            workspace_id: Some("w_1".into()),
            daemon_socket: None,
            focus: true,
        });
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"method":"browser.open","url":"http://localhost:3000/","workspace_id":"w_1","daemon_socket":null,"focus":true}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        let minimal: Request = serde_json::from_str(
            r#"{"method":"browser.open","url":"https://a.test","focus":false}"#,
        )
        .unwrap();
        assert!(matches!(
            minimal,
            Request::BrowserOpen(BrowserOpen {
                workspace_id: None,
                ..
            })
        ));
    }

    #[test]
    fn unknown_methods_and_fields_are_rejected() {
        for invalid in [
            r#"{"method":"browser.eval","url":"https://a.test","focus":true}"#,
            r#"{"method":"browser.open","url":"https://a.test","focus":true,"extra":1}"#,
            r#"{"method":"browser.open","focus":true}"#,
            r#"["browser.open"]"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn responses_round_trip() {
        for response in [
            Response::Opened {
                opened_in: OpenedIn::Tab,
                workspace_id: Some("w_1".into()),
            },
            Response::error(ErrorCode::WorkspaceNotFound, "No window shows w_9"),
        ] {
            let text = serde_json::to_string(&response).unwrap();
            assert_eq!(serde_json::from_str::<Response>(&text).unwrap(), response);
        }
        assert_eq!(
            serde_json::to_string(&Response::error(ErrorCode::Busy, "x")).unwrap(),
            r#"{"status":"error","code":"busy","message":"x"}"#
        );
    }
}
