// options.test.mjs — the notification-preferences section of the options page.
//
// The machinery (notifications.js) is fully covered in notifications.test.mjs; this closes the
// loop by asserting the options UI writes the EXACT `mm:notifPrefs` storage.local shape that
// machinery reads — a per-class toggle and the quiet-hours window.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadOptions, tick } from "./harness.mjs";

const text = (node) => (node ? node.textContent : "");

test("the Notifications section renders the per-class toggles and quiet hours", async () => {
  const { document, dom } = await loadOptions();
  const titles = [...document.querySelectorAll(".mm-section__title")].map(text);
  assert.ok(titles.includes("Notifications"), titles.join("|"));
  const checks = [...document.querySelectorAll("label.mm-check")].map(text);
  assert.ok(checks.some((t) => t.includes("rule proposals to approve")), checks.join("|"));
  assert.ok(checks.some((t) => t.includes("Quiet hours")), checks.join("|"));
  dom.window.close();
});

test("toggling a class off writes the pref to storage.local", async () => {
  const { window, document, state, dom } = await loadOptions();
  const proposalLabel = [...document.querySelectorAll("label.mm-check")].find((l) =>
    text(l).includes("rule proposals to approve"),
  );
  const cb = proposalLabel.querySelector("input[type=checkbox]");
  assert.equal(cb.checked, true, "default-on");
  cb.checked = false;
  cb.dispatchEvent(new window.Event("change"));
  await tick(window, 4);

  const prefs = state.local["mm:notifPrefs"];
  assert.ok(prefs, "prefs persisted to storage.local");
  assert.equal(prefs.classes.proposal_ready, false, "the class was turned off");
  // The other classes keep their defaults — a partial write doesn't wipe them.
  assert.equal(prefs.classes.followup_draft_ready, true);
  dom.window.close();
});

// --- Provider: Test connection -------------------------------------------------------------

const withProvider = {
  providers: [{ id: "local", kind: "ollama", endpoint: "http://localhost:11434", model: "llama3", configured: false }],
  default_provider: "local",
};

test("Test connection probes the provider and reports a reachable result", async () => {
  const { window, document, state, dom } = await loadOptions({ settings: withProvider });
  const testBtn = [...document.querySelectorAll("button")].find((b) => b.textContent === "Test connection");
  assert.ok(testBtn, "the provider card has a Test connection button");

  testBtn.dispatchEvent(new window.Event("click"));
  await tick(window, 6);

  // It sent test_provider with the provider's identity…
  const call = state.calls.find((c) => c.type === "mm:testProvider");
  assert.ok(call, "mm:testProvider was sent");
  assert.equal(call.kind, "ollama");
  assert.equal(call.endpoint, "http://localhost:11434");
  assert.equal(call.providerId, "local");
  // …and surfaced the liveness result as a toast (the default mock reply is reachable, 2 models).
  const toast = document.getElementById("mm-toast");
  assert.match(text(toast), /Connected.*2 models/, text(toast));
  dom.window.close();
});

test("Test connection reports an unreachable endpoint as an error, not a crash", async () => {
  const { window, document, dom } = await loadOptions({
    settings: withProvider,
    testProviderReply: { ok: true, reachable: false, error: "connection refused" },
  });
  const testBtn = [...document.querySelectorAll("button")].find((b) => b.textContent === "Test connection");
  testBtn.dispatchEvent(new window.Event("click"));
  await tick(window, 6);
  const toast = document.getElementById("mm-toast");
  assert.match(text(toast), /Not reachable.*connection refused/, text(toast));
  assert.ok(toast.classList.contains("mm-toast--err"), "shown as an error toast");
  dom.window.close();
});

test("the provider 'kind' dropdown no longer offers the mock kind", async () => {
  const { document, dom } = await loadOptions();
  const kinds = [...document.querySelectorAll("select")].flatMap((sel) =>
    [...sel.querySelectorAll("option")].map((o) => o.value),
  );
  assert.ok(kinds.includes("ollama"), "real kinds are present");
  assert.ok(!kinds.includes("mock"), "mock is not offered in prod");
  dom.window.close();
});

test("enabling quiet hours persists the window to storage.local", async () => {
  const { window, document, state, dom } = await loadOptions();
  const quietLabel = [...document.querySelectorAll("label.mm-check")].find((l) =>
    text(l).includes("Quiet hours"),
  );
  const enable = quietLabel.querySelector("input[type=checkbox]");
  enable.checked = true;
  enable.dispatchEvent(new window.Event("change"));
  await tick(window, 4);

  const prefs = state.local["mm:notifPrefs"];
  assert.equal(prefs.quietHours.enabled, true);
  assert.match(prefs.quietHours.start, /^\d\d:\d\d$/, "a start time is saved");
  assert.match(prefs.quietHours.end, /^\d\d:\d\d$/, "an end time is saved");
  dom.window.close();
});

// --- Categories: per-category policy + tag→category mapping ---------------------------------

const catRow = (document, name) =>
  [...document.querySelectorAll(".mm-cat-row")].find((r) => text(r.querySelector(".mm-cat-row__name")) === name);

