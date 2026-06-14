// native.js — thin wrapper over the MailMate native messaging host.
//
// Thunderbird frames each `port.postMessage(obj)` as a 32-bit native-byte-order length
// prefix + UTF-8 JSON; the Rust host (mailmate-native-host) decodes exactly that. The
// wire contract these helpers produce is validated by the host's e2e harness test
// (crates/mailmate-native-host/tests/extension_harness.rs).

/* exported connectHost, makePing, PROTOCOL_VERSION, NATIVE_HOST */

// Reverse-DNS host name; must match NativeHostManifest::HOST_NAME in the Rust host.
const NATIVE_HOST = "com.mailmate.host";
const PROTOCOL_VERSION = "1.0";

// Open the long-lived native port. The host may push notifications at any time, so the
// extension keeps this port and registers an onMessage handler (not just request/reply
// correlation).
function connectHost() {
  return browser.runtime.connectNative(NATIVE_HOST);
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
