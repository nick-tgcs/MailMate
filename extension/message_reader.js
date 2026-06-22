// message_reader.js — read a Thunderbird message into the host's wire shape.
//
// The extension is the only side that can call the MailExtension message APIs, so it reads
// the selected (or newly-arrived) message here and lowers it into the `classify_message` /
// `new_mail` payload the Rust host's ClassifyMessagePayload deserializes. Body text is sent
// only when retention allows it; remote content is never loaded for classification.

/* exported readMessageForHost, classifyPayloadFromHeader, currentlySelectedMessage,
   setBodyRetention */

// Whether the user has opted into sending body text to the host (privacy retention). Driven by
// the host's EFFECTIVE (consent-gated) retention level via setBodyRetention(); defaults to
// headers-only so a body is never sent before the host reports a body-retaining level.
let bodyRetentionAllowed = false;

// Update the body-retention gate from the host's effective retention level (`metadata` /
// `bodies` / `summaries`). The host already applies the consent gate, so the extension simply
// honours whatever effective level it reports. Called by background.js on connect + settings
// change. Accepts either a level string or a boolean.
function setBodyRetention(levelOrBool) {
  if (typeof levelOrBool === "boolean") {
    bodyRetentionAllowed = levelOrBool;
  } else {
    bodyRetentionAllowed = levelOrBool === "bodies" || levelOrBool === "summaries";
  }
}

// Map a Thunderbird MessageHeader (+ full headers) into the host payload.
//
// `getFull()` is read for EVERY message — not just when body retention is on — because the rich
// header set the host learns from (References, In-Reply-To, Reply-To, List-*, Precedence,
// Authentication-Results) lives only there. Those are metadata, always read locally. The body
// `getFull()` also returns is FORWARDED only when retention allows it; otherwise it is discarded.
async function readMessageForHost(messageHeader) {
  const accountId = messageHeader.folder ? messageHeader.folder.accountId : "unknown";
  const folderId = messageHeader.folder ? messageHeader.folder.path : "unknown";

  let full = null;
  try {
    full = await browser.messages.getFull(messageHeader.id);
  } catch (e) {
    console.warn("[MailMate] could not read full message:", e);
  }

  const bodyText = bodyRetentionAllowed && full ? extractPlainText(full) : null;

  let attachments = [];
  try {
    const list = await browser.messages.listAttachments(messageHeader.id);
    attachments = list.map((a) => ({
      filename: a.name,
      content_type: a.contentType,
      size_bytes: a.size || 0,
    }));
  } catch (e) {
    // listAttachments is unavailable for some message types; degrade gracefully.
  }

  const references = headerList(full, "references");
  const inReplyTo = headerValue(full, "in-reply-to");
  const messageId = messageHeader.headerMessageId || headerValue(full, "message-id");
  // A usable thread key without a native thread id: the root of the References chain, else the
  // In-Reply-To parent, else the message's own id — so a root and the replies that cite it group.
  const threadId = references[0] || inReplyTo || messageId || null;

  const senderEmail = senderAddress(messageHeader);
  const [inAddressBook, seenCount] = await Promise.all([
    senderInAddressBook(senderEmail),
    senderSeenCount(senderEmail),
  ]);

  return {
    thunderbird_message_id: String(messageHeader.id),
    account_id: accountId,
    folder_id: folderId,
    thread_id: threadId,
    headers: {
      from: addr(messageHeader.author),
      to: (messageHeader.recipients || []).map(addr),
      subject: messageHeader.subject || "",
      date: messageHeader.date ? new Date(messageHeader.date).toISOString() : null,
      message_id: messageId,
      references,
      in_reply_to: inReplyTo,
      reply_to: headerValue(full, "reply-to"),
      list_id: headerValue(full, "list-id"),
      list_unsubscribe: headerValue(full, "list-unsubscribe"),
      list_unsubscribe_post: headerValue(full, "list-unsubscribe-post"),
      precedence: headerValue(full, "precedence"),
      authentication_results: headerValue(full, "authentication-results"),
    },
    body_text: bodyText,
    body_retention_allowed: bodyRetentionAllowed,
    remote_content_loaded: false,
    attachments,
    sender_seen_count: seenCount,
    sender_in_address_book: inAddressBook,
  };
}

// The first value of a header from a getFull() part tree (headers are keyed lowercased, each a
// list of raw values), trimmed; null when absent. Pure.
function headerValue(full, name) {
  const values = full && full.headers ? full.headers[name] : null;
  if (!values || !values.length) {
    return null;
  }
  const v = String(values[0]).trim();
  return v || null;
}

// A header split into whitespace-separated tokens (References / message-id lists). Pure.
function headerList(full, name) {
  const raw = headerValue(full, name);
  return raw ? raw.split(/\s+/).filter(Boolean) : [];
}

// The bare lowercased address from a `Display Name <addr>` (or bare) author field. Pure.
function senderAddress(messageHeader) {
  const s = addr(messageHeader.author);
  const m = s.match(/<([^>]+)>/);
  return (m ? m[1] : s).trim().toLowerCase();
}

// Whether the sender is a known contact. Best-effort: returns null (not false) when the
// address-book API/permission is unavailable, so the host treats it as "unknown".
async function senderInAddressBook(email) {
  if (!email || typeof browser === "undefined" || !browser.contacts) {
    return null;
  }
  try {
    const matches = await browser.contacts.quickSearch({ searchString: email });
    return Array.isArray(matches) && matches.length > 0;
  } catch (e) {
    return null;
  }
}

// A bounded count of prior messages from this sender (first query page only — enough signal
// without paging the whole mailbox). Best-effort: null when the query API is unavailable.
async function senderSeenCount(email) {
  if (!email || typeof browser === "undefined" || !browser.messages || !browser.messages.query) {
    return null;
  }
  try {
    const page = await browser.messages.query({ author: email });
    return page && Array.isArray(page.messages) ? page.messages.length : null;
  } catch (e) {
    return null;
  }
}

// The first selected message in the active mail tab, or null.
async function currentlySelectedMessage() {
  const tabs = await browser.mailTabs.query({ active: true, currentWindow: true });
  if (!tabs.length) {
    return null;
  }
  const selected = await browser.mailTabs.getSelectedMessages(tabs[0].id);
  return selected.messages.length ? selected.messages[0] : null;
}

// Walk a getFull() part tree and concatenate text/plain bodies.
function extractPlainText(part) {
  if (!part) {
    return "";
  }
  if (part.contentType === "text/plain" && part.body) {
    return part.body;
  }
  if (part.parts) {
    return part.parts.map(extractPlainText).join("\n").trim();
  }
  return "";
}

// Normalize an address-ish field to a bare string.
function addr(value) {
  if (!value) {
    return "";
  }
  return Array.isArray(value) ? value.join(", ") : String(value);
}
