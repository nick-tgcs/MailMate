//! End-to-end harness test.
//!
//! Per the Testing Strategy, when true Thunderbird automation is unavailable the e2e
//! layer uses a harness that sends *the exact native messages the extension sends* and
//! verifies *the exact responses the extension consumes*. This test hand-builds the
//! wire bytes the extension's `native.js` produces (a 32-bit native-byte-order length
//! prefix + UTF-8 JSON), feeds them to the host loop, and decodes the reply the same
//! way the extension's `port.onMessage` handler would — independent of our own codec on
//! the input side, so it validates the wire contract, not just our round-trip.

use std::io::{Cursor, Read};

use mailmate_native_host::dispatch::run_loop;
use mailmate_native_host::native_stdio::FrameWriter;
use serde_json::Value;

/// Frame a JSON string exactly as Thunderbird frames an extension message.
fn frame_like_thunderbird(json: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let len = u32::try_from(json.len()).expect("test frame fits in u32");
    bytes.extend_from_slice(&len.to_ne_bytes());
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

/// Decode one host→extension frame the way the extension's reader would.
fn read_one_extension_message(bytes: &[u8]) -> Value {
    let mut cursor = Cursor::new(bytes);
    let mut len_buf = [0u8; 4];
    cursor.read_exact(&mut len_buf).expect("length prefix");
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    cursor.read_exact(&mut body).expect("frame body");
    serde_json::from_slice(&body).expect("valid JSON frame")
}

#[test]
fn extension_ping_gets_a_pong_over_the_wire() {
    // Byte-for-byte what background.js + native.js emit for a startup ping.
    let request = r#"{"protocol_version":"1.0","kind":"request","request_id":"req_ping_e2e","type":"ping","payload":{"nonce":"e2e-nonce"}}"#;
    let input = frame_like_thunderbird(request);

    let mut reader = Cursor::new(input);
    let writer = FrameWriter::new(Vec::<u8>::new());
    run_loop(&mut reader, &writer).expect("host loop runs");

    let out = writer.into_inner();
    let message = read_one_extension_message(&out);

    assert_eq!(message["kind"], "response");
    assert_eq!(message["status"], "ok");
    assert_eq!(message["request_id"], "req_ping_e2e");
    assert_eq!(message["payload"]["pong"], Value::Bool(true));
    assert_eq!(message["payload"]["echo"], "e2e-nonce");
    assert_eq!(message["protocol_version"], "1.0");
}

#[test]
fn extension_unknown_version_gets_a_structured_error() {
    // An extension built against a future protocol must get a clean, correlated error.
    let request = r#"{"protocol_version":"2.0","kind":"request","request_id":"req_future","type":"ping","payload":{}}"#;
    let input = frame_like_thunderbird(request);

    let mut reader = Cursor::new(input);
    let writer = FrameWriter::new(Vec::<u8>::new());
    run_loop(&mut reader, &writer).expect("host loop runs");

    let message = read_one_extension_message(&writer.into_inner());
    assert_eq!(message["kind"], "response");
    assert_eq!(message["status"], "error");
    assert_eq!(message["request_id"], "req_future");
    assert_eq!(message["error"]["code"], "unsupported_protocol_version");
}
