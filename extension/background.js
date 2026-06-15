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
//
// It is also the single owner of the native port: the popups (the toolbar recovery card and,
// from Milestone 1's next slice, the per-message panel) never open their own port — they ask
// the background over `browser.runtime` messaging, keeping one single-writer channel.
//
// onNewMailReceived is registered synchronously at the top of the event page so a wake-up
// from a new message is not missed.

/* global NativeHost, HOST_PHASE, registerContextMenus, readMessageForHost,
   openDraftFromResponse, executeMailCommand, consumeHostMove, openFollowupDraft,
   surfaceNeedsAttention */

const host = new NativeHost();

// --- Connection health: HostStatus -> toolbar badge + popup broadcast ------------------
// Every connection surface derives from this one status, so they can never disagree. The
// toolbar `action` badge is the always-on indicator; open popups also get a live push.
host.onStatusChange((status) => {
  updateToolbarBadge(status);
  // Push to any open popup; harmless to fail if none is listening.
  browser.runtime.sendMessage({ type: "mm:statusChanged", status }).catch(() => {});
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
// The popups (toolbar recovery card; later the per-message panel) are separate documents
// with no native port. They drive the host through these messages, so the background stays
// the single port owner.
browser.runtime.onMessage.addListener((message) => {
  switch (message && message.type) {
    case "mm:getStatus":
      return Promise.resolve({ status: host.status });
    case "mm:reconnect":
      return Promise.resolve({ status: host.reconnect() });
    default:
      return false; // not ours — let other listeners (if any) handle it
  }
});

// --- Host -> extension notifications ---------------------------------------------------
host.onNotification(async (type, payload) => {
  if (type === "classification_ready") {
    console.info("[MailMate] classification ready:", payload);
    // The host already applied the allowed actions (as mail_command frames it sent before
    // this notification); surface the review-required ones for the user to confirm.
    surfaceReviewSuggestions(payload);
  } else if (type === "mail_command") {
    const result = await executeMailCommand(payload);
    host.notifyHost("record_user_action", result);
  } else if (type === "followup_draft_ready") {
    // A scheduled follow-up came due: open its review-required draft (never auto-sent).
    await openFollowupDraft(payload);
  } else if (type === "followup_needs_attention") {
    surfaceNeedsAttention(payload);
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
    await browser.messages.update(messageHeader.id, { junk: isSpam });
    await host.request("record_user_action", {
      event_type: "junk_changed",
      thunderbird_message_id: String(messageHeader.id),
      junk: isSpam,
      user_initiated: true,
    });
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

// Surface the review-required suggestions from a classification_ready notification. The
// dashboard Review queue (Milestone 2) renders these; here we log them so the wiring is
// observable until that surface lands.
function surfaceReviewSuggestions(payload) {
  const review = payload.review_required_actions || [];
  if (review.length) {
    console.info("[MailMate] needs review:", review, payload.explanation);
  }
}
