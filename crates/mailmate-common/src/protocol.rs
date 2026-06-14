//! The native-messaging frame envelope.
//!
//! A [`Frame`] is one of three versioned envelopes — request, response, or
//! notification — discriminated by `kind`. The `Transport` port moves `Frame`s; the
//! framing/codec lives in the transport adapter, not here.

use serde::{Deserialize, Serialize};

/// The wire protocol version (`"1.0"`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub String);

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self("1.0".to_owned())
    }
}

/// Status of a response frame.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// The request succeeded.
    Ok,
    /// The request failed; see the frame's `error`.
    Error,
}

/// A structured error carried by a failed response frame.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProtocolError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// One native-messaging frame.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    /// Extension → host: a request to act, correlated by `request_id`.
    Request {
        #[serde(default)]
        protocol_version: ProtocolVersion,
        request_id: String,
        #[serde(rename = "type")]
        type_: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    /// Host → extension: the answer to a request.
    Response {
        #[serde(default)]
        protocol_version: ProtocolVersion,
        request_id: String,
        status: ResponseStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ProtocolError>,
    },
    /// Host → extension: an unsolicited notification (e.g. `classification_ready`).
    Notification {
        #[serde(default)]
        protocol_version: ProtocolVersion,
        notification_id: String,
        #[serde(rename = "type")]
        type_: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_protocol_version_is_1_0() {
        assert_eq!(
            ProtocolVersion::default(),
            ProtocolVersion("1.0".to_owned())
        );
    }

    #[test]
    fn request_frame_round_trips_with_kind_tag() {
        let frame = Frame::Request {
            protocol_version: ProtocolVersion::default(),
            request_id: "r1".to_owned(),
            type_: "classify_message".to_owned(),
            payload: serde_json::json!({ "client_message_id": "42" }),
        };
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["kind"], "request");
        assert_eq!(value["type"], "classify_message");
        let back: Frame = serde_json::from_value(value).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn error_response_carries_the_protocol_error() {
        let frame = Frame::Response {
            protocol_version: ProtocolVersion::default(),
            request_id: "r1".to_owned(),
            status: ResponseStatus::Error,
            payload: None,
            error: Some(ProtocolError {
                code: "unsupported".to_owned(),
                message: "no provider configured".to_owned(),
                details: None,
            }),
        };
        let back: Frame = serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(back, frame);
    }
}
