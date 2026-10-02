//! Typed requests and results for the daemon's scrollback methods.
//!
//! Field names, enum spellings, and the `type` tag of each result match
//! Herdr's endpoint schema (`api::schema::panes` and `ResponseResult`). Rows
//! are absolute screen-buffer rows counted from the top of the retained
//! scrollback, so a point stays put while the pane scrolls; the viewport's
//! top row is `max_offset_from_bottom - offset_from_bottom`.

use crate::{ClientHandle, Error, Result, method::Method};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The daemon refuses longer queries with `query_too_large`; checking here
/// keeps an oversized paste from reaching the wire at all.
pub const MAX_SEARCH_QUERY_BYTES: usize = 4096;

/// A cell in screen-buffer coordinates. Declared row first so the derived
/// order is reading order.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TextPoint {
    pub row: u32,
    pub col: u16,
}

/// A span of cells. `end` is inclusive: it names the last cell of the match,
/// which for a wide glyph is its second column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextRange {
    pub start: TextPoint,
    pub end: TextPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchDirection {
    /// Toward newer output: the first match starting after the origin.
    Forward,
    /// Toward older output: the last match ending before the origin.
    Backward,
}

/// `pane.copy_search` parameters. The origin is `previous` when given (its
/// end searching forward, its start backward), otherwise `cursor`. A search
/// that finds nothing past the origin wraps around.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopySearchParams {
    pub pane_id: String,
    pub query: String,
    pub direction: SearchDirection,
    pub cursor: TextPoint,
    /// The pane content the coordinates were read from. The daemon answers
    /// `stale_content` when its terminal has moved on since.
    pub content_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<TextRange>,
}

/// `pane.copy_search` result. `matches` is a bounded window around the
/// current match; `total` counts every match in the pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopySearchResult {
    pub pane_id: String,
    pub content_revision: u64,
    pub matches: Vec<TextRange>,
    pub total: u64,
    /// Index of the current match within `matches`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u32>,
    /// Index of the current match among all `total` matches, top first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_global: Option<u64>,
}

impl CopySearchResult {
    /// The current match, when the daemon named one inside `matches`.
    pub fn current_match(&self) -> Option<TextRange> {
        self.current
            .and_then(|index| self.matches.get(usize::try_from(index).ok()?))
            .copied()
    }
}

/// An endpoint error code this client acts on. Codes are open-ended on the
/// wire, so anything else is kept verbatim for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointErrorCode {
    /// The pane's content changed after the request's revision was read.
    StaleContent,
    PaneNotFound,
    QueryTooLarge,
    Other(String),
}

