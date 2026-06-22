// drafts.test.mjs — exercise the REAL apply / draft / mail-command paths headlessly.
//
// drafts.js applies safe planned actions (tag/move/mark_junk/mark_read/flag), opens review-required
// draft replies (saved, never sent), executes the host's wrapped mail_commands, and runs the
// echo-suppression bookkeeping (consumeHostMove/consumeHostTag) that stops a host-applied move/tag
// from being re-learned as a user correction. We load it into a jsdom realm with mocked
// browser.compose/messages/identities and assert both the MailExtension calls and the execution
// results reported back to the host.

import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { loadScripts } from "./bg-harness.mjs";

const live = [];
afterEach(() => {
  while (live.length) live.pop().dispose();
});

function setup(opts = {}) {
  const h = loadScripts(["drafts.js"], opts);
  live.push(h);
  return h;
}

const call = (state, path) => state.calls.find((c) => c.path === path);

test("internalToThunderbirdId inverts the host's msg_tb_ prefix, numeric or not", () => {
  const { window } = setup();
  const f = window.internalToThunderbirdId;
  assert.equal(f("msg_tb_42"), 42);
  assert.equal(f("99"), 99);
  assert.equal(f("msg_tb_abc"), "abc"); // non-numeric id survives as a string
});

test("resolveFolder builds the {accountId, path} descriptor messages.move accepts", () => {
  const { window } = setup();
  const r = window.resolveFolder("acctA", "/Archive"); // built in the realm; compare field-wise
  assert.equal(r.accountId, "acctA");
  assert.equal(r.path, "/Archive");
});

test("stash/getComposeDraft round-trips a context and ignores a null tab id", () => {
  const { exports } = setup();
  exports.stashComposeDraft(3, { draft_id: "d1" });
  assert.deepEqual(exports.getComposeDraft(3), { draft_id: "d1" });
  assert.equal(exports.getComposeDraft(999), null);
  assert.doesNotThrow(() => exports.stashComposeDraft(null, { draft_id: "x" }));
});

test("applyPlannedAction tag adds the tag (preserving existing) and arms tag echo-suppression", async () => {
  const { exports, state } = setup({
    responses: {
      "messages.get": (id) => ({ id, headerMessageId: "<m1@x>", folder: { accountId: "a" }, tags: ["old"] }),
    },
  });
  const res = await exports.applyPlannedAction({ kind: "tag", message_id: "msg_tb_5", tag: "mm:lead" });
  assert.equal(res.event_type, "action_applied");
  const upd = call(state, "messages.update");
  assert.deepEqual(new Set(upd.args[1].tags), new Set(["old", "mm:lead"]));
  // The host-applied tag is now consumable exactly once (echo-suppression).
  assert.equal(exports.consumeHostTag("<m1@x>", "mm:lead"), true);
  assert.equal(exports.consumeHostTag("<m1@x>", "mm:lead"), false);
});

test("applyPlannedAction move records the host move BEFORE moving (echo-suppression)", async () => {
  const { exports, state } = setup({
    responses: {
      "messages.get": (id) => ({ id, headerMessageId: "<mv@x>", folder: { accountId: "acctA" }, tags: [] }),
    },
  });
  const res = await exports.applyPlannedAction({ kind: "move", message_id: "msg_tb_8", to_folder: "/Done" });
  assert.equal(res.event_type, "action_applied");
  const mv = call(state, "messages.move");
  assert.deepEqual([...mv.args[0]], [8]);
  assert.equal(mv.args[1].accountId, "acctA");
  assert.equal(mv.args[1].path, "/Done");
  assert.equal(exports.consumeHostMove("<mv@x>"), true);
  assert.equal(exports.consumeHostMove("<mv@x>"), false);
});

test("applyPlannedAction handles mark_junk / mark_read / flag via messages.update", async () => {
  for (const [kind, key, val] of [
    ["mark_junk", "junk", true],
    ["mark_read", "read", true],
    ["flag", "flagged", true],
  ]) {
    const { exports, state } = setup();
    const res = await exports.applyPlannedAction({ kind, message_id: "msg_tb_1", [key]: val });
    assert.equal(res.event_type, "action_applied");
    assert.equal(call(state, "messages.update").args[1][key], val);
  }
});

