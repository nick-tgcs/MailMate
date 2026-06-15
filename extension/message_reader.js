// message_reader.js — read a Thunderbird message into the host's wire shape.
//
// The extension is the only side that can call the MailExtension message APIs, so it reads
// the selected (or newly-arrived) message here and lowers it into the `classify_message` /
// `new_mail` payload the Rust host's ClassifyMessagePayload deserializes. Body text is sent
// only when retention allows it; remote content is never loaded for classification.

/* exported readMessageForHost, classifyPayloadFromHeader, currentlySelectedMessage */

// Whether the user has opted into sending body text to the host (privacy retention). The
// real value comes from settings (Phase 12); default to headers-only.
const BODY_RETENTION_ALLOWED = false;

// Map a Thunderbird MessageHeader (+ optional full body) into the host payload.
async function readMessageForHost(messageHeader) {
  const accountId = messageHeader.folder ? messageHeader.folder.accountId : "unknown";
  const folderId = messageHeader.folder ? messageHeader.folder.path : "unknown";

  let bodyText = null;
  let attachments = [];
  if (BODY_RETENTION_ALLOWED) {
    try {
      const full = await browser.messages.getFull(messageHeader.id);
      bodyText = extractPlainText(full);
    } catch (e) {
      console.warn("[MailMate] could not read body:", e);
    }
  }
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

  return {
    thunderbird_message_id: String(messageHeader.id),
    account_id: accountId,
    folder_id: folderId,
    thread_id: null,
    headers: {
      from: addr(messageHeader.author),
      to: (messageHeader.recipients || []).map(addr),
      subject: messageHeader.subject || "",
      date: messageHeader.date ? new Date(messageHeader.date).toISOString() : null,
      message_id: messageHeader.headerMessageId || null,
      references: [],
      in_reply_to: null,
    },
    body_text: bodyText,
    body_retention_allowed: BODY_RETENTION_ALLOWED,
    remote_content_loaded: false,
    attachments,
  };
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
