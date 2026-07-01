// notifications.js — the ambient, time-driven desktop-notification layer.
//
// The dashboard space is the source of truth for "what needs me"; desktop notifications are
// narrow pointers that surface a time-driven event when the user isn't looking at MailMate, then
// deep-link back INTO the dashboard space (the right tab) on click. They never act on mail.
//
// Three notification classes (interaction-design.md §Notifications), all host→extension pushes:
//   • proposal_ready          → a learned rule is waiting for approval     → Proposals tab
//   • followup_draft_ready    → a follow-up draft is ready to review       → Follow-ups tab
//   • followup_needs_attention→ a tracked deal went stale                  → Follow-ups tab
//
// background.js owns the native port and calls showDesktopNotification() from its notification
// router; this file owns the browser.notifications surface + click routing. It applies four
// policies before a ping reaches the tray:
//   1. per-class toggles   — the user can silence a whole class
//   2. persistent dedup    — a stable per-item key in storage.local LRU, so a re-drain (or an
//                            event-page suspension + revive) never double-pings the same item
//   3. quiet hours         — inside the window, the badge/in-app still update but no ping fires
//   4. batching            — a burst (e.g. the catch-up drain) is coalesced into ONE digest, not
//                            a storm

/* exported showDesktopNotification, flushNotifications, withinQuietHours, digestSpec,
   classEnabled, loadNotifPrefs */
/* global spaceId */

"use strict";

// notificationId → the dashboard tab to focus when the user clicks it. Bounded + reaped on close
// so OS-dismissed/expired (never-clicked) notifications don't accumulate entries. In-memory click
// routing only (a suspension drops it; a click then opens the dashboard home — an acceptable edge).
const MM_NOTIF_TARGET = new Map();
const MM_NOTIF_TARGET_CAP = 200;

// --- Dedup: a fast in-memory gate + a durable mark-at-delivery -------------------------------
// Two layers, on purpose:
//   • MM_SEEN_MEM (in-memory) — same-session dedup, marked immediately at the GATE. Lost on an
//     event-page suspension, which is exactly right: a suspension must NOT seal an item.
//   • mm:notifSeen (storage.local LRU) — durable cross-session/cross-suspension dedup, marked ONLY
//     when a notification actually reaches the tray (mark-at-delivery). If a batch is dropped by a
//     mid-window suspension it was never delivered, so it is NOT sealed, and the next re-drain
//     pings it — the suspension can never silently swallow an item.
const MM_SEEN_MEM = new Set();
const MM_SEEN_MEM_CAP = 500;
const NOTIF_SEEN_KEY = "mm:notifSeen";
const NOTIF_SEEN_CAP = 200;

function memSeen(key) {
  return MM_SEEN_MEM.has(key);
}
function memMark(key) {
  MM_SEEN_MEM.add(key);
  if (MM_SEEN_MEM.size > MM_SEEN_MEM_CAP) {
    MM_SEEN_MEM.delete(MM_SEEN_MEM.values().next().value); // FIFO evict oldest
  }
}

async function persistentlySeen(key) {
  try {
    const got = await browser.storage.local.get(NOTIF_SEEN_KEY);
    const seen = got[NOTIF_SEEN_KEY];
    return Array.isArray(seen) && seen.includes(key);
  } catch {
    return false; // can't read → don't suppress (better a dup than a missed ping)
  }
}

// Persist the delivered keys, serialized behind one chain so concurrent deliveries can't lose a
// key to a read-modify-write race (the single-writer pattern, mirrored from the Rust side).
let mmSeenChain = Promise.resolve();
function markPersistentSeen(keys) {
  const fresh = keys.filter(Boolean);
  if (!fresh.length) return mmSeenChain;
  mmSeenChain = mmSeenChain.then(() => persistSeenKeys(fresh)).catch(() => {});
  return mmSeenChain;
}
async function persistSeenKeys(keys) {
  let seen = [];
  try {
    const got = await browser.storage.local.get(NOTIF_SEEN_KEY);
    seen = Array.isArray(got[NOTIF_SEEN_KEY]) ? got[NOTIF_SEEN_KEY] : [];
  } catch {
    return;
  }
  let changed = false;
  for (const k of keys) {
    if (!seen.includes(k)) {
      seen.push(k);
      changed = true;
    }
  }
  if (!changed) return;
  if (seen.length > NOTIF_SEEN_CAP) seen = seen.slice(-NOTIF_SEEN_CAP);
  try {
    await browser.storage.local.set({ [NOTIF_SEEN_KEY]: seen });
  } catch {
    /* best-effort — a write failure just risks a future dup, never a crash */
  }
}

