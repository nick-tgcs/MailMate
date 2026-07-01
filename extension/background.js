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
   openDraftFromResponse, executeMailCommand, consumeHostMove, consumeHostTag, openFollowupDraft,
   surfaceNeedsAttention, showDesktopNotification, getComposeDraft, setBodyRetention */

const host = new NativeHost();

// The dashboard-space id, set once ensureSpace() resolves. Declared up here (not beside the space
// section far below) because the FIRST onStatusChange fires synchronously during load — before that
// section runs — and refreshBadges() -> updateSpaceBadge() reads it; a `let` declared later would be
// in the temporal dead zone and reject the badge refresh on every cold start.
let spaceId = null;

// --- Connection health: HostStatus -> toolbar badge + popup broadcast ------------------
// Every connection surface derives from this one status, so they can never disagree. The
// toolbar `action` badge is the always-on indicator; open popups also get a live push.
host.onStatusChange((status) => {
  // Drive the body-retention gate from the host's EFFECTIVE (consent-gated) retention level, so
  // a body is forwarded only when the host actually retains bodies — never hard-coded.
  if (status && status.retention !== undefined) {
    setBodyRetention(status.retention);
  }
  // Push to any open popup or dashboard; harmless to fail if none is listening.
  browser.runtime.sendMessage({ type: "mm:statusChanged", status }).catch(() => {});
  // Refresh BOTH badges (toolbar action + dashboard space): connection state owns the toolbar when
  // the host isn't ready, otherwise it shows the aggregate of work waiting.
  refreshBadges();
});

