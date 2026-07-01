// native.test.mjs — exercise the REAL NativeHost port wrapper headlessly.
//
// native.js owns the request/response correlation, notification fan-out, hello handshake,
// heartbeat, timeout and reconnect logic every connection surface reads from. It can't be
// imported (classic background script), so we load it into a jsdom realm with a controllable
// fake native port (bg-harness) and drive the wire: push host frames, observe outbound frames,
// assert the derived HostStatus. This is the layer the Rust e2e harness can't reach.

import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import { loadScripts, tick } from "./bg-harness.mjs";

const live = [];
afterEach(() => {
  while (live.length) live.pop().dispose();
});

// Load native.js and construct a NativeHost, returning the host + the port/state to drive it.
// Captures the heartbeat setInterval callback into state.intervals so the test can fire it on
// demand (the real interval is 30s) without keeping a live 30s timer around.
function freshHost(opts = {}) {
  const h = loadScripts(["native.js"], {
    ...opts,
    window: (w, state) => {
      state.intervals = [];
      const orig = w.setInterval.bind(w);
      w.setInterval = (fn) => {
        state.intervals.push(fn);
        return orig(() => {}, 1e7); // park a harmless timer; dispose() clears it
      };
      if (opts.window) opts.window(w, state);
    },
  });
  live.push(h);
  const host = new h.exports.NativeHost();
  return { host, ...h };
}

// The request_id native.js minted for the Nth outbound frame of `type`.
function lastFrame(state, type) {
  return [...state.sent].reverse().find((f) => f.type === type);
}

// Answer the pending request of `type` with an ok payload.
function answerOk(state, port, type, payload = {}) {
  const f = lastFrame(state, type);
  port.emit({ kind: "response", request_id: f.request_id, status: "ok", payload });
}

test("makePing builds a correlatable ping envelope echoing the nonce", () => {
  const { exports } = freshHost();
  const ping = exports.makePing("abc");
  assert.equal(ping.kind, "request");
  assert.equal(ping.type, "ping");
  assert.equal(ping.request_id, "req_ping_abc");
  assert.equal(ping.payload.nonce, "abc");
  assert.equal(ping.protocol_version, exports.PROTOCOL_VERSION);
});

test("the constructor connects and sends a hello handshake", () => {
  const { state } = freshHost();
  const hello = lastFrame(state, "hello");
  assert.ok(hello, "a hello frame was posted on connect");
  assert.equal(hello.payload.protocol_version, "1.0");
  assert.ok(hello.payload.extension_version);
});

test("a successful handshake derives the ready status with host metadata", async () => {
  const { host, state, port, window } = freshHost();
  const seen = [];
  host.onStatusChange((s) => seen.push(s.phase));
  answerOk(state, port, "hello", {
    host_version: "9.9",
    protocol_version: "1.0",
    capabilities: ["draft"],
    retention_level: "bodies",
    drafting_available: true,
  });
  await tick(window, 4);
  assert.equal(host.status.phase, "ready");
  assert.equal(host.status.hostVersion, "9.9");
  assert.deepEqual(host.status.capabilities, ["draft"]);
  assert.equal(host.status.retention, "bodies");
  assert.equal(host.status.drafting, "available");
  // onStatusChange fires immediately (connecting) then again on ready.
  assert.ok(seen.includes("ready"));
});

test("a protocol the extension can't speak derives version_mismatch", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "2.0" });
  await tick(window, 4);
  assert.equal(host.status.phase, "version_mismatch");
});

test("a host error on hello derives disconnected with the reason", async () => {
  const { host, state, port, window } = freshHost();
  const hello = lastFrame(state, "hello");
  port.emit({
    kind: "response",
    request_id: hello.request_id,
    status: "error",
    error: { code: "boom", message: "no" },
  });
  await tick(window, 4);
  assert.equal(host.status.phase, "disconnected");
  assert.match(host.status.reason, /handshake failed/);
});

test("request resolves with the response payload and rejects on host error", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);

  const okP = host.request("get_settings", {});
  answerOk(state, port, "get_settings", { paused: true });
  assert.deepEqual(await okP, { paused: true });

  const errP = host.request("explode", {});
  const f = lastFrame(state, "explode");
  port.emit({ kind: "response", request_id: f.request_id, status: "error", error: { code: "c", message: "m" } });
  await assert.rejects(errP, /c: m/);
});

