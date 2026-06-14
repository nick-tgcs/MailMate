//! End-to-end test of the actual compiled `mailmate-native-host` binary: the manifest
//! subcommand, an unknown subcommand, and serving a real ping over real OS pipes. Cargo
//! exposes the built binary path via `CARGO_BIN_EXE_<name>`.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mailmate-native-host");

#[test]
fn manifest_subcommand_prints_valid_host_manifest() {
    let out = Command::new(BIN)
        .arg("manifest")
        .output()
        .expect("run host binary");
    assert!(out.status.success(), "manifest subcommand should succeed");

    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("manifest stdout is JSON");
    assert_eq!(value["name"], "com.mailmate.host");
    assert_eq!(value["type"], "stdio");
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let out = Command::new(BIN)
        .arg("definitely-not-a-subcommand")
        .output()
        .expect("run host binary");
    assert!(!out.status.success(), "unknown subcommand should fail");
}

#[test]
fn serves_a_ping_over_real_stdio() {
    let json = r#"{"protocol_version":"1.0","kind":"request","request_id":"req_bin","type":"ping","payload":{"nonce":"z"}}"#;
    let mut input = (u32::try_from(json.len()).unwrap()).to_ne_bytes().to_vec();
    input.extend_from_slice(json.as_bytes());

    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn host binary");

    // Write the framed ping, then drop stdin -> EOF -> the host loop returns cleanly.
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(&input)
        .expect("write ping");

    let out = child.wait_with_output().expect("await host binary");
    assert!(out.status.success(), "host should exit cleanly on EOF");

    // Decode the single framed response the way the extension would.
    assert!(out.stdout.len() >= 4, "expected a framed response");
    let len = u32::from_ne_bytes(out.stdout[0..4].try_into().unwrap()) as usize;
    let body = &out.stdout[4..4 + len];
    let value: serde_json::Value = serde_json::from_slice(body).expect("response body is JSON");

    assert_eq!(value["kind"], "response");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["request_id"], "req_bin");
    assert_eq!(value["payload"]["pong"], serde_json::Value::Bool(true));
    assert_eq!(value["payload"]["echo"], "z");
}
