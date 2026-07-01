// native.js — thin wrapper over the MailMate native messaging host.
//
// Thunderbird frames each `port.postMessage(obj)` as a 32-bit native-byte-order length
// prefix + UTF-8 JSON; the Rust host (mailmate-native-host) decodes exactly that. The
// wire contract these helpers produce is validated by the host's e2e harness test
// (crates/mailmate-native-host/tests/extension_harness.rs) and the protocol DTO tests.

/* exported NativeHost, makePing, PROTOCOL_VERSION, NATIVE_HOST, HOST_PHASE */

// Reverse-DNS host name; must match NativeHostManifest::HOST_NAME in the Rust host.
const NATIVE_HOST = "com.mailmate.host";
const PROTOCOL_VERSION = "1.0";

// The extension's own version (kept in sync with manifest.json `version`), sent in the
// `hello` handshake so the host can report a version mismatch.
const EXTENSION_VERSION = "0.1.0";

// How long to wait for the `hello` reply before declaring the channel dead, and how often
// to heartbeat (a `ping`) to detect a silently-wedged port.
const HELLO_TIMEOUT_MS = 1500;
const HEARTBEAT_MS = 30000;

// The connection lifecycle a surface renders from. Mirrors interaction-design.md
// §"Connection health": every status surface (toolbar badge, recovery card, per-message
// panel) derives from one HostStatus, so they can never disagree.
const HOST_PHASE = {
  connecting: "connecting",
  ready: "ready",
  disconnected: "disconnected",
  versionMismatch: "version_mismatch",
};

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

// A long-lived native port wrapped with request/response correlation, a notification
// fan-out, and a single source-of-truth connection status. The host may push a notification
// (classification_ready / mail_command / followup_*) at any time, so we register an onMessage
// handler — not just reply matching.
//
// On connect it performs the `hello` handshake (host/protocol version, capabilities, the
// secret-free drafting/retention posture) and then heartbeats with `ping`. The status it
// derives drives every connection surface; subscribers register via `onStatusChange`.
class NativeHost {
  constructor() {
    this.pending = new Map(); // request_id -> {resolve, reject}
    this.notificationHandlers = []; // (type, payload) => void
    this.statusHandlers = []; // (status) => void
    this.heartbeatTimer = null;

    // The authoritative status every surface reads. `lastPongAt` drives staleness; the
    // hello fields (hostVersion/protocol/capabilities/retention/drafting) gate feature UI;
    // `reason` carries the raw disconnect message for the recovery card.
    this.status = {
      phase: HOST_PHASE.connecting,
      lastPongAt: null,
      hostVersion: null,
      protocol: null,
      capabilities: [],
      retention: null,
      drafting: null,
      reason: null,
    };

    this._connect();
  }

  // (Re)open the native port and run the handshake. Used by the constructor and `reconnect`.
  _connect() {
    this._patchStatus({ phase: HOST_PHASE.connecting, reason: null });
    this.port = browser.runtime.connectNative(NATIVE_HOST);
    this.port.onMessage.addListener((message) => this._onMessage(message));
    this.port.onDisconnect.addListener((p) => this._onDisconnect(p));
    this._handshake();
    this._startHeartbeat();
  }

  // Run the `hello` handshake: prove the channel, learn the host/protocol version and
  // capabilities, and decide ready vs version_mismatch. A timeout or error → disconnected,
  // so a wedged host never leaves the UI stuck on "connecting".
  _handshake() {
    this._withTimeout(
      this.request("hello", {
        extension_version: EXTENSION_VERSION,
        protocol_version: PROTOCOL_VERSION,
      }),
      HELLO_TIMEOUT_MS,
    )
      .then((info) => {
        const protocol = info.protocol_version || null;
        // A protocol the extension cannot speak is a calm, distinct state — we stop here
        // rather than send a real request that would garble the single-writer stdout channel.
        const phase =
          protocol && protocol !== PROTOCOL_VERSION
            ? HOST_PHASE.versionMismatch
            : HOST_PHASE.ready;
        this._patchStatus({
          phase,
          lastPongAt: Date.now(),
          hostVersion: info.host_version || null,
          protocol,
          capabilities: info.capabilities || [],
          retention: info.retention_level || null,
          drafting: info.drafting_available ? "available" : "no_provider",
          reason: null,
        });
      })
      .catch((e) => {
        this._patchStatus({
          phase: HOST_PHASE.disconnected,
          reason: `handshake failed: ${e && e.message ? e.message : e}`,
        });
      });
  }

