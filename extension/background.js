// background.js — the MailMate event page.
//
// Wires the host capabilities to the native host:
//   1. read selected message      -> classify_message (context menu)
//   2. listen to new mail events   -> new_mail (messages.onNewMailReceived)
//   3. context-menu actions        -> context_menu.js
//   4. open draft replies          -> draft_reply response -> compose draft
//   5. apply safe actions          -> classification_ready / mail_command -> drafts.js
//   6. record user actions/results -> record_user_action
//   7. connection health           -> hello handshake -> HostStatus -> toolbar badge
//   8. per-message panel            -> mm:classify / mm:apply / mm:dismiss / mm:undo /
//                                      mm:correctLabel / mm:notJunk / mm:move / mm:folders
//   9. dashboard space              -> spaces.create + aggregate badge; review-queue buffer;
//                                      mm:reviewQueue / mm:resolveReview / mm:listActivity /
//                                      mm:listProposals / mm:settings / mm:setPause
//
// It is also the single owner of the native port: the popups (the toolbar recovery card and the
// per-message panel) and the dashboard space never open their own port — they ask the background
// over `browser.runtime` messaging, keeping one single-writer channel.
//
// onNewMailReceived is registered synchronously at the top of the event page so a wake-up
// from a new message is not missed.

/* global NativeHost, HOST_PHASE, registerContextMenus, readMessageForHost, applyPlannedAction,
   openDraftFromResponse, executeMailCommand, consumeHostMove, openFollowupDraft,
   surfaceNeedsAttention, showDesktopNotification */

const host = new NativeHost();

// --- Connection health: HostStatus -> toolbar badge + popup broadcast ------------------
// Every connection surface derives from this one status, so they can never disagree. The
// toolbar `action` badge is the always-on indicator; open popups also get a live push.
host.onStatusChange((status) => {
  updateToolbarBadge(status);
  // Push to any open popup or dashboard; harmless to fail if none is listening.
  browser.runtime.sendMessage({ type: "mm:statusChanged", status }).catch(() => {});
  // The aggregate space badge folds in the pending-proposal count, which needs a live host, so
  // recompute whenever the connection state changes.
  recomputeSpaceBadge();
});

// Map the connection phase onto the toolbar button's badge + tooltip (interaction-design.md
// §"Connection health"). Disconnected/mismatch raise a visible "!"; connecting is a muted
// "…"; ready is calm (no badge).
function updateToolbarBadge(status) {
  const byPhase = {
    [HOST_PHASE.ready]: { text: "", color: "#2e7d32", title: "MailMate — connected" },
    [HOST_PHASE.connecting]: { text: "…", color: "#9e9e9e", title: "MailMate — connecting…" },
    [HOST_PHASE.disconnected]: {
      text: "!",
      color: "#c0392b",
      title: "MailMate — not connected (click to reconnect)",
    },
    [HOST_PHASE.versionMismatch]: {
      text: "!",
      color: "#e67e22",
      title: "MailMate — version mismatch (update one side)",
    },
  };
  const badge = byPhase[status.phase] || byPhase[HOST_PHASE.connecting];
  browser.action.setBadgeText({ text: badge.text });
  browser.action.setBadgeBackgroundColor({ color: badge.color });
  browser.action.setTitle({ title: badge.title });
}

// --- Popup <-> background request router -----------------------------------------------
// The popups (toolbar recovery card, per-message panel) are separate documents with no native
// port. They drive the host through these messages, so the background stays the single port
// owner. Each handler returns a plain object (or a promise of one); unknown types return false
// so other listeners can claim them.
const POPUP_HANDLERS = {
  "mm:getStatus": () => ({ status: host.status }),
  "mm:reconnect": () => ({ status: host.reconnect() }),
  "mm:classify": (m) => classifyForPanel(m.messageId),
  "mm:apply": (m) => applySuggestion(m),
  "mm:dismiss": (m) => dismissSuggestion(m),
  "mm:undo": (m) => undoAuto(m),
  "mm:correctLabel": (m) => correctLabel(m),
  "mm:notJunk": (m) => markNotJunk(m),
  "mm:move": (m) => moveMessage(m),
  "mm:folders": () => listFolders(),
  // Dashboard space.
  "mm:reviewQueue": () => getReviewQueue(),
  "mm:resolveReview": (m) => resolveReview(m.decisionId),
  "mm:listActivity": (m) =>
    hostCall("list_recent_activity", { limit: m.limit || 80, event_type_filter: m.eventTypeFilter || null }),
  "mm:listProposals": () => hostCall("list_pending_reviews", {}),
  "mm:reviewProposal": (m) =>
    hostCall("review_rule_proposal", {
      proposal_id: m.proposalId,
      decision: m.decision,
      reason_code: m.reasonCode || null,
    }),
  "mm:settings": () => hostCall("get_settings", {}, (r) => ({ ok: true, settings: r })),
  "mm:setPause": (m) => hostCall("set_pause", { paused: Boolean(m.paused) }),
};

