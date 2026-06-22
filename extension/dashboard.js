// dashboard.js — the MailMate dashboard space (the product's home).
//
// A first-class Thunderbird `spaces` tab rendering a small SPA: Review / Follow-ups / Proposals
// / Activity. It owns NO native port — like the popups, it drives the host through the
// background's `mm:*` router over `browser.runtime` messaging, keeping the background the single
// single-writer owner of the stdout channel. Every cross-cutting "what needs me" lives here; the
// per-message panel and notifications are satellites that deep-link back into this tab.
//
// Honesty contract (matches the panel): send() never rejects — a dead/asleep background or a
// host that doesn't speak a verb yet resolves to { ok:false, error }, which the UI renders as a
// labeled state, never a blank or a lie. Surfaces that depend on a not-yet-shipped host endpoint
// (Pause, Settings, the live Follow-ups pipeline) degrade to an explicit message and light up
// unchanged when that endpoint lands.

"use strict";

// --- Constants ------------------------------------------------------------------------

const PHASE = {
  ready: "ready",
  connecting: "connecting",
  disconnected: "disconnected",
  versionMismatch: "version_mismatch",
};

// Locally-applyable action kinds (the panel's APPLYABLE_KINDS). require_review / create_draft
// are informational markers, never an Approve button.
const APPLYABLE_KINDS = new Set(["tag", "move", "mark_junk", "mark_read", "flag"]);

const ACTIVITY_FILTERS = [
  ["all", "All"],
  ["applied", "Applied"],
  ["blocked", "Blocked"],
  ["corrected", "Corrected"],
  ["follow_up", "Follow-ups"],
  ["proposal", "Proposals"],
];

const DEFAULT_CATEGORIES = [
  "Important",
  "Personal",
  "Work",
  "Receipts",
  "Finance",
  "Newsletters",
  "Promotions",
  "Social",
  "Travel",
];

// Inline provider setup (onboarding step 3) — the few bits of options.js's provider machinery the
// walkthrough needs, kept here so setup is self-contained (no eject to the preferences tab). Each
// kind's default endpoint pre-fills a working URL; loopback endpoints auto-list their model catalog
// (hitting localhost is not egress) so the Model field becomes a dropdown without a button.
const OB_KIND_DEFAULTS = {
  ollama: "http://localhost:11434",
  lm_studio: "http://localhost:1234/v1",
  llama_cpp: "http://localhost:8080",
  openai_compatible: "https://api.openai.com/v1",
};
const OB_PROVIDER_KINDS = Object.keys(OB_KIND_DEFAULTS);
const OB_KIND_DEFAULT_VALUES = new Set(Object.values(OB_KIND_DEFAULTS));
const obModelCache = new Map();
const obProbeKey = (p) => `${p.kind}|${p.endpoint}`;
const obIsLocal = (url) =>
  /^https?:\/\/(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d+)?(?:\/|$)/i.test((url || "").trim());

// --- State ----------------------------------------------------------------------------

let activeTab = "review";
let activityFilter = "all";
let lastPhase = null;
let settings = null; // last get_settings snapshot (best-effort)

const $ = (id) => document.getElementById(id);
const content = () => $("mm-content");

// --- Messaging (never rejects) --------------------------------------------------------

async function send(message) {
  try {
    const reply = await browser.runtime.sendMessage(message);
    return reply || { ok: false, error: "no response from background" };
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
}

function toast(text, isError) {
  const el = $("mm-toast");
  el.textContent = text;
  el.classList.toggle("mm-toast--err", Boolean(isError));
  el.hidden = false;
  clearTimeout(toast._t);
  toast._t = setTimeout(() => {
    el.hidden = true;
  }, 2600);
}

function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.title) node.title = opts.title;
  if (opts.attrs) for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

// i18n seam: resolve a message key through browser.i18n when present, else the English fallback.
// English-only v1, but the seam exists so a future locale ships `_locales/<lang>/messages.json`
// with no code change. Mirrors panel.js's helper; degrades safely in jsdom/tests where
// browser.i18n is absent or a key is unset (the fallback is the live English copy).
function t(key, fallback) {
  try {
    if (typeof browser !== "undefined" && browser.i18n && browser.i18n.getMessage) {
      const m = browser.i18n.getMessage(key);
      if (m) return m;
    }
  } catch (e) {
    // fall through to the fallback
  }
  return fallback;
}

// --- Boot -----------------------------------------------------------------------------

async function boot() {
  // Wire the (single) header + tab listeners exactly once, before the onboarding branch, so
  // finishing onboarding can enter the app without re-binding them.
  wireHeader();
  wireTabs();

  if (!(await isOnboarded())) {
    renderOnboarding(0);
    return;
  }
  await enterApp();
}

// Paint the dashboard proper: connection banner, settings-derived header chips, the active tab.
// Re-entrant (called from boot and from finishOnboarding) and binds no listeners itself.
async function enterApp() {
  $("mm-onboarding").hidden = true;
  $("mm-app").hidden = false;
  // A notification click can request a tab durably via storage.session, which survives a cold
  // space-open that races the fire-and-forget focusTab message. Consume it once before painting.
  const wanted = await consumeFocusTab();
  if (wanted) setActiveTabSelection(wanted);
  const reply = await send({ type: "mm:getStatus" });
  const status = reply.status || { phase: PHASE.disconnected, reason: reply.error };
  lastPhase = status.phase;
  renderBanner(status);
  await refreshSettings();
  await renderTab(activeTab);
  // Prime the secondary tab badges so a startup with pending work shows the count before the
  // user opens those tabs (the active tab's own render already set its count).
  if (activeTab !== "proposals") refreshProposalCount();
  if (activeTab !== "followups") refreshFollowupCount();
}

const FOCUS_TAB_KEY = "mm:focusTab";
const VALID_TABS = ["review", "followups", "proposals", "rules", "activity"];

// Read + clear the deep-link tab a notification click stashed, returning a valid tab or null.
async function consumeFocusTab() {
  try {
    const got = await browser.storage.session.get(FOCUS_TAB_KEY);
    const tab = got[FOCUS_TAB_KEY];
    if (tab !== undefined) await browser.storage.session.remove(FOCUS_TAB_KEY);
    return VALID_TABS.includes(tab) ? tab : null;
  } catch {
    return null;
  }
}

// Move the tab-bar selection to `name` without rendering (used before the first paint).
function setActiveTabSelection(name) {
  activeTab = name;
  for (const t of document.querySelectorAll(".mm-tab")) {
    t.setAttribute("aria-selected", String(t.dataset.tab === name));
  }
}

async function isOnboarded() {
  try {
    const got = await browser.storage.local.get("mm:onboarded");
    return Boolean(got["mm:onboarded"]);
  } catch {
    // No storage → treat as onboarded rather than trapping the user behind a broken walkthrough.
    return true;
  }
}

// --- Connection banner ----------------------------------------------------------------

function renderBanner(status) {
  const banner = $("mm-banner");
  if (status.phase === PHASE.ready) {
    banner.hidden = true;
    banner.className = "mm-banner";
    return;
  }
  banner.hidden = false;
  clear(banner);
  const mismatch = status.phase === PHASE.versionMismatch;
  banner.className = mismatch ? "mm-banner mm-banner--mismatch" : "mm-banner";

  const title =
    mismatch
      ? "MailMate and its helper don't match"
      : status.phase === PHASE.connecting
        ? "Connecting to MailMate's helper…"
        : "MailMate is not connected to its assistant";
  banner.appendChild(
    el("div", { class: "mm-banner__title" }, [el("span", { text: mismatch ? "⚠" : "●" }), el("span", { text: title })]),
  );

  const body = mismatch
    ? "The extension and the background helper disagree on protocol version. Update whichever is older — MailMate won't speak a version it can't trust. Your mail is unaffected."
    : "The MailMate background helper (native host) isn't responding, so new mail won't be classified and no suggestions will appear. Your mail is unaffected.";
  banner.appendChild(el("p", { class: "mm-banner__body", text: body }));

  const lastSeen = status.lastPongAt
    ? `Last seen: ${relativeTime(status.lastPongAt)}`
    : "Last seen: never this session";
  const meta = el("div", { class: "mm-banner__meta", text: lastSeen });
  if (status.reason) {
    meta.appendChild(el("div", { class: "mm-banner__reason", text: `Reason: ${status.reason}` }));
  }
  banner.appendChild(meta);

  const actions = el("div", { class: "mm-banner__actions" });
  const retry = el("button", { class: "mm-btn mm-btn--primary", text: "↻ Retry" });
  retry.addEventListener("click", reconnect);
  const dismiss = el("button", { class: "mm-btn", text: "Dismiss" });
  dismiss.addEventListener("click", () => {
    banner.hidden = true;
  });
  actions.appendChild(retry);
  actions.appendChild(dismiss);
  banner.appendChild(actions);
}

async function reconnect() {
  const reply = await send({ type: "mm:reconnect" });
  if (reply.status) {
    lastPhase = reply.status.phase;
    renderBanner(reply.status);
    if (reply.status.phase === PHASE.ready) {
      toast("Reconnected");
      await refreshSettings();
      await renderTab(activeTab);
    }
  }
}

// --- Header controls ------------------------------------------------------------------

