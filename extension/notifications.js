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
// router; this file owns the browser.notifications surface + click routing, plus a light dedup so
// the same item never double-pings. Quiet-hours and batching are honest deferrals (noted below);
// the dedup + deep-link spine is here.

/* exported showDesktopNotification */
/* global spaceId */

"use strict";

// notificationId → the dashboard tab to focus when the user clicks it. Bounded + reaped on close
// so OS-dismissed/expired (never-clicked) notifications don't accumulate entries.
const MM_NOTIF_TARGET = new Map();
const MM_NOTIF_TARGET_CAP = 200;
// Recently-notified item keys (stable per item), so a re-drain doesn't double-ping. Bounded.
const MM_NOTIF_SEEN = new Set();
const MM_NOTIF_SEEN_CAP = 200;

// Map a host notification into a desktop notification (or skip it). Returns nothing; failures are
// swallowed — a missing notification must never break the host's event handling.
function showDesktopNotification(type, payload) {
  const spec = mmNotificationSpec(type, payload);
  if (!spec) return; // not a user-facing notification class
  if (spec.dedupKey) {
    if (MM_NOTIF_SEEN.has(spec.dedupKey)) return;
    MM_NOTIF_SEEN.add(spec.dedupKey);
    if (MM_NOTIF_SEEN.size > MM_NOTIF_SEEN_CAP) {
      // Drop the oldest-ish key (Set preserves insertion order).
      MM_NOTIF_SEEN.delete(MM_NOTIF_SEEN.values().next().value);
    }
  }
  browser.notifications
    .create({
      type: "basic",
      iconUrl: browser.runtime.getURL("icons/mailmate.svg"),
      title: spec.title,
      message: spec.message,
    })
    .then((id) => {
      MM_NOTIF_TARGET.set(id, spec.tab);
      if (MM_NOTIF_TARGET.size > MM_NOTIF_TARGET_CAP) {
        MM_NOTIF_TARGET.delete(MM_NOTIF_TARGET.keys().next().value); // FIFO evict oldest
      }
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
