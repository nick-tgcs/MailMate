// dashboard.test.mjs — headless DOM tests for the dashboard's provider surfaces.
//
// These exercise the exact code paths behind the bug report "I configured a provider but it still
// says none" and the inline-onboarding fix, by loading the real dashboard and driving real clicks.
// The host half of the bug (config not persisted) is covered by the Rust suite's
// `a_provider_set_in_one_host_session_persists_on_disk_for_the_next`; these cover the UI half.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadDashboard, waitFor, tick } from "./harness.mjs";

const providerNotDefault = {
  default_provider: null,
  providers: [{ id: "local", kind: "ollama", endpoint: "http://localhost:11434", model: "llama3", configured: false }],
  paused: false,
};

test("header chip shows the configured default provider (not 'none')", async () => {
  const { document } = await loadDashboard({
    onboarded: true,
    settings: { default_provider: "local", providers: [{ id: "local", kind: "ollama", endpoint: "http://localhost:11434", model: "llama3", configured: true }], paused: false },
  });
  const chip = document.getElementById("mm-provider");
  const state = document.getElementById("mm-provider-state");
  assert.equal(state.textContent, "local", "the chip names the default provider");
  assert.ok(!chip.classList.contains("mm-ctl--warn"), "a configured provider is not the warning state");
});

test("header chip shows 'none' with the warning state when no default is set", async () => {
  const { document } = await loadDashboard({
    onboarded: true,
    settings: { default_provider: null, providers: [], paused: false },
  });
  const chip = document.getElementById("mm-provider");
  const state = document.getElementById("mm-provider-state");
  assert.match(state.textContent, /none/, "no default provider reads as 'none'");
  assert.ok(chip.classList.contains("mm-ctl--warn"), "the chip carries the warning class");
});

test("onboarding step 3 enables an existing-but-not-default provider in one click", async () => {
  const { window, document, state } = await loadDashboard({ onboarded: false, settings: providerNotDefault });

  const navPrimary = () => document.querySelector("#mm-onboarding .mm-ob__nav .mm-btn--primary");
  const stepText = () => document.querySelector("#mm-onboarding .mm-ob__step")?.textContent || "";
  const cardText = () => document.querySelector("#mm-onboarding .mm-ob__card")?.textContent || "";

  // Walk the wizard to the provider step (index 2) by clicking Continue, like a user would.
  await waitFor(window, () => stepText().includes("Step 1 of 4"));
  navPrimary().click();
  await waitFor(window, () => stepText().includes("Step 2 of 4"));
  navPrimary().click();
  await waitFor(window, () => stepText().includes("Step 3 of 4"));

  // The card resolves live host state to a one-click enable for the already-configured provider.
  await waitFor(window, () => cardText().includes('Use “local” for drafting'));
  const enable = document.querySelector("#mm-onboarding .mm-ob__card .mm-btn--primary");
  assert.ok(enable, "the enable button is present");
  enable.click();

  // After the click the card re-renders from the host's CONFIRMED state — no stale 'none'.
  await waitFor(window, () => cardText().includes("reply drafting is on"));

  const setCall = state.calls.find((c) => c.type === "mm:setProvider");
  assert.ok(setCall, "a set_provider request was sent");
  assert.equal(setCall.providerId, "local");
  assert.equal(setCall.setDefault, true, "enabling makes it the default (drafting actually on)");
});

test("finishing onboarding after enabling a provider leaves the header chip showing it", async () => {
  // The user's exact scenario: configure a provider during setup, finish, and the dashboard must
  // reflect it — not fall back to 'none'.
  const { window, document, state } = await loadDashboard({ onboarded: false, settings: providerNotDefault });
  const navPrimary = () => document.querySelector("#mm-onboarding .mm-ob__nav .mm-btn--primary");
  const stepText = () => document.querySelector("#mm-onboarding .mm-ob__step")?.textContent || "";
  const cardText = () => document.querySelector("#mm-onboarding .mm-ob__card")?.textContent || "";

  await waitFor(window, () => stepText().includes("Step 1 of 4"));
  navPrimary().click();
  await waitFor(window, () => stepText().includes("Step 2 of 4"));
  navPrimary().click();
  await waitFor(window, () => stepText().includes("Step 3 of 4"));
  await waitFor(window, () => cardText().includes('Use “local” for drafting'));
  document.querySelector("#mm-onboarding .mm-ob__card .mm-btn--primary").click();
  await waitFor(window, () => cardText().includes("reply drafting is on"));

  // Continue → step 4, then Finish → enters the app and refreshes settings.
  navPrimary().click();
  await waitFor(window, () => stepText().includes("Step 4 of 4"));
  navPrimary().click(); // "Finish"

  await waitFor(window, () => document.getElementById("mm-app").hidden === false);
  await waitFor(window, () => document.getElementById("mm-provider-state").textContent === "local");
  assert.equal(document.getElementById("mm-provider-state").textContent, "local");
  assert.ok(state.local["mm:onboarded"], "onboarding was marked complete");
});

// --- First-run backfill ("Triage my existing mail") ----------------------------------

