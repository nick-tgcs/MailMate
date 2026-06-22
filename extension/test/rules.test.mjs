// rules.test.mjs — the Rules-manager tab (Phase 6 exit half 1).
//
// An approved (active) rule appears with a readable English condition and its backed correction
// signal; a human can disable / enable / promote it, and the change re-groups the rule live.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadDashboard, tick } from "./harness.mjs";

const text = (n) => (n ? n.textContent : "");

const activeRule = {
  rule_id: "rule_stripe",
  kind: "action",
  scope: "domain",
  band: "learned_active",
  status: "active",
  risk_level: "low",
  version_number: 1,
  condition: { field: "sender_domain", op: "eq", value: "stripe.com" },
  effect: { move: "Receipts" },
  undo_count: 0,
};
const shadowRule = {
  rule_id: "rule_github",
  kind: "action",
  scope: "domain",
  band: "agent_shadow",
  status: "shadow_mode",
  risk_level: "low",
  version_number: 1,
  condition: { field: "sender_domain", op: "eq", value: "github.com" },
  effect: { move: "Code" },
  undo_count: 2,
};
const disabledRule = {
  rule_id: "rule_old",
  kind: "classification",
  scope: "domain",
  band: "learned_active",
  status: "disabled",
  risk_level: "low",
  version_number: 1,
  condition: { field: "sender_domain", op: "eq", value: "noise.example" },
  effect: { set_labels: ["promotions"] },
  undo_count: null,
};

async function openRules(opts) {
  const ctx = await loadDashboard({ settings: { providers: [], paused: false }, ...opts });
  ctx.window.selectTab("rules");
  await tick(ctx.window, 6);
  return ctx;
}

test("the Rules tab renders each rule in English under its lifecycle group", async () => {
  const { document, dom } = await openRules({ rules: [activeRule, shadowRule, disabledRule] });
  const c = document.getElementById("mm-content");

  // Group headings, in order.
  const bands = [...c.querySelectorAll(".mm-band__title")].map(text);
  assert.ok(bands.some((t) => /Active/.test(t)), bands.join("|"));
  assert.ok(bands.some((t) => /Shadow/.test(t)), bands.join("|"));
  assert.ok(bands.some((t) => /Disabled/.test(t)), bands.join("|"));

  // The active rule reads in English.
  const ruleLines = [...c.querySelectorAll(".mm-rule-en")].map(text);
  assert.ok(
    ruleLines.includes("When sender domain is “stripe.com” → move to Receipts."),
    ruleLines.join(" || "),
  );
  // Its backed correction signal: a clean record (0) reads honestly, not as a blank.
  assert.match(text(c), /no corrections yet/);
  // The shadow rule's real undo count shows.
  assert.match(text(c), /you undid this rule 2 times/);
  // The disabled rule's untracked count is honest, not "0".
  assert.match(text(c), /corrections: not tracked yet/);
  // A disabled rule stays visible AND offers a one-click Enable (you can recover it from the UI).
  assert.ok(
    [...c.querySelectorAll("button")].some((b) => b.textContent === "Enable"),
    "the disabled rule offers Enable",
  );
  dom.window.close();
});

test("disabling an active rule sends set_rule_status and re-groups it as disabled", async () => {
  const { window, document, state, dom } = await openRules({ rules: [activeRule] });
  const c = document.getElementById("mm-content");
  const disableBtn = [...c.querySelectorAll("button")].find((b) => b.textContent === "Disable");
  assert.ok(disableBtn, "an active rule offers Disable");
  disableBtn.dispatchEvent(new window.Event("click"));
  await tick(window, 8);

  const call = state.calls.find((m) => m.type === "mm:setRuleStatus");
  assert.ok(call, "set_rule_status sent");
  assert.equal(call.ruleId, "rule_stripe");
  assert.equal(call.kind, "action");
  assert.equal(call.status, "disabled");

  // After the live re-pull the rule sits under Disabled and now offers Enable.
  const c2 = document.getElementById("mm-content");
  assert.match(text(c2), /Disabled/);
  assert.ok([...c2.querySelectorAll("button")].some((b) => b.textContent === "Enable"), "offers Enable now");
  dom.window.close();
});

test("a shadow rule can be promoted to active", async () => {
  const { window, document, state, dom } = await openRules({ rules: [shadowRule] });
  const c = document.getElementById("mm-content");
  const activate = [...c.querySelectorAll("button")].find((b) => b.textContent === "Activate");
  assert.ok(activate, "a shadow rule offers Activate");
  activate.dispatchEvent(new window.Event("click"));
  await tick(window, 8);
  const call = state.calls.find((m) => m.type === "mm:setRuleStatus");
  assert.equal(call.status, "active");
  dom.window.close();
});

test("the Rules tab degrades to an honest empty state with no rules", async () => {
  const { document, dom } = await openRules({ rules: [] });
  assert.match(text(document.getElementById("mm-content")), /No learned rules yet/);
  dom.window.close();
});