impl From<String> for EndpointErrorCode {
    fn from(code: String) -> Self {
        match code.as_str() {
            "stale_content" => Self::StaleContent,
            "pane_not_found" => Self::PaneNotFound,
            "query_too_large" => Self::QueryTooLarge,
            _ => Self::Other(code),
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseResult {
    PaneCopySearch(CopySearchResult),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

impl CopySearchParams {
    /// Rejects what the daemon would refuse before it is queued.
    pub fn validate(&self) -> Result<()> {
        if self.query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(Error::Endpoint {
                code: EndpointErrorCode::QueryTooLarge,
                message: "copy search query is too large".into(),
            });
        }
        Ok(())
    }
}

impl ClientHandle {
    /// Queues `pane.copy_search`, returning the request ID its response
    /// carries.
    pub fn copy_search(&self, boot_id: &str, params: &CopySearchParams) -> Result<String> {
        params.validate()?;
        self.request(
            boot_id,
            Method::PaneCopySearch,
            serde_json::to_value(params)?,
        )
    }
}

/// Decodes the response envelope of a `pane.copy_search` request: its result,
/// or the endpoint error it carried.
pub fn decode_copy_search(response: &Value) -> Result<CopySearchResult> {
    let envelope = Envelope::deserialize(response).map_err(Error::ResponseSchema)?;
    if let Some(ErrorBody { code, message }) = envelope.error {
        return Err(Error::Endpoint {
            code: code.into(),
            message,
        });
    }
    let result = envelope.result.ok_or(Error::ResponseMissingResult)?;
    match ResponseResult::deserialize(result).map_err(Error::ResponseSchema)? {
        ResponseResult::PaneCopySearch(result) => Ok(result),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn point(row: u32, col: u16) -> TextPoint {
        TextPoint { row, col }
    }

    #[test]
    fn params_serialize_as_the_daemon_schema() {
        let mut params = CopySearchParams {
            pane_id: "w1:p1".into(),
            query: "needle".into(),
            direction: SearchDirection::Backward,
            cursor: point(40, 0),
            content_revision: 8,
            previous: None,
        };
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            json!({
                "pane_id": "w1:p1",
                "query": "needle",
                "direction": "backward",
                "cursor": {"row": 40, "col": 0},
                "content_revision": 8,
            })
        );
        params.direction = SearchDirection::Forward;
        params.previous = Some(TextRange {
            start: point(3, 4),
            end: point(3, 9),
        });
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["direction"], "forward");
        assert_eq!(
            value["previous"],
            json!({"start": {"row": 3, "col": 4}, "end": {"row": 3, "col": 9}})
        );
    }

    #[test]
    fn points_order_in_reading_order() {
        assert!(point(1, 70) < point(2, 0));
        assert!(point(2, 0) < point(2, 1));
    }

    #[test]
    fn decodes_a_result_and_its_current_match() {
        let response = json!({
            "id": "gpui-3",
            "result": {
                "type": "pane_copy_search",
                "pane_id": "w1:p1",
                "content_revision": 8,
                "matches": [
                    {"start": {"row": 1, "col": 0}, "end": {"row": 1, "col": 5}},
                    {"start": {"row": 9, "col": 2}, "end": {"row": 10, "col": 1}},
                ],
                "total": 7,
                "current": 1,
                "current_global": 4,
            }
        });
        let result = decode_copy_search(&response).unwrap();
        assert_eq!(result.total, 7);
        assert_eq!(result.current_global, Some(4));
        assert_eq!(
            result.current_match(),
            Some(TextRange {
                start: point(9, 2),
                end: point(10, 1)
            })
        );

        // No matches: the optional indices are absent on the wire.
        let empty = decode_copy_search(&json!({"id": "x", "result": {
            "type": "pane_copy_search", "pane_id": "p", "content_revision": 2,
            "matches": [], "total": 0,
        }}))
        .unwrap();
        assert_eq!(empty.current_match(), None);
        assert_eq!(empty.current_global, None);
    }

    #[test]
    fn a_current_index_outside_the_window_names_no_match() {
        let result = CopySearchResult {
            pane_id: "p".into(),
            content_revision: 2,
            matches: vec![],
            total: 3,
            current: Some(5),
            current_global: Some(1),
        };
        assert_eq!(result.current_match(), None);
    }

    #[test]
    fn endpoint_errors_keep_their_code_as_a_variant() {
        let error = decode_copy_search(&json!({"id": "x", "error": {
            "code": "stale_content", "message": "pane content changed",
        }}))
        .unwrap_err();
        assert!(matches!(
            error,
            Error::Endpoint {
                code: EndpointErrorCode::StaleContent,
                ..
            }
        ));
        let error = decode_copy_search(&json!({"id": "x", "error": {
            "code": "future_code", "message": "later",
        }}))
        .unwrap_err();
        assert!(matches!(
            &error,
            Error::Endpoint { code: EndpointErrorCode::Other(code), message }
                if code == "future_code" && message == "later"
        ));
        assert_eq!(error.to_string(), "later");
    }

    #[test]
    fn malformed_or_foreign_results_are_schema_errors() {
        for response in [
            json!({"id": "x", "result": {"type": "pane_selection", "pane_id": "p", "text": ""}}),
            json!({"id": "x", "result": {"type": "pane_copy_search", "pane_id": "p"}}),
            json!({"id": "x", "result": {"type": "pane_copy_search", "pane_id": "p",
                "content_revision": 2, "matches": [{"start": {"row": -1, "col": 0},
                "end": {"row": 0, "col": 0}}], "total": 1}}),
            json!("not an envelope"),
        ] {
            let error = decode_copy_search(&response).unwrap_err();
            assert!(matches!(error, Error::ResponseSchema(_)), "{response}");
            assert!(std::error::Error::source(&error).is_some());
        }
        assert!(matches!(
            decode_copy_search(&json!({"id": "x"})).unwrap_err(),
            Error::ResponseMissingResult
        ));
    }

    #[test]
    fn oversized_queries_are_refused_before_sending() {
        let mut params = CopySearchParams {
            pane_id: "p".into(),
            query: "x".repeat(MAX_SEARCH_QUERY_BYTES),
            direction: SearchDirection::Forward,
            cursor: TextPoint::default(),
            content_revision: 0,
            previous: None,
        };
        params.validate().unwrap();
        params.query.push('x');
        assert!(matches!(
            params.validate().unwrap_err(),
            Error::Endpoint {
                code: EndpointErrorCode::QueryTooLarge,
                ..
            }
        ));
    }
}