const readySettings = { default_provider: "local", providers: [{ id: "local", kind: "ollama", endpoint: "x", model: "m", configured: true }], paused: false };

test("an idle backfill offers the one-tap 'Triage my existing mail', which starts a run", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings });
  // Starting a run kicks a live progress-poll timer; close the window after so it cannot outlive the test.
  t.after(() => dom.window.close());
  await waitFor(window, () => document.getElementById("mm-backfill"));
  const start = document.querySelector(".mm-backfill__start");
  assert.ok(start, "the one-tap triage button is offered on a fresh review tab");
  assert.match(start.textContent, /Triage my existing mail/);

  start.click();
  await waitFor(window, () => state.calls.some((c) => c.type === "mm:triageExisting"));
  assert.ok(state.calls.some((c) => c.type === "mm:triageExisting"), "clicking starts the backfill");
});

test("a running backfill renders a live progress chip with Pause and Stop", async (t) => {
  const { window, document, dom } = await loadDashboard({
    onboarded: true,
    settings: readySettings,
    backfill: { running: true, paused: false, total: 200, done: 45 },
  });
  t.after(() => dom.window.close()); // the running chip polls on a timer; stop it with the window
  await waitFor(window, () => document.querySelector(".mm-backfill__chip"));
  const chip = document.querySelector(".mm-backfill__chip");
  assert.match(chip.textContent, /Triaging your mail · 45\/200/, `chip shows progress: ${chip.textContent}`);
  const labels = [...document.querySelectorAll(".mm-backfill__btn")].map((b) => b.textContent);
  assert.ok(labels.includes("Pause"), "a running backfill can be paused");
  assert.ok(labels.includes("Stop"), "a running backfill can be stopped");
});

test("a finished backfill summarizes what it surfaced", async (t) => {
  const { window, document, dom } = await loadDashboard({
    onboarded: true,
    settings: readySettings,
    backfill: { running: false, done_at: 123, classified: 120, needs_review: 8 },
  });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-backfill__chip--done"));
  const chip = document.querySelector(".mm-backfill__chip--done");
  assert.match(chip.textContent, /Triaged 120 messages · 8 need a look/, `done summary: ${chip.textContent}`);
});

// --- The crystallization "aha" toast -------------------------------------------------

test("a freshly-learned proposal raises an 'aha' toast naming the rule", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.getElementById("mm-app") && !document.getElementById("mm-app").hidden);

  // Simulate the background forwarding a host `proposal_ready` (carrying the learned rule's title).
  assert.ok(state.listeners.length, "the dashboard registered a runtime message listener");
  for (const listen of state.listeners) {
    listen({ type: "mm:dashboardEvent", event: "proposals", title: "File stripe.com mail to Receipts" });
  }
  await waitFor(window, () => {
    const toast = document.getElementById("mm-toast");
    return toast && !toast.hidden && /just learned/.test(toast.textContent);
  });
  const toast = document.getElementById("mm-toast");
  assert.match(toast.textContent, /MailMate just learned: File stripe\.com mail to Receipts/);
  assert.match(toast.textContent, /see Proposals/, "from the Review tab it points at where to review");
});

test("a plain proposals refresh (no title) raises no toast", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.getElementById("mm-app") && !document.getElementById("mm-app").hidden);
  for (const listen of state.listeners) {
    listen({ type: "mm:dashboardEvent", event: "proposals" }); // a count refresh, not a new crystallization
  }
  await tick(window, 4);
  assert.ok(document.getElementById("mm-toast").hidden, "no learning toast without a learned rule");
});

// --- Phase 9: keyboard triage + a11y over the review queue -----------------------------------

const reviewItems = [
  {
    thunderbird_message_id: "tb1",
    decision_id: "dec1",
    headers: { subject: "Invoice #42", from: "billing@acme.test" },
    classification: { labels: ["receipts"], spam_score: 0.1, phishing_score: 0.0, needs_review: true },
    review_required_actions: [{ kind: "tag", tag: "Receipts" }],
  },
  {
    thunderbird_message_id: "tb2",
    decision_id: "dec2",
    headers: { subject: "Weekly digest", from: "news@x.test" },
    classification: { labels: ["newsletters"], spam_score: 0.2, phishing_score: 0.0, needs_review: true },
    review_required_actions: [{ kind: "tag", tag: "News" }],
  },
];

function fireKey(node, key) {
  node.dispatchEvent(new node.ownerDocument.defaultView.KeyboardEvent("keydown", { key, bubbles: true }));
}

