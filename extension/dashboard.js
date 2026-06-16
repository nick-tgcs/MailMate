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
const VALID_TABS = ["review", "followups", "proposals", "activity"];

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
  $("mm-settings").addEventListener("click", async () => {
    try {
      await browser.runtime.openOptionsPage();
    } catch {
      toast("The Settings page lands with configuration (Milestone 4)", true);
    }
  });
  $("mm-pause").addEventListener("click", togglePause);
  $("mm-provider").addEventListener("click", async () => {
    try {
      await browser.runtime.openOptionsPage();
    } catch {
      toast("Provider setup lands with configuration (Milestone 4)", true);
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

  if (!items.length) {
    c.appendChild(
      emptyState("✓", "Inbox triaged — nothing waiting", [
        "Crystallized rules file safely in the background.",
        "New suggestions appear here whenever MailMate isn't yet sure — correct them in one click and it learns.",
      ]),
    );
    return;
  }

  for (const item of items) c.appendChild(reviewCard(item));
}

function reviewCard(item) {
  const cls = item.classification || {};
  const tbId = item.thunderbird_message_id;
  const decisionId = item.decision_id;
  const card = el("div", { class: "mm-card" });

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
    const approveAll = el("button", { class: "mm-btn mm-btn--primary", text: "Approve all safe" });
    approveAll.addEventListener("click", () => approveAllSafe(suggestions, decisionId, tbId, card, item));
    actions.appendChild(approveAll);
  }
  const dismiss = el("button", { class: "mm-btn", text: "Dismiss" });
  dismiss.addEventListener("click", () => dismissCard(item, card));
  actions.appendChild(dismiss);
  const explain = el("button", { class: "mm-btn", text: "Explain" });
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
  toast("Dismissed");
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

function proposalCard(p) {
  const risk = (p.risk_level || "low").toLowerCase();
  const card = el("div", { class: "mm-card" });
  card.appendChild(
    el("div", { class: "mm-card__top" }, [
      el("span", { class: `mm-glyph mm-glyph--${risk === "high" ? "high" : risk === "medium" ? "med" : "low"}`, text: "◆" }),
      el("span", { class: "mm-card__title", text: p.title || p.proposal_type }),
      el("span", { class: "mm-card__from", text: `${p.proposal_type} · ${risk} risk` }),
    ]),
  );
  card.appendChild(el("p", { class: "mm-card__sub", text: p.rationale || "" }));
  card.appendChild(
    el("p", { class: "mm-card__sub mm-muted", text: `Recommended: ${p.recommended_status || "review"}` }),
  );
  card.appendChild(el("hr", { class: "mm-card__hr" }));

  // Every approval materializes the rule into its recommended status (shadow / pending-review) —
  // a rule that runs and logs but never acts on its own until you later promote it. There is no
  // one-click "→ active": activation is a separate, deliberate step, so even a HIGH-risk approval
  // here is safe (the rule shadows, it does not act). Rejection feeds the curator's negative signal.
  const actions = el("div", { class: "mm-card__actions" });
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
  // The provider card is filled in async from the host's REAL settings — never hardcode
  // "no provider", which would be a lie the moment one is configured.
  const card = el("div", { class: "mm-ob__card", text: "Checking for an AI provider…" });
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
        text: "Those rules come from you. MailMate watches how you file and correct mail, and when it sees the same safe choice enough times — say, three messages from one sender moved to the same folder — it proposes a rule in the Proposals tab for you to approve. It never activates one on its own; as approved rules prove out, it starts handling that pattern automatically — one-tap Undo, and never for sending or deleting. None of this needs a provider.",
      }),
      card,
      el("p", {
        class: "mm-muted",
        text: "A provider is off by default. Without one, “draft a reply” simply says “drafting needs a provider” instead of failing — nothing else changes. You can add or change one anytime in Settings.",
      }),
    ],
    () => renderOnboarding(1),
    () => renderOnboarding(3),
  );
  send({ type: "mm:settings" }).then((reply) => {
    clear(card);
    const s = reply && reply.ok ? reply.settings : null;
    const providers = (s && s.providers) || [];
    const active = s && s.default_provider;
    let configured = false;
    let line;
    if (!s) {
      line = "◌ Provider status needs the helper — connect it to manage providers";
    } else if (active) {
      const p = providers.find((x) => x.id === active);
      line = `● Provider: ${active}${p && p.kind ? ` (${p.kind})` : ""} — reply drafting available`;
      configured = true;
    } else if (providers.length) {
      line = "◌ A provider is added but none is set as default — pick one in Settings to enable drafting";
    } else {
      line = "◌ No provider configured — reply drafting is off (everything else works)";
    }
    card.appendChild(el("div", { text: line }));
    const setup = el("button", {
      class: "mm-btn",
      text: configured ? "Manage in Settings →" : "Set up a provider →",
    });
    setup.addEventListener("click", async () => {
      try {
        await browser.runtime.openOptionsPage();
      } catch {
        toast("Open MailMate's Settings to add a provider", true);
      }
    });
    card.appendChild(setup);
  });
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
