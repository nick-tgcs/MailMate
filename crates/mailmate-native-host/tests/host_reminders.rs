//! Phase-9 end-to-end probe: durable notify-only reminders over the *real* `build_router`
//! composition. It arms a reminder with `remind_me`, drives the host's `drain_reminders`, and
//! proves the whole vertical: a reminder that has come due emits exactly one **notify-only**
//! `reminder_due` notification (no draft, no AI), leaves a `reminder_fired` audit row, and never
//! re-nudges on a second drain (idempotent). Snooze pushes a due reminder back out of the drain.

use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{json, Value};

use mailmate_common::protocol::{Frame, ResponseStatus};
use mailmate_common::protocol::ProtocolVersion;
use mailmate_native_host::config::AppConfig;
use mailmate_native_host::router::HostRouter;
use mailmate_native_host::runtime::build_router;
use mailmate_storage::{open_and_migrate, SqliteBackend, StorageConfig};
use mailmate_test_support::fakes::FakeTransport;

fn request(type_: &str, payload: Value) -> Frame {
    Frame::Request {
        protocol_version: ProtocolVersion::default(),
        request_id: format!("req_{type_}"),
        type_: type_.to_owned(),
        payload,
    }
}

fn last_ok(out: &FakeTransport) -> Value {
    match out.sent_frames().last().expect("a response frame") {
        Frame::Response {
            status: ResponseStatus::Ok,
            payload: Some(payload),
            ..
        } => payload.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

fn notifications(out: &FakeTransport, type_: &str) -> Vec<Value> {
    out.sent_frames()
        .into_iter()
        .filter_map(|f| match f {
            Frame::Notification { type_: t, payload, .. } if t == type_ => Some(payload),
            _ => None,
        })
        .collect()
}

fn router(backend: &Arc<SqliteBackend>, out: Arc<FakeTransport>) -> HostRouter {
    router_with_cap(backend, out, 0)
}

fn router_with_cap(backend: &Arc<SqliteBackend>, out: Arc<FakeTransport>, cap: usize) -> HostRouter {
    let dir = std::env::temp_dir().join(format!("mm_reminders_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut config = AppConfig::default();
    if cap > 0 {
        config.followups.reminder_batch_cap = cap;
    }
    build_router(&config, backend, out, dir.join("secrets.json"), None, None).unwrap()
}

#[test]
fn a_due_reminder_nudges_once_and_never_again() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    // Arm a reminder in the PAST so it is due immediately.
    block_on(router.handle(request(
        "remind_me",
        json!({
            "message_id": "msg_1",
            "title": "reply to Dana with the quote",
            "note": "send-later: draft saved",
            "due_at": "2020-01-01T00:00:00Z"
        }),
    )))
    .unwrap();
    let armed = last_ok(&out);
    assert!(armed["reminder_id"].as_str().unwrap().starts_with("rem_"));

    // Drain: exactly one notify-only nudge, carrying the title — and NO draft notification.
    block_on(router.drain_reminders()).unwrap();
    let nudges = notifications(&out, "reminder_due");
    assert_eq!(nudges.len(), 1, "exactly one nudge");
    assert_eq!(nudges[0]["title"], json!("reply to Dana with the quote"));
    assert_eq!(nudges[0]["note"], json!("send-later: draft saved"));
    assert!(
        notifications(&out, "followup_draft_ready").is_empty(),
        "a reminder is notify-only — it never drafts"
    );

    // A second drain finds nothing (idempotent — the reminder is terminal `fired`).
    block_on(router.drain_reminders()).unwrap();
    assert_eq!(notifications(&out, "reminder_due").len(), 1, "no double-nudge on re-drain");
}

#[test]
fn a_future_reminder_does_not_nudge_until_snoozed_into_the_past() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request(
        "remind_me",
        json!({ "title": "quarterly review", "due_at": "2999-01-01T00:00:00Z" }),
    )))
    .unwrap();
    let id = last_ok(&out)["reminder_id"].as_str().unwrap().to_owned();

    // Not due yet → no nudge.
    block_on(router.drain_reminders()).unwrap();
    assert!(notifications(&out, "reminder_due").is_empty(), "a future reminder must not fire");

    // Snooze it into the past, then it nudges.
    block_on(router.handle(request(
        "snooze_reminder",
        json!({ "reminder_id": id, "due_at": "2020-01-01T00:00:00Z" }),
    )))
    .unwrap();
    block_on(router.drain_reminders()).unwrap();
    assert_eq!(notifications(&out, "reminder_due").len(), 1, "snoozed-into-the-past now fires");
}

#[test]
fn list_reminders_shows_pending_and_cancel_removes_it() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());

    block_on(router.handle(request(
        "remind_me",
        json!({ "title": "one", "due_at": "2999-01-01T00:00:00Z" }),
    )))
    .unwrap();
    let id = last_ok(&out)["reminder_id"].as_str().unwrap().to_owned();

    block_on(router.handle(request("list_reminders", json!({})))).unwrap();
    assert_eq!(last_ok(&out)["reminders"].as_array().unwrap().len(), 1);

    block_on(router.handle(request("cancel_reminder", json!({ "reminder_id": id })))).unwrap();
    assert_eq!(last_ok(&out)["cancelled"], json!(true));
    block_on(router.handle(request("list_reminders", json!({})))).unwrap();
    assert!(last_ok(&out)["reminders"].as_array().unwrap().is_empty(), "cancelled is not pending");
}

#[test]
fn the_launch_catch_up_drains_a_backlog_larger_than_one_batch_cap() {
    // A long-offline backlog: 5 reminders all due, with a batch cap of 2. One drain pass nudges
    // at most the cap; the launch catch-up loops until the whole backlog clears.
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router_with_cap(&backend, out.clone(), 2);
    for i in 0..5 {
        block_on(router.handle(request(
            "remind_me",
            json!({ "title": format!("backlog {i}"), "due_at": "2020-01-01T00:00:00Z" }),
        )))
        .unwrap();
    }

    // A single drain pass is bounded to the cap (2 nudges).
    block_on(router.drain_reminders()).unwrap();
    assert_eq!(notifications(&out, "reminder_due").len(), 2, "one pass is capped at 2");

    // The launch catch-up loops until the whole backlog is nudged (the remaining 3).
    block_on(router.drain_reminders_to_empty()).unwrap();
    assert_eq!(notifications(&out, "reminder_due").len(), 5, "catch-up clears the full backlog");

    // And it is idempotent — a further catch-up nudges nothing more.
    block_on(router.drain_reminders_to_empty()).unwrap();
    assert_eq!(notifications(&out, "reminder_due").len(), 5, "no re-nudge after the backlog cleared");
}

#[test]
fn remind_me_without_a_due_at_is_an_invalid_payload() {
    let backend = open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap();
    let out = Arc::new(FakeTransport::new());
    let router = router(&backend, out.clone());
    block_on(router.handle(request("remind_me", json!({ "title": "no time" })))).unwrap();
    match out.sent_frames().last().unwrap() {
        Frame::Response { status: ResponseStatus::Error, error: Some(err), .. } => {
            assert_eq!(err.code, "invalid_payload");
        }
        other => panic!("expected an error response, got {other:?}"),
    }
}
