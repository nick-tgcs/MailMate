// background.js — the MailMate event page.
//
// Wires the six Phase-10 capabilities to the native host:
//   1. read selected message      -> classify_message (context menu)
//   2. listen to new mail events   -> new_mail (messages.onNewMailReceived)
//   3. context-menu actions        -> context_menu.js
//   4. open draft replies          -> draft_reply response -> compose draft
//   5. apply safe actions          -> classification_ready / mail_command -> drafts.js
//   6. record user actions/results -> record_user_action
//
// onNewMailReceived is registered synchronously at the top of the event page so a wake-up
// from a new message is not missed.

/* global NativeHost, registerContextMenus, readMessageForHost, openDraftFromResponse,
   executeMailCommand, consumeHostMove, makePing, openFollowupDraft, surfaceNeedsAttention */

const host = new NativeHost();

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

// Surface the review-required suggestions from a classification_ready notification. A real UI
// (Phase 12) renders these; here we log them so the wiring is observable.
function surfaceReviewSuggestions(payload) {
  const review = payload.review_required_actions || [];
  if (review.length) {
    console.info("[MailMate] needs review:", review, payload.explanation);
  }
}

// Prove the channel on startup (the Phase-1 contract; harness-tested).
const nonce = Date.now().toString(36) + Math.random().toString(36).slice(2);
host.port.postMessage(makePing(nonce));
