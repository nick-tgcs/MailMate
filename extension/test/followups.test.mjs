// followups.test.mjs — exercise the REAL sales-pipeline / follow-up surface headlessly.
//
// followups.js is thin glue: outbound control verbs lower to host.request payloads, a reply on a
// tracked thread rides host.notifyHost, and a `followup_draft_ready` reuses drafts.js's
// openDraftFromResponse. We load drafts.js + followups.js together (the latter calls the former),
// drive each verb against a recording fake host, and assert the exact wire payloads — the contract
// the Rust host deserializes.

import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { loadScripts } from "./bg-harness.mjs";

const live = [];
afterEach(() => {
  while (live.length) live.pop().dispose();
});

// A recording fake of the NativeHost surface followups.js uses.
function fakeHost() {
  const requests = [];
  const notes = [];
  return {
    requests,
    notes,
    request: (type, payload) => {
      requests.push({ type, payload });
      return Promise.resolve({ ok: true });
    },
    notifyHost: (type, payload) => notes.push({ type, payload }),
  };
}

function setup() {
  const h = loadScripts(["drafts.js", "followups.js"]);
  live.push(h);
  return h;
}

test("enrollPipelineItem lowers a tracked message into the enroll payload", async () => {
  const { exports } = setup();
  const host = fakeHost();
  const header = {
    id: 55,
    headerMessageId: "<deal@x>",
    subject: "Quote #9",
    recipients: ["buyer@acme.test"],
    folder: { accountId: "acctA" },
  };
  await exports.enrollPipelineItem(host, header, "wf_followup");
  assert.equal(host.requests.length, 1);
  const { type, payload } = host.requests[0];
  assert.equal(type, "enroll_pipeline_item");
  assert.equal(payload.account_id, "acctA");
  assert.equal(payload.thread_id, "<deal@x>");
  assert.equal(payload.anchor_thunderbird_message_id, "55");
  assert.equal(payload.counterparty_email, "buyer@acme.test");
  assert.equal(payload.title, "Quote #9");
  assert.equal(payload.item_type, "quote");
  assert.equal(payload.workflow_id, "wf_followup");
});

test("enrollPipelineItem tolerates a header with no folder/recipients", async () => {
  const { exports } = setup();
  const host = fakeHost();
  await exports.enrollPipelineItem(host, { id: 1 }, "wf");
  const { payload } = host.requests[0];
  assert.equal(payload.account_id, "");
  assert.equal(payload.counterparty_email, "");
});

test("reviewFollowup / cancelSequence / snoozeFollowup / markPipelineStage map to their verbs", async () => {
  const { exports } = setup();
  const host = fakeHost();
  await exports.reviewFollowup(host, "wi_1", "skip");
  await exports.cancelSequence(host, "pi_1");
  await exports.snoozeFollowup(host, "wi_1", "2026-07-01T00:00:00Z");
  await exports.markPipelineStage(host, "pi_1", "won");
  assert.deepEqual(
    host.requests.map((r) => r.type),
    ["review_followup", "cancel_sequence", "snooze", "update_pipeline_stage"],
  );
  assert.equal(host.requests[0].payload.resolution, "skip");
  assert.equal(host.requests[2].payload.next_due_at, "2026-07-01T00:00:00Z");
  assert.equal(host.requests[3].payload.stage, "won");
});

test("reportReplyOnThread is a fire-and-forget reply_received notification", () => {
  const { exports } = setup();
  const host = fakeHost();
  exports.reportReplyOnThread(host, "<thread@x>");
  assert.equal(host.requests.length, 0, "no request/response round-trip");
  assert.equal(host.notes.length, 1);
  assert.equal(host.notes[0].type, "record_user_action");
  assert.equal(host.notes[0].payload.event_type, "reply_received");
  assert.equal(host.notes[0].payload.thread_id, "<thread@x>");
});

test("openFollowupDraft opens a review-required draft via drafts.js (never sent)", async () => {
  const { exports, state } = setup();
  await exports.openFollowupDraft({
    explanation: "3-day nudge",
    draft: { subject: "Following up", body: "Any thoughts?", safety_notes: [] },
  });
  // It is thread-anchored (no in-reply-to), so it begins a fresh compose draft and saves it.
  const beganNew = state.calls.find((c) => c.path === "compose.beginNew");
  assert.ok(beganNew, "a fresh compose draft was begun");
  assert.equal(beganNew.args[0].subject, "Following up");
  const saved = state.calls.find((c) => c.path === "compose.saveMessage");
  assert.ok(saved, "the draft was saved for review");
  assert.equal(saved.args[1].mode, "draft");
});

test("surfaceNeedsAttention logs the nudge without throwing", () => {
  const { exports } = setup();
  assert.doesNotThrow(() =>
    exports.surfaceNeedsAttention({ workflow_instance_id: "wi", reason: "stale", skipped_step_indexes: [1] }),
  );
});
