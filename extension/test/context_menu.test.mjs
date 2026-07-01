// context_menu.test.mjs — exercise the REAL message context-menu wiring headlessly.
//
// context_menu.js registers four menu items and one onClicked dispatcher that reads the targeted
// message and routes to the supplied handlers (classify / draftReply / recordCorrection). We load
// it into a jsdom realm, capture the registered onClicked listener via the browser mock, and fire
// synthetic clicks to assert the routing — including the spam/not-spam correction polarity and the
// "no message targeted" early return.

import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { loadScripts } from "./bg-harness.mjs";

const live = [];
afterEach(() => {
  while (live.length) live.pop().dispose();
});

// Spy-backed handlers + a load of context_menu.js. registerContextMenus wires the onClicked
// listener; we return it so a test can fire menu clicks.
function setup() {
  const h = loadScripts(["context_menu.js"]);
  live.push(h);
  const calls = [];
  const handlers = {
    classify: (m) => calls.push(["classify", m]),
    draftReply: (m) => calls.push(["draftReply", m]),
    recordCorrection: (m, isSpam) => calls.push(["recordCorrection", m, isSpam]),
  };
  h.exports.registerContextMenus(handlers);
  const onClicked = h.state.listeners["menus.onClicked"][0];
  return { ...h, calls, onClicked, MENU: h.exports.MAILMATE_MENU };
}

const withSelected = (id, msg) => ({ menuItemId: id, selectedMessages: { messages: [msg] } });

test("registerContextMenus removes stale items then creates the four MailMate items", () => {
  const { state, MENU } = setup();
  assert.ok(state.calls.some((c) => c.path === "menus.removeAll"), "removeAll ran first (idempotent)");
  const created = state.calls.filter((c) => c.path === "menus.create").map((c) => c.args[0].id);
  assert.deepEqual(
    created.sort(),
    [MENU.classify, MENU.draft, MENU.markSpam, MENU.markNotSpam].sort(),
  );
  // Each item is offered on the message list + the message-display action menu.
  const one = state.calls.find((c) => c.path === "menus.create").args[0];
  assert.ok(one.contexts.includes("message_list"));
});

test("clicking Classify routes the targeted message to the classify handler", async () => {
  const { onClicked, calls, MENU } = setup();
  const msg = { id: 7 };
  await onClicked(withSelected(MENU.classify, msg));
  assert.deepEqual(calls, [["classify", msg]]);
});

test("clicking Draft reply routes to the draftReply handler", async () => {
  const { onClicked, calls, MENU } = setup();
  await onClicked(withSelected(MENU.draft, { id: 1 }));
  assert.deepEqual(calls[0].slice(0, 1), ["draftReply"]);
});

test("This is spam / Not spam route to recordCorrection with the right polarity", async () => {
  const { onClicked, calls, MENU } = setup();
  await onClicked(withSelected(MENU.markSpam, { id: 1 }));
  await onClicked(withSelected(MENU.markNotSpam, { id: 1 }));
  assert.equal(calls[0][0], "recordCorrection");
  assert.equal(calls[0][2], true, "markSpam -> isSpam true");
  assert.equal(calls[1][2], false, "markNotSpam -> isSpam false");
});

test("an unknown menu item is a no-op", async () => {
  const { onClicked, calls } = setup();
  await onClicked(withSelected("some-other-menu", { id: 1 }));
  assert.equal(calls.length, 0);
});

test("a click with no targeted message returns early (no handler fired)", async () => {
  const { onClicked, calls, MENU } = setup();
  await onClicked({ menuItemId: MENU.classify, selectedMessages: { messages: [] } });
  assert.equal(calls.length, 0);
});

test("firstSelected falls back to the displayed message, then to null", () => {
  const { window } = setup();
  const fs = window.firstSelected;
  assert.equal(fs({ displayedMessages: [{ id: 9 }] }).id, 9, "displayed message used when no selection");
  assert.equal(fs({}), null, "neither selection nor display -> null");
  assert.equal(fs({ selectedMessages: { messages: [{ id: 3 }] } }).id, 3, "selection wins");
});
