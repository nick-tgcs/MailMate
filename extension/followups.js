// followups.js — the sales-pipeline + follow-up surface (Phase 11).
//
// Two inbound notifications reach here from the host:
//   1. `followup_draft_ready`     -> open the review-required follow-up draft (NEVER sent).
//   2. `followup_needs_attention` -> surface a nudge; the item went stale, no draft.
//
// And the outbound control requests the user drives:
//   enroll_pipeline_item / update_pipeline_stage / cancel_sequence /
//   reschedule_followup / snooze / review_followup.
//
// A reply landing on a tracked thread rides `record_user_action` (`reply_received`), so the
// host can exit the sequence — there is no new request type for it. As with every draft in
// MailMate, a follow-up is saved for human review and never auto-sent.

/* global openDraftFromResponse */
/* exported openFollowupDraft, surfaceNeedsAttention, enrollPipelineItem, reviewFollowup,
   cancelSequence, snoozeFollowup, markPipelineStage, reportReplyOnThread */

// Open the review-required follow-up draft from a `followup_draft_ready` payload. It is
// anchored to the thread, not a single message, so it begins as a fresh compose draft the
// user edits and sends themselves.
async function openFollowupDraft(payload) {
  const draft = payload.draft || {};
  await openDraftFromResponse(
    { subject: draft.subject, body: draft.body || "", safety_notes: draft.safety_notes || [] },
    null,
  );
  console.info("[MailMate] follow-up draft ready for review:", payload.explanation);
}

// Surface a stale-item nudge (a real UI lands in Phase 12; here we log it so the wiring is
// observable).
function surfaceNeedsAttention(payload) {
  console.info(
    "[MailMate] follow-up needs attention:",
    payload.workflow_instance_id,
    payload.reason,
    payload.skipped_step_indexes,
  );
}

// Tag a sent quote/proposal as a tracked deal and arm a follow-up workflow on it.
async function enrollPipelineItem(host, messageHeader, workflowId) {
  const account = messageHeader.folder ? messageHeader.folder.accountId : undefined;
  return host.request("enroll_pipeline_item", {
    account_id: account || "",
    thread_id: messageHeader.headerMessageId || "",
    anchor_thunderbird_message_id: String(messageHeader.id),
    counterparty_email: String(messageHeader.recipients ? messageHeader.recipients[0] || "" : ""),
    counterparty_domain: "",
    title: messageHeader.subject || "",
    item_type: "quote",
    workflow_id: workflowId,
  });
}

// Resolve a surfaced follow-up draft: send (user-confirmed) / edit / skip. None auto-sends.
async function reviewFollowup(host, workflowInstanceId, resolution) {
  return host.request("review_followup", {
    workflow_instance_id: workflowInstanceId,
    resolution,
  });
}

// Stop the workflow on a pipeline item.
async function cancelSequence(host, pipelineItemId) {
  return host.request("cancel_sequence", { pipeline_item_id: pipelineItemId });
}

// Push the next follow-up step out to `nextDueAtIso` (RFC3339).
async function snoozeFollowup(host, workflowInstanceId, nextDueAtIso) {
  return host.request("snooze", {
    workflow_instance_id: workflowInstanceId,
    next_due_at: nextDueAtIso,
  });
}

// Mark a deal won/lost (closes the sequence).
async function markPipelineStage(host, pipelineItemId, stage) {
  return host.request("update_pipeline_stage", { pipeline_item_id: pipelineItemId, stage });
}

// Report a reply on a tracked thread so the host exits the sequence (rides record_user_action).
function reportReplyOnThread(host, threadHeaderMessageId) {
  host.notifyHost("record_user_action", {
    event_type: "reply_received",
    thread_id: threadHeaderMessageId,
  });
}
