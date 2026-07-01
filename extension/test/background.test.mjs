// background.test.mjs — exercise the REAL event-page core headlessly.
//
// background.js owns the single native port, the popup<->background mm:* router, the new-mail /
// onMoved / onUpdated / compose listeners, the dashboard-space review buffer and the toolbar/space
// aggregate badge. We load the whole background context (every manifest background script, in
// order) into one jsdom realm with a controllable fake native port that auto-answers `hello` (so
// the host reaches `ready`) and replies to each request from a per-test map. Then we fire popup
// messages and host events and assert the wire frames + returned shapes — the integration layer
// neither the Rust suite nor the per-file unit tests reach.

import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { loadScripts, tick } from "./bg-harness.mjs";

// The manifest's background.scripts, in load order (native first, background.js last).
const SCRIPTS = [
  "native.js",
  "message_reader.js",
  "drafts.js",
  "followups.js",
  "context_menu.js",
  "notifications.js",
  "background.js",
];

const live = [];
afterEach(() => {
  while (live.length) live.pop().dispose();
});

// Boot the full background context with an auto-answering host. `opts.responses` maps a host
// request type -> the ok payload (or a fn of the request payload, or { __error } to reply error).
async function boot(opts = {}) {
  const responses = opts.responses || {};
  const h = loadScripts(SCRIPTS, {
    ...opts,
    onPost: (frame, port) => {
      if (frame.kind !== "request") return;
      if (frame.type === "hello") {
        port.emit({
          kind: "response",
          request_id: frame.request_id,
          status: "ok",
          payload: {
            host_version: "0.1.0",
            protocol_version: "1.0",
            capabilities: ["draft"],
            retention_level: opts.retention || "metadata",
            drafting_available: opts.drafting !== false,
          },
        });
        return;
      }
      const has = Object.prototype.hasOwnProperty.call(responses, frame.type);
      const r = has ? (typeof responses[frame.type] === "function" ? responses[frame.type](frame.payload) : responses[frame.type]) : {};
      if (r && r.__error) {
        port.emit({ kind: "response", request_id: frame.request_id, status: "error", error: r.__error });
        return;
      }
      port.emit({ kind: "response", request_id: frame.request_id, status: "ok", payload: r || {} });
    },
  });
  live.push(h);
  await tick(h.window, 4); // settle the hello handshake -> ready
  h.fire = (msg) => h.state.listeners["runtime.onMessage"][0](msg);
  h.listener = (path) => (h.state.listeners[path] || [])[0];
  h.emitNote = (type, payload) => h.port.emit({ kind: "notification", type, payload: payload || {} });
  h.sentTypes = () => h.state.sent.map((f) => f.type);
  h.lastSent = (type) => [...h.state.sent].reverse().find((f) => f.type === type);
  return h;
}

// ---- wiring on load ----------------------------------------------------------------------

test("loading the background reaches a ready host, sends hello, and registers the space", async () => {
  const h = await boot();
  assert.equal(h.exports ? undefined : undefined, undefined);
  assert.ok(h.sentTypes().includes("hello"), "handshake sent");
  // ensureSpace ran on load: queried first (reuse), then created the space.
  assert.ok(h.state.calls.some((c) => c.path === "spaces.create"), "dashboard space created");
  // Becoming ready paints the toolbar badge.
  assert.ok(h.state.badges.length > 0, "toolbar badge set");
});

test("mm:getStatus reports the live host status; mm:reconnect re-runs the handshake", async () => {
  const h = await boot();
  const status = await h.fire({ type: "mm:getStatus" });
  assert.equal(status.status.phase, "ready");
  const before = h.state.sent.length;
  await h.fire({ type: "mm:reconnect" });
  assert.ok(h.state.sent.length > before, "reconnect posts a fresh hello");
});

test("an unknown message type is not claimed (returns false)", async () => {
  const h = await boot();
  assert.equal(h.fire({ type: "mm:nope" }), false);
});

// ---- the hostCall-backed router verbs ----------------------------------------------------

