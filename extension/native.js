// native.js — thin wrapper over the MailMate native messaging host.
//
// Thunderbird frames each `port.postMessage(obj)` as a 32-bit native-byte-order length
// prefix + UTF-8 JSON; the Rust host (mailmate-native-host) decodes exactly that. The
// wire contract these helpers produce is validated by the host's e2e harness test
// (crates/mailmate-native-host/tests/extension_harness.rs) and the protocol DTO tests.

/* exported NativeHost, makePing, PROTOCOL_VERSION, NATIVE_HOST */

// Reverse-DNS host name; must match NativeHostManifest::HOST_NAME in the Rust host.
const NATIVE_HOST = "com.mailmate.host";
const PROTOCOL_VERSION = "1.0";

// A monotonic per-session request-id counter (cheap correlation beyond the nonce).
let requestSeq = 0;
function nextRequestId(kind) {
  requestSeq += 1;
  return `req_${kind}_${requestSeq}`;
}

// Build a `ping` request envelope. `request_id` is echoed back by the host for
// correlation; `payload.nonce` is echoed in the pong.
function makePing(nonce) {
  return {
    protocol_version: PROTOCOL_VERSION,
    kind: "request",
    request_id: `req_ping_${nonce}`,
    type: "ping",
    payload: { nonce },
  };
}

// A long-lived native port wrapped with request/response correlation AND a notification
// fan-out. The host may push a notification (classification_ready / mail_command /
// followup_*) at any time, so we register an onMessage handler — not just reply matching.
class NativeHost {
  constructor() {
    this.port = browser.runtime.connectNative(NATIVE_HOST);
    this.pending = new Map(); // request_id -> {resolve, reject}
    this.notificationHandlers = []; // (type, payload) => void

    this.port.onMessage.addListener((message) => this._onMessage(message));
    this.port.onDisconnect.addListener((p) => {
      const error = p.error ? p.error.message : "(no error)";
      for (const { reject } of this.pending.values()) {
        reject(new Error(`native host disconnected: ${error}`));
      }
      this.pending.clear();
      console.warn("[MailMate] native host disconnected:", error);
    });
  }

  _onMessage(message) {
    if (message.kind === "response") {
      const entry = this.pending.get(message.request_id);
      if (!entry) {
        return; // a stale/duplicate response; ignore
      }
      this.pending.delete(message.request_id);
      if (message.status === "ok") {
        entry.resolve(message.payload || {});
      } else {
        const err = message.error || { code: "unknown", message: "host error" };
        entry.reject(new Error(`${err.code}: ${err.message}`));
      }
      return;
    }
    if (message.kind === "notification") {
      for (const handler of this.notificationHandlers) {
        handler(message.type, message.payload || {});
      }
    }
  }

  // Register a handler for unsolicited host notifications.
  onNotification(handler) {
    this.notificationHandlers.push(handler);
  }

  // Send a request and resolve with its response payload (or reject on host error).
  request(type, payload) {
    const requestId = nextRequestId(type);
    const frame = {
      protocol_version: PROTOCOL_VERSION,
      kind: "request",
      request_id: requestId,
      type,
      payload: payload || {},
    };
    return new Promise((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject });
      try {
        this.port.postMessage(frame);
      } catch (e) {
        this.pending.delete(requestId);
        reject(e);
      }
    });
  }

  // Fire-and-forget: forward a background event the host answers with a notification
  // (e.g. new_mail -> classification_ready), so we do not await a response.
  notifyHost(type, payload) {
    this.port.postMessage({
      protocol_version: PROTOCOL_VERSION,
      kind: "request",
      request_id: nextRequestId(type),
      type,
      payload: payload || {},
    });
  }
}