// Map the connection phase onto a toolbar badge spec (interaction-design.md §"Connection health").
// Disconnected/mismatch raise a visible "!"; connecting is a muted "…".
function connectionBadge(phase) {
  const byPhase = {
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
  return byPhase[phase] || byPhase[HOST_PHASE.connecting];
}

// The toolbar button's badge. When the host isn't ready, the connection state owns it (a down host
// is the most urgent thing). When ready, it shows the aggregate count of work waiting — red when a
// review-required suggestion is among it, amber for follow-ups/proposals only, calm (no badge) at
// zero.
function updateToolbarBadge(status, agg) {
  let badge;
  if (status.phase === HOST_PHASE.ready) {
    const total = (agg && agg.total) || 0;
    const reviews = (agg && agg.reviews) || 0;
    badge = {
      text: total > 0 ? String(total) : "",
      color: reviews > 0 ? "#c0392b" : "#e67e22",
      title:
        total > 0
          ? `MailMate — ${total} item${total === 1 ? "" : "s"} need your attention`
          : "MailMate — connected",
    };
  } else {
    badge = connectionBadge(status.phase);
  }
  browser.action.setBadgeText({ text: badge.text }).catch(() => {});
  browser.action.setBadgeBackgroundColor({ color: badge.color }).catch(() => {});
  browser.action.setTitle({ title: badge.title }).catch(() => {});
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
  "mm:signalWrong": (m) => signalWrong(m),
  "mm:notJunk": (m) => markNotJunk(m),
  "mm:junk": (m) => markJunk(m),
  "mm:markRead": (m) => markRead(m),
  "mm:draftReply": (m) => draftReply(m),
  "mm:move": (m) => moveMessage(m),
  "mm:unsubscribe": (m) => unsubscribe(m),
  "mm:openDashboard": (m) => openDashboard(m),
  "mm:aggregate": () => aggregateForPopup(),
  "mm:folders": () => listFolders(),
  // Dashboard space.
  "mm:reviewQueue": () => getReviewQueue(),
  "mm:resolveReview": (m) => resolveReview(m.decisionId),
  "mm:listActivity": (m) =>
    hostCall("list_recent_activity", { limit: m.limit || 80, event_type_filter: m.eventTypeFilter || null }),
  "mm:listProposals": () => hostCall("list_pending_reviews", {}),
  // Rules manager: list every evaluated + disabled rule, and flip a rule's lifecycle status
  // (enable / disable / promote-to-active) — the host hot-reloads so the change is live at once.
  "mm:listRules": () => hostCall("list_rules", {}),
  "mm:setRuleStatus": (m) =>
    hostCall("set_rule_status", { rule_id: m.ruleId, kind: m.kind, status: m.status }),
  "mm:reviewProposal": (m) =>
    hostCall("review_rule_proposal", {
      proposal_id: m.proposalId,
      decision: m.decision,
      reason_code: m.reasonCode || null,
    }),
  "mm:settings": () => hostCall("get_settings", {}, (r) => ({ ok: true, settings: r })),
  "mm:setPause": (m) => hostCall("set_pause", { paused: Boolean(m.paused) }),
  "mm:setSettings": (m) =>
    hostCall("set_settings", {
      retention_level: m.retentionLevel,
      follow_up_tick_seconds: m.followUpTickSeconds,
      catch_up_on_launch: m.catchUpOnLaunch,
    }),
  // Triage tuning (Phase 5): per-category action policy, per-account scope, tag→category mapping.
  // Each is a host config write; the host re-reads and returns the fresh snapshot, which options
  // confirms against (the "host owns the truth" invariant).
  "mm:setCategoryPolicy": (m) =>
    hostCall("set_category_policy", { category: m.category, policy: m.policy }),
  "mm:setAccountScope": (m) =>
    hostCall("set_account_scope", { account_id: m.accountId, enabled: Boolean(m.enabled) }),
  "mm:setTagMapping": (m) =>
    hostCall("set_tag_mapping", { tag: m.tag, category: m.category }),
  "mm:setProvider": (m) =>
    hostCall("set_provider", {
      provider_id: m.providerId,
      kind: m.kind,
      endpoint: m.endpoint,
      model: m.model,
      set_default: m.setDefault,
      remove: m.remove,
    }),
  "mm:setSecret": (m) => hostCall("set_secret", { provider_id: m.providerId, secret: m.secret }),
  // Provider model discovery: probe an endpoint (a saved provider_id attaches its stored key for
  // an authenticated cloud catalog) so options can offer a pick-list instead of free text.
  "mm:listModels": (m) =>
    hostCall(
      "list_models",
      { kind: m.kind, endpoint: m.endpoint, provider_id: m.providerId || null },
      undefined,
      20000, // a catalog probe should be quick; bound it so a stalled endpoint errors, not hangs
    ),
  // Test connection: probe a provider for a REAL liveness result (reachable + model_count, or an
  // error string). A down endpoint comes back ok with reachable:false — it is data, not a failure.
  "mm:testProvider": (m) =>
    hostCall(
      "test_provider",
      { kind: m.kind, endpoint: m.endpoint, provider_id: m.providerId || null },
      undefined,
      20000,
    ),
  // Follow-ups pipeline.
  "mm:listFollowups": (m) =>
    hostCall("list_followups", { status_filter: m.statusFilter || null, limit: m.limit || 100 }),
  "mm:followupReschedule": (m) =>
    hostCall(m.verb === "snooze" ? "snooze" : "reschedule_followup", {
      workflow_instance_id: m.workflowInstanceId,
      next_due_at: m.nextDueAt,
    }),
  "mm:followupStage": (m) =>
    hostCall("update_pipeline_stage", { pipeline_item_id: m.pipelineItemId, stage: m.stage }),
  "mm:followupReview": (m) =>
    hostCall("review_followup", { workflow_instance_id: m.workflowInstanceId, resolution: m.resolution }),
  "mm:followupCancel": (m) => hostCall("cancel_sequence", { pipeline_item_id: m.pipelineItemId }),
  // Compose review panel: the draft's annotation + provider posture, and a steered re-draft.
  "mm:composeContext": (m) => composeContext(m.tabId),
  "mm:regenerateDraft": (m) => regenerateDraft(m),
  // First-run backfill: sweep existing mail through the dry-run triage path (applies nothing).
  "mm:triageExisting": () => startBackfill(),
  "mm:backfillStatus": () => backfillStatus(),
  "mm:backfillControl": (m) => backfillControl(m && m.action),
};

// --- First-run backfill ("Triage my existing mail") -----------------------------------
//
// One tap pages `browser.messages.query` and feeds each page to the host's `triage_existing_mail`
// (a DRY RUN — the host classifies and mines deliberate folder placements but mutates no mail).
// The background owns the loop; the dashboard renders the progress chip from `mm:backfillStatus`
// and steers it with `mm:backfillControl`. Resumable across an event-page nap via storage.local.

const BACKFILL_KEY = "mm:backfill";
const BACKFILL_PAGE = 40; // messages per host round-trip
const BACKFILL_CAP = 5000; // a sanity bound so a huge mailbox can't run unbounded

let backfill = freshBackfill();

function freshBackfill() {
  return {
    running: false,
    paused: false,
    cancelled: false,
    total: 0,
    done: 0,
    classified: 0,
    needs_review: 0,
    placements: 0,
    done_at: null,
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function startBackfill() {
  if (backfill.running) return { ok: true, started: false, running: true };
  if (host.status.phase !== HOST_PHASE.ready) {
    return { ok: false, error: "MailMate host not connected" };
  }
  backfill = { ...freshBackfill(), running: true };
  // Kick the loop without blocking this message's reply — the dashboard polls for progress.
  runBackfill().catch((e) => {
    console.warn("[MailMate] backfill failed:", errMessage(e));
    backfill.running = false;
    backfill.done_at = Date.now();
  });
  return { ok: true, started: true };
}

async function backfillStatus() {
  // Hydrate the last finished summary across an event-page restart so the dashboard can still
  // show "Triaged N messages" after the worker was suspended.
  if (!backfill.running && !backfill.done_at) {
    try {
      const got = await browser.storage.local.get(BACKFILL_KEY);
      if (got[BACKFILL_KEY]) backfill = { ...backfill, ...got[BACKFILL_KEY], running: false };
    } catch {
      /* no storage — report the in-memory state */
    }
  }
  return { ok: true, ...backfill };
}

function backfillControl(action) {
  if (action === "pause") backfill.paused = true;
  else if (action === "resume") backfill.paused = false;
  else if (action === "cancel") backfill.cancelled = true;
  return { ok: true, ...backfill };
}

async function runBackfill() {
  try {
    let page = await browser.messages.query({});
    while (page) {
      if (backfill.cancelled || backfill.done >= BACKFILL_CAP) break;
      while (backfill.paused && !backfill.cancelled) await sleep(300);
      if (backfill.cancelled) break;

      const headers = page.messages || [];
      const messages = [];
      for (const h of headers) {
        try {
          messages.push(await readMessageForHost(h));
        } catch (e) {
          console.debug("[MailMate] backfill skip:", errMessage(e));
        }
      }
      if (messages.length) {
        const res = await host.request("triage_existing_mail", { messages, record_placements: true });
        backfill.classified += (res && res.classified) || 0;
        backfill.needs_review += (res && res.needs_review) || 0;
        backfill.placements += (res && res.placements_recorded) || 0;
      }
      backfill.done += headers.length;
      backfill.total = backfill.done; // best-effort: TB does not give a cheap total up front
      page = page.id ? await browser.messages.continueList(page.id) : null;
    }
  } finally {
    backfill.running = false;
    backfill.done_at = Date.now();
    try {
      await browser.storage.local.set({
        [BACKFILL_KEY]: {
          done_at: backfill.done_at,
          classified: backfill.classified,
          needs_review: backfill.needs_review,
          placements: backfill.placements,
        },
      });
    } catch {
      /* best-effort persistence */
    }
  }
}

// The composeAction panel's context: the MailMate draft annotation for this compose window (or
// null for a hand-written compose) plus the provider posture, so the panel can render the
// rationale + commitments guard and the degraded "drafting needs a provider" state honestly.
// `providerConfigured` comes from `provider_status.available` (computed by the host's build_provider,
// so it can't drift from what the draft path would actually get); `provider` carries kind + model
// for the "Drafted via …" provenance line.
async function composeContext(tabId) {
  const draft = typeof tabId === "number" ? getComposeDraft(tabId) : null;
  let providerConfigured = null; // null = unknown (host not ready / no admin)
  let provider = null;
  if (host.status.phase === HOST_PHASE.ready) {
    try {
      const status = await host.request("provider_status");
      providerConfigured = Boolean(status && status.available);
      if (status && status.provider) {
        provider = { kind: status.provider.kind, model: status.provider.model };
      }
    } catch {
      /* leave unknown — the panel degrades to a neutral provider line */
    }
  }
  return { ok: true, draft, providerConfigured, provider };
}

// Build + run a draft over the host, open it as a review-required Thunderbird draft, and stash its
// context (so the panel can annotate and regenerate it). Shared by the context-menu action and any
// future draft entry point.
async function runDraft(request, inReplyToMessageId) {
  const draft = await host.request("draft_reply", request);
  await openDraftFromResponse(draft, inReplyToMessageId, request);
  return draft;
}

// Re-draft the reply for an open compose tab, folding the panel's steer (quick-steer chips +
// free-text) into the host's `regenerate_draft`, then replacing the editable compose body in place
// (still a draft; still never sent). Returns the SAME shape as composeContext so the panel
// re-renders with the fresh rationale + guard.
async function regenerateDraft(m) {
  const tabId = m && m.tabId;
  const ctx = typeof tabId === "number" ? getComposeDraft(tabId) : null;
  if (!ctx || !ctx.request) {
    return { ok: false, error: "no MailMate draft to regenerate in this window" };
  }
  let draft;
  try {
    draft = await host.request("regenerate_draft", {
      ...ctx.request,
      adjustments: m.adjustments || [],
      steer: m.steer || null,
    });
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
  try {
    await browser.compose.setComposeDetails(tabId, { plainTextBody: draft.body, isPlainText: true });
  } catch (e) {
    return { ok: false, error: `couldn't update the draft: ${String(e && e.message ? e.message : e)}` };
  }
  // Re-stash the fresh annotation (keeping the request + reply-to so a further Regenerate works).
  stashComposeDraft(tabId, {
    ...ctx,
    draft_id: draft.draft_id || ctx.draft_id,
    safety_notes: draft.safety_notes || [],
    commitments: draft.commitments || null,
    rationale: draft.rationale || null,
    drafted_body: draft.body || "",
  });
  // Drafting just demonstrably succeeded, so pin providerConfigured:true — don't let a transient
  // failure of the follow-up provider_status probe hide the refine controls the user just used.
  const ctx2 = await composeContext(tabId);
  return { ...ctx2, providerConfigured: true };
}

// --- Edit-divergence learning hook ----------------------------------------------------
// When the user sends a MailMate draft they had EDITED, that divergence is a signal the draft
// missed the mark. `compose.onBeforeSend` fires with the final body just before Thunderbird sends
// — MailMate itself never sends; we only observe. We compare the final body to what MailMate
// drafted; a meaningful edit records a `draft_diverged` signal. We never cancel or modify the
// message (return {}), so this is purely observational.
// tab.id -> recipients captured at onBeforeSend, flushed when the send is confirmed.
const pendingSendRecipients = new Map();

// The recipients (To + Cc) of a compose, from the onBeforeSend details. Each entry is a full
// address string the host parses to a domain — the learn-from-Sent / VIP signal.
function recipientsOf(details) {
  if (!details) return [];
  return [...(details.to || []), ...(details.cc || [])].filter((r) => typeof r === "string" && r);
}

if (typeof browser !== "undefined" && browser.compose && browser.compose.onBeforeSend) {
  browser.compose.onBeforeSend.addListener((tab, details) => {
    recordDraftDivergence(tab, details).catch(() => {});
    // Stash the recipients now (onBeforeSend reliably carries details.to/cc); we only REPORT them
    // once onAfterSend confirms the message actually left, so a cancelled send teaches nothing.
    if (tab && tab.id != null) {
      pendingSendRecipients.set(tab.id, recipientsOf(details));
    }
    return {}; // never touch the user's Send
  });
}

// onAfterSend fires once the message has actually been sent. Report the stashed recipients as
// outbound evidence so the host can learn VIP/priority rules for people the user emails often. We
// never send or modify anything here — this is purely observational, like the divergence hook.
if (typeof browser !== "undefined" && browser.compose && browser.compose.onAfterSend) {
  browser.compose.onAfterSend.addListener((tab, sendInfo) => {
    const tabId = tab && tab.id != null ? tab.id : null;
    const recipients = tabId != null ? pendingSendRecipients.get(tabId) || [] : [];
    if (tabId != null) pendingSendRecipients.delete(tabId);
    // Only a real "send" mode counts as outbound (a save-as-draft is not a sent message).
    if (sendInfo && sendInfo.mode && sendInfo.mode !== "sendNow" && sendInfo.mode !== "sendLater") {
      return;
    }
    if (!recipients.length) return;
    host.request("record_sent_mail", { recipients }).catch(() => {});
  });
}

// A compose window can close WITHOUT onAfterSend firing — the user cancels the send dialog or it
// fails — which would otherwise strand its recipient stash forever. Drop it when the tab closes so
// the Map can't grow unbounded across a long session.
if (typeof browser !== "undefined" && browser.tabs && browser.tabs.onRemoved) {
  browser.tabs.onRemoved.addListener((tabId) => {
    pendingSendRecipients.delete(tabId);
  });
}

async function recordDraftDivergence(tab, details) {
  const ctx = getComposeDraft(tab && tab.id);
  if (!ctx || !ctx.draft_id) return; // not a MailMate draft — nothing to learn from
  const original = ctx.drafted_body || "";
  if (!original) return;
  const finalBody = (details && (details.plainTextBody || details.body)) || "";
  // Containment, not equality: Thunderbird appends the quoted original to a reply, so an unedited
  // draft still CONTAINS MailMate's text verbatim. Only a real edit breaks containment.
  if (collapseWs(finalBody).includes(collapseWs(original))) return; // sent unchanged — no signal
  await host.request("record_user_action", {
    event_type: "draft_diverged",
    draft_id: ctx.draft_id,
    thread_id: ctx.request && ctx.request.thread_id ? ctx.request.thread_id : null,
    // The replied-to message correlates the divergence to a message even when the context-menu
    // draft carried no thread id (its only entry point omits one) — keeps the audit row anchored.
    thunderbird_message_id: ctx.reply_to_message_id != null ? String(ctx.reply_to_message_id) : null,
    user_initiated: true,
  });
}

// Collapse runs of whitespace so trivial reflow differences don't read as an edit.
function collapseWs(s) {
  return String(s).replace(/\s+/g, " ").trim();
}

// A guarded host round-trip for the dashboard's read/write requests. Returns the host payload
// merged onto { ok:true } (or a custom mapper's shape); a disconnected host or a verb this build
// doesn't speak resolves to { ok:false, error } so the dashboard degrades, never lies.
async function hostCall(type, payload, mapper, timeoutMs) {
  if (host.status.phase !== HOST_PHASE.ready) {
    console.warn(`[MailMate] hostCall ${type}: host not ready (${host.status.phase})`);
    return { ok: false, error: "MailMate host not connected", reason: "host_not_ready" };
  }
  try {
    const result = await host.request(type, payload || {}, timeoutMs);
    console.debug(`[MailMate] hostCall ${type}: ok`);
    return mapper ? mapper(result) : { ok: true, ...result };
  } catch (e) {
    console.warn(`[MailMate] hostCall ${type}: ${errMessage(e)}`);
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
      // The host enriches an auto-applied move with the full inverse action (a move back to the
      // origin folder), so the Undo just replays it rather than reconstructing the target.
      reverse = action.reverses_to;
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

// "This reason is wrong": record the rejected salient signal as negative classification
// feedback keyed on the signal id. No mail mutation — nothing visible happened, so a failed
// record IS the failure and is reported as such (mirrors dismissSuggestion).
async function signalWrong({ decisionId, messageId, signalId, priorLabel }) {
  try {
    await host.request("record_user_action", {
      event_type: "signal_marked_wrong",
      decision_id: decisionId,
      thunderbird_message_id: String(messageId),
      signal_id: signalId,
      prior_label: priorLabel || null,
      user_initiated: true,
    });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  return { ok: true };
}

// One-click unsubscribe from a List-Unsubscribe affordance the host parsed. We prefer a
// pre-addressed compose (the user stays in control; no silent tracking ping). If only an RFC
// 8058 one-click HTTPS target exists, a single background POST does it; otherwise we open the
// unsubscribe page in the browser. Never auto-sends anything: a compose still waits for the user.
async function unsubscribe({ unsubscribe: u }) {
  if (!u) {
    return { ok: false, error: "no unsubscribe information for this message" };
  }
  if (u.mailto && u.mailto.to) {
    try {
      await browser.compose.beginNew({
        to: [u.mailto.to],
        subject: u.mailto.subject || "unsubscribe",
        body: "Please unsubscribe this address from your list.",
      });
      return { ok: true, method: "compose" };
    } catch (e) {
      return { ok: false, error: errMessage(e) };
    }
  }
  if (u.one_click && u.http_url) {
    try {
      await fetch(u.http_url, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: "List-Unsubscribe=One-Click",
      });
      return { ok: true, method: "post" };
    } catch (e) {
      return { ok: false, error: errMessage(e) };
    }
  }
  if (u.http_url) {
    try {
      if (browser.windows && browser.windows.openDefaultBrowser) {
        await browser.windows.openDefaultBrowser(u.http_url);
      } else {
        await browser.tabs.create({ url: u.http_url });
      }
      return { ok: true, method: "open" };
    } catch (e) {
      return { ok: false, error: errMessage(e) };
    }
  }
  return { ok: false, error: "no usable unsubscribe target" };
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

// "Junk & block": flip the junk flag (the visible effect), then record the spam correction
// best-effort — the strongest spam-axis teaching signal. A user-initiated junk, so it is a real
// correction (not a host echo).
async function markJunk({ messageId }) {
  const id = Number(messageId);
  try {
    await browser.messages.update(id, { junk: true });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  await recordBestEffort(() => recordJunkChanged(id, true), "junk correction");
  return { ok: true };
}

// "Mark read": a plain client mutation, no learning signal (reading is not a classification
// correction). Reported truthfully so the panel can confirm or surface a failure.
async function markRead({ messageId }) {
  try {
    await browser.messages.update(Number(messageId), { read: true });
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
  return { ok: true };
}

// "Draft a reply": open a real reply compose window addressed to the sender. The draft is the
// user's to edit and send — MailMate never sends (the host has no send path); this is just the
// compose affordance, so the panel's primary action set is complete.
async function draftReply({ messageId }) {
  try {
    await browser.compose.beginReply(Number(messageId), "replyToSender");
  } catch (e) {
    return { ok: false, error: errMessage(e) };
  }
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

// Open the dashboard tab, optionally deep-linked to a message's explanation timeline (the
// panel's "Explain in dashboard" link). Reuses an open dashboard tab when possible.
async function openDashboard({ explain, tab } = {}) {
  // Deep-link to a dashboard tab via the same durable focus mechanism notifications use: enterApp
  // consumes mm:focusTab on a cold load; an already-open dashboard gets the live focusTab event.
  if (tab) {
    await browser.storage.session.set({ "mm:focusTab": tab }).catch(() => {});
  }
  const base = browser.runtime.getURL("dashboard.html");
  const url = explain ? `${base}#explain=${encodeURIComponent(explain)}` : base;
  try {
    await browser.tabs.create({ url });
    if (tab) {
      browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "focusTab", tab }).catch(() => {});
    }
    return { ok: true };
  } catch (e) {
    // The open failed, so clear the focus stash — otherwise the NEXT cold dashboard open (from any
    // entry point) would consume this stale tab and jump somewhere the user didn't just ask for.
    if (tab) {
      browser.storage.session.remove("mm:focusTab").catch(() => {});
    }
    return { ok: false, error: errMessage(e) };
  }
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
    browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "followups" }).catch(() => {});
  } else if (type === "followup_needs_attention") {
    surfaceNeedsAttention(payload);
    await bumpFollowupAttention(payload);
    showDesktopNotification(type, payload);
    browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "followups" }).catch(() => {});
  } else if (type === "proposal_ready") {
    // The curator promoted a learned behavior to a pending proposal — refresh the badge + ping.
    // Carry the rule's title so the dashboard can show the same-session crystallization "aha"
    // ("MailMate just learned …") rather than a silent badge bump.
    await refreshBadges();
    browser.runtime
      .sendMessage({ type: "mm:dashboardEvent", event: "proposals", title: payload && payload.title })
      .catch(() => {});
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
    const request = {
      message_ids: [String(messageHeader.id)],
      subject: messageHeader.subject || "",
      counterparty: String(messageHeader.author || ""),
      excerpt: messageHeader.subject || "",
      forbidden_commitments: ["dates", "prices", "payment_changes", "legal_positions"],
    };
    await runDraft(request, messageHeader.id);
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
// The account a message belongs to, read off its folder — folded into a correction so the host can
// scope a learned rule per account. `null` when unknown (the host then falls back to its cache).
function accountOf(messageHeader) {
  return messageHeader && messageHeader.folder ? messageHeader.folder.accountId : null;
}

browser.messages.onMoved.addListener(async (_originalMessages, movedMessages) => {
  for (const messageHeader of movedMessages.messages) {
    if (consumeHostMove(messageHeader.headerMessageId)) {
      continue; // MailMate moved it — not a user correction.
    }
    host.request("record_user_action", {
      event_type: "message_moved",
      thunderbird_message_id: String(messageHeader.id),
      to_folder_id: messageHeader.folder ? messageHeader.folder.path : null,
      account_id: accountOf(messageHeader),
      user_initiated: true,
    });
  }
});

// onUpdated fires when a message's properties change, including its tags. A tag add/remove is a
// first-class category signal (tags-as-signal), but onUpdated reports only the NEW tag set, so we
// diff it against the last-seen set per message to recover the add/remove direction. Tags MailMate
// itself just applied are echo-suppressed via consumeHostTag (the twin of the onMoved guard), so
// MailMate never learns from its own tagging — only a genuine user tag reaches the host.
const lastSeenTags = new Map(); // tb message id -> Set<tag>

browser.messages.onUpdated.addListener((messageHeader, changedProperties) => {
  if (!changedProperties || !("tags" in changedProperties)) {
    return; // not a tag change
  }
  const next = new Set(messageHeader.tags || []);
  const prev = lastSeenTags.get(messageHeader.id) || new Set();
  lastSeenTags.set(messageHeader.id, next);

  for (const tag of next) {
    if (prev.has(tag)) {
      continue; // unchanged
    }
    if (consumeHostTag(messageHeader.headerMessageId, tag)) {
      continue; // MailMate added it — not a user signal
    }
    host.request("record_user_action", {
      event_type: "tag_changed",
      thunderbird_message_id: String(messageHeader.id),
      tag,
      added: true,
      account_id: accountOf(messageHeader),
      user_initiated: true,
    });
  }
  for (const tag of prev) {
    if (next.has(tag)) {
      continue; // unchanged
    }
    host.request("record_user_action", {
      event_type: "tag_changed",
      thunderbird_message_id: String(messageHeader.id),
      tag,
      added: false,
      account_id: accountOf(messageHeader),
      user_initiated: true,
    });
  }
});

// Per-message header badge: when a message is displayed, classify it (best-effort) and set the
// messageDisplayAction badge so the verdict is visible at a glance — an in-flight dot while it
// classifies, then green "✓" benign / amber "!" risky / blue "·" needs-review, or no badge when
// the host is down. Inform-only: it takes no action and warms the classify feature cache.
if (browser.messageDisplay && browser.messageDisplay.onMessageDisplayed) {
  browser.messageDisplay.onMessageDisplayed.addListener((tab, message) => {
    updateMessageBadge(tab.id, message.id).catch(() => {});
  });
}

async function updateMessageBadge(tabId, messageId) {
  const set = (text, color) => {
    try {
      browser.messageDisplayAction.setBadgeText({ tabId, text });
      if (text) {
        browser.messageDisplayAction.setBadgeBackgroundColor({ tabId, color });
      }
    } catch (e) {
      /* the badge is cosmetic — never let it throw into the event loop */
    }
  };
  if (host.status.phase !== HOST_PHASE.ready) {
    set("", null);
    return;
  }
  set("·", "#9e9e9e"); // in-flight dot
  try {
    const reply = await classifyForPanel(String(messageId));
    if (!reply.ok) {
      set("", null);
      return;
    }
    const badge = badgeForClassification(reply.result.classification || {});
    set(badge.text, badge.color);
  } catch (e) {
    set("", null);
  }
}

// Map a verdict onto a per-message header badge: amber "!" for a risky verdict, blue "·" when it
// needs review, green "✓" for a confident benign verdict.
function badgeForClassification(c) {
  const risky =
    (c.labels || []).some((l) => /spam|phish|junk|suspicious|malware/.test(String(l).toLowerCase())) ||
    Math.max(c.spam_score || 0, c.phishing_score || 0) >= 0.6;
  if (risky) {
    return { text: "!", color: "#e67e22" };
  }
  if (c.needs_review) {
    return { text: "·", color: "#1565c0" };
  }
  return { text: "✓", color: "#2e7d32" };
}

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
  await refreshBadges();
  return { ok: true };
}

// Buffer a `classification_ready` decision and tell the open dashboard to refresh.
async function surfaceReviewSuggestions(payload) {
  await bufferReview(payload);
  await refreshBadges();
  browser.runtime.sendMessage({ type: "mm:dashboardEvent", event: "review" }).catch(() => {});
}

// Track distinct stale follow-ups (by workflow instance) for the aggregate badge. The live
// The follow-ups pipeline view is live in the dashboard; this keeps the toolbar count in sync.
async function bumpFollowupAttention(payload) {
  const ids = await sessionGet(ATTENTION_KEY, []);
  const id = payload.workflow_instance_id;
  if (id && !ids.includes(id)) {
    ids.push(id);
    await sessionSet(ATTENTION_KEY, ids);
  }
  await refreshBadges();
}

// --- Dashboard space registration + aggregate badge -----------------------------------

const SPACE_NAME = "mailmate";
// `spaceId` is declared near the top of the file (it is read by the first synchronous badge refresh
// during load, long before this section runs).

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
    await refreshBadges();
  } catch (e) {
    console.warn("[MailMate] dashboard space registration failed:", e);
  }
}

// The aggregate of work the user must act on now: pending suggestions + stale follow-ups +
// pending proposals. A buffered decision with ONLY auto-applied actions (the crystallized path) is
// kept for Undo but is NOT work, so only decisions that still carry a review-required suggestion
// count. The pending-proposal count needs a live host; a transient error just omits it.
async function computeAggregate() {
  const items = await sessionGet(REVIEW_KEY, []);
  const reviews = items.filter((i) => (i.review_required_actions || []).length > 0).length;
  const attention = (await sessionGet(ATTENTION_KEY, [])).length;
  let proposals = 0;
  if (host.status.phase === HOST_PHASE.ready) {
    try {
      const r = await host.request("list_pending_reviews");
      proposals = (r.pending_reviews || []).length;
    } catch {
      /* a transient host error just omits the proposal count from the aggregate */
    }
  }
  return { reviews, attention, proposals, total: reviews + attention + proposals };
}

// Refresh BOTH badges (toolbar action + dashboard space) from ONE aggregate computation, so they
// can never disagree. Skips the host round-trip when the host isn't ready (the count is then 0 and
// the toolbar shows the connection state instead).
async function refreshBadges() {
  const zero = { reviews: 0, attention: 0, proposals: 0, total: 0 };
  const agg = host.status.phase === HOST_PHASE.ready ? await computeAggregate() : zero;
  // Re-read status AFTER the await: a disconnect may have landed while computeAggregate ran, and a
  // stale count must not be painted onto either badge (they must never disagree).
  const live = host.status.phase === HOST_PHASE.ready ? agg : zero;
  updateToolbarBadge(host.status, live);
  updateSpaceBadge(live);
}

// The dashboard-space tab badge: the same aggregate count, red when a review-required suggestion
// is waiting, amber for follow-ups/proposals only. Suppressed at zero.
function updateSpaceBadge(agg) {
  if (spaceId == null) return;
  browser.spaces
    .update(spaceId, null, {
      badgeText: agg.total > 0 ? String(agg.total) : "",
      badgeBackgroundColor: agg.reviews > 0 ? "#c0392b" : "#e67e22",
    })
    .catch(() => {
      /* badge update is cosmetic — never fatal */
    });
}

// The popup's one round-trip: the aggregate breakdown + the pause kill-switch state.
async function aggregateForPopup() {
  const agg =
    host.status.phase === HOST_PHASE.ready
      ? await computeAggregate()
      : { reviews: 0, attention: 0, proposals: 0, total: 0 };
  let paused = false;
  if (host.status.phase === HOST_PHASE.ready) {
    try {
      const settings = await host.request("get_settings");
      paused = Boolean(settings && settings.paused);
    } catch {
      /* leave paused=false — the popup degrades to "running" rather than guessing */
    }
  }
  return { ok: true, ...agg, paused };
}

// Register the space on event-page start (fire and forget; failures are logged, not fatal).
ensureSpace();
