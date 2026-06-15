//! End-to-end test of the actual compiled `mailmate-native-host` binary: the manifest /
//! config / bench / simulate / export-rules subcommands, an unknown subcommand, and serving a
//! real ping over real OS pipes. Cargo exposes the built binary path via `CARGO_BIN_EXE_<name>`.
//!
//! The serving + storage-touching subcommands are pointed at a tempdir via `MAILMATE_DATA_DIR`
//! so every run is hermetic (no `$HOME` writes).

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

    let data_dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(BIN)
        .env("MAILMATE_DATA_DIR", data_dir.path())
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

#[test]
fn config_subcommand_prints_the_effective_toml() {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(BIN)
        .arg("config")
        .env("MAILMATE_DATA_DIR", data_dir.path())
        .env_remove("MAILMATE_CONFIG")
        .output()
        .expect("run host binary");
    assert!(out.status.success(), "config subcommand should succeed");
    let text = String::from_utf8(out.stdout).expect("config stdout is UTF-8");
    assert!(text.contains("# data directory:"));
    assert!(
        text.contains("[retention]"),
        "renders the retention section: {text}"
    );
    assert!(text.contains("[followups]"));
}

#[test]
fn bench_subcommand_reports_the_hot_paths() {
    let out = Command::new(BIN)
        .args(["bench", "2"])
        .output()
        .expect("run host binary");
    assert!(out.status.success(), "bench subcommand should succeed");
    let text = String::from_utf8(out.stdout).expect("bench stdout is UTF-8");
    assert!(text.contains("feature_extraction"));
    assert!(text.contains("classify_plan_guard_no_rules"));
    assert!(text.contains("classify_plan_guard_tier1_hit"));
}

#[test]
fn simulate_subcommand_runs_a_scenario_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scenario = dir.path().join("scenario.json");
    // No rules + no provider → the one message degrades to needs_review.
    std::fs::write(
        &scenario,
        r#"{"rules":[],"messages":[{"from":"a@b.test","subject":"hi"}]}"#,
    )
    .unwrap();

    let out = Command::new(BIN)
        .arg("simulate")
        .arg(&scenario)
        .output()
        .expect("run host binary");
    assert!(out.status.success(), "simulate subcommand should succeed");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("simulate stdout is JSON");
    let outcomes = report["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["needs_review"], true);
}

#[test]
fn export_rules_subcommand_writes_a_manifest() {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let manifest = data_dir.path().join("rules.json");
    let out = Command::new(BIN)
        .arg("export-rules")
        .arg(&manifest)
        .env("MAILMATE_DATA_DIR", data_dir.path())
        .env_remove("MAILMATE_CONFIG")
        .output()
        .expect("run host binary");
    assert!(out.status.success(), "export-rules should succeed");
    // A fresh database has no rules; the manifest is still written.
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).expect("manifest is JSON");
    assert_eq!(written["manifest_version"], 1);
    assert_eq!(written["rules"].as_array().unwrap().len(), 0);
}

#[test]
fn an_unknown_subcommand_lists_the_known_ones() {
    let out = Command::new(BIN)
        .arg("frobnicate")
        .output()
        .expect("run host binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("unknown subcommand"));
    assert!(
        stderr.contains("simulate"),
        "names the known subcommands: {stderr}"
    );
}
