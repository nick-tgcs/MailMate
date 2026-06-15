// drafts.js — apply safe actions, open review-required draft replies, and execute the
// host's mail commands.
//
// Two inbound paths reach here:
//   1. A `draft_reply` RESPONSE -> open a Thunderbird draft via compose.* (review-required,
//      saved as a draft, NEVER sent).
//   2. A host `mail_command` NOTIFICATION (apply / create_draft) -> execute the safe action
//      through the MailExtension APIs, then report the execution result back to the host.
//
// These are the "apply safe actions returned by the Rust host" and "open draft replies"
// capabilities. There is intentionally no send path — drafts are saved for human review.

/* exported openDraftFromResponse, executeMailCommand, applyPlannedAction, consumeHostMove */

// Moves MailMate itself just commanded, keyed by the stable RFC Message-ID (which survives a
// folder move; the numeric Thunderbird id does not). background.js consumes this in its
// onMoved listener so a host-applied move is NOT echoed back as a user-taught filing
// correction (it was already recorded once, in the audit log).
const hostMoves = new Map();

function rememberHostMove(headerMessageId, folder) {
  if (headerMessageId) {
    hostMoves.set(headerMessageId, { folder, at: Date.now() });
  }
}

// True (consuming the record) if `headerMessageId` was a move MailMate commanded.
function consumeHostMove(headerMessageId) {
  if (headerMessageId && hostMoves.has(headerMessageId)) {
    hostMoves.delete(headerMessageId);
    return true;
  }
  return false;
}

// Open a draft from a `draft_reply` response payload. Saved as a draft; never sent.
async function openDraftFromResponse(payload, inReplyToMessageId) {
  const details = {
    subject: payload.subject,
    plainTextBody: payload.body,
    isPlainText: true,
  };
  let tab;
  if (inReplyToMessageId != null) {
    tab = await browser.compose.beginReply(Number(inReplyToMessageId), "replyToSender", details);
  } else {
    tab = await browser.compose.beginNew(details);
  }
  // Persist as a draft for review — the review surface shows payload.safety_notes alongside.
  await browser.compose.saveMessage(tab.id, { mode: "draft" });
  return tab;
}

// Execute one host `mail_command` (apply / create_draft) and return an execution result the
// caller reports back to the host via record_user_action.
//
// The host wraps every command one level deep: { command, body: {...} } (see
// ThunderbirdMailClient::send_command), so BOTH branches read from `payload.body`.
async function executeMailCommand(payload) {
  const command = payload.command;
  const body = payload.body || {};
  if (command === "apply") {
    return applyPlannedAction(body);
  }
  if (command === "create_draft") {
    const spec = body.spec || {};
    // in_reply_to is the host's internal id (`msg_tb_<id>`); invert it to Thunderbird's id.
    const inReplyTo = spec.in_reply_to ? internalToThunderbirdId(spec.in_reply_to) : null;
    await openDraftFromResponse(
      { subject: spec.subject, body: spec.body, safety_notes: spec.safety_notes || [] },
      inReplyTo,
    );
    return { event_type: "action_applied", result: "ok", draft_id: body.draft_id };
  }
  return { event_type: "action_failed", result: `unknown command: ${command}` };
}

// Apply a single safe action (tag / move / mark_junk / mark_read / flag) to a message.
async function applyPlannedAction(action) {
  const messageId = internalToThunderbirdId(action.message_id);
  try {
    switch (action.kind) {
      case "tag":
        await addTag(messageId, action.tag);
        break;
      case "move": {
        // Remember this host-commanded move (by stable Message-ID) BEFORE moving, so the
        // resulting onMoved echo is not re-reported as a user filing correction.
        const header = await browser.messages.get(messageId);
        rememberHostMove(header.headerMessageId, action.to_folder);
        const folder = await resolveFolder(action.to_folder);
        await browser.messages.move([messageId], folder);
        break;
      }
      case "mark_junk":
        await browser.messages.update(messageId, { junk: action.junk });
        break;
      case "mark_read":
        await browser.messages.update(messageId, { read: action.read });
        break;
      case "flag":
        await browser.messages.update(messageId, { flagged: action.flagged });
        break;
      default:
        return { event_type: "action_failed", result: `unsupported kind: ${action.kind}` };
    }
  } catch (e) {
    return { event_type: "action_failed", thunderbird_message_id: String(messageId), result: String(e) };
  }
  return { event_type: "action_applied", thunderbird_message_id: String(messageId), result: "ok" };
}

// Add a tag key to a message, preserving existing tags.
async function addTag(messageId, tag) {
  const header = await browser.messages.get(messageId);
  const tags = new Set(header.tags || []);
  tags.add(tag);
  await browser.messages.update(messageId, { tags: Array.from(tags) });
}

// Resolve a folder path string to a MailFolder, re-resolved per session.
async function resolveFolder(folderPath) {
  // A move target is a session-scoped folder path; the extension re-resolves it against the
  // account tree. The simplest robust form is to pass the path through (Thunderbird accepts a
  // {accountId, path} or a MailFolder); production wiring caches the account id.
  return folderPath;
}

// The host mints internal ids as `msg_tb_<thunderbird_id>`; recover the Thunderbird id.
function internalToThunderbirdId(internalId) {
  const prefix = "msg_tb_";
  const raw = internalId.startsWith(prefix) ? internalId.slice(prefix.length) : internalId;
  const numeric = Number(raw);
  return Number.isNaN(numeric) ? raw : numeric;
}