// --- Preferences: per-class toggles + quiet hours -------------------------------------------
const NOTIF_PREFS_KEY = "mm:notifPrefs";
const DEFAULT_NOTIF_PREFS = {
  classes: { proposal_ready: true, followup_draft_ready: true, followup_needs_attention: true },
  quietHours: { enabled: false, start: "22:00", end: "07:00" },
};

async function loadNotifPrefs() {
  try {
    const got = await browser.storage.local.get(NOTIF_PREFS_KEY);
    const p = got[NOTIF_PREFS_KEY];
    if (!p) return DEFAULT_NOTIF_PREFS;
    return {
      classes: { ...DEFAULT_NOTIF_PREFS.classes, ...(p.classes || {}) },
      quietHours: { ...DEFAULT_NOTIF_PREFS.quietHours, ...(p.quietHours || {}) },
    };
  } catch {
    return DEFAULT_NOTIF_PREFS;
  }
}

// A class is enabled unless explicitly turned off (default-on for an unknown class).
function classEnabled(prefs, type) {
  return !prefs || !prefs.classes || prefs.classes[type] !== false;
}

// Whether `nowMinutes` (minutes since local midnight) falls in the quiet window. Handles a
// same-day window (start < end) and an overnight wrap (start > end, e.g. 22:00→07:00). Pure.
function withinQuietHours(nowMinutes, quiet) {
  if (!quiet || !quiet.enabled) return false;
  const start = hhmmToMinutes(quiet.start);
  const end = hhmmToMinutes(quiet.end);
  if (start == null || end == null || start === end) return false;
  if (start < end) return nowMinutes >= start && nowMinutes < end;
  return nowMinutes >= start || nowMinutes < end; // overnight wrap
}

