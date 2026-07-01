// notifications.test.mjs — headless tests for the desktop-notification policies.
//
// notifications.js gates every host push through four policies before the tray: per-class toggles,
// persistent dedup (storage.local LRU, survives an event-page suspension), quiet hours, and
// batching (a catch-up burst → one digest, not a storm). These exercise all four against the real
// notifications.js with a mocked browser.notifications + storage.local.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadNotifications, tick } from "./harness.mjs";

const followup = (id, subject) => ({ workflow_instance_id: id, step_index: 0, draft: { subject } });

// --- Pure policy helpers (called directly off the loaded global) -----------------------------

test("withinQuietHours handles same-day and overnight windows", async () => {
  const { window, dom } = await loadNotifications();
  const overnight = { enabled: true, start: "22:00", end: "07:00" };
  assert.equal(window.withinQuietHours(23 * 60, overnight), true, "23:00 is inside an overnight window");
  assert.equal(window.withinQuietHours(3 * 60, overnight), true, "03:00 is inside");
  assert.equal(window.withinQuietHours(12 * 60, overnight), false, "noon is outside");
  const daytime = { enabled: true, start: "09:00", end: "17:00" };
  assert.equal(window.withinQuietHours(13 * 60, daytime), true);
  assert.equal(window.withinQuietHours(20 * 60, daytime), false);
  // Disabled → never quiet.
  assert.equal(window.withinQuietHours(23 * 60, { enabled: false, start: "22:00", end: "07:00" }), false);
  dom.window.close();
});

test("digestSpec summarizes counts per class and picks the dominant tab", async () => {
  const { window, dom } = await loadNotifications();
  const digest = window.digestSpec([
    { tab: "followups" },
    { tab: "followups" },
    { tab: "proposals" },
  ]);
  assert.ok(digest.title.includes("3 updates"), digest.title);
  assert.ok(digest.message.includes("1 rule proposal"), digest.message);
  assert.ok(digest.message.includes("2 follow-ups"), digest.message);
  assert.equal(digest.tab, "followups", "the dominant class is the click target");
  dom.window.close();
});

// --- Batching --------------------------------------------------------------------------------

test("a burst coalesces into ONE digest, not a storm", async () => {
  const { window, state, dom } = await loadNotifications();
  // Three distinct follow-up drafts arrive in a burst (the catch-up drain shape).
  await window.showDesktopNotification("followup_draft_ready", followup(1, "Deal A"));
  await window.showDesktopNotification("followup_draft_ready", followup(2, "Deal B"));
  await window.showDesktopNotification("followup_draft_ready", followup(3, "Deal C"));
  // Flush the batch window deterministically (the timer path is covered separately).
  window.flushNotifications();
  await tick(window, 2);

  assert.equal(state.created.length, 1, "the burst produced exactly one notification");
  assert.ok(state.created[0].title.includes("3 updates"), state.created[0].title);
  dom.window.close();
});

test("a single notification pings on its own (no digest wrapper)", async () => {
  const { window, state, dom } = await loadNotifications();
  await window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "File Stripe receipts" });
  window.flushNotifications();
  await tick(window, 2);
  assert.equal(state.created.length, 1);
  assert.ok(state.created[0].message.includes("File Stripe receipts"), state.created[0].message);
  dom.window.close();
});

test("the batch timer auto-flushes the burst", async () => {
  const { window, state, dom } = await loadNotifications();
  window.MM_BATCH_MS = 25; // shrink the window so the real timer fires quickly
  await window.showDesktopNotification("followup_draft_ready", followup(1, "A"));
  await window.showDesktopNotification("followup_draft_ready", followup(2, "B"));
  await new Promise((r) => window.setTimeout(r, 60)); // let the real batch timer fire
  assert.equal(state.created.length, 1, "the timer coalesced the burst");
  dom.window.close();
});

// --- Persistent dedup (survives a suspension) ------------------------------------------------

test("dedup suppresses a re-fire of the same item", async () => {
  const { window, state, dom } = await loadNotifications();
  await window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" });
  window.flushNotifications();
  await window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" }); // same id
  window.flushNotifications();
  await tick(window, 2);
  assert.equal(state.created.length, 1, "the second fire of p1 was deduped");
  dom.window.close();
});

test("a DELIVERED notification's dedup survives an event-page suspension (storage.local LRU)", async () => {
  // Instance A delivers p1; its dedup key is sealed in storage.local at delivery time.
  const a = await loadNotifications();
  await a.window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" });
  a.window.flushNotifications();
  await tick(a.window, 2);
  assert.equal(a.state.created.length, 1);

  // Suspend + revive: a fresh realm (in-memory state lost) but the SAME storage.local.
  const b = await loadNotifications({ local: a.state.local });
  await b.window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" });
  b.window.flushNotifications();
  await tick(b.window, 2);
  assert.equal(b.state.created.length, 0, "a delivered item is not re-pinged after a suspension");
  a.dom.window.close();
  b.dom.window.close();
});

test("a batch dropped by a mid-window suspension is NOT sealed — it pings on revive", async () => {
  // Instance A queues p1 but the event page suspends BEFORE the batch flushes (no delivery, so no
  // durable seal). This is the regression guard for the mark-before-queue data-loss bug.
  const a = await loadNotifications();
  await a.window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" });
  // (no flush — simulate suspension inside the batch window)
  assert.equal(a.state.created.length, 0, "nothing delivered yet");
  assert.ok(!(a.state.local["mm:notifSeen"] || []).includes("prop:p1"), "an undelivered item is NOT sealed");

  // Revive with the same storage.local: the host re-drains the same item — it MUST still ping.
  const b = await loadNotifications({ local: a.state.local });
  await b.window.showDesktopNotification("proposal_ready", { proposal_id: "p1", title: "R" });
  b.window.flushNotifications();
  await tick(b.window, 2);
  assert.equal(b.state.created.length, 1, "the dropped item survived the suspension and pinged");
  a.dom.window.close();
  b.dom.window.close();
});

// --- Per-class toggle + quiet hours (integration) --------------------------------------------

test("a disabled class is silenced", async () => {
  const a = await loadNotifications({ local: { "mm:notifPrefs": { classes: { proposal_ready: false } } } });
  await a.window.showDesktopNotification("proposal_ready", { proposal_id: "p9", title: "R" });
  a.window.flushNotifications();
  await tick(a.window, 2);
  assert.equal(a.state.created.length, 0, "proposal_ready is off → no ping");
  // …but another class still pings.
  await a.window.showDesktopNotification("followup_draft_ready", followup(7, "Deal"));
  a.window.flushNotifications();
  await tick(a.window, 2);
  assert.equal(a.state.created.length, 1, "an enabled class is unaffected");
  a.dom.window.close();
});

test("quiet hours silence the ping (badge covers it) without durably sealing the item", async () => {
  // A quiet window covering the whole day, so 'now' is always inside it.
  const local = { "mm:notifPrefs": { quietHours: { enabled: true, start: "00:00", end: "23:59" } } };
  const a = await loadNotifications({ local });
  await a.window.showDesktopNotification("proposal_ready", { proposal_id: "pq", title: "R" });
  a.window.flushNotifications();
  await tick(a.window, 2);
  assert.equal(a.state.created.length, 0, "no ping inside quiet hours");
  // It is NOT persistently sealed — so a later session (after the window) can still ping it; only
  // the in-memory same-session gate holds, which a suspension/new session forgets.
  assert.ok(!(a.state.local["mm:notifSeen"] || []).includes("prop:pq"), "not durably sealed by a quiet skip");
  a.dom.window.close();
});
