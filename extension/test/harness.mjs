// harness.mjs — load the REAL dashboard into jsdom with a mocked native-messaging host.
//
// The dashboard never talks to the host directly: it sends `mm:*` messages over
// `browser.runtime.sendMessage`, which the background's router answers. So a faithful UI test
// only has to stand up a `browser` mock that returns the SAME payload shapes the real host does
// (verified against the live binary by the framed-stdin probes), then load the actual
// dashboard.html + dashboard.js and assert what renders. No real Thunderbird, no network — but the
// genuine render/click code runs, which is exactly the layer the Rust suite cannot reach.

import { readFileSync } from "node:fs";
import vm from "node:vm";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { JSDOM } from "jsdom";

const here = dirname(fileURLToPath(import.meta.url));
const extDir = join(here, "..");

// Read an extension source file and tag it with a `//# sourceURL` so the V8 engine attributes the
// eval'd inline script to the real file on disk. That gives test stack traces a real filename AND
// lets c8/NODE_V8_COVERAGE measure line coverage of the source (without it the script is anonymous
// and coverage tools see nothing). The comment is invisible to execution.
function readSource(name) {
  return `${readFileSync(join(extDir, name), "utf8")}\n//# sourceURL=${join(extDir, name)}\n`;
}

// Run a readSource() string as its own vm.Script in the jsdom realm, passing the real file path as
// `filename` (parsed back out of the appended `//# sourceURL`) so c8/V8 attributes line coverage —
// including top-level statements — to the real file. An inline <script> only attributes nested
// function calls, silently understating every classic script.
function runInRealm(dom, js) {
  const m = js.match(/\/\/# sourceURL=(.+?)\s*$/);
  // Pass the path as the vm `filename` and STRIP the `//# sourceURL` comment: when both are present
  // V8 resolves the script url from the comment, and that path attributes only nested function calls
  // (top-level statements show uncovered). filename-only attributes the whole script.
  const src = m ? js.replace(/\n\/\/# sourceURL=.+\n?$/, "\n") : js;
  new vm.Script(src, m ? { filename: m[1].trim() } : undefined).runInContext(dom.getInternalVMContext());
}

const clone = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

// A `browser` mock whose `runtime.sendMessage` answers the dashboard's `mm:*` verbs from a small
// mutable settings store — so set_provider really flips the in-memory default and a following
// get_settings reflects it, just like the host. Every call is recorded for assertions.
export function makeBrowserMock({ settings, models = [], onboarded = true, backfill, proposals = [], rules = [], reviewQueue = [], activity = [], followups = [], messages = {} } = {}) {
  const state = {
    backfill: backfill ? clone(backfill) : null, // first-run backfill status the dashboard reads
    proposals: clone(proposals), // pending_reviews the Proposals tab renders
    rules: clone(rules), // active+shadow rules the Rules-manager tab renders
    reviewQueue: clone(reviewQueue), // the review-queue items the Review tab renders
    activity: clone(activity), // audit events the Activity tab renders
    followups: clone(followups), // tracked deals the Follow-ups tab renders
    // Deep-clone so each loaded dashboard owns its snapshot — the mock mutates it (set_provider
    // flips default_provider), and a shared reference would leak that change into later tests.
    settings: settings ? clone(settings) : null, // the get_settings snapshot (null = host unreachable)
    models,
    calls: [], // every { type, ...message } the UI sent
    local: { "mm:onboarded": onboarded },
    session: {},
    listeners: [],
  };

  async function sendMessage(message) {
    state.calls.push(clone(message));
    const t = message && message.type;
    switch (t) {
      case "mm:getStatus":
        return { status: { phase: "ready", hostVersion: "0.1.0", protocol: "1.0", retention: "metadata" } };
      case "mm:settings":
        return state.settings ? { ok: true, settings: clone(state.settings) } : { ok: false, error: "host unreachable" };
      case "mm:reviewQueue":
        return { items: clone(state.reviewQueue) };
      // First-run backfill: the dashboard reads status to render the chip and starts/steers a run.
      // Tests seed `backfill` to assert each rendered state.
      case "mm:backfillStatus":
        return { ok: true, ...(state.backfill || { running: false, done_at: null }) };
      case "mm:triageExisting":
        state.backfill = { running: true, paused: false, total: 0, done: 0 };
        return { ok: true, started: true };
      case "mm:backfillControl":
        if (state.backfill) {
          if (message.action === "pause") state.backfill.paused = true;
          if (message.action === "resume") state.backfill.paused = false;
          if (message.action === "cancel") state.backfill = { running: false, done_at: 1 };
        }
        return { ok: true };
      case "mm:listProposals":
        return { ok: true, pending_reviews: clone(state.proposals) };
      case "mm:listRules":
        return { ok: true, rules: clone(state.rules) };
      case "mm:setRuleStatus": {
        const r = state.rules.find((x) => x.rule_id === message.ruleId);
        if (r) r.status = message.status;
        return { ok: true, rule_id: message.ruleId, status: message.status };
      }
      case "mm:listFollowups":
        return { ok: true, followups: clone(state.followups) };
      case "mm:followupReschedule":
      case "mm:followupReview":
      case "mm:followupStage":
      case "mm:followupCancel":
        return { ok: true };
      case "mm:reconnect":
        return { status: { phase: "ready", hostVersion: "0.1.0", protocol: "1.0", retention: "metadata" } };
      case "mm:reviewProposal":
        return { ok: true, proposal_id: message.proposalId, decision: message.decision };
      case "mm:listActivity":
        return { ok: true, events: clone(state.activity) };
      case "mm:listModels":
        return { ok: true, models: clone(state.models) };
      case "mm:setPause":
        if (state.settings) state.settings.paused = Boolean(message.paused);
        return { ok: true, paused: Boolean(message.paused) };
      case "mm:setProvider": {
        // Mirror the host: upsert the provider, optionally make it the default, return the fresh
        // snapshot with persisted:true (the behaviour the config-persistence fix guarantees).
        const s = (state.settings ??= { providers: [], default_provider: null, paused: false });
        s.providers ??= [];
        let p = s.providers.find((x) => x.id === message.providerId);
        if (!p) {
          p = { id: message.providerId, kind: message.kind, endpoint: message.endpoint, model: message.model, configured: false };
          s.providers.push(p);
        } else {
          if (message.kind !== undefined) p.kind = message.kind;
          if (message.endpoint !== undefined) p.endpoint = message.endpoint;
          if (message.model !== undefined) p.model = message.model;
        }
        if (message.setDefault) s.default_provider = message.providerId;
        return { ok: true, settings: clone(s), persisted: true };
      }
      case "mm:setSecret": {
        const p = state.settings?.providers?.find((x) => x.id === message.providerId);
        if (p) p.configured = true;
        return { ok: true, configured: true, provider_id: message.providerId, stored: true };
      }
      // Review-queue actions the keyboard-triage path drives (apply a suggestion, dismiss a card,
      // resolve a decision). The host audits each; the mock just acknowledges so the UI advances.
      case "mm:apply":
      case "mm:dismiss":
      case "mm:resolveReview":
        return { ok: true };
      default:
        return { ok: false, error: `unhandled ${t}` };
    }
  }

  const browser = {
    runtime: {
      sendMessage,
      openOptionsPage: async () => { state.calls.push({ type: "openOptionsPage" }); },
      onMessage: { addListener: (fn) => state.listeners.push(fn), removeListener: () => {} },
    },
    storage: {
      local: {
        get: async (key) => (typeof key === "string" ? { [key]: state.local[key] } : { ...state.local }),
        set: async (obj) => { Object.assign(state.local, obj); },
      },
      session: {
        get: async (key) => (typeof key === "string" ? { [key]: state.session[key] } : { ...state.session }),
        set: async (obj) => { Object.assign(state.session, obj); },
        remove: async (key) => { delete state.session[key]; },
      },
    },
    mailTabs: { query: async () => [], create: async () => ({ id: 1 }), setSelectedMessages: async () => {} },
    tabs: { update: async () => {} },
    // The i18n seam: a seeded `messages` map flips a key to a localized string; an unseeded key
    // returns "" so the UI's English fallback shows (matching browser.i18n's "missing → empty").
    i18n: { getMessage: (key) => (Object.prototype.hasOwnProperty.call(messages, key) ? messages[key] : "") },
  };

  return { browser, state };
}

// Load the real dashboard into a fresh jsdom, with the mock installed BEFORE the script runs.
// Returns { window, document, state } once boot() has settled (or the timeout elapses).
export async function loadDashboard(opts = {}) {
  let html = readFileSync(join(extDir, "dashboard.html"), "utf8");
  // Drop the external <script src> (we inject the source inline so the mock is in place first) and
  // the CSP meta (jsdom doesn't enforce CSP, but removing it avoids any inline-script ambiguity).
  html = html
    .replace(/<script src="dashboard\.js"><\/script>/, "")
    .replace(/<meta[^>]*Content-Security-Policy[^>]*>/s, "");
  const js = readSource("dashboard.js");

  const dom = new JSDOM(html, { runScripts: "dangerously", pretendToBeVisual: true });
  const { browser, state } = makeBrowserMock(opts);
  dom.window.browser = browser;

  // Append the dashboard source as an inline classic script: it runs synchronously in global
  // scope (top-level boot() fires), exactly as the browser would run dashboard.js.
  // Run the source as its own vm.Script in the realm (filename via the `//# sourceURL` readSource
  // appended) so c8/V8 attributes line coverage — including top-level statements — to the real
  // file. An inline <script> only attributes nested function calls, understating every script.
  runInRealm(dom, js);

  await tick(dom.window, 8); // let boot()'s async chain settle
  return { window: dom.window, document: dom.window.document, state, dom };
}

// Advance microtasks + jsdom timers a few rounds.
export async function tick(window, rounds = 4) {
  for (let i = 0; i < rounds; i++) {
    await new Promise((r) => window.setTimeout(r, 0));
  }
}

// Poll `predicate` until truthy or `timeoutMs` elapses; returns the last value (throws on timeout).
export async function waitFor(window, predicate, timeoutMs = 1000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = predicate();
    if (v) return v;
    if (Date.now() > deadline) throw new Error("waitFor: timed out");
    await new Promise((r) => window.setTimeout(r, 5));
  }
}

// --- compose-panel harness ------------------------------------------------------------------
//
// The composeAction review panel (compose.html + compose.js) is a separate page from the
// dashboard, with its own background verbs (`mm:composeContext`, `mm:regenerateDraft`). This mock
// answers them with the SAME shapes the background returns, so the real render/click code runs.

// A default regenerate: echo a fresh draft whose rationale reflects the steer and whose typed
// guard is cleared, so a test can assert the panel repainted from the regenerate round-trip. A
// custom `regenerate(message, state)` in opts overrides this.
function defaultRegen(message, state) {
  const base = state.draft || {};
  const tag = (message.adjustments || []).join(",") || message.steer || "regenerated";
  return { draft: { ...base, rationale: `Regenerated (${tag})`, commitments: { findings: [] } } };
}

export function makeComposeMock({ draft = null, providerConfigured = null, provider = null, regenerate } = {}) {
  const state = {
    draft: draft ? clone(draft) : null,
    providerConfigured,
    provider: provider ? clone(provider) : null,
    calls: [],
  };

  async function sendMessage(message) {
    state.calls.push(clone(message));
    const t = message && message.type;
    if (t === "mm:composeContext") {
      return { ok: true, draft: clone(state.draft), providerConfigured: state.providerConfigured, provider: clone(state.provider) };
    }
    if (t === "mm:regenerateDraft") {
      const next = (regenerate ? regenerate(message, state) : defaultRegen(message, state)) || {};
      if (next.draft) state.draft = clone(next.draft);
      if (next.fail) return { ok: false, error: next.fail };
      return { ok: true, draft: clone(state.draft), providerConfigured: state.providerConfigured, provider: clone(state.provider) };
    }
    return { ok: false, error: `unhandled ${t}` };
  }

  const browser = {
    runtime: {
      sendMessage,
      openOptionsPage: async () => { state.calls.push({ type: "openOptionsPage" }); },
    },
    tabs: { query: async () => [{ id: 7 }] },
  };
  return { browser, state };
}

// --- toolbar mini-hub (action popup) harness ------------------------------------------------
//
// The toolbar `action` popup (action.html + action.js) reads the HostStatus + the work aggregate
// from the background and renders connection health, the "what needs me" breakdown, Open dashboard,
// and the Pause toggle. This mock answers its verbs with the production shapes.

export function makeActionMock({ status, aggregate, paused } = {}) {
  const state = {
    status: status ? clone(status) : { phase: "ready", hostVersion: "0.1.0", protocol: "1.0", retention: "metadata" },
    aggregate: aggregate ? clone(aggregate) : null, // { reviews, attention, proposals, total? }
    paused: Boolean(paused),
    calls: [],
    listeners: [],
  };

  async function sendMessage(message) {
    state.calls.push(clone(message));
    const t = message && message.type;
    switch (t) {
      case "mm:getStatus":
        return { status: clone(state.status) };
      case "mm:reconnect":
        state.status = { ...state.status, phase: "ready" };
        return { status: clone(state.status) };
      case "mm:aggregate": {
        const a = state.aggregate || { reviews: 0, attention: 0, proposals: 0, total: 0 };
        const total = a.total != null ? a.total : (a.reviews || 0) + (a.attention || 0) + (a.proposals || 0);
        return { ok: true, reviews: a.reviews || 0, attention: a.attention || 0, proposals: a.proposals || 0, total, paused: state.paused };
      }
      case "mm:setPause":
        state.paused = Boolean(message.paused);
        return { ok: true, paused: state.paused };
      case "mm:openDashboard":
        return { ok: true };
      default:
        return { ok: false, error: `unhandled ${t}` };
    }
  }

  const browser = {
    runtime: {
      sendMessage,
      onMessage: { addListener: (fn) => state.listeners.push(fn), removeListener: () => {} },
    },
    storage: { session: { set: async () => {}, get: async () => ({}) } },
  };
  return { browser, state };
}

// --- options-page harness -------------------------------------------------------------------
//
// The options page reads the host snapshot (mm:settings) and writes notification preferences to
// storage.local. This mock answers mm:settings with a minimal snapshot and backs storage.local so
// a test can assert the exact `mm:notifPrefs` shape the options UI writes (and notifications.js reads).

export function makeOptionsMock({ settings, local, accounts, testProviderReply } = {}) {
  // The default snapshot mirrors the host's get_settings: scalar prefs + the starter category
  // vocabulary + the (sparse) Phase-5 triage maps. A passed `settings` is MERGED over this so a
  // test can vary one field (e.g. add a provider) without re-spelling the whole snapshot.
  const defaults = {
    retention_level: "metadata",
    body_consent: true,
    providers: [],
    default_provider: null,
    paused: false,
    catch_up_on_launch: true,
    follow_up_tick_seconds: 0,
    database: "(memory)",
    categories: [
      { key: "personal", label: "Personal" },
      { key: "work", label: "Work" },
      { key: "newsletters", label: "Newsletters" },
      { key: "promotions", label: "Promotions" },
      { key: "receipts", label: "Receipts" },
    ],
    category_policies: {},
    account_scopes: {},
    tag_mappings: {},
  };
  const state = {
    settings: { ...defaults, ...(settings ? clone(settings) : {}) },
    local: local ? clone(local) : {},
    accounts: accounts
      ? clone(accounts)
      : [
          { id: "acct_work", name: "Work (IMAP)", type: "imap" },
          { id: "acct_personal", name: "Personal (IMAP)", type: "imap" },
        ],
    // Default to a reachable provider with a small catalog, override per test.
    testProviderReply: testProviderReply || { ok: true, reachable: true, model_count: 2 },
    calls: [],
  };

  async function sendMessage(message) {
    state.calls.push(clone(message));
    const t = message && message.type;
    const s = state.settings;
    if (t === "mm:settings") return { ok: true, settings: clone(s) };
    if (t === "mm:setPause") {
      s.paused = Boolean(message.paused);
      return { ok: true };
    }
    if (t === "mm:setCategoryPolicy") {
      if (message.policy === "auto") delete s.category_policies[message.category];
      else s.category_policies[message.category] = message.policy;
      return { ok: true };
    }
    if (t === "mm:setAccountScope") {
      if (message.enabled) delete s.account_scopes[message.accountId];
      else s.account_scopes[message.accountId] = false;
      return { ok: true };
    }
    if (t === "mm:setTagMapping") {
      const cat = (message.category || "").trim().toLowerCase();
      if (cat) s.tag_mappings[message.tag] = cat;
      else delete s.tag_mappings[message.tag];
      return { ok: true };
    }
    if (t === "mm:testProvider") return clone(state.testProviderReply);
    return { ok: true };
  }

  const browser = {
    runtime: { sendMessage, openOptionsPage: async () => {} },
    accounts: { list: async () => clone(state.accounts) },
    storage: {
      local: {
        get: async (key) => (typeof key === "string" ? { [key]: clone(state.local[key]) } : clone(state.local)),
        set: async (obj) => { Object.assign(state.local, clone(obj)); },
      },
    },
  };
  return { browser, state };
}

export async function loadOptions(opts = {}) {
  let html = readFileSync(join(extDir, "options.html"), "utf8");
  html = html
    .replace(/<script src="options\.js"><\/script>/, "")
    .replace(/<meta[^>]*Content-Security-Policy[^>]*>/s, "");
  const js = readSource("options.js");

  const dom = new JSDOM(html, { runScripts: "dangerously" });
  dom.window.close = () => {};
  const { browser, state } = makeOptionsMock(opts);
  dom.window.browser = browser;

  // Run the source as its own vm.Script in the realm (filename via the `//# sourceURL` readSource
  // appended) so c8/V8 attributes line coverage — including top-level statements — to the real
  // file. An inline <script> only attributes nested function calls, understating every script.
  runInRealm(dom, js);

  await tick(dom.window, 8);
  return { window: dom.window, document: dom.window.document, state, dom };
}

// --- notifications harness ------------------------------------------------------------------
//
// notifications.js is a background script (no DOM): it owns the browser.notifications surface and
// the dedup/quiet-hours/batching policies. This mock records every notifications.create and backs
// storage.local so a "suspension" can be simulated by reloading the script with the same store.

export function makeNotifMock({ local } = {}) {
  const state = {
    created: [], // every browser.notifications.create({...}) payload
    local: local ? clone(local) : {}, // storage.local (carry across a simulated suspension)
    listeners: {},
  };
  let n = 0;
  const browser = {
    notifications: {
      create: async (opts) => {
        const id = `n${++n}`;
        state.created.push({ id, ...opts });
        return id;
      },
      clear: async () => {},
      onClicked: { addListener: (fn) => { state.listeners.clicked = fn; } },
      onClosed: { addListener: (fn) => { state.listeners.closed = fn; } },
    },
    storage: {
      local: {
        get: async (key) => (typeof key === "string" ? { [key]: clone(state.local[key]) } : clone(state.local)),
        set: async (obj) => { Object.assign(state.local, clone(obj)); },
      },
      session: { get: async () => ({}), set: async () => {} },
    },
    runtime: { getURL: (p) => p, sendMessage: async () => {} },
    spaces: { open: async () => {} },
    tabs: { create: async () => {} },
  };
  return { browser, state };
}

// Load notifications.js into a fresh realm with a mocked browser. Returns the window (its
// top-level functions are global properties) + the recording state.
export async function loadNotifications(opts = {}) {
  const js = readSource("notifications.js");
  const dom = new JSDOM("<!DOCTYPE html><html><body></body></html>", { runScripts: "dangerously" });
  dom.window.close = () => {};
  const { browser, state } = makeNotifMock(opts);
  dom.window.browser = browser;

  // Run the source as its own vm.Script in the realm (filename via the `//# sourceURL` readSource
  // appended) so c8/V8 attributes line coverage — including top-level statements — to the real
  // file. An inline <script> only attributes nested function calls, understating every script.
  runInRealm(dom, js);

  await tick(dom.window, 4);
  return { window: dom.window, state, dom };
}

export async function loadAction(opts = {}) {
  let html = readFileSync(join(extDir, "action.html"), "utf8");
  html = html
    .replace(/<script src="action\.js"><\/script>/, "")
    .replace(/<meta[^>]*Content-Security-Policy[^>]*>/s, "");
  const js = readSource("action.js");

  // No pretendToBeVisual: action.js uses no rAF, so jsdom starts no timer that would keep node
  // alive — the test exits without a teardown. And neutralize window.close(): the popup calls it
  // (correct in production) but in jsdom a real close stops timers, which would hang a later tick().
  const dom = new JSDOM(html, { runScripts: "dangerously" });
  dom.window.close = () => {};
  const { browser, state } = makeActionMock(opts);
  dom.window.browser = browser;

  // Run the source as its own vm.Script in the realm (filename via the `//# sourceURL` readSource
  // appended) so c8/V8 attributes line coverage — including top-level statements — to the real
  // file. An inline <script> only attributes nested function calls, understating every script.
  runInRealm(dom, js);

  await tick(dom.window, 8);
  return { window: dom.window, document: dom.window.document, state, dom };
}

// Load the real compose panel into a fresh jsdom, with the mock installed BEFORE the script runs.
export async function loadCompose(opts = {}) {
  let html = readFileSync(join(extDir, "compose.html"), "utf8");
  html = html
    .replace(/<script src="compose\.js"><\/script>/, "")
    .replace(/<meta[^>]*Content-Security-Policy[^>]*>/s, "");
  const js = readSource("compose.js");

  const dom = new JSDOM(html, { runScripts: "dangerously", pretendToBeVisual: true });
  const { browser, state } = makeComposeMock(opts);
  dom.window.browser = browser;

  // Run the source as its own vm.Script in the realm (filename via the `//# sourceURL` readSource
  // appended) so c8/V8 attributes line coverage — including top-level statements — to the real
  // file. An inline <script> only attributes nested function calls, understating every script.
  runInRealm(dom, js);

  await tick(dom.window, 8); // let boot()'s async chain settle
  return { window: dom.window, document: dom.window.document, state, dom };
}
