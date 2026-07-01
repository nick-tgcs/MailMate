import { test } from "node:test";
import assert from "node:assert/strict";
import { loadDashboard, tick, makeBrowserMock } from "./harness.mjs";

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

test("verify listRules returns disabled rule after setRuleStatus", async () => {
  // Manually test the mock behavior
  const { browser, state } = makeBrowserMock({ rules: [activeRule] });
  
  // Initial listRules
  let reply = await browser.runtime.sendMessage({ type: "mm:listRules" });
  console.log("Initial listRules response:", JSON.stringify(reply, null, 2));
  assert.equal(reply.rules[0].status, "active");
  
  // setRuleStatus
  reply = await browser.runtime.sendMessage({ type: "mm:setRuleStatus", ruleId: "rule_stripe", kind: "action", status: "disabled" });
  console.log("setRuleStatus response:", JSON.stringify(reply, null, 2));
  
  // listRules after setRuleStatus
  reply = await browser.runtime.sendMessage({ type: "mm:listRules" });
  console.log("After setRuleStatus, listRules response:", JSON.stringify(reply, null, 2));
  assert.equal(reply.rules[0].status, "disabled", "Rule status should be disabled after setRuleStatus");
});
