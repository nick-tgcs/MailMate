// compose.test.mjs — headless DOM tests for the composeAction review panel.
//
// They load the REAL compose.html + compose.js and assert what renders for a MailMate draft: the
// typed four-category commitments guard (with cited spans), the "why this draft" rationale, the
// provider provenance line, and the refine controls (quick-steer chips / Adjust / Regenerate)
// that drive `mm:regenerateDraft`. The background's compose-API glue (beginReply, setComposeDetails)
// is reviewed, not DOM-tested — that boundary is documented in the extension-ui-test-harness note.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadCompose, tick } from "./harness.mjs";

// A draft whose body committed to a date, a price, a payment term, and a legal position — the host
// would have run the model-free guard and returned this typed report.
const guardedDraft = {
  draft_id: "draft_1",
  subject: "Re: Order",
  rationale: "Confirmed the date and price you asked about, and noted the agreement.",
  safety_notes: [],
  requires_human_review: true,
  commitments: {
    findings: [
      { category: "date", text: "Friday", start: 18, end: 24 },
      { category: "price", text: "$1,200", start: 30, end: 36 },
      { category: "payment", text: "net 30", start: 50, end: 56 },
      { category: "legal", text: "I agree", start: 60, end: 67 },
    ],
  },
};

const text = (node) => (node ? node.textContent : "");

test("the typed guard renders one row per category with the cited spans", async () => {
  const { document, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: true,
    provider: { kind: "ollama", model: "llama3" },
  });

  // The flagged badge counts every finding.
  const badge = document.querySelector(".mm-guard__badge--flagged");
  assert.ok(badge, "a guarded draft shows the flagged badge");
  assert.ok(text(badge).includes("4 things to check"), text(badge));

  // Each category present is a labelled row.
  const labels = [...document.querySelectorAll(".mm-guard__cat-label")].map(text);
  assert.ok(labels.some((l) => l.includes("Dates & deadlines")), labels.join("|"));
  assert.ok(labels.some((l) => l.includes("Prices & amounts")), labels.join("|"));
  assert.ok(labels.some((l) => l.includes("Payment terms")), labels.join("|"));
  assert.ok(labels.some((l) => l.includes("Legal & binding")), labels.join("|"));

  // The cited spans are the exact matched text, class-tagged by category.
  assert.equal(text(document.querySelector(".mm-cite--date")), "Friday");
  assert.equal(text(document.querySelector(".mm-cite--price")), "$1,200");
  assert.equal(text(document.querySelector(".mm-cite--payment")), "net 30");
  assert.equal(text(document.querySelector(".mm-cite--legal")), "I agree");

  dom.window.close();
});

test("an all-clear draft shows the clear badge and no category rows", async () => {
  const { document, dom } = await loadCompose({
    draft: { ...guardedDraft, commitments: { findings: [] } },
    providerConfigured: true,
  });
  assert.ok(document.querySelector(".mm-guard__badge--clear"), "clear badge present");
  assert.equal(document.querySelectorAll(".mm-guard__cat-label").length, 0, "no category rows when clear");
  dom.window.close();
});

test("the rationale and provider provenance render", async () => {
  const { document, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: true,
    provider: { kind: "ollama", model: "llama3" },
  });
  const bodyText = text(document.getElementById("mm-body"));
  assert.ok(bodyText.includes("Confirmed the date and price"), "real rationale shown");
  assert.ok(text(document.querySelector(".mm-prov")).includes("ollama"), "provider provenance shown");
  assert.ok(text(document.querySelector(".mm-prov")).includes("llama3"), "model shown");
  dom.window.close();
});

test("the reply-from identity is surfaced when known", async () => {
  const { document, dom } = await loadCompose({
    draft: { ...guardedDraft, from_identity: "me@work.test" },
    providerConfigured: true,
  });
  assert.ok(
    text(document.getElementById("mm-body")).includes("Replying from: me@work.test"),
    "the From identity is shown so the user sees which address replies",
  );
  dom.window.close();
});

test("a quick-steer chip regenerates with that adjustment and repaints", async () => {
  const { window, document, state, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: true,
    provider: { kind: "ollama", model: "llama3" },
  });

  const chips = [...document.querySelectorAll(".mm-chip")].map(text);
  assert.deepEqual(chips, ["Shorter", "Warmer", "More formal", "More direct"], chips.join("|"));

  const shorter = [...document.querySelectorAll(".mm-chip")].find((c) => text(c) === "Shorter");
  shorter.dispatchEvent(new window.Event("click"));
  await tick(window, 6);

  const regen = state.calls.find((c) => c.type === "mm:regenerateDraft");
  assert.ok(regen, "a chip click sends mm:regenerateDraft");
  assert.deepEqual(regen.adjustments, ["Shorter"], "the tapped chip is the adjustment");
  assert.equal(regen.tabId, 7, "it carries the compose tab id");

  // The panel repainted from the regenerate round-trip (mock stamps the steer into the rationale).
  assert.ok(text(document.getElementById("mm-body")).includes("Regenerated (Shorter)"), "panel repainted");
  dom.window.close();
});

test("the Adjust free-text Apply regenerates with a free-text steer", async () => {
  const { window, document, state, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: true,
  });
  const input = document.querySelector(".mm-adjust__input");
  input.value = "warmer, and ask for the PO number";
  const apply = [...document.querySelectorAll(".mm-adjust button")].find((b) => text(b) === "Apply");
  apply.dispatchEvent(new window.Event("click"));
  await tick(window, 6);

  const regen = state.calls.find((c) => c.type === "mm:regenerateDraft");
  assert.ok(regen, "Apply sends mm:regenerateDraft");
  assert.equal(regen.steer, "warmer, and ask for the PO number", "the free-text steer is carried");
  dom.window.close();
});

test("no refine controls are offered when no provider can draft", async () => {
  const { document, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: false,
  });
  assert.equal(document.querySelector(".mm-refine"), null, "no refine block without a provider");
  assert.equal(document.querySelectorAll(".mm-chip").length, 0, "no chips without a provider");
  // …and the degraded provider call-to-action IS shown instead.
  assert.ok(document.querySelector(".mm-degraded"), "the degraded provider state is shown");
  dom.window.close();
});

test("a draft with no typed report degrades to the safety_notes list", async () => {
  const { document, dom } = await loadCompose({
    draft: {
      draft_id: "d2",
      subject: "Re: Hi",
      rationale: "",
      safety_notes: ["Mentions a refund — double-check that's intended."],
      requires_human_review: true,
      // no `commitments` field (an older host)
    },
    providerConfigured: true,
  });
  const bodyText = text(document.getElementById("mm-body"));
  assert.ok(bodyText.includes("Mentions a refund"), "legacy safety_notes listed");
  // No typed category rows in the legacy path.
  assert.equal(document.querySelectorAll(".mm-guard__cat-label").length, 0);
  dom.window.close();
});

test("a failed regenerate keeps the draft and surfaces an inline error", async () => {
  const { window, document, dom } = await loadCompose({
    draft: guardedDraft,
    providerConfigured: true,
    regenerate: () => ({ fail: "provider timed out" }),
  });
  const regen = [...document.querySelectorAll(".mm-refine button")].find((b) => text(b) === "Regenerate");
  regen.dispatchEvent(new window.Event("click"));
  await tick(window, 6);

  const note = document.getElementById("mm-regen-note");
  assert.ok(text(note).includes("provider timed out"), "the failure is surfaced inline");
  // The original draft survives (its rationale is still on screen).
  assert.ok(text(document.getElementById("mm-body")).includes("Confirmed the date and price"), "draft kept");
  dom.window.close();
});
