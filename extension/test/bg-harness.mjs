// bg-harness.mjs — load the REAL background/event-page scripts headlessly.
//
// background.js, native.js, drafts.js, followups.js, context_menu.js and message_reader.js are
// classic (non-module) scripts the manifest concatenates into one event-page realm: top-level
// `function`s land on the global object, but top-level `const`/`let`/`class` (e.g. NativeHost,
// MAILMATE_MENU) live in the shared global *lexical* scope — visible to sibling scripts in the
// same realm but NOT as window properties, and NOT preserved across separate vm.runInContext
// calls. So we load them as sibling <script> tags in ONE jsdom realm (matching the browser), then
// inject a final synthetic script that lifts each file's `/* exported … */` names onto
// window.__exports. A rich mocked `browser` is installed BEFORE any script runs; its addListener
// shims capture the registered handlers so a test can fire onMessage/onNewMailReceived/etc., and
// runtime.connectNative returns a controllable fake native port.
//
// Each source is tagged with a `//# sourceURL` (via readSource) so c8/V8 attributes line coverage
// to the real file on disk. dispose() closes the jsdom window, stopping the heartbeat/timers so
// the test process exits.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";
import { JSDOM } from "jsdom";

const here = dirname(fileURLToPath(import.meta.url));
const extDir = join(here, "..");

function readSource(name) {
  return `${readFileSync(join(extDir, name), "utf8")}\n//# sourceURL=${join(extDir, name)}\n`;
}