function wireHeader() {
  $("mm-refresh").addEventListener("click", async () => {
    await refreshSettings();
    await renderTab(activeTab);
    toast("Refreshed");
  });
  // Re-open the first-run walkthrough on demand — onboarding is otherwise shown once and hidden for
  // good, leaving no way back to "the setup". Finishing it returns to the app via finishOnboarding.
  $("mm-help").addEventListener("click", () => renderOnboarding(0));
  $("mm-settings").addEventListener("click", async () => {
    try {
      await browser.runtime.openOptionsPage();
    } catch {
      toast("Couldn't open Settings — reload the add-on (about:debugging → Reload)", true);
    }
  });
  $("mm-pause").addEventListener("click", togglePause);
  $("mm-provider").addEventListener("click", async () => {
    try {
      await browser.runtime.openOptionsPage();
    } catch {
      toast("Couldn't open provider settings — reload the add-on (about:debugging → Reload)", true);
    }
  });
}

async function refreshSettings() {
  const reply = await send({ type: "mm:settings" });
  settings = reply.ok ? reply.settings : null;
  const state = $("mm-provider-state");
  const chip = $("mm-provider");
  if (settings && settings.default_provider) {
    state.textContent = settings.default_provider;
    chip.classList.remove("mm-ctl--warn");
    chip.title = "AI provider configured";
  } else {
    state.textContent = settings ? "none ⚠" : "unknown";
    chip.classList.add("mm-ctl--warn");
    chip.title = settings ? "No provider — drafting needs one" : "Settings unavailable";
  }
  // Reflect host-side pause state when the snapshot carries it (a Milestone-4 read addition).
  const paused = Boolean(settings && settings.paused);
  setPauseLabel(paused);
}

function setPauseLabel(paused) {
  $("mm-pause-label").textContent = paused ? "Resume auto-apply" : "Pause auto-apply";
  $("mm-pause").firstChild.textContent = paused ? "▶ " : "⏸ ";
  $("mm-pause").setAttribute("aria-pressed", String(paused));
}

async function togglePause() {
  const next = $("mm-pause").getAttribute("aria-pressed") !== "true";
  const reply = await send({ type: "mm:setPause", paused: next });
  if (!reply.ok) {
    toast(`Pause isn't available yet: ${reply.error || "host can't set pause"}`, true);
    return;
  }
  setPauseLabel(Boolean(reply.paused));
  toast(reply.paused ? "Auto-apply paused" : "Auto-apply resumed");
}

// --- Tabs -----------------------------------------------------------------------------

function wireTabs() {
  for (const tab of document.querySelectorAll(".mm-tab")) {
    tab.addEventListener("click", () => selectTab(tab.dataset.tab));
  }
}

async function selectTab(name) {
  activeTab = name;
  for (const tab of document.querySelectorAll(".mm-tab")) {
    tab.setAttribute("aria-selected", String(tab.dataset.tab === name));
  }
  await renderTab(name);
}

async function renderTab(name) {
  const c = content();
  clear(c);
  c.appendChild(el("p", { class: "mm-muted", text: "Loading…" }));
  if (name === "review") return renderReview();
  if (name === "activity") return renderActivity();
  if (name === "proposals") return renderProposals();
  if (name === "followups") return renderFollowups();
  if (name === "rules") return renderRules();
}

function setTabCount(name, count) {
  const badge = document.querySelector(`.mm-tabcount[data-count="${name}"]`);
  if (!badge) return;
  if (count > 0) {
    badge.textContent = String(count);
    badge.hidden = false;
    badge.setAttribute("data-filled", "1");
  } else {
    badge.hidden = true;
    badge.removeAttribute("data-filled");
  }
}

// --- Tab 1: Review queue --------------------------------------------------------------

async function renderReview() {
  const reply = await send({ type: "mm:reviewQueue" });
  const items = (reply.items || []).slice().reverse(); // newest first
  setTabCount("review", items.length);
  const c = content();
  clear(c);

  // The first-run backfill affordance / live progress sits above the queue, so a fresh install
  // can populate suggestions from existing mail in one tap.
  await renderBackfill(c);

  if (!items.length) {
    c.appendChild(
      emptyState("✓", t("dashInboxTriaged", "Inbox triaged — nothing waiting"), [
        "Crystallized rules file safely in the background.",
        "New suggestions appear here whenever MailMate isn't yet sure — correct them in one click and it learns.",
      ]),
    );
    return;
  }

  // The queue is a keyboard-navigable list: j/k or ↑/↓ move between cards, and the focused card
  // is triaged without the mouse (Enter/a = approve-all-safe, e = explain, x/Del = dismiss).
  const queue = el("div", {
    class: "mm-queue",
    attrs: { role: "list", "aria-label": "Review queue", "aria-keyshortcuts": "j k ArrowUp ArrowDown Enter e x" },
  });
  items.forEach((item, i) => queue.appendChild(reviewCard(item, i === 0)));
  wireQueueKeyboard(queue);
  c.appendChild(queue);
}

// Keyboard triage over the review queue (full a11y): a roving-tabindex list where exactly one
// card is in the tab order, the arrow/vim keys move focus, and single keys fire the focused
// card's actions. Typing into a field is never hijacked. Tested in jsdom via synthetic
// KeyboardEvents (focus moves + the right action button is clicked).
function wireQueueKeyboard(queue) {
  const cards = () => [...queue.querySelectorAll(".mm-card[data-card]")];

  function focusCard(list, idx) {
    if (!list.length) return;
    const clamped = Math.max(0, Math.min(idx, list.length - 1));
    for (const card of list) card.setAttribute("tabindex", "-1");
    const target = list[clamped];
    target.setAttribute("tabindex", "0");
    target.focus();
  }

  queue.addEventListener("keydown", (e) => {
    // Never eat modifier chords (Ctrl/Cmd/Alt-<key> belong to the browser / screen reader), and
    // never steal keys typed into an input/textarea/select within a card.
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const tag = e.target && e.target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;

    const list = cards();
    if (!list.length) return;
    const focused = e.target.closest ? e.target.closest(".mm-card[data-card]") : null;
    const current = focused ? list.indexOf(focused) : -1;

    // Single-letter action shortcuts fire ONLY when the card itself holds focus — not when a child
    // button is focused (Tab-ing onto "Approve all safe" then pressing 'x' must not dismiss).
    // Navigation keys (arrows/j/k/Home/End) still work from anywhere in the queue.
    const fire = (act) => {
      if (!focused || e.target !== focused) return;
      const btn = focused.querySelector(`[data-act="${act}"]:not([disabled])`);
      if (btn) {
        btn.click();
        e.preventDefault();
      }
    };

    switch (e.key) {
      case "ArrowDown":
      case "j":
        focusCard(list, current < 0 ? 0 : current + 1);
        e.preventDefault();
        break;
      case "ArrowUp":
      case "k":
        focusCard(list, current < 0 ? 0 : current - 1);
        e.preventDefault();
        break;
      case "Home":
        focusCard(list, 0);
        e.preventDefault();
        break;
      case "End":
        focusCard(list, list.length - 1);
        e.preventDefault();
        break;
      case "Enter":
      case "a":
        fire("approve");
        break;
      case "e":
        fire("explain");
        break;
      case "x":
      case "Delete":
      case "Backspace":
        fire("dismiss");
        break;
      default:
        break;
    }
  });
}

// --- First-run backfill ("Triage my existing mail") -----------------------------------
//
// One tap sweeps already-present mail through the same classify path (the host applies nothing
// and mines deliberate folder placements to warm suggestions). The background owns the paging;
// the dashboard renders an idle one-tap / a live progress chip / a finished summary, driven by
// `mm:backfillStatus`, and polls while a run is in flight.

let backfillPoll = null;

async function renderBackfill(container) {
  const box = el("div", { class: "mm-backfill", attrs: { id: "mm-backfill" } });
  container.appendChild(box);
  await paintBackfill(box);
}

async function paintBackfill(box) {
  if (!box || !box.isConnected) return; // the tab moved on; stop touching a detached node
  const status = await send({ type: "mm:backfillStatus" });
  if (!box.isConnected) return; // detached while we awaited the status — do not paint into a dead node
  const st = status && status.ok ? status : {};
  clear(box);

  if (st.running) {
    const total = st.total ? String(st.total) : "…";
    box.appendChild(
      el("span", {
        class: "mm-backfill__chip",
        text: st.paused
          ? `Paused · ${st.done || 0}/${total} triaged`
          : `Triaging your mail · ${st.done || 0}/${total}`,
      }),
    );
    const toggle = el("button", { class: "mm-backfill__btn", text: st.paused ? "Resume" : "Pause" });
    toggle.addEventListener("click", async () => {
      await send({ type: "mm:backfillControl", action: st.paused ? "resume" : "pause" });
      await paintBackfill(box);
    });
    const stop = el("button", { class: "mm-backfill__btn mm-backfill__btn--stop", text: "Stop" });
    stop.addEventListener("click", async () => {
      await send({ type: "mm:backfillControl", action: "cancel" });
      await paintBackfill(box);
    });
    box.append(toggle, stop);
    schedulePoll(box);
    return;
  }

  stopPoll();
  if (st.done_at) {
    // A finished run summarizes what it surfaced (and links onward to the warmed proposals).
    const n = st.classified || 0;
    const review = st.needs_review || 0;
    box.appendChild(
      el("span", {
        class: "mm-backfill__chip mm-backfill__chip--done",
        text: review
          ? `Triaged ${n} messages · ${review} need a look`
          : `Triaged ${n} messages — suggestions are warming up`,
      }),
    );
    return;
  }

  // Idle: the one-tap invitation.
  box.appendChild(
    el("span", {
      class: "mm-backfill__hint",
      text: "New here? Scan what's already in your folders to warm up suggestions — nothing is moved.",
    }),
  );
  const start = el("button", { class: "mm-backfill__start", text: "Triage my existing mail" });
  start.addEventListener("click", async () => {
    start.disabled = true;
    const r = await send({ type: "mm:triageExisting" });
    if (!r || r.ok === false) {
      toast(r && r.error ? r.error : "Couldn't start triage", true);
      start.disabled = false;
      return;
    }
    await paintBackfill(box);
  });
  box.appendChild(start);
}

