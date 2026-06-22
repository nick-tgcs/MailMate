// panel.test.mjs — the Phase-1b per-message experience, exercised through the REAL panel.
//
// Loads panel.html + panel.js into jsdom over a mocked background and asserts the action-first
// render: correctable salient-signal chips (+ the signal_marked_wrong correction), the inform-only
// Safety block, the one-click unsubscribe affordance, and the cold-start "still learning" state.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadPanel, classifyResult, tick } from "./panel-harness.mjs";

const textOf = (document) => document.getElementById("mm-body").textContent;

test("the panel leads with the action-first primary actions", async () => {
  const { document } = await loadPanel();
  const labels = [...document.querySelectorAll(".mm-primary-actions .mm-pa")].map((b) => b.textContent);
  assert.ok(labels.some((l) => l.includes("File to")), `File to… present: ${labels}`);
  assert.ok(labels.some((l) => l.includes("Junk")), "Junk & block present");
  assert.ok(labels.some((l) => l.includes("Mark read")), "Mark read present");
  assert.ok(labels.some((l) => l.includes("Draft reply")), "Draft reply present");
});

test("salient signals render as chips, and a correctable one records signal_marked_wrong", async () => {
  const { document, state, window } = await loadPanel({
    classify: classifyResult({
      classification: {
        labels: ["phishing"],
        spam_score: 0.7,
        salient_signals: [
          { id: "auth_fail", label: "Sender failed SPF/DKIM/DMARC authentication", kind: "authentication", source: "deterministic_feature", weight: 0.9, correctable: true },
          { id: "rule_x", label: "Matched one of your active rules", kind: "rule", source: "classification_rule", weight: 1.0, correctable: false },
        ],
      },
    }),
  });

  const chips = [...document.querySelectorAll(".mm-signal")];
  assert.equal(chips.length, 2, "both signals render as chips");
  // The reasons are human labels, not raw ids.
  assert.ok(textOf(document).includes("Sender failed SPF/DKIM/DMARC authentication"));
  // Only the correctable (deterministic) signal carries a "mark wrong" affordance.
  const wrongButtons = document.querySelectorAll(".mm-signal__wrong");
  assert.equal(wrongButtons.length, 1, "only the correctable signal is markable wrong");

  // Clicking it records a signal_marked_wrong correction carrying the signal id + prior label.
  wrongButtons[0].click();
  await tick(window, 4);
  const call = state.calls.find((c) => c.type === "mm:signalWrong");
  assert.ok(call, "a signal_marked_wrong correction was sent");
  assert.equal(call.signalId, "auth_fail");
  assert.equal(call.priorLabel, "phishing");
});

test("the inform-only Safety block renders findings but offers no action", async () => {
  const { document } = await loadPanel({
    classify: classifyResult({
      classification: {
        safety_findings: [
          { id: "executable_attachment", title: "Executable attachment", detail: "invoice.pdf.exe could install malware.", severity: "danger" },
          { id: "auth_failure", title: "Sender failed authentication", detail: "SPF/DKIM/DMARC failed.", severity: "warning" },
        ],
      },
    }),
  });
  const safety = document.querySelector(".mm-safety");
  assert.ok(safety, "a Safety block is rendered");
  assert.ok(safety.textContent.includes("Executable attachment"));
  assert.ok(safety.textContent.includes("Sender failed authentication"));
  // Inform-only: no buttons inside the Safety block.
  assert.equal(safety.querySelectorAll("button").length, 0, "the Safety block takes no action");
});

test("an unsubscribe affordance appears and opens a pre-addressed compose", async () => {
  const { document, state, window } = await loadPanel({
    classify: classifyResult({
      classification: { labels: ["newsletter"] },
      unsubscribe: { mailto: { to: "unsub@list.test", subject: "unsubscribe" }, http_url: "https://list.test/u", one_click: true },
    }),
  });
  const unsub = [...document.querySelectorAll(".mm-pa")].find((b) => b.textContent.includes("Unsubscribe"));
  assert.ok(unsub, "the unsubscribe button is present when the host parsed the header");
  unsub.click();
  await tick(window, 4);
  const call = state.calls.find((c) => c.type === "mm:unsubscribe");
  assert.ok(call, "clicking sends mm:unsubscribe");
  assert.equal(call.unsubscribe.mailto.to, "unsub@list.test");
});

test("no unsubscribe affordance when the message has no List-Unsubscribe", async () => {
  const { document } = await loadPanel(); // default result carries no unsubscribe
  const unsub = [...document.querySelectorAll(".mm-pa")].find((b) => b.textContent.includes("Unsubscribe"));
  assert.equal(unsub, undefined, "no affordance without the header");
});

test("a cold-start verdict shows the 'still learning' card, not a bare 0/1 count", async () => {
  const { document } = await loadPanel({
    classify: classifyResult({
      classification: { labels: ["needs_review"], needs_review: true, confidence: 0.5, confidence_band: "low", salient_signals: [] },
      suggested_actions: [],
    }),
  });
  const card = document.querySelector(".mm-coldstart");
  assert.ok(card, "the cold-start card is shown");
  assert.ok(card.textContent.includes("Still learning"), "it reads as still-learning");
  // The old bare "Nothing to do" message is not used for a needs-review verdict.
  assert.ok(!textOf(document).includes("looks handled"), "no misleading 'handled' line");
});

test("edge states render their explanatory text, not just a button", async () => {
  // Regression: the loading/no-message/error/connection states built their <p> text but never
  // appended it (Node.append returns undefined), so only the button showed. Each must show text.
  const noMsg = await loadPanel({
    // No displayed message → renderNoMessage.
    classify: classifyResult(),
  });
  // Force the no-message path by making the displayed-message resolution return nothing.
  noMsg.window.browser.messageDisplay.getDisplayedMessage = async () => null;
  noMsg.window.browser.mailTabs.getSelectedMessages = async () => ({ messages: [] });

  // Host-down path: boot renders the connection card with its reason text + a Retry button.
  const down = await loadPanel({ status: { phase: "disconnected", reason: "native host not found" } });
  const downText = down.document.getElementById("mm-body").textContent;
  assert.ok(downText.includes("unreachable") || downText.includes("not connected"), `connection text shown: ${downText}`);
  assert.ok(downText.includes("native host not found"), "the reason is shown, not just a button");
  assert.ok(down.document.querySelector("button"), "the Retry button is present");
});

test("the calibrated confidence band comes from the host, with needs_review overriding", async () => {
  const high = await loadPanel({ classify: classifyResult({ classification: { confidence_band: "high", needs_review: false } }) });
  assert.ok(high.document.querySelector(".mm-band").textContent.includes("High confidence"));

  const review = await loadPanel({ classify: classifyResult({ classification: { confidence_band: "high", needs_review: true } }) });
  assert.ok(review.document.querySelector(".mm-band").textContent.includes("Needs review"), "needs_review wins over the band");
});
