// compose_more.test.mjs — cover the compose-review paths the original suite didn't reach: the
// free-text Adjust + plain Regenerate refine controls, and the degraded "drafting needs a provider"
// call-to-action (the zero-provider-by-default contract). Same harness + idiom as compose.test.mjs.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadCompose } from "./harness.mjs";

const buttonByText = (root, label) =>
  [...root.querySelectorAll("button")].find((b) => (b.textContent || "").trim() === label);

const draft = { draft_id: "d1", subject: "Re: your quote", rationale: "kept it short", commitments: null, safety_notes: [] };

test("the free-text Adjust box regenerates the draft with the typed steer", async (t) => {
  const { window, document, state } = await loadCompose({ draft, providerConfigured: true });
  t.after(() => window.close());
  const input = document.querySelector(".mm-adjust__input");
  assert.ok(input, "the Adjust free-text box renders when a provider can draft");
  input.value = "make it warmer";
  buttonByText(document.body, "Apply").click();
  await new Promise((r) => window.setTimeout(r, 0));
  const regen = state.calls.find((c) => c.type === "mm:regenerateDraft");
  assert.ok(regen, "a regenerate was requested");
  assert.equal(regen.steer, "make it warmer");
});

test("the plain Regenerate button re-drafts with no adjustments", async (t) => {
  const { window, document, state } = await loadCompose({ draft, providerConfigured: true });
  t.after(() => window.close());
  buttonByText(document.body, "Regenerate").click();
  await new Promise((r) => window.setTimeout(r, 0));
  assert.ok(state.calls.some((c) => c.type === "mm:regenerateDraft"));
});

test("an empty Adjust box does not fire a pointless regenerate", async (t) => {
  const { window, document, state } = await loadCompose({ draft, providerConfigured: true });
  t.after(() => window.close());
  const input = document.querySelector(".mm-adjust__input");
  input.value = "   "; // whitespace only
  buttonByText(document.body, "Apply").click();
  await new Promise((r) => window.setTimeout(r, 0));
  assert.equal(state.calls.filter((c) => c.type === "mm:regenerateDraft").length, 0);
});

test("with no provider configured, compose shows a loud call-to-action, not a silent failure", async (t) => {
  const { window, document, state } = await loadCompose({ draft, providerConfigured: false });
  t.after(() => window.close());
  assert.match(document.body.textContent, /needs an AI provider|add a provider/i);
  // No refine controls are offered when nothing can draft.
  assert.equal(document.querySelector(".mm-refine"), null, "no refine box when drafting can't act");
  // The CTA opens the options page.
  const open = buttonByText(document.body, "Open MailMate settings");
  assert.ok(open, "an 'Open settings' affordance is offered");
  open.click();
  await new Promise((r) => window.setTimeout(r, 0));
  assert.ok(state.calls.some((c) => c.type === "openOptionsPage"));
});