function schedulePoll(box) {
  stopPoll();
  backfillPoll = setTimeout(() => paintBackfill(box), 900);
}

function stopPoll() {
  if (backfillPoll) {
    clearTimeout(backfillPoll);
    backfillPoll = null;
  }
}

function reviewCard(item, isFirst = false) {
  const cls = item.classification || {};
  const tbId = item.thunderbird_message_id;
  const decisionId = item.decision_id;
  // A listitem in the keyboard-navigable queue. Roving tabindex: only the first card is in the
  // tab order (0); the rest are reachable by arrow/vim keys (-1). The aria-label is what a screen
  // reader announces on focus — subject, sender, and the one-line verdict.
  const card = el("div", {
    class: "mm-card",
    attrs: {
      role: "listitem",
      "data-card": "1",
      tabindex: isFirst ? "0" : "-1",
      "aria-label": `${subjectOf(item)} — from ${fromOf(item)}. ${verdictLine(cls)}`,
    },
  });

  const risk = riskOf(cls);
  const top = el("div", { class: "mm-card__top" }, [
    el("span", { class: `mm-glyph mm-glyph--${risk.level}`, text: risk.glyph }),
    el("span", { class: "mm-card__title", text: subjectOf(item) }),
    el("span", { class: "mm-card__from", text: fromOf(item) }),
  ]);
  const link = el("button", { class: "mm-deeplink", text: "›", title: "Open this message" });
  link.addEventListener("click", () => deepLink(tbId));
  top.appendChild(link);
  card.appendChild(top);

  card.appendChild(el("p", { class: "mm-card__sub", text: verdictLine(cls) }));

  const rows = el("div", { class: "mm-rows" });
  for (const a of item.applied_actions || []) rows.appendChild(autoRow(a, decisionId, tbId, card, item));
  const suggestions = [];
  const reviews = [];
  for (const a of item.review_required_actions || []) {
    (APPLYABLE_KINDS.has(a.kind) ? suggestions : reviews).push(a);
  }
  for (const a of suggestions) rows.appendChild(suggestRow(a, decisionId, tbId, card, item));
  for (const a of reviews) rows.appendChild(reviewRow(a));
  for (const b of item.blocked_actions || []) rows.appendChild(blockedRow(b));
  card.appendChild(rows);

  card.appendChild(el("hr", { class: "mm-card__hr" }));

  const actions = el("div", { class: "mm-card__actions" });
  if (suggestions.length) {
    const approveAll = el("button", { class: "mm-btn mm-btn--primary", text: t("dashApproveAllSafe", "Approve all safe"), attrs: { "data-act": "approve" } });
    approveAll.addEventListener("click", () => approveAllSafe(suggestions, decisionId, tbId, card, item));
    actions.appendChild(approveAll);
  }
  const dismiss = el("button", { class: "mm-btn", text: t("dashDismiss", "Dismiss"), attrs: { "data-act": "dismiss" } });
  dismiss.addEventListener("click", () => dismissCard(item, card));
  actions.appendChild(dismiss);
  const explain = el("button", { class: "mm-btn", text: t("dashExplain", "Explain"), attrs: { "data-act": "explain" } });
  explain.addEventListener("click", () => explainInActivity(tbId));
  actions.appendChild(explain);
  card.appendChild(actions);

  return card;
}

function autoRow(action, decisionId, tbId, card, item) {
  const sub = el("span", { class: "mm-arow__sub", text: `auto · ${ruleNote(action)}` });
  const row = el("div", { class: "mm-arow mm-arow--auto" }, [
    el("span", { class: "mm-arow__mark", text: "✓" }),
    el("span", {}, [document.createTextNode(actionText(action)), sub]),
  ]);
  const undo = el("button", { class: "mm-btn", text: "↩ Undo" });
  // Reversing an auto-applied move needs the prior folder (`reverses_to`), which rides the
  // host's apply_state enrichment (a tracked backend addition). Until it's on the wire, present
  // the Undo as honestly unavailable for moves rather than a button that always errors —
  // tag/junk undo, which need no prior state, stay live. (Move it back manually in Thunderbird.)
  if (action.kind === "move" && !action.reverses_to) {
    undo.disabled = true;
    // Mark as statically disabled (not in-flight) so the review-refresh coalescer ignores it.
    undo.setAttribute("data-static", "1");
    undo.title = "Undo for auto-filed moves arrives with folder-history — move it back in Thunderbird for now";
  } else {
    undo.addEventListener("click", async () => {
      undo.disabled = true;
      const reply = await send({ type: "mm:undo", action, decisionId, messageId: tbId });
      if (reply.ok) {
        toast("Undone");
        row.remove();
      } else {
        undo.disabled = false;
        toast(reply.error || "couldn't undo", true);
      }
    });
  }
  row.appendChild(undo);
  return row;
}

function suggestRow(action, decisionId, tbId, card, item) {
  const row = el("div", { class: "mm-arow mm-arow--suggest" }, [
    el("span", { class: "mm-arow__mark", text: "◻" }),
    el("span", { text: actionText(action) }),
  ]);
  const approve = el("button", { class: "mm-btn mm-btn--primary", text: "Approve" });
  approve.addEventListener("click", async () => {
    approve.disabled = true;
    const reply = await send({ type: "mm:apply", action, decisionId, messageId: tbId });
    if (reply.ok) {
      toast("Applied — MailMate is learning from this");
      row.remove();
      maybeResolve(item, card);
    } else {
      approve.disabled = false;
      toast(reply.error || "couldn't apply", true);
    }
  });
  row.appendChild(approve);
  return row;
}

function reviewRow(action) {
  return el("div", { class: "mm-arow mm-arow--review" }, [
    el("span", { class: "mm-arow__mark", text: "⚑" }),
    el("span", {}, [
      document.createTextNode(actionText(action)),
      el("span", { class: "mm-arow__sub", text: "requires your review" }),
    ]),
  ]);
}

function blockedRow(blocked) {
  const a = blocked.action || {};
  return el("div", { class: "mm-arow mm-arow--blocked" }, [
    el("span", { class: "mm-arow__mark", text: "⛔" }),
    el("span", {}, [
      document.createTextNode(`${actionText(a)} — blocked by policy`),
      el("span", { class: "mm-arow__sub", text: `${blocked.policy_id || "policy"} · ${blocked.reason || ""}` }),
    ]),
  ]);
}

async function approveAllSafe(suggestions, decisionId, tbId, card, item) {
  let ok = 0;
  for (const action of suggestions) {
    const reply = await send({ type: "mm:apply", action, decisionId, messageId: tbId });
    if (reply.ok) ok += 1;
  }
  if (ok === suggestions.length) {
    // Every suggestion applied — drop the decision from the buffer and remove the card (mirrors
    // the single-approve path), so the card and counts can't survive a bulk approve.
    await resolveReview(item);
    card.remove();
    await refreshReviewCount();
    toast(`Approved ${ok}`);
  } else {
    // A partial failure leaves the card; re-pull so the still-pending suggestions are accurate.
    toast(`Approved ${ok} of ${suggestions.length}`, ok === 0);
    await renderReview();
  }
}

async function dismissCard(item, card) {
  // Record one dismissal per suggested action (the host audits the ignore signal), then drop it.
  for (const a of item.review_required_actions || []) {
    await send({ type: "mm:dismiss", decisionId: item.decision_id, actionKind: a.kind, messageId: item.thunderbird_message_id });
  }
  await resolveReview(item);
  card.remove();
  toast(t("dashDismissed", "Dismissed"));
  await refreshReviewCount();
}

async function maybeResolve(item, card) {
  // When every suggestion row is gone, drop the card from the buffer.
  if (!card.querySelector(".mm-arow--suggest")) {
    await resolveReview(item);
    card.remove();
    await refreshReviewCount();
  }
}

async function resolveReview(item) {
  await send({ type: "mm:resolveReview", decisionId: item.decision_id });
}

async function refreshReviewCount() {
  const reply = await send({ type: "mm:reviewQueue" });
  setTabCount("review", (reply.items || []).length);
}

// --- Tab 4: Activity ------------------------------------------------------------------

let activitySeq = 0;