test("the Categories section renders a policy control per category", async () => {
  const { document, dom } = await loadOptions();
  const titles = [...document.querySelectorAll(".mm-section__title")].map(text);
  assert.ok(titles.includes("Categories"), titles.join("|"));
  // One policy select per category in the host vocabulary.
  assert.ok(catRow(document, "Newsletters"), "a row for Newsletters");
  assert.ok(catRow(document, "Receipts"), "a row for Receipts");
  dom.window.close();
});

test("changing a category policy writes set_category_policy and the re-render reflects it", async () => {
  const { window, document, state, dom } = await loadOptions();
  const select = catRow(document, "Newsletters").querySelector("select");
  assert.equal(select.value, "auto", "default policy is auto");
  select.value = "off";
  select.dispatchEvent(new window.Event("change"));
  await tick(window, 8);

  const call = state.calls.find((c) => c.type === "mm:setCategoryPolicy");
  assert.ok(call, "set_category_policy sent");
  assert.equal(call.category, "newsletters");
  assert.equal(call.policy, "off");
  // Host-owns-the-truth: the snapshot now carries it, and the re-rendered control shows it.
  assert.equal(state.settings.category_policies.newsletters, "off");
  assert.equal(catRow(document, "Newsletters").querySelector("select").value, "off");
  dom.window.close();
});

test("adding a tag→category mapping writes set_tag_mapping", async () => {
  const { window, document, state, dom } = await loadOptions();
  const newRow = [...document.querySelectorAll(".mm-row")].find((r) => text(r).includes("New mapping"));
  const inputs = newRow.querySelectorAll("input[type=text]");
  inputs[0].value = "$label1";
  inputs[1].value = "VIP";
  const addBtn = [...newRow.querySelectorAll("button")].find((b) => b.textContent === "Add mapping");
  addBtn.dispatchEvent(new window.Event("click"));
  await tick(window, 8);

  const call = state.calls.find((c) => c.type === "mm:setTagMapping");
  assert.ok(call, "set_tag_mapping sent");
  assert.equal(call.tag, "$label1");
  assert.equal(call.category, "VIP");
  assert.equal(state.settings.tag_mappings["$label1"], "vip", "host lower-cases + stores it");
  dom.window.close();
});

test("an existing tag mapping renders and can be removed", async () => {
  const { window, document, state, dom } = await loadOptions({
    settings: { tag_mappings: { "$label2": "leads" } },
  });
  const mappingRow = [...document.querySelectorAll(".mm-cat-row")].find((r) =>
    text(r.querySelector(".mm-cat-row__name")).includes("$label2 → leads"),
  );
  assert.ok(mappingRow, "the existing mapping is shown");
  mappingRow.querySelector("button").dispatchEvent(new window.Event("click"));
  await tick(window, 8);
  const call = state.calls.find((c) => c.type === "mm:setTagMapping" && c.tag === "$label2");
  assert.ok(call, "remove sent set_tag_mapping");
  assert.equal(call.category, "", "an empty category clears the mapping");
  assert.equal(state.settings.tag_mappings["$label2"], undefined, "mapping gone");
  dom.window.close();
});

// --- Accounts: per-account triage scope ----------------------------------------------------

test("the Accounts section lists each account with a scope checkbox", async () => {
  const { document, dom } = await loadOptions();
  const titles = [...document.querySelectorAll(".mm-section__title")].map(text);
  assert.ok(titles.includes("Accounts"), titles.join("|"));
  const checks = [...document.querySelectorAll("label.mm-check")].map(text);
  assert.ok(checks.some((t) => t.includes("Work (IMAP)")), checks.join("|"));
  assert.ok(checks.some((t) => t.includes("Personal (IMAP)")), checks.join("|"));
  dom.window.close();
});

test("excluding an account writes set_account_scope enabled:false and reflects it", async () => {
  const { window, document, state, dom } = await loadOptions();
  const workLabel = [...document.querySelectorAll("label.mm-check")].find((l) => text(l).includes("Work (IMAP)"));
  const cb = workLabel.querySelector("input[type=checkbox]");
  assert.equal(cb.checked, true, "in scope by default");
  cb.checked = false;
  cb.dispatchEvent(new window.Event("change"));
  await tick(window, 8);

  const call = state.calls.find((c) => c.type === "mm:setAccountScope");
  assert.ok(call, "set_account_scope sent");
  assert.equal(call.accountId, "acct_work");
  assert.equal(call.enabled, false);
  assert.equal(state.settings.account_scopes.acct_work, false);
  // The re-rendered checkbox stays unchecked (host owns the truth).
  const cb2 = [...document.querySelectorAll("label.mm-check")]
    .find((l) => text(l).includes("Work (IMAP)"))
    .querySelector("input[type=checkbox]");
  assert.equal(cb2.checked, false, "re-render shows out-of-scope");
  dom.window.close();
});

test("the Accounts section degrades to a note when no accounts are reported", async () => {
  const { document, dom } = await loadOptions({ accounts: [] });
  const titles = [...document.querySelectorAll(".mm-section__title")].map(text);
  assert.ok(titles.includes("Accounts"));
  const hints = [...document.querySelectorAll(".mm-hint")].map(text);
  assert.ok(hints.some((t) => t.includes("No mail accounts")), hints.join("|"));
  dom.window.close();
});