function hhmmToMinutes(s) {
  if (typeof s !== "string") return null;
  const m = /^(\d{1,2}):(\d{2})$/.exec(s.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  if (h > 23 || min > 59) return null;
  return h * 60 + min;
}

function nowMinutes() {
  const d = new Date();
  return d.getHours() * 60 + d.getMinutes();
}

// --- Batching: a burst becomes ONE digest, not a storm --------------------------------------
// A fixed window (not a resetting debounce) from the FIRST queued spec, so a steady trickle still
// pings individually but a catch-up storm coalesces. `MM_BATCH_MS` is a var so a test can shrink it.
var MM_BATCH_MS = 1200; // eslint-disable-line no-var
let MM_PENDING = [];
let MM_BATCH_TIMER = null;

function queueNotification(spec) {
  // In-batch dedup: the same item queued twice in one window shouldn't inflate the digest count.
  if (spec.dedupKey && MM_PENDING.some((s) => s.dedupKey === spec.dedupKey)) return;
  MM_PENDING.push(spec);
  if (MM_BATCH_TIMER == null) {
    MM_BATCH_TIMER = setTimeout(flushNotifications, MM_BATCH_MS);
  }
}

// Emit the pending batch: a single spec pings on its own; two or more coalesce into one digest.
// The constituent specs ride along so their dedup keys are sealed ONLY now (mark-at-delivery).
function flushNotifications() {
  if (MM_BATCH_TIMER != null) {
    clearTimeout(MM_BATCH_TIMER);
    MM_BATCH_TIMER = null;
  }
  const specs = MM_PENDING;
  MM_PENDING = [];
  if (!specs.length) return;
  createDesktopNotification(specs.length === 1 ? specs[0] : digestSpec(specs), specs);
}

// Summarize N specs into one digest: counts per class + the dominant tab as the click target.
function digestSpec(specs) {
  const byTab = {};
  for (const s of specs) byTab[s.tab] = (byTab[s.tab] || 0) + 1;
  const label = { proposals: "rule proposal", followups: "follow-up", review: "suggestion" };
  const parts = [];
  for (const tab of ["proposals", "followups", "review"]) {
    if (byTab[tab]) parts.push(`${byTab[tab]} ${label[tab]}${byTab[tab] > 1 ? "s" : ""}`);
  }
  let tab = specs[0].tab;
  let best = 0;
  for (const t of Object.keys(byTab)) {
    if (byTab[t] > best) {
      best = byTab[t];
      tab = t;
    }
  }
  return {
    title: `MailMate — ${specs.length} updates`,
    message: `${parts.join(" · ")}. Open MailMate to review.`,
    tab,
  };
}

// Map a host notification into a desktop notification (or skip it), applying every policy. Async
// (it reads prefs + the persistent dedup store); failures are swallowed — a missing notification
// must never break the host's event handling.
async function showDesktopNotification(type, payload) {
  try {
    const spec = mmNotificationSpec(type, payload);
    if (!spec) return; // not a user-facing notification class
    const prefs = await loadNotifPrefs();
    if (!classEnabled(prefs, type)) return; // (1) per-class toggle off
    if (spec.dedupKey) {
      // (2) dedup gate — fast in-memory first, then the durable store. NOT sealed here; the durable
      // seal happens only at delivery, so a dropped/quiet-suppressed item can still ping later.
      if (memSeen(spec.dedupKey) || (await persistentlySeen(spec.dedupKey))) return;
      memMark(spec.dedupKey); // same-session only; a suspension intentionally forgets it
    }
    if (withinQuietHours(nowMinutes(), prefs.quietHours)) return; // (3) quiet hours: badge only, no ping
    queueNotification(spec); // (4) batched
  } catch (e) {
    console.warn("[MailMate] notification failed:", e);
  }
}

// Actually create the desktop notification, record its click target, and ONLY NOW durably seal the
// delivered specs' dedup keys (mark-at-delivery) — so an item that never reached the tray (a
// suspension-dropped batch, a quiet-hours skip) is never sealed and a later re-drain can ping it.
function createDesktopNotification(out, specs) {
  browser.notifications
    .create({
      type: "basic",
      iconUrl: browser.runtime.getURL("icons/mailmate.svg"),
      title: out.title,
      message: out.message,
    })
    .then((id) => {
      MM_NOTIF_TARGET.set(id, out.tab);
      if (MM_NOTIF_TARGET.size > MM_NOTIF_TARGET_CAP) {
        MM_NOTIF_TARGET.delete(MM_NOTIF_TARGET.keys().next().value); // FIFO evict oldest
      }
      markPersistentSeen((specs || [out]).map((s) => s.dedupKey));
    })
    .catch((e) => console.warn("[MailMate] notification create failed:", e));
}

// The title/message/target-tab/dedup-key for each host notification class, or null to skip.
function mmNotificationSpec(type, payload) {
  if (type === "proposal_ready") {
    const risk = payload.risk_level ? ` (${payload.risk_level} risk)` : "";
    return {
      title: "MailMate — new rule to approve",
      message: `${payload.title || "A learned rule"}${risk}. Open Proposals to review.`,
      tab: "proposals",
      dedupKey: payload.proposal_id ? `prop:${payload.proposal_id}` : null,
    };
  }
  if (type === "followup_draft_ready") {
    const subject = (payload.draft && payload.draft.subject) || "a tracked deal";
    return {
      title: "MailMate — follow-up draft ready",
      message: `Review the draft for “${subject}” — MailMate never sends it for you.`,
      tab: "followups",
      dedupKey:
        payload.workflow_instance_id != null
          ? `fu:${payload.workflow_instance_id}:${payload.step_index}`
          : null,
    };
  }
  if (type === "followup_needs_attention") {
    return {
      title: "MailMate — follow-up needs attention",
      message: `A tracked deal went stale (${payload.reason || "no recent activity"}). Decide what's next.`,
      tab: "followups",
      dedupKey: payload.workflow_instance_id != null ? `att:${payload.workflow_instance_id}` : null,
    };
  }
  // Everything else (classification_ready, mail_command, …) is not a desktop-notification class —
  // those drive the in-app surfaces, not the OS notification tray.
  return null;
}

// On click, open the MailMate space and ask the dashboard to focus the relevant tab. The
// dashboard space is the home every satellite deep-links back into.
browser.notifications.onClicked.addListener(async (id) => {
  const tab = MM_NOTIF_TARGET.get(id);
  MM_NOTIF_TARGET.delete(id);
  try {
    // Stash the wanted tab durably FIRST: a cold space-open loads the dashboard fresh and its
    // listener isn't ready for the fire-and-forget message below, so enterApp consumes this
    // instead. The warm path (dashboard already open) is handled by the message + clears it.
    if (tab) await browser.storage.session.set({ "mm:focusTab": tab }).catch(() => {});
    // `spaceId` is owned by background.js (shared background scope). When present, focus the
    // existing space tab; otherwise fall back to opening the dashboard document directly.
    if (typeof spaceId === "number") {
      await browser.spaces.open(spaceId);
    } else {
      await browser.tabs.create({ url: browser.runtime.getURL("dashboard.html") });
    }
    if (tab) {
      browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "focusTab", tab }).catch(() => {});
    }
  } catch (e) {
    console.warn("[MailMate] notification click open failed:", e);
  }
  browser.notifications.clear(id).catch(() => {});
});

// Reap the id→tab mapping when the OS dismisses or expires a notification the user never clicked,
// so the Map can't grow while the event page stays awake (mirrors the SEEN-set cap).
browser.notifications.onClosed.addListener((id) => {
  MM_NOTIF_TARGET.delete(id);
});