test("the host-backed dashboard/options verbs lower to the right host request", async () => {
  // Map each popup message -> the host request type it must produce.
  const cases = [
    [{ type: "mm:listProposals" }, "list_pending_reviews"],
    [{ type: "mm:listRules" }, "list_rules"],
    [{ type: "mm:setRuleStatus", ruleId: "r1", kind: "learned", status: "active" }, "set_rule_status"],
    [{ type: "mm:reviewProposal", proposalId: "p1", decision: "approve" }, "review_rule_proposal"],
    [{ type: "mm:settings" }, "get_settings"],
    [{ type: "mm:setPause", paused: true }, "set_pause"],
    [{ type: "mm:setSettings", retentionLevel: "bodies" }, "set_settings"],
    [{ type: "mm:setCategoryPolicy", category: "promo", policy: "Off" }, "set_category_policy"],
    [{ type: "mm:setAccountScope", accountId: "a", enabled: false }, "set_account_scope"],
    [{ type: "mm:setTagMapping", tag: "t", category: "c" }, "set_tag_mapping"],
    [{ type: "mm:setProvider", providerId: "p", kind: "ollama" }, "set_provider"],
    [{ type: "mm:setSecret", providerId: "p", secret: "s" }, "set_secret"],
    [{ type: "mm:listModels", kind: "ollama", endpoint: "http://x" }, "list_models"],
    [{ type: "mm:testProvider", kind: "ollama", endpoint: "http://x" }, "test_provider"],
    [{ type: "mm:listFollowups" }, "list_followups"],
    [{ type: "mm:followupReschedule", workflowInstanceId: "w", nextDueAt: "t" }, "reschedule_followup"],
    [{ type: "mm:followupReschedule", verb: "snooze", workflowInstanceId: "w", nextDueAt: "t" }, "snooze"],
    [{ type: "mm:followupStage", pipelineItemId: "p", stage: "won" }, "update_pipeline_stage"],
    [{ type: "mm:followupReview", workflowInstanceId: "w", resolution: "skip" }, "review_followup"],
    [{ type: "mm:followupCancel", pipelineItemId: "p" }, "cancel_sequence"],
    [{ type: "mm:listActivity" }, "list_recent_activity"],
  ];
  const h = await boot();
  for (const [msg, expected] of cases) {
    const before = h.state.sent.length;
    const res = await h.fire(msg);
    assert.equal(res.ok, true, `${msg.type} -> ok`);
    const produced = h.state.sent.slice(before).map((f) => f.type);
    assert.ok(produced.includes(expected), `${msg.type} produced ${expected} (got ${produced.join(",")})`);
  }
});

test("mm:settings maps the host payload into { ok, settings }", async () => {
  const h = await boot({ responses: { get_settings: { paused: true, providers: [] } } });
  const res = await h.fire({ type: "mm:settings" });
  assert.equal(res.ok, true);
  assert.equal(res.settings.paused, true);
});

test("a host that isn't ready degrades every hostCall to { ok:false }, never throws", async () => {
  const h = await boot();
  h.port.emitDisconnect({ message: "gone" }); // -> disconnected
  await tick(h.window, 2);
  const res = await h.fire({ type: "mm:listRules" });
  assert.equal(res.ok, false);
  assert.match(res.error, /not connected/);
});

test("a host error response surfaces as { ok:false, error } from hostCall", async () => {
  const h = await boot({ responses: { list_rules: { __error: { code: "x", message: "boom" } } } });
  const res = await h.fire({ type: "mm:listRules" });
  assert.equal(res.ok, false);
  assert.match(res.error, /boom/);
});

// ---- per-message panel handlers ----------------------------------------------------------

test("mm:classify reads + classifies the displayed message", async () => {
  const h = await boot({ responses: { classify_message: { classification: { labels: ["benign"] } } } });
  const res = await h.fire({ type: "mm:classify", messageId: 5 });
  assert.equal(res.ok, true);
  assert.ok(h.sentTypes().includes("classify_message"));
});

