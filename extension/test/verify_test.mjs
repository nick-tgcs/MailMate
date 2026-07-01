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

async function openRules(opts) {
  const ctx = await loadDashboard({ settings: { providers: [], paused: false }, ...opts });
  ctx.window.selectTab("rules");
  await tick(ctx.window, 6);
  return ctx;
}

test("debugging: check all calls", async () => {
  const { window, document, state, dom } = await openRules({ rules: [activeRule] });
  const c = document.getElementById("mm-content");
  const disableBtn = [...c.querySelectorAll("button")].find((b) => b.textContent === "Disable");
  console.log("Before click, calls:", state.calls.map(c => c.type));
  
  disableBtn.dispatchEvent(new window.Event("click"));
  await tick(window, 8);
  
  console.log("After disable, calls:", state.calls.map(c => c.type));
  
  // Check if there's a listRules call after setRuleStatus
  const setRuleIdx = state.calls.findIndex((m) => m.type === "mm:setRuleStatus");
  const listRulesIdx = state.calls.findIndex((m, idx) => m.type === "mm:listRules" && idx > setRuleIdx);
  
  console.log("setRuleStatus at index:", setRuleIdx);
  console.log("mm:listRules after it at index:", listRulesIdx);
  
  dom.window.close();
});
