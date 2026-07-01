// proposals.test.mjs — the Proposals tab renders a learned rule in English with its back-test.
//
// Phase 6 exit (half 2): a proposal card shows its rule in English with back-test numbers. The
// rule→English renderer (ruleToEnglish/conditionToEnglish/effectToEnglish) is a pure, total
// function shared with the Rules-manager tab, so it is unit-tested directly off the loaded global
// AND through the rendered card.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadDashboard, tick } from "./harness.mjs";

const text = (n) => (n ? n.textContent : "");

// A filing rule: sender_domain == stripe.com  →  move to Receipts.
const stripeRule = {
  kind: "action",
  scope: "domain",
  condition: { field: "sender_domain", op: "eq", value: "stripe.com" },
  effect: { move: "Receipts" },
};

const proposalWithRule = {
  id: "prop_1",
  title: "File stripe.com mail to Receipts",
  proposal_type: "new_rule",
  recommended_status: "shadow_mode",
  risk_level: "low",
  rationale: "You moved 23 messages from stripe.com to Receipts.",
  rule_draft: stripeRule,
  back_test: { precision: 0.88, support: 23 },
};

test("ruleToEnglish renders a bare predicate rule as a sentence", async () => {
  const { window, dom } = await loadDashboard({ settings: { providers: [], paused: false } });
  assert.equal(
    window.ruleToEnglish(stripeRule),
    "When sender domain is “stripe.com” → move to Receipts.",
  );
  dom.window.close();
});

test("conditionToEnglish handles all/any/not nesting", async () => {
  const { window, dom } = await loadDashboard({ settings: { providers: [], paused: false } });
  const cond = {
    all: [
      { field: "sender_domain", op: "in", value: ["github.com", "stripe.com"] },
      { any: [
        { field: "subject", op: "contains_any", value: ["receipt", "invoice"] },
        { not: { field: "is_spam", op: "eq", value: true } },
      ] },
    ],
  };
  const en = window.conditionToEnglish(cond);
  assert.match(en, /sender domain is one of “github\.com”, “stripe\.com”/);
  assert.match(en, / and /);
  assert.match(en, /subject contains any of “receipt”, “invoice” or not \(spam is yes\)/);
  dom.window.close();
});

test("an unknown field/op degrades to its raw token instead of throwing", async () => {
  const { window, dom } = await loadDashboard({ settings: { providers: [], paused: false } });
  const en = window.ruleToEnglish({
    condition: { field: "future_field", op: "newop", value: "x" },
    effect: { tag: ["vip"] },
  });
  assert.match(en, /future field newop “x”/);
  assert.match(en, /tag with vip/);
  dom.window.close();
});

test("backTestSummary renders precision·support, support-only, or nothing", async () => {
  const { window, dom } = await loadDashboard({ settings: { providers: [], paused: false } });
  assert.equal(window.backTestSummary({ precision: 0.88, support: 23 }), "precision 0.88 · support 23 msgs");
  assert.equal(window.backTestSummary({ support: 1 }), "support 1 msg");
  assert.equal(window.backTestSummary(null), "");
  dom.window.close();
});

// A Phase-7 decay proposal: retire an existing rule (no draft to shadow; it names a target).
const retireProposal = {
  id: "prop_retire",
  title: "Retire a rule you keep undoing",
  proposal_type: "retire_rule",
  recommended_status: "retired",
  risk_level: "medium",
  rationale: "You undid this rule's action 3 times. Retire it?",
  rule_draft: null,
  target_rule_id: "rule_noisy",
  target_rule_kind: "action",
};

test("a retire proposal card offers Retire/Keep, never the new-rule Approve→shadow", async () => {
  const { window, document, dom } = await loadDashboard({
    settings: { providers: [], paused: false },
    proposals: [retireProposal],
  });
  window.selectTab("proposals");
  await tick(window, 6);

  const content = document.getElementById("mm-content");
  const buttons = [...content.querySelectorAll("button")].map((b) => b.textContent);
  assert.ok(buttons.includes("Approve → retire rule"), buttons.join("|"));
  assert.ok(buttons.includes("Keep rule"), buttons.join("|"));
  assert.ok(
    !buttons.includes("Approve → shadow"),
    "a retire references an existing rule — no misleading shadow wording",
  );
  // The rationale (the only explanation a retire carries) renders; there is no rule-draft line.
  assert.match(content.textContent, /undid this rule's action 3 times/);
  assert.equal(content.querySelector(".mm-rule-en"), null, "a retire has no draft to render in English");
  dom.window.close();
});

// A Phase-7 induced proposal that conflicts with an existing active rule → forced human review.
const conflictingProposal = {
  id: "prop_conflict",
  title: "Label auth-fail + no-prior-contact as suspicious",
  proposal_type: "new_rule",
  recommended_status: "pending_human_review",
  risk_level: "medium",
  rationale: "You labelled 3 such messages suspicious.",
  rule_draft: {
    kind: "classification",
    scope: "global",
    condition: { all: [{ field: "auth_result", op: "eq", value: "fail" }] },
    effect: { set_labels: ["suspicious"] },
  },
  back_test: { precision: 1.0, support: 3 },
  conflicts: [
    {
      kind: "overlap",
      severity: "medium",
      description: "candidate is more specific than (subsumed by) rule rule_spam and its effect differs from that rule's on the mail they both match",
      existing_rule_id: "rule_spam",
    },
  ],
};

test("a conflicting proposal card warns about the overlap and recommends human review", async () => {
  const { window, document, dom } = await loadDashboard({
    settings: { providers: [], paused: false },
    proposals: [conflictingProposal],
  });
  window.selectTab("proposals");
  await tick(window, 6);

  const content = document.getElementById("mm-content");
  // The conflict warning surfaces with the count and the human description.
  assert.match(content.textContent, /Conflicts with 1 existing rule — needs your review/);
  assert.match(content.textContent, /subsumed by/);
  // And the card recommends the forced-review landing status (never auto-shadow).
  assert.match(content.textContent, /Recommended: pending_human_review/);
  dom.window.close();
});

test("the Proposals tab card shows the rule in English with its back-test", async () => {
  const { window, document, dom } = await loadDashboard({
    settings: { providers: [], paused: false },
    proposals: [proposalWithRule],
  });
  window.selectTab("proposals");
  await tick(window, 6);

  const content = document.getElementById("mm-content");
  const ruleLine = content.querySelector(".mm-rule-en");
  assert.ok(ruleLine, "the card renders the rule in English");
  assert.equal(text(ruleLine), "When sender domain is “stripe.com” → move to Receipts.");
  // The back-test numbers are on the card.
  assert.match(text(content), /precision 0\.88 · support 23 msgs/, text(content));
  dom.window.close();
});
