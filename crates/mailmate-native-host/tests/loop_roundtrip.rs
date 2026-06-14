//! Integration test: the full read → dispatch → emit pipeline across the codec and
//! dispatch module boundaries, driven through in-memory pipes (no real stdio).

use std::io::Cursor;

use mailmate_common::protocol::{Frame, ProtocolVersion, ResponseStatus};
use mailmate_native_host::dispatch::run_loop;
use mailmate_native_host::native_stdio::{read_frame, write_frame, FrameWriter};
use serde_json::json;

fn ping(id: &str, nonce: &str) -> Frame {
    Frame::Request {
        protocol_version: ProtocolVersion::default(),
        request_id: id.to_owned(),
        type_: "ping".to_owned(),
        payload: json!({ "nonce": nonce }),
    }
}

#[test]
fn host_loop_answers_a_sequence_of_requests_in_order() {
    // Two pings and one unknown request type, framed back to back on stdin.
    let mut input = Vec::new();
    write_frame(&mut input, &ping("req_a", "alpha")).unwrap();
    write_frame(
        &mut input,
        &Frame::Request {
            protocol_version: ProtocolVersion::default(),
            request_id: "req_b".to_owned(),
            type_: "not_a_real_type".to_owned(),
            payload: json!({}),
        },
    )
    .unwrap();
    write_frame(&mut input, &ping("req_c", "gamma")).unwrap();

    let mut reader = Cursor::new(input);
    let writer = FrameWriter::new(Vec::<u8>::new());
    run_loop(&mut reader, &writer).unwrap();

    let mut out = Cursor::new(writer.into_inner());
    let responses: Vec<Frame> = std::iter::from_fn(|| read_frame(&mut out).unwrap()).collect();
    assert_eq!(responses.len(), 3, "one response per request, in order");

    // req_a: ok pong echoing the nonce.
    match &responses[0] {
        Frame::Response {
            request_id,
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => {
            assert_eq!(request_id, "req_a");
            assert_eq!(payload["pong"], json!(true));
            assert_eq!(payload["echo"], json!("alpha"));
        }
        other => panic!("unexpected first response: {other:?}"),
    }

    // req_b: error, unknown request type, correlation preserved.
    match &responses[1] {
        Frame::Response {
            request_id,
            status: ResponseStatus::Error,
            error: Some(err),
            ..
        } => {
            assert_eq!(request_id, "req_b");
            assert_eq!(err.code, "unknown_request_type");
        }
        other => panic!("unexpected second response: {other:?}"),
    }

    // req_c: ok pong, correlation preserved (proves ordering, not just matching).
    match &responses[2] {
        Frame::Response {
            request_id,
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => {
            assert_eq!(request_id, "req_c");
            assert_eq!(payload["echo"], json!("gamma"));
        }
        other => panic!("unexpected third response: {other:?}"),
    }
}

#[test]
fn host_loop_exits_cleanly_on_eof_with_no_input() {
    let mut reader = Cursor::new(Vec::<u8>::new());
    let writer = FrameWriter::new(Vec::<u8>::new());
    run_loop(&mut reader, &writer).unwrap();
    assert!(writer.into_inner().is_empty(), "no input -> no output");
}