test("mm:apply applies a safe action and records the acceptance", async () => {
  const h = await boot();
  const res = await h.fire({
    type: "mm:apply",
    action: { kind: "mark_read", message_id: "msg_tb_3", read: true },
    decisionId: "d1",
    messageId: 3,
  });
  assert.equal(res.ok, true);
  const rec = h.lastSent("record_user_action");
  assert.equal(rec.payload.event_type, "action_applied");
  assert.equal(rec.payload.source, "suggestion_accepted");
});

test("mm:dismiss records the negative signal", async () => {
  const h = await boot();
  const res = await h.fire({ type: "mm:dismiss", decisionId: "d1", actionKind: "tag", messageId: 3 });
  assert.equal(res.ok, true);
  assert.equal(h.lastSent("record_user_action").payload.event_type, "suggestion_dismissed");
});

test("mm:undo reverses a tag and records the undo; a move with no prior folder is refused", async () => {
  const h = await boot();
  const ok = await h.fire({ type: "mm:undo", action: { kind: "tag", message_id: "msg_tb_2", tag: "x" }, decisionId: "d", messageId: 2 });
  assert.equal(ok.ok, true);
  assert.equal(h.lastSent("record_user_action").payload.event_type, "action_undone");

  const noMove = await h.fire({ type: "mm:undo", action: { kind: "move", message_id: "msg_tb_2" }, decisionId: "d", messageId: 2 });
  assert.equal(noMove.ok, false);
  assert.match(noMove.error, /no prior folder/);

  const notReversible = await h.fire({ type: "mm:undo", action: { kind: "create_draft" }, decisionId: "d", messageId: 2 });
  assert.equal(notReversible.ok, false);
});

test("mm:correctLabel / mm:signalWrong record their feedback verbs", async () => {
  const h = await boot();
  await h.fire({ type: "mm:correctLabel", decisionId: "d", messageId: 1, label: "promo", priorLabel: "primary" });
  assert.equal(h.lastSent("record_user_action").payload.event_type, "classification_corrected");
  await h.fire({ type: "mm:signalWrong", decisionId: "d", messageId: 1, signalId: "sig-1" });
  assert.equal(h.lastSent("record_user_action").payload.event_type, "signal_marked_wrong");
});

test("mm:notJunk / mm:junk flip the flag then record the correction", async () => {
  const h = await boot();
  await h.fire({ type: "mm:notJunk", messageId: 4 });
  assert.equal(h.state.calls.filter((c) => c.path === "messages.update").length, 1);
  assert.equal(h.lastSent("record_user_action").payload.junk, false);
  await h.fire({ type: "mm:junk", messageId: 4 });
  assert.equal(h.lastSent("record_user_action").payload.junk, true);
});

test("mm:markRead / mm:draftReply / mm:move are plain client mutations", async () => {
  const h = await boot();
  assert.equal((await h.fire({ type: "mm:markRead", messageId: 1 })).ok, true);
  assert.equal((await h.fire({ type: "mm:draftReply", messageId: 1 })).ok, true);
  assert.ok(h.state.calls.some((c) => c.path === "compose.beginReply"));
  assert.equal((await h.fire({ type: "mm:move", messageId: 1, folder: { accountId: "a", path: "/X" } })).ok, true);
  assert.ok(h.state.calls.some((c) => c.path === "messages.move"));
});

test("mm:unsubscribe prefers a mailto compose, then one-click POST, then opening the page", async () => {
  const mailto = await boot();
  const r1 = await mailto.fire({ type: "mm:unsubscribe", unsubscribe: { mailto: { to: "x@list.test" } } });
  assert.equal(r1.method, "compose");

  const oneClick = await boot();
  const r2 = await oneClick.fire({ type: "mm:unsubscribe", unsubscribe: { one_click: true, http_url: "https://u/x" } });
  assert.equal(r2.method, "post");
  assert.ok(oneClick.state.calls.some((c) => c.path === "fetch"));

  const open = await boot();
  const r3 = await open.fire({ type: "mm:unsubscribe", unsubscribe: { http_url: "https://u/x" } });
  assert.equal(r3.ok, true);

  const none = await boot();
  assert.equal((await none.fire({ type: "mm:unsubscribe", unsubscribe: null })).ok, false);
});