test("applyPlannedAction reports action_failed for an unsupported kind", async () => {
  const { exports } = setup();
  const res = await exports.applyPlannedAction({ kind: "teleport", message_id: "msg_tb_1" });
  assert.equal(res.event_type, "action_failed");
  assert.match(res.result, /unsupported kind: teleport/);
});

test("applyPlannedAction surfaces a thrown MailExtension error as action_failed", async () => {
  const { exports } = setup({
    responses: {
      "messages.update": () => {
        throw new Error("permission denied");
      },
    },
  });
  const res = await exports.applyPlannedAction({ kind: "mark_read", message_id: "msg_tb_3", read: true });
  assert.equal(res.event_type, "action_failed");
  assert.equal(res.thunderbird_message_id, "3");
  assert.match(res.result, /permission denied/);
});

test("executeMailCommand apply delegates to applyPlannedAction", async () => {
  const { exports } = setup();
  const res = await exports.executeMailCommand({ command: "apply", body: { kind: "flag", message_id: "msg_tb_2", flagged: true } });
  assert.equal(res.event_type, "action_applied");
});

test("executeMailCommand create_draft opens a draft and reports the draft id", async () => {
  const { exports, state } = setup();
  const res = await exports.executeMailCommand({
    command: "create_draft",
    body: { draft_id: "d9", spec: { subject: "Re: hi", body: "...", in_reply_to: "msg_tb_4" } },
  });
  assert.equal(res.event_type, "action_applied");
  assert.equal(res.draft_id, "d9");
  // in_reply_to was inverted to a numeric Thunderbird id -> a reply (not a fresh compose).
  assert.ok(call(state, "compose.beginReply"), "opened as a reply to the inverted message id");
});

test("executeMailCommand rejects an unknown command", async () => {
  const { exports } = setup();
  const res = await exports.executeMailCommand({ command: "self_destruct", body: {} });
  assert.equal(res.event_type, "action_failed");
  assert.match(res.result, /unknown command: self_destruct/);
});

test("openDraftFromResponse begins a fresh review draft and stashes its context", async () => {
  const { exports, state } = setup();
  const tab = await exports.openDraftFromResponse(
    { draft_id: "d1", subject: "Hello", body: "Body", safety_notes: ["check date"], rationale: "polite" },
    null,
  );
  assert.ok(call(state, "compose.beginNew"));
  assert.equal(call(state, "compose.saveMessage").args[1].mode, "draft");
  const ctx = exports.getComposeDraft(tab.id);
  assert.equal(ctx.draft_id, "d1");
  assert.equal(ctx.rationale, "polite");
  assert.equal(ctx.drafted_body, "Body"); // kept only to detect later user edits
});

test("openDraftFromResponse replies from the account's identity when in-reply-to is known", async () => {
  const { exports, state } = setup({
    responses: {
      "messages.get": (id) => ({ id, folder: { accountId: "acctA" } }),
      "identities.list": () => [{ id: "id-1", email: "me@acme.test" }],
    },
  });
  const tab = await exports.openDraftFromResponse({ subject: "Re", body: "hi" }, "msg_tb_12");
  const reply = call(state, "compose.beginReply");
  assert.ok(reply, "opened as a reply");
  assert.equal(reply.args[2].identityId, "id-1");
  assert.equal(exports.getComposeDraft(tab.id).from_identity, "me@acme.test");
});

test("openDraftFromResponse degrades to the default identity when none is resolvable", async () => {
  const { exports, state } = setup({
    responses: { "identities.list": () => [] },
  });
  const tab = await exports.openDraftFromResponse({ subject: "Re", body: "hi" }, "msg_tb_12");
  const reply = call(state, "compose.beginReply");
  assert.equal(reply.args[2].identityId, undefined, "no identity forced -> Thunderbird default");
  assert.equal(exports.getComposeDraft(tab.id).from_identity, null);
});

test("the onRemoved listener forgets a compose context when its tab closes", async () => {
  const { exports, state } = setup();
  exports.stashComposeDraft(42, { draft_id: "d" });
  assert.ok(exports.getComposeDraft(42));
  state.listeners["tabs.onRemoved"][0](42); // tab closed
  assert.equal(exports.getComposeDraft(42), null);
});