// Pull the public surface a file documents via its `/* exported a, b, c */` annotation (the block
// may span lines, as in drafts.js/followups.js).
function exportedNames(src) {
  const names = new Set();
  for (const m of src.matchAll(/\/\*\s*exported\s+([\s\S]*?)\*\//g)) {
    for (const raw of m[1].split(/[\s,]+/)) {
      const n = raw.trim();
      if (n) names.add(n);
    }
  }
  return [...names];
}

// A controllable fake of the long-lived native port browser.runtime.connectNative returns.
// Tests push host frames via emit()/emitNotification() and observe outbound frames in `sent`.
function makeNativePort(state) {
  const messageListeners = [];
  const disconnectListeners = [];
  const port = {
    error: null,
    sent: state.sent,
    postMessage(frame) {
      state.sent.push(frame);
      if (state.onPost) state.onPost(frame, port);
    },
    // Calling disconnect() yourself does NOT fire your own onDisconnect listener in the real
    // WebExtension API — only the other end dropping does (emitDisconnect below simulates that).
    disconnect() {
      state.disconnected = true;
    },
    onMessage: { addListener: (fn) => messageListeners.push(fn) },
    onDisconnect: { addListener: (fn) => disconnectListeners.push(fn) },
    // Test controls:
    emit: (message) => messageListeners.forEach((l) => l(message)),
    emitDisconnect: (error) => {
      port.error = error || null;
      disconnectListeners.forEach((l) => l(port));
    },
  };
  return port;
}

// Capture-style event hub: `addListener` records the handler under `path` so a test can fire it.
function listenerSlot(state, path) {
  state.listeners[path] = state.listeners[path] || [];
  return {
    addListener: (fn) => state.listeners[path].push(fn),
    removeListener: () => {},
    hasListener: () => false,
  };
}

// A real in-memory storage.local/session area: get(key|keys|null)/set(obj)/remove(key) backed by a
// plain object, so code that writes then reads back (the review buffer, backfill resume) works.
function kvStore(state, area) {
  const data = (state.store = state.store || {});
  data[area] = data[area] || {};
  const store = data[area];
  return {
    get: async (key) => {
      if (key == null) return { ...store };
      if (typeof key === "string") return key in store ? { [key]: store[key] } : {};
      const out = {};
      for (const k of key) if (k in store) out[k] = store[k];
      return out;
    },
    set: async (obj) => {
      Object.assign(store, obj);
    },
    remove: async (key) => {
      for (const k of [].concat(key)) delete store[k];
    },
  };
}

// Record every method call into state.calls and return the opts-provided result (or a default).
function recorder(state, path, deflt) {
  return async (...args) => {
    state.calls.push({ path, args });
    const override = state.responses[path];
    if (typeof override === "function") return override(...args);
    if (override !== undefined) return override;
    return typeof deflt === "function" ? deflt(...args) : deflt;
  };
}

// A broad WebExtension `browser` mock covering the surface the background scripts touch. Anything
// a specific test needs to assert/override is reachable via state (calls/listeners/responses).
export function makeHostBrowserMock(opts = {}) {
  const state = {
    sent: [], // outbound native frames
    calls: [], // { path, args } for every recorded API call
    listeners: {}, // path -> [handler] captured from addListener
    responses: opts.responses || {}, // path -> value|fn override for recorder()s
    disconnected: false,
    onPost: opts.onPost || null, // (frame, port) => void, e.g. to auto-answer requests
    notifications: [],
    badges: [],
  };
  const port = makeNativePort(state);
  state.port = port;

  const browser = {
    runtime: {
      connectNative: () => port,
      getURL: (p) => p,
      onMessage: listenerSlot(state, "runtime.onMessage"),
      onConnect: listenerSlot(state, "runtime.onConnect"),
      sendMessage: recorder(state, "runtime.sendMessage", {}),
      lastError: null,
    },
    menus: {
      removeAll: recorder(state, "menus.removeAll"),
      create: (obj) => state.calls.push({ path: "menus.create", args: [obj] }),
      onClicked: listenerSlot(state, "menus.onClicked"),
    },
    messages: {
      get: recorder(state, "messages.get", (id) => ({
        id,
        headerMessageId: `<mid-${id}@x>`,
        folder: { accountId: "acct1", path: "/Inbox" },
        tags: [],
      })),
      move: recorder(state, "messages.move", {}),
      update: recorder(state, "messages.update", {}),
      getFull: recorder(state, "messages.getFull", { headers: {}, parts: [] }),
      query: recorder(state, "messages.query", { messages: [] }),
      continueList: recorder(state, "messages.continueList", null),
      onNewMailReceived: listenerSlot(state, "messages.onNewMailReceived"),
      onMoved: listenerSlot(state, "messages.onMoved"),
      onUpdated: listenerSlot(state, "messages.onUpdated"),
    },
    messageDisplay: {
      onMessageDisplayed: listenerSlot(state, "messageDisplay.onMessageDisplayed"),
      getDisplayedMessage: recorder(state, "messageDisplay.getDisplayedMessage", null),
    },
    compose: {
      beginReply: recorder(state, "compose.beginReply", { id: 10 }),
      beginNew: recorder(state, "compose.beginNew", { id: 11 }),
      saveMessage: recorder(state, "compose.saveMessage", {}),
      getComposeDetails: recorder(state, "compose.getComposeDetails", {}),
      setComposeDetails: recorder(state, "compose.setComposeDetails", {}),
      onBeforeSend: listenerSlot(state, "compose.onBeforeSend"),
      onAfterSend: listenerSlot(state, "compose.onAfterSend"),
    },
    identities: {
      list: recorder(state, "identities.list", []),
    },
    folders: {
      query: recorder(state, "folders.query", []),
    },
    accounts: {
      list: recorder(state, "accounts.list", []),
    },
    windows: {
      openDefaultBrowser: recorder(state, "windows.openDefaultBrowser", {}),
    },
    messageDisplayAction: {
      setBadgeText: (o) => state.calls.push({ path: "messageDisplayAction.setBadgeText", args: [o] }),
      setBadgeBackgroundColor: (o) =>
        state.calls.push({ path: "messageDisplayAction.setBadgeBackgroundColor", args: [o] }),
    },
    tabs: {
      create: recorder(state, "tabs.create", { id: 99 }),
      onRemoved: listenerSlot(state, "tabs.onRemoved"),
      query: recorder(state, "tabs.query", []),
      update: recorder(state, "tabs.update", {}),
    },
    spaces: {
      create: recorder(state, "spaces.create", { id: 1 }),
      query: recorder(state, "spaces.query", []),
      update: recorder(state, "spaces.update", {}),
      open: recorder(state, "spaces.open", {}),
    },
    notifications: {
      create: (...args) => {
        state.notifications.push(args);
        state.calls.push({ path: "notifications.create", args });
        return Promise.resolve("notif-1");
      },
      onClicked: listenerSlot(state, "notifications.onClicked"),
      onClosed: listenerSlot(state, "notifications.onClosed"),
      clear: recorder(state, "notifications.clear", true),
    },
    action: {
      // These are awaited with `.catch(…)` in background.js, so they must return a promise.
      setBadgeText: (o) => {
        state.badges.push(o);
        return Promise.resolve();
      },
      setBadgeBackgroundColor: recorder(state, "action.setBadgeBackgroundColor", {}),
      setTitle: recorder(state, "action.setTitle", {}),
    },
    storage: {
      // local + session are backed by real in-memory stores so round-trips behave like the
      // browser (the review-queue buffer and backfill resume both read back what they wrote).
      local: kvStore(state, "local"),
      session: kvStore(state, "session"),
    },
    i18n: { getMessage: (k) => k },
    ...(opts.browserExtra || {}),
  };
  if (opts.mutateBrowser) opts.mutateBrowser(browser, state);
  return { browser, state, port };
}

// Load the named background scripts (in order) into one jsdom realm with the mocked browser.
// Returns { window, exports, state, port, browser, dispose }. `exports` holds each file's
// documented `/* exported … */` surface.
//
// Each file is run as its own vm.Script in the jsdom realm's context (dom.getInternalVMContext())
// with the real file path as `filename`. That is what makes c8/V8 attribute line coverage —
// including the top-level/script-scope statements — to the real file (an inline <script> only
// attributes nested function calls, silently understating every classic script). A short bridge
// appended to each file lifts its documented `const`/`class` surface onto globalThis, so later
// files (and the returned `exports`) can see bindings that, being lexical, don't cross vm.Script
// boundaries on their own (function declarations attach to globalThis already).
export function loadScripts(names, opts = {}) {
  const dom = new JSDOM("<!DOCTYPE html><html><body></body></html>", {
    runScripts: "dangerously",
    pretendToBeVisual: true,
  });
  const ctx = dom.getInternalVMContext();
  const { browser, state, port } = makeHostBrowserMock(opts);
  dom.window.browser = browser;
  // A recording global fetch (background.js's RFC-8058 one-click unsubscribe POSTs through it).
  dom.window.fetch = (...args) => {
    state.calls.push({ path: "fetch", args });
    if (opts.fetch) return opts.fetch(...args);
    return Promise.resolve({ ok: true, status: 200 });
  };
  if (opts.window) opts.window(dom.window, state);

  const allExports = new Set();
  for (const name of names) {
    const path = join(extDir, name);
    const src = readFileSync(path, "utf8");
    const names2 = exportedNames(src);
    names2.forEach((n) => allExports.add(n));
    const bridge = names2.map((n) => `try{globalThis.${n}=${n};}catch(e){}`).join("");
    new vm.Script(`${src}\n;${bridge}`, { filename: path }).runInContext(ctx);
  }

  const exports = {};
  for (const n of allExports) exports[n] = dom.window[n];

  return {
    window: dom.window,
    exports,
    state,
    port,
    browser,
    dispose: () => {
      try {
        dom.window.close();
      } catch {
        /* already closed */
      }
    },
  };
}

// Drain microtasks + jsdom timers a few rounds (mirrors harness.mjs tick).
export async function tick(window, rounds = 4) {
  for (let i = 0; i < rounds; i++) {
    await new Promise((r) => window.setTimeout(r, 0));
  }
}

export { readSource };