test("mm:openDashboard opens the dashboard tab (deep-linked when explain is set)", async () => {
  const h = await boot();
  const res = await h.fire({ type: "mm:openDashboard", explain: "msg_1" });
  assert.equal(res.ok, true);
  const created = h.state.calls.find((c) => c.path === "tabs.create");
  assert.match(created.args[0].url, /explain=/);
});

test("mm:folders returns a flattened, account-named folder list", async () => {
  const h = await boot({
    responses: {},
    mutateBrowser: (browser) => {
      browser.folders.query = async () => [{ accountId: "a", path: "/Inbox", name: "Inbox" }];
      browser.accounts.list = async () => [{ id: "a", name: "Work" }];
    },
  });
  const res = await h.fire({ type: "mm:folders" });
  assert.equal(res.folders[0].accountName, "Work");
});

test("mm:aggregate returns the breakdown + pause state in one round-trip", async () => {
  const h = await boot({ responses: { list_pending_reviews: { pending_reviews: [{ id: 1 }] }, get_settings: { paused: true } } });
  const res = await h.fire({ type: "mm:aggregate" });
  assert.equal(res.ok, true);
  assert.equal(res.proposals, 1);
  assert.equal(res.paused, true);
});

// ---- compose review / regenerate ---------------------------------------------------------

test("mm:composeContext returns the stashed draft annotation + provider posture", async () => {
  const h = await boot({ responses: { provider_status: { available: true, provider: { kind: "ollama", model: "llama" } } } });
  h.exports.stashComposeDraft(7, { draft_id: "d1", request: { thread_id: "<t>" }, rationale: "polite" });
  const res = await h.fire({ type: "mm:composeContext", tabId: 7 });
  assert.equal(res.draft.draft_id, "d1");
  assert.equal(res.providerConfigured, true);
  assert.equal(res.provider.kind, "ollama");
});

test("mm:regenerateDraft re-drafts and replaces the compose body in place", async () => {
  const h = await boot({
    responses: { regenerate_draft: { body: "fresh body", draft_id: "d2" }, provider_status: { available: true } },
  });
  h.exports.stashComposeDraft(7, { draft_id: "d1", request: { thread_id: "<t>" } });
  const res = await h.fire({ type: "mm:regenerateDraft", tabId: 7, steer: "shorter" });
  assert.equal(res.providerConfigured, true);
  assert.ok(h.state.calls.some((c) => c.path === "compose.setComposeDetails"));
  assert.ok(h.sentTypes().includes("regenerate_draft"));
});

test("mm:regenerateDraft with no MailMate draft in the window is refused", async () => {
  const h = await boot();
  const res = await h.fire({ type: "mm:regenerateDraft", tabId: 999 });
  assert.equal(res.ok, false);
});

// ---- host -> extension notifications -----------------------------------------------------

test("classification_ready buffers a review card the dashboard can read and resolve", async () => {
  const h = await boot();
  h.emitNote("classification_ready", {
    decision_id: "dec1",
    review_required_actions: [{ kind: "move" }],
    applied_actions: [],
  });
  await tick(h.window, 3);
  const q = await h.fire({ type: "mm:reviewQueue" });
  assert.equal(q.items.length, 1);
  assert.equal(q.items[0].decision_id, "dec1");
  const resolved = await h.fire({ type: "mm:resolveReview", decisionId: "dec1" });
  assert.equal(resolved.ok, true);
  assert.equal((await h.fire({ type: "mm:reviewQueue" })).items.length, 0);
});

test("a decision with only auto-applied actions is NOT counted as work in the aggregate", async () => {
  const h = await boot({ responses: { list_pending_reviews: { pending_reviews: [] } } });
  h.emitNote("classification_ready", { decision_id: "auto1", review_required_actions: [], applied_actions: [{ kind: "move" }] });
  await tick(h.window, 3);
  const res = await h.fire({ type: "mm:aggregate" });
  assert.equal(res.reviews, 0, "auto-applied-only decision is kept for Undo but is not 'work'");
});