async function renderActivity() {
  // Sequence guard: a rapid filter-chip click (or a tab switch) starts a newer render while this
  // one is awaiting; when our fetch resolves we paint events only if we are still the newest, so
  // two filters' events can never interleave under one chip bar.
  const myseq = ++activitySeq;
  const c = content();
  clear(c);

  const filters = el("div", { class: "mm-filters" });
  for (const [key, label] of ACTIVITY_FILTERS) {
    const chip = el("button", { class: "mm-chip", text: label, attrs: { "aria-pressed": String(key === activityFilter) } });
    chip.addEventListener("click", () => {
      activityFilter = key;
      renderActivity();
    });
    filters.appendChild(chip);
  }
  c.appendChild(filters);

  const reply = await send({
    type: "mm:listActivity",
    limit: 80,
    eventTypeFilter: activityFilter === "all" ? null : activityFilter,
  });
  if (myseq !== activitySeq) return; // a newer render superseded us — don't paint stale events

  if (!reply.ok) {
    c.appendChild(emptyState("⚠", "Activity unavailable", [reply.error || "the host didn't answer"]));
    return;
  }
  const events = reply.events || [];
  if (!events.length) {
    c.appendChild(
      emptyState("🗒", "Nothing has happened yet", [
        "Once MailMate classifies a message or applies a rule, every step shows here — classification, policy checks, what was applied or blocked, and your corrections — fully replayable and offline.",
      ]),
    );
    return;
  }

  for (const ev of events) {
    const tbId = ev.payload && (ev.payload.thunderbird_message_id || ev.payload.tb_id);
    const row = el("div", { class: "mm-event" }, [
      el("span", { class: "mm-event__time", text: relativeTime(ev.created_at) }),
      el("span", { text: eventGlyph(ev.event_type) }),
      el("span", { class: "mm-event__what", text: eventSummary(ev) }),
      el("span", { class: "mm-event__type", text: ev.event_type }),
    ]);
    if (tbId) {
      const link = el("button", { class: "mm-deeplink", text: "›", title: "Open this message" });
      link.addEventListener("click", () => deepLink(tbId));
      row.appendChild(link);
    }
    c.appendChild(row);
  }
}

function eventGlyph(type) {
  if (type === "action_applied") return "✓";
  if (type === "action_blocked_by_policy") return "⛔";
  if (type === "classification_failed" || type === "new_mail_rejected") return "⚠";
  if (type.startsWith("rule_") || type === "proposal_reviewed") return "🧠";
  if (type.startsWith("workflow") || type.startsWith("pipeline") || type.startsWith("followup")) return "📝";
  if (type === "suggestion_dismissed" || type === "classification_corrected" || type === "action_undone") return "👤";
  return "•";
}

function eventSummary(ev) {
  const p = ev.payload || {};
  if (ev.event_type === "action_applied" && p.kind) return `applied ${p.kind}${p.to_folder ? ` → ${folderName(p.to_folder)}` : ""}`;
  if (ev.event_type === "action_blocked_by_policy") return `blocked ${(p.action && p.action.kind) || "action"}`;
  if (ev.event_type === "classification_corrected") return `you corrected the category${p.corrected_label ? ` → ${p.corrected_label}` : ""}`;
  if (ev.event_type === "suggestion_dismissed") return `you dismissed a ${p.action_kind || "suggestion"}`;
  if (ev.event_type === "action_undone") return `you undid a ${p.action_kind || "action"}`;
  return ev.event_type.replace(/_/g, " ");
}

function explainInActivity(tbId) {
  // The dashboard's Activity tab is the global stream; "Explain" focuses the user there. (A
  // per-message focused timeline via explain_decision is the panel's "Explain in dashboard"
  // deep target; here we switch tabs so the user sees the surrounding history.)
  selectTab("activity");
  if (tbId) toast("Showing recent activity");
}

// --- Tab 3: Proposals — the materialization gate --------------------------------------

let proposalsSeq = 0;

// Refresh just the Proposals tab badge without rebuilding the cards — so the count stays right
// even when the user is on another tab (mirrors refreshReviewCount for the Review tab).
async function refreshProposalCount() {
  const reply = await send({ type: "mm:listProposals" });
  setTabCount("proposals", reply.ok ? (reply.pending_reviews || []).length : 0);
}

async function renderProposals() {
  // Sequence guard (mirrors renderActivity): only the newest re-pull paints, so overlapping
  // renders from a review action + a proposal_ready event can't interleave or show a stale count.
  const myseq = ++proposalsSeq;
  const reply = await send({ type: "mm:listProposals" });
  if (myseq !== proposalsSeq) return;
  const c = content();
  clear(c);

  if (!reply.ok) {
    // No admin surface wired, or the host can't answer — honest, not blank.
    c.appendChild(
      emptyState("🧠", "No rules waiting for approval", [
        reply.error && reply.error.includes("admin")
          ? "The proposal store isn't wired into this host build yet."
          : "As you correct MailMate, it discovers patterns and proposes deterministic rules here for you to approve. Nothing activates on its own.",
      ]),
    );
    setTabCount("proposals", 0);
    return;
  }
  const proposals = reply.pending_reviews || [];
  setTabCount("proposals", proposals.length);
  if (!proposals.length) {
    c.appendChild(
      emptyState("🧠", "No rules waiting for approval", [
        "As you correct MailMate (Dismiss / Not junk / refile), it discovers patterns and proposes deterministic rules here for you to approve. Nothing activates on its own — every rule is your decision.",
      ]),
    );
    return;
  }

  for (const p of proposals) c.appendChild(proposalCard(p));
}

// --- Rule → English -------------------------------------------------------------------------
//
// Render a rule's condition→effect AST (the same JSON the host stores: `{all|any|not}` over
// `{field,op,value}` predicates + a RuleEffect) as a readable sentence. Shared by the Proposals
// card and the Rules-manager tab so a learned rule always reads the same way. Pure + total: an
// unknown field/op falls back to its raw token rather than throwing, so a forward-compatible
// rule from a newer host still renders something honest.

const FIELD_LABELS = {
  sender_domain: "sender domain",
  sender_email: "sender",
  sender_seen_count: "times seen from this sender",
  subject_normalized: "subject",
  subject: "subject",
  is_spam: "spam",
  is_phishing: "phishing",
  thread_id: "thread",
  "classification.labels": "label",
  "classification.priority": "priority",
  account_id: "account",
  folder_id: "folder",
};
const fieldLabel = (f) => FIELD_LABELS[f] || String(f || "").replace(/[._]/g, " ");

const OP_LABELS = {
  eq: "is",
  in: "is one of",
  contains: "contains",
  contains_any: "contains any of",
  contains_all: "contains all of",
  gt: "is more than",
  gte: "is at least",
  lt: "is less than",
  lte: "is at most",
  before: "is before",
  after: "is after",
  exists: "is present",
  matches_regex: "matches",
};