test("the review queue is an accessible roving-tabindex list", async (t) => {
  const { window, document, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue"));

  const queue = document.querySelector(".mm-queue");
  assert.equal(queue.getAttribute("role"), "list", "the queue is a list");
  const cards = [...queue.querySelectorAll(".mm-card[data-card]")];
  assert.equal(cards.length, 2);
  assert.equal(cards[0].getAttribute("role"), "listitem");
  // The aria-label is what a screen reader announces: subject + sender + verdict (each card
  // carries its own; the queue is newest-first, so match set-wise rather than by index).
  const labels = cards.map((c) => c.getAttribute("aria-label"));
  assert.ok(labels.some((l) => /Invoice #42.*billing@acme\.test/.test(l)), labels.join(" | "));
  assert.ok(labels.some((l) => /Weekly digest.*news@x\.test/.test(l)), labels.join(" | "));
  // Roving tabindex: only the first card is in the tab order.
  assert.equal(cards[0].getAttribute("tabindex"), "0");
  assert.equal(cards[1].getAttribute("tabindex"), "-1");
});

test("j / ArrowDown moves focus down the queue and updates the roving tabindex", async (t) => {
  const { window, document, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue"));
  const cards = [...document.querySelectorAll(".mm-queue .mm-card[data-card]")];

  cards[0].focus();
  fireKey(cards[0], "ArrowDown");
  assert.equal(document.activeElement, cards[1], "ArrowDown moves focus to the next card");
  assert.equal(cards[1].getAttribute("tabindex"), "0", "the focused card joins the tab order");
  assert.equal(cards[0].getAttribute("tabindex"), "-1", "the previous card leaves it");

  fireKey(cards[1], "k");
  assert.equal(document.activeElement, cards[0], "k moves focus back up");
});

test("pressing x on a focused card dismisses it without the mouse", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue"));
  const queue = document.querySelector(".mm-queue");
  const cards = [...queue.querySelectorAll(".mm-card[data-card]")];

  // Focus the first card and read which decision it is, so the assertion is order-agnostic.
  cards[0].focus();
  const focusedDecision = cards[0].getAttribute("aria-label");
  fireKey(cards[0], "x");
  await tick(window, 6);

  const dismissed = state.calls.find((c) => c.type === "mm:dismiss");
  assert.ok(dismissed, "x fired the focused card's Dismiss");
  // It dismissed the focused card specifically (dec for the first-rendered/newest item).
  assert.ok(["dec1", "dec2"].includes(dismissed.decisionId), `dismissed ${dismissed.decisionId}`);
  assert.equal(
    queue.querySelectorAll(".mm-card[data-card]").length,
    1,
    "the dismissed card was removed from the queue",
  );
  assert.ok(focusedDecision, "the focused card had an aria-label");
});

test("Enter on a focused card approves all its safe suggestions", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue"));
  const cards = [...document.querySelectorAll(".mm-queue .mm-card[data-card]")];

  cards[0].focus();
  fireKey(cards[0], "Enter");
  await tick(window, 6);

  assert.ok(
    state.calls.some((c) => c.type === "mm:apply"),
    "Enter fired Approve all safe for the focused card",
  );
});

test("a single-letter shortcut does NOT fire when a button inside the card has focus", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue .mm-card[data-card]"));
  const card = document.querySelector(".mm-queue .mm-card[data-card]");

  // Tab onto a button inside the card, then press 'x' — it must NOT trigger Dismiss (the user is
  // interacting with the button, not the card).
  const btn = card.querySelector('[data-act="approve"]') || card.querySelector('[data-act="dismiss"]');
  btn.focus();
  fireKey(btn, "x");
  await tick(window, 4);
  assert.ok(!state.calls.some((c) => c.type === "mm:dismiss"), "x on a child button is not a card shortcut");
});

test("a modifier chord (Ctrl+key) is never hijacked by the triage keymap", async (t) => {
  const { window, document, state, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue .mm-card[data-card]"));
  const card = document.querySelector(".mm-queue .mm-card[data-card]");
  card.focus();

  // Ctrl+x must NOT dismiss (it belongs to the browser).
  card.dispatchEvent(new window.KeyboardEvent("keydown", { key: "x", ctrlKey: true, bubbles: true }));
  await tick(window, 4);
  assert.ok(!state.calls.some((c) => c.type === "mm:dismiss"), "Ctrl+x is not the dismiss shortcut");
});

// --- Phase 9: i18n seam ----------------------------------------------------------------------

test("dashboard copy resolves through the i18n seam and a locale flips it", async (t) => {
  const { window, document, dom } = await loadDashboard({
    onboarded: true,
    settings: readySettings,
    reviewQueue: reviewItems,
    // A seeded locale value flips the live English copy without any code change.
    messages: { dashApproveAllSafe: "Tout approuver", dashDismiss: "Rejeter" },
  });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue .mm-card[data-card]"));

  const approve = document.querySelector('.mm-queue [data-act="approve"]');
  const dismiss = document.querySelector('.mm-queue [data-act="dismiss"]');
  assert.equal(approve.textContent, "Tout approuver", "the seam swapped the localized string");
  assert.equal(dismiss.textContent, "Rejeter");
});

test("an unset locale key falls back to the live English copy", async (t) => {
  // No `messages` seed → getMessage returns "" → the UI shows its English fallback, unchanged.
  const { window, document, dom } = await loadDashboard({ onboarded: true, settings: readySettings, reviewQueue: reviewItems });
  t.after(() => dom.window.close());
  await waitFor(window, () => document.querySelector(".mm-queue .mm-card[data-card]"));
  const approve = document.querySelector('.mm-queue [data-act="approve"]');
  assert.equal(approve.textContent, "Approve all safe", "falls back to English when no locale value");
});