  // Heartbeat with `ping` so a silently-wedged port (no onDisconnect) is still detected. A
  // failed round-trip declares it disconnected; the user retries from the recovery card.
  _startHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
    }
    this.heartbeatTimer = setInterval(() => {
      this._withTimeout(this.request("ping", { nonce: "hb" }), HELLO_TIMEOUT_MS)
        .then(() => {
          if (this.status.phase === HOST_PHASE.ready) {
            this._patchStatus({ lastPongAt: Date.now() });
          }
        })
        .catch(() => {
          // The port is alive enough to reject but not to answer — treat as stale.
          if (this.status.phase === HOST_PHASE.ready) {
            this._patchStatus({
              phase: HOST_PHASE.disconnected,
              reason: "heartbeat timed out",
            });
          }
        });
    }, HEARTBEAT_MS);
  }

  // Tear down and re-establish the port (the Retry affordance). The MV3 event page's
  // connectNative fires once at construction with no auto-retry, so reconnection is explicit.
  reconnect() {
    try {
      this.port.disconnect();
    } catch (e) {
      // Already gone — fine; we are about to reconnect.
    }
    for (const { reject } of this.pending.values()) {
      reject(new Error("reconnecting"));
    }
    this.pending.clear();
    this._connect();
    return this.status;
  }

  _onDisconnect(p) {
    const error = p.error ? p.error.message : "(no error)";
    for (const { reject } of this.pending.values()) {
      reject(new Error(`native host disconnected: ${error}`));
    }
    this.pending.clear();
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    this._patchStatus({ phase: HOST_PHASE.disconnected, reason: error });
    console.warn("[MailMate] native host disconnected:", error);
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

  // Subscribe to connection-status changes; fires immediately with the current status so a
  // late subscriber paints correctly.
  onStatusChange(handler) {
    this.statusHandlers.push(handler);
    handler(this.status);
  }

  // Merge a status patch and notify subscribers (immutable snapshot per change).
  _patchStatus(patch) {
    this.status = { ...this.status, ...patch };
    for (const handler of this.statusHandlers) {
      handler(this.status);
    }
  }

  // Reject a promise if it does not settle within `ms` (used to bound the handshake/heartbeat
  // so a dead host never hangs the status forever).
  _withTimeout(promise, ms) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timed out after ${ms}ms`)), ms);
      promise.then(
        (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        (e) => {
          clearTimeout(timer);
          reject(e);
        },
      );
    });
  }

  // Send a request and resolve with its response payload (or reject on host error). An optional
  // `timeoutMs` rejects (and forgets the pending entry) if the host never answers — without it a
  // wedged request hangs forever and reaches the caller only as a silent "no response". The timeout
  // is opt-in so long-running calls (LLM drafting) are unaffected; the fast admin/discovery
  // round-trips pass one so a stalled host surfaces as a clean, logged error.
  request(type, payload, timeoutMs) {
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
      let timer = null;
      if (timeoutMs && timeoutMs > 0) {
        timer = setTimeout(() => {
          // Fire only if still pending — a real response or a disconnect may have settled it first.
          if (this.pending.delete(requestId)) {
            reject(new Error(`request ${type} timed out after ${timeoutMs}ms`));
          }
        }, timeoutMs);
        // Clear the timer however the request settles (response via _onMessage, or disconnect).
        const entry = this.pending.get(requestId);
        entry.resolve = (v) => {
          clearTimeout(timer);
          resolve(v);
        };
        entry.reject = (e) => {
          clearTimeout(timer);
          reject(e);
        };
      }
      try {
        this.port.postMessage(frame);
      } catch (e) {
        this.pending.delete(requestId);
        if (timer) {
          clearTimeout(timer);
        }
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