function formatRuleValue(value) {
  if (value == null) return "";
  if (Array.isArray(value)) return value.map((v) => `“${v}”`).join(", ");
  if (typeof value === "string") return `“${value}”`;
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

function predicateToEnglish(pred) {
  const field = fieldLabel(pred.field);
  const op = OP_LABELS[pred.op] || String(pred.op || "");
  if (pred.op === "exists") return `${field} is present`;
  if (pred.op === "matches_regex") return `${field} matches /${pred.value}/`;
  const val = formatRuleValue(pred.value);
  return val ? `${field} ${op} ${val}` : `${field} ${op}`;
}

// Only an and/or combinator needs parens for precedence when nested inside another; a `not (...)`
// is already self-delimiting and a leaf predicate needs none.
function clauseToEnglish(cond, nested) {
  if (cond && nested && (Array.isArray(cond.all) || Array.isArray(cond.any))) {
    return `(${conditionToEnglish(cond)})`;
  }
  return conditionToEnglish(cond);
}

function conditionToEnglish(cond) {
  if (!cond || typeof cond !== "object") return "any message";
  if (Array.isArray(cond.all)) {
    if (!cond.all.length) return "any message";
    return cond.all.map((c) => clauseToEnglish(c, true)).join(" and ");
  }
  if (Array.isArray(cond.any)) {
    if (!cond.any.length) return "any message";
    return cond.any.map((c) => clauseToEnglish(c, true)).join(" or ");
  }
  if (cond.not) return `not (${conditionToEnglish(cond.not)})`;
  return predicateToEnglish(cond);
}

function effectToEnglish(effect) {
  if (!effect || typeof effect !== "object") return "do nothing";
  const parts = [];
  if (effect.move != null) parts.push(`move to ${effect.move}`);
  if (Array.isArray(effect.tag) && effect.tag.length) parts.push(`tag with ${effect.tag.join(", ")}`);
  if (effect.mark_junk === true) parts.push("mark as junk");
  if (effect.mark_junk === false) parts.push("unmark as junk");
  if (Array.isArray(effect.set_labels) && effect.set_labels.length) parts.push(`label as ${effect.set_labels.join(", ")}`);
  if (effect.priority != null) parts.push(`set priority ${effect.priority}`);
  if (Array.isArray(effect.require_review_for) && effect.require_review_for.length) {
    parts.push(`hold ${effect.require_review_for.join(", ")} for review`);
  }
  return parts.length ? parts.join(", ") : "do nothing";
}

// `draft` is a RuleDraft / rule version: { condition, effect, ... }. Returns a full sentence.
function ruleToEnglish(draft) {
  if (!draft || typeof draft !== "object") return "";
  return `When ${conditionToEnglish(draft.condition)} → ${effectToEnglish(draft.effect)}.`;
}

// The glyph class for a risk level — total over the four RiskLevel variants (critical/high/
// medium/low) and any unexpected token (→ low). A bare ternary silently rendered `critical` as
// `low`; this maps it explicitly.
const RISK_GLYPH = { critical: "critical", high: "high", medium: "med", low: "low" };
const riskGlyphClass = (risk) => RISK_GLYPH[String(risk || "low").toLowerCase()] || "low";

// "precision 0.88 · support 23 msgs", or "support N msgs" when precision is absent (fired on
// nothing), or "" when there is no back-test at all.
function backTestSummary(bt) {
  if (!bt || typeof bt !== "object") return "";
  const support = `support ${bt.support} msg${bt.support === 1 ? "" : "s"}`;
  return typeof bt.precision === "number" ? `precision ${bt.precision.toFixed(2)} · ${support}` : support;
}

function proposalCard(p) {
  const risk = (p.risk_level || "low").toLowerCase();
  const card = el("div", { class: "mm-card" });
  card.appendChild(
    el("div", { class: "mm-card__top" }, [
      el("span", { class: `mm-glyph mm-glyph--${riskGlyphClass(risk)}`, text: "◆" }),
      el("span", { class: "mm-card__title", text: p.title || p.proposal_type }),
      el("span", { class: "mm-card__from", text: `${p.proposal_type} · ${risk} risk` }),
    ]),
  );
  card.appendChild(el("p", { class: "mm-card__sub", text: p.rationale || "" }));
  // The candidate rule in English + the back-test the gate admitted it on — so the card is
  // reviewable without drilling into a detail view (the Phase-6 exit: a proposal shows its rule
  // in English with back-test numbers).
  if (p.rule_draft) {
    card.appendChild(el("p", { class: "mm-rule-en", text: ruleToEnglish(p.rule_draft) }));
  }
  const bt = backTestSummary(p.back_test);
  if (bt) card.appendChild(el("p", { class: "mm-card__sub mm-muted", text: bt }));
  // Conflicts with existing active rules (subsumption/overlap/contradiction). A non-empty list is
  // why the host recommends pending_human_review — surface it so the user adjudicates with eyes
  // open instead of blindly approving an overlapping rule.
  const conflicts = Array.isArray(p.conflicts) ? p.conflicts : [];
  if (conflicts.length) {
    const block = el("div", { class: "mm-card__conflicts" });
    block.appendChild(
      el("p", {
        class: "mm-card__sub mm-warn",
        text: `⚠ Conflicts with ${conflicts.length} existing rule${conflicts.length === 1 ? "" : "s"} — needs your review`,
      }),
    );
    for (const c of conflicts) {
      block.appendChild(el("p", { class: "mm-card__sub mm-muted", text: `· ${c.description || c.kind || "conflict"}` }));
    }
    card.appendChild(block);
  }
  card.appendChild(
    el("p", { class: "mm-card__sub mm-muted", text: `Recommended: ${p.recommended_status || "review"}` }),
  );
  card.appendChild(el("hr", { class: "mm-card__hr" }));

  const actions = el("div", { class: "mm-card__actions" });
  // A retire proposal references an EXISTING rule (no new draft to shadow): accepting it retires
  // that rule, so the card must say so — never the new-rule "Approve → shadow" wording, which
  // would promise a shadow step that does not happen.
  if (p.proposal_type === "retire_rule") {
    const retireBtn = el("button", { class: "mm-btn mm-btn--primary", text: "Approve → retire rule" });
    retireBtn.title = "Retire the rule you keep undoing — it stops acting (you can re-learn it later)";
    retireBtn.addEventListener("click", () => reviewProposal(p, "accept_for_shadow_mode", card));
    const keep = el("button", { class: "mm-btn", text: "Keep rule" });
    keep.addEventListener("click", () => reviewProposal(p, "reject", card));
    actions.appendChild(retireBtn);
    actions.appendChild(keep);
    card.appendChild(actions);
    return card;
  }

  // Every new-rule approval materializes the rule into its recommended status (shadow / pending-
  // review) — a rule that runs and logs but never acts on its own until you later promote it.
  // There is no one-click "→ active": activation is a separate, deliberate step, so even a
  // HIGH-risk approval here is safe (the rule shadows, it does not act). Rejection feeds the
  // curator's negative signal.
  const approve = el("button", { class: "mm-btn mm-btn--primary", text: "Approve → shadow" });
  approve.title = "Materialize as a shadow rule (runs + logs, never acts until you promote it)";
  approve.addEventListener("click", () => reviewProposal(p, "accept_for_shadow_mode", card));
  const reject = el("button", { class: "mm-btn", text: "Reject" });
  reject.addEventListener("click", () => reviewProposal(p, "reject", card));
  actions.appendChild(approve);
  actions.appendChild(reject);
  if (risk === "high") {
    actions.appendChild(
      el("span", { class: "mm-note", text: "high-risk — approval shadows only; promotion to active is a separate step" }),
    );
  }
  card.appendChild(actions);
  return card;
}

async function reviewProposal(p, decision, card) {
  for (const b of card.querySelectorAll("button")) b.disabled = true;
  const reply = await send({
    type: "mm:reviewProposal",
    proposalId: p.id,
    decision,
    reasonCode: decision === "reject" ? "user_rejected" : null,
  });
  if (!reply.ok) {
    for (const b of card.querySelectorAll("button")) b.disabled = false;
    toast(reply.error || "couldn't apply that decision", true);
    return;
  }
  card.remove();
  // The rule lands in the proposal's *recommended* status (shadow / pending-review); the host's
  // resulting_status is the proposal disposition ("accepted"), not the rule mode, so show the
  // mode the card promised.
  toast(decision === "reject" ? "Rejected" : `Approved → ${p.recommended_status || "shadow"}`);
  await renderProposals(); // re-pull so the tab count + any remaining cards are accurate
}

// --- Tab: Rules manager ---------------------------------------------------------------
//
// Every evaluated (active + shadow) and disabled rule, grouped by lifecycle, each rendered in
// English (reusing ruleToEnglish) with its backed correction signal and a one-click status flip.
// Disabling/enabling/promoting hot-reloads the host engines, so the change is live immediately.

const RULE_GROUPS = [
  ["active", "Active — acting on their own"],
  ["shadow_mode", "Shadow — testing, never acts"],
  ["disabled", "Disabled — off until you re-enable"],
];

async function renderRules() {
  const reply = await send({ type: "mm:listRules" });
  const c = document.getElementById("mm-content");
  clear(c);
  if (!reply.ok) {
    c.appendChild(el("p", { class: "mm-muted", text: reply.error || "Couldn't load rules." }));
    return;
  }
  const rules = reply.rules || [];
  if (!rules.length) {
    c.appendChild(
      el("p", { class: "mm-muted", text: "No learned rules yet. As you correct MailMate it proposes rules; the ones you approve appear here." }),
    );
    return;
  }
  const byStatus = {};
  for (const r of rules) (byStatus[r.status] ||= []).push(r);
  for (const [status, heading] of RULE_GROUPS) {
    const group = byStatus[status];
    if (!group || !group.length) continue;
    c.appendChild(el("h3", { class: "mm-band__title", text: `${heading} · ${group.length}` }));
    for (const r of group) c.appendChild(ruleCard(r));
  }
}

function ruleCard(r) {
  const card = el("div", { class: "mm-card" });
  const risk = (r.risk_level || "low").toLowerCase();
  card.appendChild(
    el("div", { class: "mm-card__top" }, [
      el("span", { class: `mm-glyph mm-glyph--${riskGlyphClass(risk)}`, text: "▣" }),
      el("span", { class: "mm-card__title", text: `${r.kind} rule` }),
      el("span", { class: `mm-pill mm-pill--${r.status}`, text: (r.status || "").replace(/_/g, " ") }),
    ]),
  );
  card.appendChild(el("p", { class: "mm-rule-en", text: ruleToEnglish({ condition: r.condition, effect: r.effect }) }));

  // The backed correction signal: undos of this rule's auto-applied actions. `null` = not yet
  // tracked (never a misleading "0 corrections"); 0 = a genuine clean record.
  const undo = r.undo_count;
  const undoText =
    undo == null
      ? "corrections: not tracked yet"
      : undo === 0
        ? "no corrections yet — looking good"
        : `you undid this rule ${undo} time${undo === 1 ? "" : "s"}`;
  card.appendChild(el("p", { class: "mm-card__sub mm-muted", text: undoText }));

  const actions = el("div", { class: "mm-card__actions" });
  if (r.status === "disabled") {
    const enable = el("button", { class: "mm-btn mm-btn--primary", text: "Enable" });
    enable.addEventListener("click", () => setRuleStatus(r, "active", card));
    actions.appendChild(enable);
  } else {
    if (r.status === "shadow_mode") {
      const promote = el("button", { class: "mm-btn mm-btn--primary", text: "Activate" });
      promote.title = "Promote this shadow rule to active so it can act on its own";
      promote.addEventListener("click", () => setRuleStatus(r, "active", card));
      actions.appendChild(promote);
    }
    const disable = el("button", { class: "mm-btn", text: "Disable" });
    disable.addEventListener("click", () => setRuleStatus(r, "disabled", card));
    actions.appendChild(disable);
  }
  card.appendChild(actions);
  return card;
}

async function setRuleStatus(r, status, card) {
  for (const b of card.querySelectorAll("button")) b.disabled = true;
  const reply = await send({ type: "mm:setRuleStatus", ruleId: r.rule_id, kind: r.kind, status });
  if (!reply.ok) {
    for (const b of card.querySelectorAll("button")) b.disabled = false;
    toast(reply.error || "couldn't change that rule", true);
    return;
  }
  toast(status === "disabled" ? "Rule disabled" : status === "active" ? "Rule enabled" : "Rule updated");
  await renderRules(); // re-pull so the rule re-groups under its new status
}

// --- Tab 2: Follow-ups pipeline -------------------------------------------------------

let followupsSeq = 0;

async function refreshFollowupCount() {
  const reply = await send({ type: "mm:listFollowups" });
  const deals = reply.ok ? reply.followups || [] : [];
  setTabCount("followups", deals.filter(isActionableDeal).length);
}

function isActionableDeal(d) {
  // "Needs attention" + live deals are the work; closed (won/lost) deals are history.
  return d.needs_attention || d.stage === "open" || d.stage === "engaged";
}

async function renderFollowups() {
  const myseq = ++followupsSeq;
  const reply = await send({ type: "mm:listFollowups" });
  if (myseq !== followupsSeq) return;
  const c = content();
  clear(c);

  if (!reply.ok) {
    c.appendChild(emptyState("⚠", "Follow-ups unavailable", [reply.error || "the host didn't answer"]));
    setTabCount("followups", 0);
    return;
  }
  const deals = reply.followups || [];
  setTabCount("followups", deals.filter(isActionableDeal).length);

  if (!deals.length) {
    c.appendChild(
      emptyState("📭", "No deals tracked yet", [
        "Send a quote or proposal, then enroll it (from the message's MailMate panel) to have MailMate time the nudges. MailMate drafts the follow-ups for you to review — it never sends them.",
      ]),
    );
    return;
  }

  const attention = deals.filter((d) => d.needs_attention);
  const active = deals.filter((d) => !d.needs_attention && (d.stage === "open" || d.stage === "engaged"));
  const closed = deals.filter((d) => d.stage === "won" || d.stage === "lost");

  if (attention.length) {
    c.appendChild(el("p", { class: "mm-card__sub mm-muted", text: "NEEDS ATTENTION" }));
    for (const d of attention) c.appendChild(followupCard(d, true));
  }
  if (active.length) {
    c.appendChild(el("p", { class: "mm-card__sub mm-muted", text: "ACTIVE PIPELINE" }));
    for (const d of active) c.appendChild(followupCard(d, false));
  }
  if (closed.length) {
    const won = closed.filter((d) => d.stage === "won").length;
    const lost = closed.filter((d) => d.stage === "lost").length;
    c.appendChild(el("p", { class: "mm-card__sub mm-muted", text: `Closed — ${won} won · ${lost} lost` }));
  }
}

function followupCard(d, attention) {
  const card = el("div", { class: "mm-card" });
  const top = el("div", { class: "mm-card__top" }, [
    el("span", { class: `mm-glyph mm-glyph--${attention ? "high" : "med"}`, text: attention ? "⏳" : "▸" }),
    el("span", { class: "mm-card__title", text: d.title || "Tracked deal" }),
    el("span", { class: "mm-card__from", text: followupStatusLine(d) }),
  ]);
  if (d.anchor_thunderbird_message_id) {
    const link = el("button", { class: "mm-deeplink", text: "›", title: "Open the thread" });
    link.addEventListener("click", () => deepLink(d.anchor_thunderbird_message_id));
    top.appendChild(link);
  }
  card.appendChild(top);
  card.appendChild(el("hr", { class: "mm-card__hr" }));

  // Only offer actions the workflow engine will actually accept for this instance's status:
  //  - reschedule (Snooze) is valid ONLY for `active`/`snoozed` instances;
  //  - an `awaiting_review` draft is advanced with review_followup (Skip), not reschedule;
  //  - everything can be Marked won/lost or Cancelled.
  // (Reviewing/sending the actual draft happens in the compose window the follow-up notification
  // opens — the dashboard offers the cadence-management actions.)
  const status = d.status || "";
  const reschedulable = status === "active" || status === "snoozed";
  const actions = el("div", { class: "mm-card__actions" });
  if (status === "awaiting_review") {
    addFollowupAction(actions, "Skip step", () => reviewFollowup(d, "skip", card));
  }
  if (reschedulable) {
    addFollowupAction(actions, "Snooze 1d", () => rescheduleFollowup(d, "snooze", 1, card));
    addFollowupAction(actions, "Snooze 3d", () => rescheduleFollowup(d, "snooze", 3, card));
  }
  addFollowupAction(actions, "Mark won", () => stageFollowup(d, "won", card));
  addFollowupAction(actions, "Mark lost", () => stageFollowup(d, "lost", card));
  addFollowupAction(actions, "Cancel sequence", () => cancelFollowup(d, card));
  card.appendChild(actions);
  return card;
}

function addFollowupAction(container, label, fn) {
  const b = el("button", { class: "mm-btn", text: label });
  b.addEventListener("click", fn);
  container.appendChild(b);
}

function followupStatusLine(d) {
  const status = d.status ? d.status.replace(/_/g, " ") : "unarmed";
  const due =
    d.next_due_at && (d.stage === "open" || d.stage === "engaged")
      ? ` · next ${relativeDue(d.next_due_at)}`
      : "";
  return `${status}${due}`;
}

function relativeDue(value) {
  const ms = toMillis(value);
  if (ms == null) return "";
  const delta = ms - Date.now();
  if (delta <= 0) return "due now";
  const h = Math.round(delta / 3600000);
  if (h < 24) return `in ${h}h`;
  return `in ${Math.round(h / 24)}d`;
}

async function rescheduleFollowup(d, verb, days, card) {
  if (!d.workflow_instance_id) return;
  const nextDueAt = new Date(Date.now() + days * 86400000).toISOString();
  await runFollowupAction(card, { type: "mm:followupReschedule", verb, workflowInstanceId: d.workflow_instance_id, nextDueAt }, "Snoozed");
}

async function reviewFollowup(d, resolution, card) {
  if (!d.workflow_instance_id) return;
  await runFollowupAction(
    card,
    { type: "mm:followupReview", workflowInstanceId: d.workflow_instance_id, resolution },
    resolution === "skip" ? "Step skipped" : "Resolved",
  );
}

async function stageFollowup(d, stage, card) {
  await runFollowupAction(card, { type: "mm:followupStage", pipelineItemId: d.pipeline_item_id, stage }, stage === "won" ? "Marked won" : "Marked lost");
}

async function cancelFollowup(d, card) {
  await runFollowupAction(card, { type: "mm:followupCancel", pipelineItemId: d.pipeline_item_id }, "Sequence cancelled");
}

async function runFollowupAction(card, message, okText) {
  if (card) for (const b of card.querySelectorAll("button")) b.disabled = true;
  const reply = await send(message);
  if (!reply.ok) {
    if (card) for (const b of card.querySelectorAll("button")) b.disabled = false;
    toast(reply.error || "couldn't do that", true);
    return;
  }
  toast(okText);
  await renderFollowups();
}

// --- Onboarding -----------------------------------------------------------------------

function renderOnboarding(stepIndex) {
  $("mm-app").hidden = true;
  $("mm-banner").hidden = true;
  const root = $("mm-onboarding");
  root.hidden = false;
  clear(root);

  const steps = [onboardStep1, onboardStep2, onboardStep3, onboardStep4];
  steps[stepIndex](root, stepIndex);
}

function onboardShell(root, index, title, bodyNodes, onBack, onNext, nextLabel) {
  root.appendChild(el("div", { class: "mm-ob__step", text: `Step ${index + 1} of 4` }));
  root.appendChild(el("div", { class: "mm-ob__title", text: title }));
  for (const n of bodyNodes) root.appendChild(n);
  const nav = el("div", { class: "mm-ob__nav" });
  const left = el("button", { class: "mm-btn", text: index === 0 ? "Skip setup" : "← Back" });
  left.addEventListener("click", onBack);
  const right = el("button", { class: "mm-btn mm-btn--primary", text: nextLabel || "Continue →" });
  right.addEventListener("click", onNext);
  nav.appendChild(left);
  nav.appendChild(right);
  root.appendChild(nav);
}

function onboardStep1(root, index) {
  const will = el("ul", { class: "mm-ob__list" });
  for (const t of [
    "Read incoming mail and suggest how to file, tag, or prioritize it.",
    "Learn from your corrections — one click teaches it.",
    "Draft replies for you to review (only if you turn on a provider).",
  ])
    will.appendChild(el("li", { text: t }));
  const never = el("ul", { class: "mm-ob__list mm-ob__never" });
  never.appendChild(el("li", { text: "✕ Send a message on your behalf — drafts always wait for you." }));
  never.appendChild(el("li", { text: "✕ Delete mail — ever. A hard policy guard forbids it." }));
  const how = el("p", {
    text: "At first MailMate only suggests, and you approve each action. Once it has watched you do the same safe thing enough times, it asks to handle that one thing automatically — with one-tap Undo, and never for sending or deleting.",
  });
  onboardShell(
    root,
    index,
    "Your local-first mail assistant",
    [
      el("p", { text: "Everything runs on this machine. Here's what it will do for you:" }),
      will,
      el("p", { class: "mm-muted", text: "What it will never do:" }),
      never,
      how,
    ],
    finishOnboarding,
    () => renderOnboarding(1),
  );
}

function onboardStep2(root, index) {
  const card = el("div", { class: "mm-ob__card", text: "Checking the connection to MailMate's helper…" });
  onboardShell(root, index, "Connect the helper", [card], () => renderOnboarding(0), () => renderOnboarding(2));
  send({ type: "mm:getStatus" }).then((reply) => {
    const status = reply.status || { phase: PHASE.disconnected, reason: reply.error };
    clear(card);
    if (status.phase === PHASE.ready) {
      card.appendChild(el("div", { text: `● Connected   helper ${status.hostVersion || ""} · protocol ${status.protocol || ""}` }));
      card.appendChild(el("div", { class: "mm-muted", text: `✓ Handshake OK   retention: ${status.retention || "metadata"} (no message bodies stored by default)` }));
    } else {
      card.appendChild(el("div", { text: "● Not connected yet" }));
      card.appendChild(el("div", { class: "mm-muted", text: status.reason ? `Reason: ${status.reason}` : "The helper isn't responding." }));
      const retry = el("button", { class: "mm-btn", text: "↻ Retry" });
      retry.addEventListener("click", () => renderOnboarding(1));
      card.appendChild(retry);
      card.appendChild(el("p", { class: "mm-muted", text: "You can finish setup — classification begins once the helper connects." }));
    }
  });
}

function onboardStep3(root, index) {
  // Provider setup happens INLINE in this card — no jump out to the preferences tab (which looked
  // different, lost the wizard's place, and left this step's status stale). The card drives the
  // host directly and re-renders from its confirmed state after every change.
  const card = el("div", { class: "mm-ob__card" });
  onboardShell(
    root,
    index,
    "AI provider — optional",
    [
      el("p", {
        text: "MailMate sorts your mail — files, tags, prioritizes, tracks follow-ups — using rules that run on this machine, instantly, with no AI and nothing leaving your computer. An AI provider is optional: the one thing it adds is drafting replies for you to review.",
      }),
      el("p", {
        class: "mm-muted",
        text: "Off by default. Without one, “draft a reply” just says “drafting needs a provider” — nothing else changes. Set one up right here, or skip with Continue and add it later in Settings.",
      }),
      card,
    ],
    () => renderOnboarding(1),
    () => renderOnboarding(3),
  );
  renderProviderSetup(card);
}

function onboardStep4(root, index) {
  const list = el("ul", { class: "mm-ob__list" });
  for (const t of [
    "New mail gets a small MailMate panel in its header — open it to see the suggestion and correct it with one click.",
    "This MailMate space (left toolbar) is your dashboard: Review, Follow-ups, Proposals, and Activity.",
    "Right now everything is a suggestion. As you correct it, MailMate learns — and asks before handling anything on its own.",
  ])
    list.appendChild(el("li", { text: t }));
  onboardShell(
    root,
    index,
    "✓ You're set up",
    [el("p", { text: "MailMate is now watching for new mail. What happens now:" }), list, el("p", { class: "mm-muted", text: "Tip: the toolbar icon shows how many items are waiting for you." })],
    () => renderOnboarding(2),
    finishOnboarding,
    "Finish",
  );
}

// Render the live provider state into the onboarding card, then let the user enable or set one up
// without leaving the walkthrough. Re-called after every change so the card always shows the host's
// confirmed state (the old card polled once and went stale the moment a provider was added).
async function renderProviderSetup(card) {
  clear(card);
  card.appendChild(el("div", { class: "mm-muted", text: "Checking for an AI provider…" }));
  const reply = await send({ type: "mm:settings" });
  clear(card);
  const s = reply && reply.ok ? reply.settings : null;
  if (!s) {
    card.appendChild(el("div", { text: "◌ Provider status needs the helper — connect it (Step 2) to manage providers." }));
    return;
  }
  const providers = s.providers || [];
  const activeId = s.default_provider || null;

  if (activeId) {
    const p = providers.find((x) => x.id === activeId) || { id: activeId };
    card.appendChild(el("div", { text: `● Provider: ${p.id}${p.kind ? ` (${p.kind})` : ""} — reply drafting is on.` }));
    if (p.model) card.appendChild(el("div", { class: "mm-muted", text: `Model: ${p.model}` }));
    const change = el("button", { class: "mm-btn", text: "Change provider" });
    change.addEventListener("click", () => renderProviderForm(card, p));
    card.appendChild(el("div", { class: "mm-ob__formnav" }, [change]));
    return;
  }

  if (providers.length) {
    const p = providers[0];
    card.appendChild(el("div", { text: `◌ “${p.id}” is set up but not enabled for drafting yet.` }));
    const use = el("button", { class: "mm-btn mm-btn--primary", text: `Use “${p.id}” for drafting` });
    use.addEventListener("click", async () => {
      use.disabled = true;
      const r = await send({ type: "mm:setProvider", providerId: p.id, setDefault: true });
      if (r && r.ok) {
        toast("Provider enabled — drafting is on");
        renderProviderSetup(card);
      } else {
        use.disabled = false;
        toast((r && r.error) || "Couldn't enable that provider", true);
      }
    });
    const other = el("button", { class: "mm-btn", text: "Set up a different one" });
    other.addEventListener("click", () => renderProviderForm(card, null));
    card.appendChild(el("div", { class: "mm-ob__formnav" }, [use, other]));
    return;
  }

  renderProviderForm(card, null);
}

// The compact provider form: kind + endpoint + model (a dropdown for local endpoints) + an optional
// key for cloud. "Use this provider" writes it AND makes it the default in one step, so drafting is
// actually on afterwards — unlike the options page's Add, which leaves it non-default (a separate,
// confusing step). `editing` pre-fills from an existing provider; null is a fresh setup.
function renderProviderForm(card, editing) {
  clear(card);
  const initialKind = (editing && editing.kind) || "ollama";

  const kind = el("select");
  for (const k of OB_PROVIDER_KINDS) kind.appendChild(el("option", { text: k, attrs: { value: k } }));
  kind.value = initialKind;

  const endpoint = el("input", { attrs: { type: "text", placeholder: "http://localhost:11434" } });
  endpoint.value = (editing && editing.endpoint) || OB_KIND_DEFAULTS[initialKind] || "";

  let pickedModel = (editing && editing.model) || "";
  const model = obModelField(
    () => ({ kind: kind.value, endpoint: endpoint.value.trim() }),
    pickedModel,
    (v) => (pickedModel = v),
  );

  const key = el("input", { attrs: { type: "password", placeholder: "API key (cloud providers only)" } });
  const keyRow = obField("API key", key);
  const syncKey = () => (keyRow.hidden = obIsLocal(endpoint.value.trim()));
  syncKey();

  endpoint.addEventListener("change", () => {
    model.refresh();
    syncKey();
  });
  kind.addEventListener("change", () => {
    const cur = endpoint.value.trim();
    if (!cur || OB_KIND_DEFAULT_VALUES.has(cur)) {
      endpoint.value = OB_KIND_DEFAULTS[kind.value] || "";
    }
    model.refresh();
    syncKey();
  });

  const save = el("button", { class: "mm-btn mm-btn--primary", text: "Use this provider" });
  save.addEventListener("click", async () => {
    const ep = endpoint.value.trim();
    const providerId = (editing && editing.id) || kind.value;
    save.disabled = true;
    const r = await send({
      type: "mm:setProvider",
      providerId,
      kind: kind.value,
      endpoint: ep || undefined,
      model: (pickedModel || "").trim() || undefined,
      setDefault: true,
    });
    if (!r || !r.ok) {
      save.disabled = false;
      toast((r && r.error) || "Couldn't save the provider", true);
      return;
    }
    if (!obIsLocal(ep) && key.value) {
      const ks = await send({ type: "mm:setSecret", providerId, secret: key.value });
      if (!ks || !ks.ok) toast((ks && ks.error) || "Provider saved, but the key didn't", true);
    }
    toast("Provider enabled — drafting is on");
    renderProviderSetup(card);
  });

  const buttons = [save];
  if (editing) {
    const cancel = el("button", { class: "mm-btn", text: "Cancel" });
    cancel.addEventListener("click", () => renderProviderSetup(card));
    buttons.push(cancel);
  }

  card.appendChild(obField("Kind", kind));
  card.appendChild(obField("Endpoint", endpoint));
  card.appendChild(obField("Model", model.node));
  card.appendChild(keyRow);
  card.appendChild(el("div", { class: "mm-ob__formnav" }, buttons));
}

// A labeled field row for the inline provider form.
function obField(label, control) {
  return el("div", { class: "mm-ob__field" }, [el("label", { text: label }), control]);
}

// The Model control for onboarding: a <select> once a non-empty catalog is known for the current
// endpoint, a free-text <input> otherwise. Local (loopback) endpoints auto-list as soon as the URL
// is in place; a refresh button and a "type manually" escape always exist. Mirrors options.js's
// modelField, trimmed to what the walkthrough needs. probe() yields the live { kind, endpoint }.
function obModelField(probe, initialValue, onChange) {
  const node = el("div", { class: "mm-ob__model" });
  let value = initialValue || "";
  let manual = false;
  let listing = false;
  const set = (v) => {
    value = v;
    onChange(v);
  };

  async function discover(explicit) {
    const p = probe();
    if (listing || !p.endpoint) return;
    const key = obProbeKey(p);
    if (!explicit && obModelCache.has(key)) return; // already probed (success or empty) — don't repeat
    listing = true;
    paint();
    const reply = await send({ type: "mm:listModels", kind: p.kind, endpoint: p.endpoint, providerId: null });
    listing = false;
    const models = reply && reply.ok ? reply.models || [] : [];
    obModelCache.set(key, models);
    if (explicit) {
      if (!reply || !reply.ok) toast((reply && reply.error) || "Couldn't list models", true);
      else if (!models.length) toast(`No models found at ${p.endpoint}`, true);
      else toast(`${models.length} model${models.length === 1 ? "" : "s"} found`);
    }
    if (models.length && !value) set(models[0]);
    manual = false;
    paint();
  }

  function paint() {
    clear(node);
    const models = obModelCache.get(obProbeKey(probe())) || [];
    if (models.length && !manual) {
      const select = el("select");
      const opts = value && !models.includes(value) ? [value, ...models] : models;
      for (const m of opts) select.appendChild(el("option", { text: m, attrs: { value: m } }));
      select.value = value || models[0];
      if (select.value !== value) set(select.value);
      select.addEventListener("change", () => set(select.value));
      const refreshBtn = el("button", { class: "mm-btn mm-btn--icon", text: "↻", title: "Refresh model list" });
      refreshBtn.disabled = listing;
      refreshBtn.addEventListener("click", () => discover(true));
      const manualBtn = el("button", { class: "mm-linkbtn", text: "type manually" });
      manualBtn.addEventListener("click", () => {
        manual = true;
        paint();
      });
      node.append(select, refreshBtn, manualBtn);
    } else {
      const input = el("input", { attrs: { type: "text", placeholder: "model (e.g. llama3)" } });
      input.value = value;
      input.addEventListener("input", () => set(input.value));
      const listBtn = el("button", { class: "mm-btn", text: listing ? "Listing…" : "List models" });
      listBtn.disabled = listing;
      listBtn.addEventListener("click", () => discover(true));
      node.append(input, listBtn);
    }
  }

  function refresh() {
    manual = false;
    paint();
    maybeAuto();
  }
  function maybeAuto() {
    const p = probe();
    if (p.endpoint && obIsLocal(p.endpoint) && !obModelCache.has(obProbeKey(p))) discover(false);
  }

  paint();
  maybeAuto();
  return { node, refresh };
}

async function finishOnboarding() {
  try {
    await browser.storage.local.set({ "mm:onboarded": true });
  } catch {
    /* best-effort — a failed write just means the walkthrough shows again next time */
  }
  await enterApp();
}

// --- Deep-link ------------------------------------------------------------------------

async function deepLink(thunderbirdMessageId) {
  if (!thunderbirdMessageId) return;
  try {
    let [tab] = await browser.mailTabs.query({ currentWindow: true });
    if (!tab) tab = await browser.mailTabs.create();
    await browser.mailTabs.setSelectedMessages(tab.id, [Number(thunderbirdMessageId)]);
    await browser.tabs.update(tab.id, { active: true });
  } catch (e) {
    toast(`Couldn't open that message: ${String(e && e.message ? e.message : e)}`, true);
  }
}

// --- Shared rendering helpers ---------------------------------------------------------

function emptyState(glyph, title, lines) {
  const node = el("div", { class: "mm-empty" }, [
    el("div", { class: "mm-empty__glyph", text: glyph }),
    el("div", { class: "mm-empty__title", text: title }),
  ]);
  for (const line of lines) node.appendChild(el("p", { class: "mm-empty__body", text: line }));
  return node;
}

function riskOf(cls) {
  if (cls.needs_review || (cls.phishing_score || 0) >= 0.5 || (cls.spam_score || 0) >= 0.8) {
    return { level: "high", glyph: "⚠" };
  }
  if ((cls.priority || "") === "high") return { level: "med", glyph: "●" };
  return { level: "low", glyph: "○" };
}

function verdictLine(cls) {
  const labels = (cls.labels || []).join(", ") || "uncategorized";
  const band = confidenceBand(cls);
  return `${labels} · ${band}${cls.priority ? ` · priority ${cls.priority}` : ""}`;
}

// A banded confidence (never a raw score in the user's face), derived defensively from whichever
// scores the classification carries. Mirrors the panel's confidenceBand intent.
function confidenceBand(cls) {
  if (cls.needs_review) return "needs review";
  const top = Math.max(cls.spam_score || 0, cls.phishing_score || 0);
  const conf = top >= 0.5 ? top : 1 - top; // confidence in the leading call
  if (conf >= 0.9) return "very high confidence";
  if (conf >= 0.75) return "high confidence";
  if (conf >= 0.6) return "medium confidence";
  return "low confidence";
}

function actionText(action) {
  switch (action.kind) {
    case "move":
      return `File → ${folderName(action.to_folder)}`;
    case "tag":
      return `Tag “${action.tag}”`;
    case "mark_read":
      return "Mark read";
    case "mark_junk":
      return action.junk === false ? "Mark not junk" : "Mark as junk";
    case "flag":
      return "Flag";
    case "require_review":
      return action.target ? `Review: ${action.target}` : "Flagged for your review";
    case "create_draft":
      return "Draft a reply";
    default:
      return action.kind || "action";
  }
}

function ruleNote(action) {
  return action.applied_by_rule_id || action.rule_id ? `learned rule ${action.applied_by_rule_id || action.rule_id}` : "crystallized rule";
}

function folderName(folder) {
  if (!folder) return "a folder";
  const s = typeof folder === "string" ? folder : folder.path || folder.name || String(folder);
  const parts = s.split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : s;
}

function subjectOf(item) {
  return (item.headers && item.headers.subject) || (item.explanation && item.explanation.summary) || "(message)";
}

function fromOf(item) {
  return (item.headers && item.headers.from) || "";
}

function relativeTime(value) {
  const ms = toMillis(value);
  if (ms == null) return "";
  const delta = Date.now() - ms;
  if (delta < 0) return "just now";
  const s = Math.floor(delta / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return new Date(ms).toLocaleDateString();
}

function toMillis(value) {
  if (value == null) return null;
  if (typeof value === "number") return value > 1e12 ? value : value * 1000;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

// --- Live updates ---------------------------------------------------------------------

browser.runtime.onMessage.addListener((message) => {
  if (!message || typeof message.type !== "string") return false;
  // While the onboarding walkthrough owns the screen, it has its own connection step — don't let
  // a background status push pop the banner over it or mutate the hidden app.
  if ($("mm-app").hidden) return false;
  if (message.type === "mm:statusChanged") {
    const status = message.status || {};
    const was = lastPhase;
    lastPhase = status.phase;
    if (status.phase !== PHASE.ready) {
      renderBanner(status);
    } else if (was && was !== PHASE.ready) {
      // Recovered: clear the banner and re-pull, since we may have missed events while down.
      renderBanner(status);
      refreshSettings().then(() => renderTab(activeTab));
    }
    return false;
  }
  if (message.type === "mm:dashboardEvent") {
    if (message.event === "review") {
      // Refresh counts always, and coalesce a Review re-render so a burst of new mail can't yank
      // a card out mid-interaction.
      refreshReviewCount();
      if (activeTab === "review") scheduleReviewRefresh();
    } else if (message.event === "proposals") {
      // A new proposal arrived — refresh the badge always, rebuild cards only if the tab is open.
      refreshProposalCount();
      if (activeTab === "proposals") renderProposals();
      // The same-session crystallization "aha": a freshly-learned rule just appeared. Celebrate it
      // by name (the host dedups, so each proposal_ready is a genuinely new learning), and point
      // the user at where to review it — unless they're already on the Proposals tab.
      if (message.title) {
        toast(
          activeTab === "proposals"
            ? `MailMate just learned: ${message.title}`
            : `MailMate just learned: ${message.title} — see Proposals`,
        );
      }
    } else if (message.event === "followups") {
      // A follow-up came due / went stale — refresh the badge always, rebuild if the tab is open.
      refreshFollowupCount();
      if (activeTab === "followups") renderFollowups();
    } else if (message.event === "focusTab" && VALID_TABS.includes(message.tab)) {
      // A desktop-notification click asked us to focus a specific tab (warm path — the dashboard
      // was already open). Clear the durable stash too so the cold-open consumer can't re-fire it.
      browser.storage.session.remove(FOCUS_TAB_KEY).catch(() => {});
      selectTab(message.tab);
    }
    return false;
  }
  return false;
});

// Coalesce Review re-renders: at most one pending, fired on the next tick, and never while the
// user is mid-action on an open card (a card with a disabled — in-flight — button).
let reviewRefreshTimer = null;
function scheduleReviewRefresh() {
  if (reviewRefreshTimer) return;
  reviewRefreshTimer = setTimeout(() => {
    reviewRefreshTimer = null;
    if (activeTab !== "review") return;
    // A transiently-disabled button means an action is mid-flight on some card; don't rebuild
    // under it. Statically-disabled buttons (e.g. an unavailable move-Undo) carry data-static
    // and are ignored, so they can't stall the refresh forever.
    if (content().querySelector("button:disabled:not([data-static])")) {
      scheduleReviewRefresh();
      return;
    }
    renderReview();
  }, 400);
}

boot().catch((e) => {
  const c = content();
  clear(c);
  c.appendChild(emptyState("⚠", "MailMate dashboard failed to load", [String(e && e.message ? e.message : e)]));
});