test("an unsolicited notification fans out to registered handlers", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  const got = [];
  host.onNotification((type, payload) => got.push([type, payload]));
  port.emit({ kind: "notification", type: "classification_ready", payload: { id: "m1" } });
  assert.deepEqual(got, [["classification_ready", { id: "m1" }]]);
});

test("a stale/unknown response id is ignored, not thrown", async () => {
  const { host, port } = freshHost();
  // No pending entry for this id — must be a silent no-op.
  assert.doesNotThrow(() => port.emit({ kind: "response", request_id: "nope", status: "ok", payload: {} }));
  assert.ok(host);
});

test("request with timeoutMs rejects and forgets the pending entry when unanswered", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  const p = host.request("slow", {}, 10);
  await assert.rejects(p, /slow timed out after 10ms/);
});

test("reconnect rejects pending requests, reopens the port, and clears the heartbeat", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  const pending = host.request("never", {});
  const before = state.sent.length;
  host.reconnect();
  await assert.rejects(pending, /reconnecting/);
  // Reconnect re-runs the handshake -> a new hello frame is posted.
  assert.ok(state.sent.length > before);
  assert.ok(lastFrame(state, "hello"));
});

test("onDisconnect rejects pending requests and marks disconnected", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  const pending = host.request("inflight", {});
  port.emitDisconnect({ message: "pipe closed" });
  await assert.rejects(pending, /native host disconnected/);
  assert.equal(host.status.phase, "disconnected");
  assert.equal(host.status.reason, "pipe closed");
});

test("notifyHost posts a fire-and-forget frame with no pending entry", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  host.notifyHost("record_user_action", { event_type: "reply_received" });
  const f = lastFrame(state, "record_user_action");
  assert.ok(f);
  assert.equal(f.payload.event_type, "reply_received");
  assert.equal(host.pending.size, 0);
});

test("a postMessage failure rejects the request synchronously", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  port.postMessage = () => {
    throw new Error("port dead");
  };
  await assert.rejects(host.request("x", {}), /port dead/);
  assert.equal(host.pending.size, 0);
});

test("a timed request that IS answered clears its timer and resolves/rejects", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);

  // Resolve path: answered before the (generous) timeout -> clears timer, resolves.
  const okP = host.request("quick", {}, 5000);
  answerOk(state, port, "quick", { v: 1 });
  assert.deepEqual(await okP, { v: 1 });

  // Reject path: host error before timeout -> clears timer, rejects.
  const errP = host.request("nope", {}, 5000);
  const f = lastFrame(state, "nope");
  port.emit({ kind: "response", request_id: f.request_id, status: "error", error: { code: "e", message: "x" } });
  await assert.rejects(errP, /e: x/);
  assert.equal(host.pending.size, 0);
});

test("a timed request whose postMessage throws clears the timer and rejects", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  port.postMessage = () => {
    throw new Error("dead");
  };
  await assert.rejects(host.request("x", {}, 5000), /dead/);
  assert.equal(host.pending.size, 0);
});

test("a heartbeat round-trip refreshes lastPongAt while ready", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  const before = host.status.lastPongAt;
  assert.equal(state.intervals.length, 1, "the heartbeat interval was registered");
  state.intervals[0](); // fire the heartbeat tick
  answerOk(state, port, "ping", { nonce: "hb" });
  await tick(window, 3);
  assert.ok(host.status.lastPongAt >= before);
  assert.equal(host.status.phase, "ready");
});

test("a heartbeat that can't round-trip marks the ready host disconnected", async () => {
  const { host, state, port, window } = freshHost();
  answerOk(state, port, "hello", { protocol_version: "1.0" });
  await tick(window, 2);
  port.postMessage = () => {
    throw new Error("wedged");
  };
  state.intervals[0](); // heartbeat tick -> ping request rejects -> stale
  await tick(window, 3);
  assert.equal(host.status.phase, "disconnected");
  assert.match(host.status.reason, /heartbeat timed out/);
});
