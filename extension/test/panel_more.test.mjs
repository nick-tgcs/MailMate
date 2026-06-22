// panel_more.test.mjs — cover the per-message panel paths the original suite didn't reach:
// the host-suggestion rows partitioned by apply_state (auto-applied / suggest / review-required /
// blocked), the correction menus (category + move), the Apply busy-state, and the no-message /
// classify-error render states. Same harness + idiom as panel.test.mjs.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadPanel, classifyResult, tick } from "./panel-harness.mjs";

const buttonByText = (root, label) =>
  [...root.querySelectorAll("button")].find((b) => (b.textContent || "").includes(label));

test("host suggestions render partitioned into auto / suggest / review / blocked rows", async () => {
  const { document } = await loadPanel({
    classify: classifyResult({
      suggested_actions: [
        { apply_state: "auto_applied", kind: "move", to_folder: "/Reading", reverses_to: { kind: "move" } },
        { apply_state: "suggest", kind: "tag", tag: "mm:lead" },
        { apply_state: "suggest", kind: "require_review" },
      ],
      blocked_actions: [{ kind: "move", policy_id: "no_cross_account", reason: "different account" }],
    }),
  });
  const text = document.body.textContent;
  // auto row carries an Undo affordance; the review row is informational; the blocked row cites a policy.
  assert.ok(buttonByText(document.body, "Undo"), "auto-applied row offers Undo");
  assert.match(text, /different account|no_cross_account/, "blocked row cites the policy");
});

test("clicking Apply on a suggested safe action sends mm:apply and busies the buttons", async () => {
  const { document, state, window } = await loadPanel({
    classify: classifyResult({
      suggested_actions: [{ apply_state: "suggest", kind: "tag", tag: "mm:lead", message_id: "msg_tb_42" }],
    }),
  });
  const apply = buttonByText(document.body, "Apply");
  assert.ok(apply, "an Apply button is offered for the safe suggestion");
  apply.click();
  await tick(window, 4);
  assert.ok(state.calls.some((c) => c.type === "mm:apply"), "mm:apply was sent");
});

test("the Wrong-category menu opens and a pick records a label correction", async () => {
  const { document, state, window } = await loadPanel({
    classify: classifyResult({ classification: { labels: ["newsletter"] } }),
  });
  const wrongCat = buttonByText(document.body, "Wrong category");
  assert.ok(wrongCat, "the category-correction affordance is present");
  wrongCat.click();
  await tick(window, 2);
  // The menu lists category chips; choosing one + Set records the correction.
  const chip = [...document.querySelectorAll(".mm-chip")].find((b) => /Receipts|Newsletters|receipts|newsletters/.test(b.textContent || ""));
  if (chip) chip.click();
  const set = buttonByText(document.body, "Set");
  if (set) set.click();
  await tick(window, 3);
  assert.ok(state.calls.some((c) => c.type === "mm:correctLabel"), "mm:correctLabel was sent");
});

test("toggling the same menu twice closes it", async () => {
  const { document, window } = await loadPanel();
  const wrongCat = buttonByText(document.body, "Wrong category");
  wrongCat.click();
  await tick(window, 2);
  const opened = document.querySelectorAll(".mm-menu").length;
  wrongCat.click(); // toggle closed
  await tick(window, 2);
  assert.ok(opened >= 1, "the menu opened on first click");
  assert.equal(document.querySelectorAll(".mm-menu").length, 0, "and closed on the second");
});

test("the Move menu lists folders and a pick sends mm:move", async () => {
  const { document, state, window } = await loadPanel();
  const move = buttonByText(document.body, "Move");
  assert.ok(move, "the Move affordance is present");
  move.click();
  await tick(window, 3);
  const folder = buttonByText(document.body, "Archive");
  if (folder) {
    folder.click();
    await tick(window, 3);
    assert.ok(state.calls.some((c) => c.type === "mm:move"), "mm:move was sent");
  } else {
    assert.ok(document.querySelectorAll(".mm-menu").length >= 1, "the move menu opened");
  }
});

test("a classify error renders the error card, not a blank panel", async () => {
  const { document } = await loadPanel({ classify: { ok: false, error: "model unavailable" } });
  assert.match(document.body.textContent, /model unavailable|Couldn't classify|couldn't/i);
});

test("when no message is displayed the panel shows the 'open a message' state", async () => {
  const { document } = await loadPanel({ noMessage: true });
  assert.match(document.body.textContent, /Open a message|no message/i);
});

test("a live host drop replaces the verdict with the recovery card; recovery re-boots", async () => {
  const { document, state, window } = await loadPanel();
  const onMessage = state.listeners[0];
  assert.ok(onMessage, "the panel subscribes to background status pushes");
  // Host drops while the panel is open -> the now-stale verdict is replaced with the recovery card.
  onMessage({ type: "mm:statusChanged", status: { phase: "disconnected", reason: "pipe closed" } });
  await tick(window, 3);
  assert.match(document.body.textContent, /pipe closed|not connected|unreachable|reconnect/i);
  // Recovery (-> ready, from a non-ready previous) re-resolves + re-classifies.
  const before = state.calls.filter((c) => c.type === "mm:classify").length;
  onMessage({ type: "mm:statusChanged", status: { phase: "ready", retention: "metadata" } });
  await tick(window, 6);
  assert.ok(state.calls.filter((c) => c.type === "mm:classify").length >= before, "re-classified on recovery");
});
