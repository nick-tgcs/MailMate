// panel-harness.mjs — load the REAL per-message panel into jsdom with a mocked host.
//
// The panel owns no native port: it drives the background's `mm:*` router over
// browser.runtime.sendMessage. So a faithful UI test stands up a `browser` mock that returns the
// SAME shapes the background does (the classify payload shape is verified against the live binary
// by the Rust framed-stdin probes), loads the actual panel.html + panel.js, and asserts what
// renders / what the buttons send. No real Thunderbird, no network.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";
import { JSDOM } from "jsdom";

const here = dirname(fileURLToPath(import.meta.url));
const extDir = join(here, "..");

// Tag the eval'd source with a `//# sourceURL` so V8/c8 attributes coverage (and stack traces) to
// the real file on disk. Invisible to execution. See harness.mjs readSource for the rationale.
function readSource(name) {
  return `${readFileSync(join(extDir, name), "utf8")}\n//# sourceURL=${join(extDir, name)}\n`;
}

const clone = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

// A default benign classify result the panel can render. Tests override pieces of it.
export function classifyResult(overrides = {}) {
  return {
    ok: true,
    bodyRetentionAllowed: false,
    result: {
      decision_id: "dec_1",
      thunderbird_message_id: "42",
      classification: {
        labels: ["newsletter"],
        spam_score: 0.1,
        phishing_score: 0.0,
        priority: "normal",
        needs_review: false,
        confidence: 0.9,
        confidence_band: "high",
        salient_signals: [],
        safety_findings: [],
        ...(overrides.classification || {}),
      },
      suggested_actions: overrides.suggested_actions || [],
      blocked_actions: overrides.blocked_actions || [],
      explanation: overrides.explanation || { summary: "0 allowed, 0 need review, 0 blocked.", labels: [], policy_checks: [] },
      ...(overrides.unsubscribe !== undefined ? { unsubscribe: overrides.unsubscribe } : {}),
    },
  };
}

export function makePanelBrowser({ classify, settings, status, noMessage = false } = {}) {
  const state = {
    calls: [],
    listeners: [],
    classify: classify || classifyResult(),
    settings: settings === undefined ? { categories: [{ key: "newsletters", label: "Newsletters" }, { key: "receipts", label: "Receipts" }] } : settings,
    status: status || { phase: "ready", hostVersion: "0.1.0", protocol: "1.0", retention: "metadata" },
  };

  async function sendMessage(message) {
    state.calls.push(clone(message));
    switch (message && message.type) {
      case "mm:getStatus":
        return { status: clone(state.status) };
      case "mm:settings":
        return state.settings ? { ok: true, settings: clone(state.settings) } : { ok: false, error: "unreachable" };
      case "mm:classify":
        return clone(state.classify);
      case "mm:folders":
        return { folders: [{ accountId: "acct", path: "/Archive", name: "Archive", accountName: "Acct" }] };
      // Every action verb succeeds by default; the test inspects `state.calls`.
      case "mm:signalWrong":
      case "mm:unsubscribe":
      case "mm:junk":
      case "mm:markRead":
      case "mm:draftReply":
      case "mm:move":
      case "mm:notJunk":
      case "mm:correctLabel":
      case "mm:apply":
      case "mm:dismiss":
      case "mm:undo":
      case "mm:openDashboard":
        return { ok: true, method: message.type === "mm:unsubscribe" ? "compose" : undefined };
      default:
        return { ok: false, error: `unhandled ${message && message.type}` };
    }
  }

  const browser = {
    runtime: {
      sendMessage,
      onMessage: { addListener: (fn) => state.listeners.push(fn), removeListener: () => {} },
    },
    tabs: { query: async () => [{ id: 1 }] },
    messageDisplay: { getDisplayedMessage: async () => (noMessage ? null : { id: 42, subject: "Weekly digest" }) },
    mailTabs: { getSelectedMessages: async () => ({ messages: [] }) },
  };
  return { browser, state };
}

export async function loadPanel(opts = {}) {
  let html = readFileSync(join(extDir, "panel.html"), "utf8");
  html = html
    .replace(/<script src="panel\.js"><\/script>/, "")
    .replace(/<meta[^>]*Content-Security-Policy[^>]*>/s, "");
  const js = readSource("panel.js");

  const dom = new JSDOM(html, { runScripts: "dangerously", pretendToBeVisual: true });
  const { browser, state } = makePanelBrowser(opts);
  dom.window.browser = browser;

  // vm.Script in-realm with the real path as `filename` (NOT the `//# sourceURL` comment, which
  // attributes only nested function calls) so c8 measures top-level statements of panel.js too.
  const srcPath = join(extDir, "panel.js");
  const src = js.replace(/\n\/\/# sourceURL=.+\n?$/, "\n");
  new vm.Script(src, { filename: srcPath }).runInContext(dom.getInternalVMContext());

  await tick(dom.window, 10); // let boot()'s async chain settle (status → categories → classify)
  return { window: dom.window, document: dom.window.document, state, dom };
}

export async function tick(window, rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await new Promise((r) => window.setTimeout(r, 0));
  }
}
