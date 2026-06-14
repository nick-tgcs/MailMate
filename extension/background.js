// background.js — Phase 1 skeleton: prove the native channel with a startup ping.
//
// Later phases register messages.onNewMailReceived here (synchronously, at the top of
// the event page) and route classification/follow-up notifications to the UI. For now
// this only opens the port and pings, logging the pong.

const port = connectHost();

port.onMessage.addListener((message) => {
  // Host → extension frames: responses (correlated by request_id) and unsolicited
  // notifications (classification_ready / followup_* in later phases).
  console.info("[MailMate] native host message:", message);
});

port.onDisconnect.addListener((p) => {
  const error = p.error ? p.error.message : "(no error)";
  console.warn("[MailMate] native host disconnected:", error);
});

// A per-session nonce so the pong is unambiguously ours.
const nonce = Date.now().toString(36) + Math.random().toString(36).slice(2);
port.postMessage(makePing(nonce));