test("a mail_command notification executes and reports the result back", async () => {
  const h = await boot();
  h.emitNote("mail_command", { command: "apply", body: { kind: "mark_read", message_id: "msg_tb_1", read: true } });
  await tick(h.window, 3);
  assert.ok(h.sentTypes().includes("record_user_action"));
});

test("followup_draft_ready opens the review draft and pings the dashboard", async () => {
  const h = await boot();
  h.emitNote("followup_draft_ready", { draft: { subject: "Nudge", body: "?" }, explanation: "3-day" });
  await tick(h.window, 3);
  assert.ok(h.state.calls.some((c) => c.path === "compose.beginNew"));
});

test("proposal_ready refreshes the badge without buffering a card", async () => {
  const h = await boot({ responses: { list_pending_reviews: { pending_reviews: [{ id: 1 }] } } });
  const beforeBadges = h.state.badges.length;
  h.emitNote("proposal_ready", { title: "VIP rule" });
  await tick(h.window, 3);
  assert.ok(h.state.badges.length >= beforeBadges);
});

// ---- mail event listeners ----------------------------------------------------------------

test("onNewMailReceived lowers each new message to a new_mail host event", async () => {
  const h = await boot();
  await h.listener("messages.onNewMailReceived")(
    { id: "f" },
    { messages: [{ id: 9, author: "a@x", subject: "hi", folder: { accountId: "a", path: "/Inbox" } }] },
  );
  await tick(h.window, 2);
  assert.ok(h.sentTypes().includes("new_mail"));
});

test("a genuine user move is recorded; a move MailMate just made is echo-suppressed", async () => {
  const h = await boot();
  // Genuine user move (no prior host move for this Message-ID).
  await h.listener("messages.onMoved")({}, { messages: [{ id: 9, headerMessageId: "<u@x>", folder: { accountId: "a", path: "/Done" } }] });
  await tick(h.window, 2);
  assert.equal(h.lastSent("record_user_action").payload.event_type, "message_moved");

  // Now MailMate applies a move on message 8 (remembers <mid-8@x>), then onMoved echoes it.
  await h.fire({ type: "mm:apply", action: { kind: "move", message_id: "msg_tb_8", to_folder: "/A" }, decisionId: "d", messageId: 8 });
  const beforeMoves = h.state.sent.filter((f) => f.type === "record_user_action").length;
  await h.listener("messages.onMoved")({}, { messages: [{ id: 8, headerMessageId: "<mid-8@x>", folder: { accountId: "a", path: "/A" } }] });
  await tick(h.window, 2);
  const afterMoves = h.state.sent.filter((f) => f.type === "record_user_action" && f.payload.event_type === "message_moved").length;
  assert.equal(afterMoves, 1, "the host's own move was not re-reported as a user correction");
  assert.ok(beforeMoves >= 1);
});

test("onUpdated records a user tag add and a tag remove, with direction recovered", async () => {
  const h = await boot();
  const onUpdated = h.listener("messages.onUpdated");
  // First sighting with tag "lead" -> added.
  onUpdated({ id: 9, headerMessageId: "<t@x>", tags: ["lead"], folder: { accountId: "a" } }, { tags: ["lead"] });
  await tick(h.window, 2);
  let rec = h.lastSent("record_user_action");
  assert.equal(rec.payload.event_type, "tag_changed");
  assert.equal(rec.payload.added, true);
  // Now the tag is gone -> removed.
  onUpdated({ id: 9, headerMessageId: "<t@x>", tags: [], folder: { accountId: "a" } }, { tags: [] });
  await tick(h.window, 2);
  rec = h.lastSent("record_user_action");
  assert.equal(rec.payload.added, false);
});

test("onUpdated ignores a non-tag property change", async () => {
  const h = await boot();
  const before = h.state.sent.length;
  h.listener("messages.onUpdated")({ id: 1, tags: [] }, { read: true });
  await tick(h.window, 2);
  assert.equal(h.state.sent.length, before, "a read-state change is not a tag signal");
});

