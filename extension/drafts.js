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

/* exported openDraftFromResponse, executeMailCommand, applyPlannedAction, consumeHostMove,
   consumeHostTag, getComposeDraft, stashComposeDraft */

// The MailMate draft context for an open compose window, keyed by compose tab id. The
// composeAction review panel reads this (the rationale + commitments guard + draft id the compose
// window itself can't show) via the background; it is cleared when the compose tab closes.
const composeDrafts = new Map();

function getComposeDraft(tabId) {
  return composeDrafts.get(tabId) || null;
}

// Replace the stashed context for a compose tab (used after a Regenerate re-draft).
function stashComposeDraft(tabId, ctx) {
  if (tabId != null) composeDrafts.set(tabId, ctx);
}

// Forget a compose context when its window closes (no per-tab leak across a session).
if (typeof browser !== "undefined" && browser.tabs && browser.tabs.onRemoved) {
  browser.tabs.onRemoved.addListener((tabId) => composeDrafts.delete(tabId));
}

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

// Tags MailMate itself just applied, keyed by stable Message-ID → the set of tag keys. The
// onUpdated tag listener consumes these so a host-applied tag is not echoed back as a
// user-taught `tag_changed` signal (the twin of consumeHostMove; the Phase-10 echo-suppression
// pattern, now for the tags-as-signal path).
const hostTags = new Map();

function rememberHostTag(headerMessageId, tag) {
  if (!headerMessageId) {
    return;
  }
  const set = hostTags.get(headerMessageId) || new Set();
  set.add(tag);
  hostTags.set(headerMessageId, set);
}

// True (consuming the record) if MailMate itself just added `tag` to `headerMessageId`.
function consumeHostTag(headerMessageId, tag) {
  const set = headerMessageId ? hostTags.get(headerMessageId) : null;
  if (set && set.has(tag)) {
    set.delete(tag);
    if (set.size === 0) {
      hostTags.delete(headerMessageId);
    }
    return true;
  }
  return false;
}

// Open a draft from a `draft_reply` response payload. Saved as a draft; never sent. `request` is
// the original draft context (when known): it is stashed so the compose panel's Regenerate can
// re-draft with a steer over the same thread/counterparty/excerpt.
async function openDraftFromResponse(payload, inReplyToMessageId, request) {
  const details = {
    subject: payload.subject,
    plainTextBody: payload.body,
    isPlainText: true,
  };
  // Reply from the identity that owns the replied-to message's account, so the From line is right.
  const identity = inReplyToMessageId != null ? await identityForMessage(inReplyToMessageId) : null;
  if (identity && identity.id) details.identityId = identity.id;
  let tab;
  if (inReplyToMessageId != null) {
    tab = await browser.compose.beginReply(Number(inReplyToMessageId), "replyToSender", details);
  } else {
    tab = await browser.compose.beginNew(details);
  }
  // Persist as a draft for review — the review surface shows the commitments guard alongside.
  await browser.compose.saveMessage(tab.id, { mode: "draft" });
  // Stash the review context so the composeAction panel can annotate this draft (rationale + the
  // typed commitments guard + the request, so Regenerate can re-draft) — the compose window can't
  // show those on its own. Body is intentionally NOT stored; it's editable in the compose area.
  composeDrafts.set(tab.id, {
    draft_id: payload.draft_id || null,
    subject: payload.subject || "",
    safety_notes: payload.safety_notes || [],
    commitments: payload.commitments || null,
    requires_human_review: payload.requires_human_review !== false, // advisory by construction
    rationale: payload.rationale || (payload.explanation && payload.explanation.summary) || null,
    request: request || null,
    reply_to_message_id: inReplyToMessageId != null ? Number(inReplyToMessageId) : null,
    // The identity the reply goes out from ("Replying from: …" in the panel), when known.
    from_identity: identity && identity.email ? identity.email : null,
    // The body MailMate drafted, kept ONLY to detect later user edits (the edit-divergence signal);
    // it never leaves the extension and is dropped when the tab closes.
    drafted_body: payload.body || "",
  });
  return tab;
}

// The identity ({ id, email }) whose account owns `messageId`, so a reply goes out from the right
// address. Best-effort: any failure (no identity, API absent) returns null and Thunderbird picks
// its default.
async function identityForMessage(messageId) {
  try {
    const header = await browser.messages.get(Number(messageId));
    const accountId = header && header.folder && header.folder.accountId;
    if (!accountId || !browser.identities) return null;
    const identities = await browser.identities.list(accountId);
    if (!identities || !identities.length) return null;
    return { id: identities[0].id, email: identities[0].email || null };
  } catch {
    return null;
  }
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
        const folder = resolveFolder(header.folder.accountId, action.to_folder);
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

// Add a tag key to a message, preserving existing tags. Remembers the host-applied tag (by
// stable Message-ID) BEFORE updating, so the resulting onUpdated echo is not re-reported as a
// user-taught tag signal.
async function addTag(messageId, tag) {
  const header = await browser.messages.get(messageId);
  rememberHostTag(header.headerMessageId, tag);
  const tags = new Set(header.tags || []);
  tags.add(tag);
  await browser.messages.update(messageId, { tags: Array.from(tags) });
}

// Resolve a host-supplied destination folder path to the {accountId, path} descriptor
// Thunderbird's messages.move() accepts. The host's FolderId carries only the path (the wire
// drops the account), so the account is taken from the message being moved — moves stay within
// the message's own account, which is the case for every triage move.
function resolveFolder(accountId, folderPath) {
  return { accountId, path: folderPath };
}

// The host mints internal ids as `msg_tb_<thunderbird_id>`; recover the Thunderbird id.
function internalToThunderbirdId(internalId) {
  const prefix = "msg_tb_";
  const raw = internalId.startsWith(prefix) ? internalId.slice(prefix.length) : internalId;
  const numeric = Number(raw);
  return Number.isNaN(numeric) ? raw : numeric;
}