// A guarded host round-trip for the dashboard's read/write requests. Returns the host payload
// merged onto { ok:true } (or a custom mapper's shape); a disconnected host or a verb this build
// doesn't speak resolves to { ok:false, error } so the dashboard degrades, never lies.
async function hostCall(type, payload, mapper) {
  if (host.status.phase !== HOST_PHASE.ready) {
    return { ok: false, error: "MailMate host not connected", reason: "host_not_ready" };
  }
  try {
    const result = await host.request(type, payload || {});
    return mapper ? mapper(result) : { ok: true, ...result };
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
}

browser.runtime.onMessage.addListener((message) => {
  const handler = message && POPUP_HANDLERS[message.type];
  if (!handler) {
    return false; // not ours — let other listeners (if any) handle it
  }
  // Normalize ANY handler rejection into a uniform { ok:false, error } so the popup's awaited
  // send() never throws and a button is never left stuck-disabled with no feedback. The panel
  // adds the same guard for the case the event page is asleep and this listener isn't reached.
  // "Degrade, never lie" — the user always learns the outcome.
  return Promise.resolve()
    .then(() => handler(message))
    .catch((e) => ({ ok: false, error: errMessage(e) }));
});

function errMessage(e) {
  return String(e && e.message ? e.message : e);
}

// Run a best-effort learning record after a VISIBLE action already succeeded. A failed record
// (host error / dropped port) must not report the action as failed — the mutation happened — so
// we log the lost signal rather than lying that nothing occurred.
async function recordBestEffort(fn, what) {
  try {
    await fn();
  } catch (e) {
    console.warn(`[MailMate] action done but failed to record ${what}:`, e);
  }
}

// Read + classify the panel's displayed message. Returns the raw classify_message payload plus
// the retention posture the panel renders its "headers only" notice from. Guards on the host
// being ready so the panel can show the recovery card instead of a misleading failure.
async function classifyForPanel(messageId) {
  if (host.status.phase !== HOST_PHASE.ready) {
    return { ok: false, reason: "host_not_ready", status: host.status };
  }
  try {
    const header = await browser.messages.get(Number(messageId));
    const payload = await readMessageForHost(header);
    const result = await host.request("classify_message", payload);
    return { ok: true, result, bodyRetentionAllowed: payload.body_retention_allowed };
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
}

// Apply a suggested SAFE action locally, then record the acceptance. Accepting a suggestion is
// exactly the positive signal the learning loop needs to one day crystallize it into an auto
// rule (interaction-design.md §Apply). applyPlannedAction catches its own mail-op errors and
// returns a result object, so the only thing that can fail loudly is the best-effort record.
async function applySuggestion({ action, decisionId, messageId }) {
  const result = await applyPlannedAction(action);
  if (result.event_type !== "action_applied") {
    return { ok: false, error: result.result || "couldn't apply that action" };
  }
  await recordBestEffort(
    () =>
      host.request("record_user_action", {
        event_type: "action_applied",
        source: "suggestion_accepted",
        decision_id: decisionId,
        action_kind: action.kind,
        thunderbird_message_id: String(messageId),
        user_initiated: true,
      }),
    "suggestion acceptance",
  );
  return { ok: true };
}

// Dismiss a suggestion: record the negative signal (ignore/reject). No mail mutation — there is
// no chosen label or folder, so the host routes this to audit, never a fabricated feedback row.
// Nothing visible happened, so a failed record IS the failure and is reported as such.
async function dismissSuggestion({ decisionId, actionKind, messageId }) {
  try {
    await host.request("record_user_action", {
      event_type: "suggestion_dismissed",
      decision_id: decisionId,
      action_kind: actionKind,
      thunderbird_message_id: String(messageId),
      user_initiated: true,
    });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  return { ok: true };
}

// Reverse an auto-applied crystallized action locally, then record the undo (the strongest
// rule-demotion signal). A move needs its prior folder, which rides the host's apply_state
// enrichment as `reverses_to`; absent that we cannot safely reverse a move, so we say so.
async function undoAuto({ action, decisionId, messageId }) {
  let reverse;
  switch (action.kind) {
    case "tag":
      reverse = { kind: "untag", message_id: action.message_id, tag: action.tag };
      break;
    case "mark_junk":
      reverse = { kind: "mark_junk", message_id: action.message_id, junk: !action.junk };
      break;
    case "move":
      if (!action.reverses_to) {
        return { ok: false, error: "no prior folder to undo the move" };
      }
      reverse = { kind: "move", message_id: action.message_id, to_folder: action.reverses_to };
      break;
    default:
      // require_review / create_draft (and anything else) are not reversible mail mutations.
      return { ok: false, error: `cannot undo ${action.kind}` };
  }
  const result = await applyReverse(reverse);
  if (result.event_type !== "action_applied") {
    return { ok: false, error: result.result };
  }
  await recordBestEffort(
    () =>
      host.request("record_user_action", {
        event_type: "action_undone",
        decision_id: decisionId,
        rule_id: action.rule_id || null,
        action_kind: action.kind,
        thunderbird_message_id: String(messageId),
        user_initiated: true,
      }),
    "undo",
  );
  return { ok: true };
}

// Reverse helper: "untag" removes a tag (the inverse of applyPlannedAction's tag); everything
// else is a normal safe action applyPlannedAction already performs.
async function applyReverse(reverse) {
  if (reverse.kind === "untag") {
    try {
      const raw = String(reverse.message_id).replace(/^msg_tb_/, "");
      const numeric = Number(raw);
      const id = Number.isNaN(numeric) ? raw : numeric;
      const header = await browser.messages.get(id);
      const tags = (header.tags || []).filter((t) => t !== reverse.tag);
      await browser.messages.update(id, { tags });
      return { event_type: "action_applied", result: "ok" };
    } catch (e) {
      return { event_type: "action_failed", result: errMessage(e) };
    }
  }
  return applyPlannedAction(reverse);
}

// Wrong-category correction: routes to classification_feedback via the host's CorrectLabel
// vocabulary entry (corrected_label is the chosen label; prior_label is the AI's override).
async function correctLabel({ decisionId, messageId, label, priorLabel }) {
  try {
    await host.request("record_user_action", {
      event_type: "classification_corrected",
      decision_id: decisionId,
      thunderbird_message_id: String(messageId),
      corrected_label: label,
      prior_label: priorLabel || null,
      user_initiated: true,
    });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  return { ok: true };
}

// "Not junk" / "This is legitimate": flip the junk flag (the visible effect), then record the
// correction best-effort — a lost record must not be reported as a failed correction.
async function markNotJunk({ messageId }) {
  const id = Number(messageId);
  try {
    await browser.messages.update(id, { junk: false });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  await recordBestEffort(() => recordJunkChanged(id, false), "junk correction");
  return { ok: true };
}

// Trigger a real move; the onMoved listener below records the message_moved filing correction
// (this is NOT a host-commanded move, so it is not suppressed). One genuine interaction.
async function moveMessage({ messageId, folder }) {
  try {
    await browser.messages.move([Number(messageId)], folder);
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  return { ok: true };
}

// Flat, file-able folder list for the panel's Move picker. Degrades to an empty list (the
// picker shows "No folders available") rather than throwing.
async function listFolders() {
  try {
    const folders = await browser.folders.query({ canFileMessages: true });
    const accounts = await browser.accounts.list(false);
    const nameById = new Map(accounts.map((a) => [a.id, a.name]));
    return {
      folders: folders.map((f) => ({
        accountId: f.accountId,
        path: f.path,
        name: f.name,
        accountName: nameById.get(f.accountId) || "",
      })),
    };
  } catch (e) {
    return { folders: [], error: String(e) };
  }
}

// Record a junk/not-junk correction (the host.request leg, shared by the context menu and the
// panel's markNotJunk).
async function recordJunkChanged(messageId, isSpam) {
  await host.request("record_user_action", {
    event_type: "junk_changed",
    thunderbird_message_id: String(messageId),
    junk: isSpam,
    user_initiated: true,
  });
}

// Flip the junk flag and record it — the context-menu correction path.
async function applyJunkCorrection(messageId, isSpam) {
  await browser.messages.update(messageId, { junk: isSpam });
  await recordJunkChanged(messageId, isSpam);
}

// --- Host -> extension notifications ---------------------------------------------------
host.onNotification(async (type, payload) => {
  if (type === "classification_ready") {
    // The host already applied the allowed actions; buffer this decision into the dashboard
    // Review queue (suggested + auto-applied) and refresh the aggregate badge.
    await surfaceReviewSuggestions(payload);
  } else if (type === "mail_command") {
    const result = await executeMailCommand(payload);
    host.notifyHost("record_user_action", result);
  } else if (type === "followup_draft_ready") {
    // A scheduled follow-up came due: open its review-required draft (never auto-sent) and ping.
    await openFollowupDraft(payload);
    showDesktopNotification(type, payload);
  } else if (type === "followup_needs_attention") {
    surfaceNeedsAttention(payload);
    await bumpFollowupAttention(payload);
    showDesktopNotification(type, payload);
  } else if (type === "proposal_ready") {
    // The curator promoted a learned behavior to a pending proposal — refresh the badge + ping.
    await recomputeSpaceBadge();
    browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "proposals" }).catch(() => {});
    showDesktopNotification(type, payload);
  } else {
    console.info("[MailMate] notification:", type, payload);
  }
});

// --- New-mail intake (registered synchronously) ---------------------------------------
browser.messages.onNewMailReceived.addListener(async (_folder, messages) => {
  for (const messageHeader of messages.messages) {
    const payload = await readMessageForHost(messageHeader);
    host.notifyHost("new_mail", payload);
  }
});

// --- Context-menu actions -------------------------------------------------------------
registerContextMenus({
  classify: async (messageHeader) => {
    const payload = await readMessageForHost(messageHeader);
    const result = await host.request("classify_message", payload);
    console.info("[MailMate] classification:", result);
  },
  draftReply: async (messageHeader) => {
    const draft = await host.request("draft_reply", {
      message_ids: [String(messageHeader.id)],
      subject: messageHeader.subject || "",
      counterparty: String(messageHeader.author || ""),
      excerpt: messageHeader.subject || "",
      forbidden_commitments: ["dates", "prices", "payment_changes", "legal_positions"],
    });
    await openDraftFromResponse(draft, messageHeader.id);
  },
  recordCorrection: async (messageHeader, isSpam) => {
    await applyJunkCorrection(messageHeader.id, isSpam);
  },
});

// --- Observe user moves so the host can learn filing ----------------------------------
// onMoved fires for EVERY move Thunderbird sees, including ones MailMate itself just applied.
// A host-applied move was already recorded once (in the audit log), so re-reporting it as a
// user-initiated move would double-route it AND poison the filing-correction signal that feeds
// crystallization. consumeHostMove (keyed by the stable RFC Message-ID) suppresses those echoes;
// only a genuine user move reaches the host as a filing correction.
browser.messages.onMoved.addListener(async (_originalMessages, movedMessages) => {
  for (const messageHeader of movedMessages.messages) {
    if (consumeHostMove(messageHeader.headerMessageId)) {
      continue; // MailMate moved it — not a user correction.
    }
    host.request("record_user_action", {
      event_type: "message_moved",
      thunderbird_message_id: String(messageHeader.id),
      to_folder_id: messageHeader.folder ? messageHeader.folder.path : null,
      user_initiated: true,
    });
  }
});

// --- Dashboard space: review-queue buffer, follow-up attention, aggregate badge --------
//
// The Review queue is built from buffered `classification_ready` payloads. We persist the buffer
// in `storage.session` so it survives an event-page suspension (the documented limitation that a
// durable host `list_review_queue` would later remove); it is intentionally NOT `storage.local`,
// so a browser restart does not resurrect stale suggestions. Everything degrades to an empty
// queue if `storage.session` is unavailable, never an error.

const REVIEW_KEY = "mm:reviewQueue";
const ATTENTION_KEY = "mm:followupAttention";
const REVIEW_CAP = 50;

async function sessionGet(key, fallback) {
  try {
    const got = await browser.storage.session.get(key);
    return got[key] === undefined ? fallback : got[key];
  } catch {
    return fallback;
  }
}

async function sessionSet(key, value) {
  try {
    await browser.storage.session.set({ [key]: value });
  } catch {
    /* no session storage → the buffer is best-effort, not load-bearing */
  }
}

// The buffered review queue, newest last. Returned to the dashboard's mm:reviewQueue request.
async function getReviewQueue() {
  return { items: await sessionGet(REVIEW_KEY, []) };
}

// Buffer one classification decision if it has anything to act on (a suggestion to approve or an
// auto-applied action to undo). De-dupes by decision_id so a re-classification replaces, not
// duplicates, the card.
async function bufferReview(payload) {
  const hasWork =
    (payload.review_required_actions || []).length > 0 || (payload.applied_actions || []).length > 0;
  if (!hasWork) return;
  const items = await sessionGet(REVIEW_KEY, []);
  const deduped = items.filter((i) => i.decision_id !== payload.decision_id);
  deduped.push(payload);
  await sessionSet(REVIEW_KEY, deduped.slice(-REVIEW_CAP));
}

// Drop a resolved decision from the buffer (approved / dismissed) and refresh the badge.
async function resolveReview(decisionId) {
  const items = await sessionGet(REVIEW_KEY, []);
  await sessionSet(
    REVIEW_KEY,
    items.filter((i) => i.decision_id !== decisionId),
  );
  await recomputeSpaceBadge();
  return { ok: true };
}

// Buffer a `classification_ready` decision and tell the open dashboard to refresh.
async function surfaceReviewSuggestions(payload) {
  await bufferReview(payload);
  await recomputeSpaceBadge();
  browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "review" }).catch(() => {});
}

// Track distinct stale follow-ups (by workflow instance) for the aggregate badge. The live
// Follow-ups pipeline view lands in Milestone 4; the count is correct in the meantime.
async function bumpFollowupAttention(payload) {
  const ids = await sessionGet(ATTENTION_KEY, []);
  const id = payload.workflow_instance_id;
  if (id && !ids.includes(id)) {
    ids.push(id);
    await sessionSet(ATTENTION_KEY, ids);
  }
  await recomputeSpaceBadge();
}

// --- Dashboard space registration + aggregate badge -----------------------------------

const SPACE_NAME = "mailmate";
let spaceId = null;

// Register (or re-attach to) the MailMate space exactly once. spaces.create throws if the name
// already exists (e.g. after an event-page restart), so we query first and reuse the id.
async function ensureSpace() {
  try {
    const existing = await browser.spaces.query({ name: SPACE_NAME }).catch(() => []);
    if (existing && existing.length) {
      spaceId = existing[0].id;
    } else {
      const space = await browser.spaces.create(SPACE_NAME, "dashboard.html", {
        title: "MailMate",
        defaultIcons: "icons/mailmate.svg",
      });
      spaceId = space.id;
    }
    await recomputeSpaceBadge();
  } catch (e) {
    console.warn("[MailMate] dashboard space registration failed:", e);
  }
}

// The aggregate toolbar badge = work the user must act on now: pending suggestions + stale
// follow-ups + pending proposals. A buffered decision with ONLY auto-applied actions (the
// crystallized path) is kept for Undo but is NOT work, so it is excluded from the count — only
// decisions that still carry a review-required suggestion count. Red when suggestions are
// waiting, amber when only follow-ups/proposals are. Suppressed at zero.
async function recomputeSpaceBadge() {
  if (spaceId == null) return;
  const items = await sessionGet(REVIEW_KEY, []);
  const reviews = items.filter((i) => (i.review_required_actions || []).length > 0).length;
  const attention = (await sessionGet(ATTENTION_KEY, [])).length;
  let proposals = 0;
  if (host.status.phase === HOST_PHASE.ready) {
    try {
      const r = await host.request("list_pending_reviews");
      proposals = (r.pending_reviews || []).length;
    } catch {
      /* a transient host error just omits the proposal count from the badge */
    }
  }
  const total = reviews + attention + proposals;
  try {
    await browser.spaces.update(spaceId, null, {
      badgeText: total > 0 ? String(total) : "",
      badgeBackgroundColor: reviews > 0 ? "#c0392b" : "#e67e22",
    });
  } catch {
    /* badge update is cosmetic — never fatal */
  }
}

// Register the space on event-page start (fire and forget; failures are logged, not fatal).
ensureSpace();