test("the message-display badge classifies the opened message (inform-only)", async () => {
  const h = await boot({ responses: { classify_message: { classification: { labels: ["benign"], needs_review: false } } } });
  await h.listener("messageDisplay.onMessageDisplayed")({ id: 2 }, { id: 9 });
  await tick(h.window, 3);
  assert.ok(h.state.calls.some((c) => c.path === "messageDisplayAction.setBadgeText"));
});

// ---- compose observers (edit-divergence + learn-from-Sent) -------------------------------

test("sending an EDITED MailMate draft records draft_diverged; an unchanged one does not", async () => {
  const h = await boot();
  const onBeforeSend = h.listener("compose.onBeforeSend");
  h.exports.stashComposeDraft(7, { draft_id: "d1", drafted_body: "the original body", request: { thread_id: "<t>" } });

  // Edited: final body does not contain MailMate's text -> divergence recorded.
  onBeforeSend({ id: 7 }, { plainTextBody: "a completely different message", to: ["x@y"], cc: [] });
  await tick(h.window, 3);
  assert.equal(h.lastSent("record_user_action").payload.event_type, "draft_diverged");

  // Unchanged: final body CONTAINS the drafted text (TB quotes it) -> no new signal.
  const before = h.state.sent.length;
  h.exports.stashComposeDraft(8, { draft_id: "d2", drafted_body: "keep me", request: {} });
  onBeforeSend({ id: 8 }, { plainTextBody: "keep me\n> quoted reply", to: [], cc: [] });
  await tick(h.window, 3);
  assert.equal(h.state.sent.length, before, "an unedited draft teaches nothing");
});

test("a confirmed send reports the recipients as learn-from-Sent evidence", async () => {
  const h = await boot();
  h.listener("compose.onBeforeSend")({ id: 7 }, { to: ["vip@acme.test"], cc: ["cc@x"] });
  h.listener("compose.onAfterSend")({ id: 7 }, { mode: "sendNow" });
  await tick(h.window, 3);
  const sent = h.lastSent("record_sent_mail");
  assert.ok(sent, "record_sent_mail posted");
  assert.deepEqual([...sent.payload.recipients], ["vip@acme.test", "cc@x"]);
});

test("a save-as-draft (not a real send) reports nothing", async () => {
  const h = await boot();
  h.listener("compose.onBeforeSend")({ id: 7 }, { to: ["x@y"] });
  const before = h.state.sent.length;
  h.listener("compose.onAfterSend")({ id: 7 }, { mode: "saveAsDraft" });
  await tick(h.window, 2);
  assert.equal(h.state.sent.length, before);
});

// ---- first-run backfill ------------------------------------------------------------------

test("mm:triageExisting sweeps a page through the dry-run triage path and reports progress", async () => {
  const h = await boot({
    responses: { triage_existing_mail: { classified: 1, needs_review: 0, placements_recorded: 0 } },
    mutateBrowser: (browser) => {
      // One page, no continuation id -> the loop runs once and finishes.
      browser.messages.query = async () => ({ messages: [{ id: 1, author: "a@x", subject: "s", folder: { accountId: "a", path: "/I" } }] });
    },
  });
  const start = await h.fire({ type: "mm:triageExisting" });
  assert.equal(start.started, true);
  await tick(h.window, 6);
  assert.ok(h.sentTypes().includes("triage_existing_mail"));
  const status = await h.fire({ type: "mm:backfillStatus" });
  assert.equal(status.classified, 1);
});

test("mm:backfillControl pauses, resumes and cancels the run", async () => {
  const h = await boot();
  assert.equal((await h.fire({ type: "mm:backfillControl", action: "pause" })).paused, true);
  assert.equal((await h.fire({ type: "mm:backfillControl", action: "resume" })).paused, false);
  assert.equal((await h.fire({ type: "mm:backfillControl", action: "cancel" })).cancelled, true);
});

test("starting a backfill while the host is down is refused cleanly", async () => {
  const h = await boot();
  h.port.emitDisconnect({ message: "gone" });
  await tick(h.window, 2);
  const res = await h.fire({ type: "mm:triageExisting" });
  assert.equal(res.ok, false);
});
