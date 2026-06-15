// action.js — the toolbar popup (connection mini-hub / recovery card).
//
// The popup owns no native port; it reads the single HostStatus from the background
// (`mm:getStatus`), renders the connection state, offers a one-click Retry (`mm:reconnect`)
// when the host is down, and live-updates while open (`mm:statusChanged`). This is the
// recovery path for the otherwise-invisible "host not connected" failure.

const PHASE_LABEL = {
  ready: "Connected",
  connecting: "Connecting…",
  disconnected: "Offline",
  version_mismatch: "Version mismatch",
};

const els = {
  dot: document.getElementById("mm-dot"),
  phase: document.getElementById("mm-phase"),
  body: document.getElementById("mm-body"),
  footer: document.getElementById("mm-footer"),
};

// Render the whole popup from one status snapshot.
function render(status) {
  const phase = status.phase || "connecting";
  els.dot.className = `mm-dot mm-dot--${phase}`;
  els.phase.textContent = PHASE_LABEL[phase] || "Connecting…";
  els.body.replaceChildren();
  els.footer.replaceChildren();

  if (phase === "ready") {
    renderReady(status);
  } else if (phase === "version_mismatch") {
    renderVersionMismatch(status);
  } else if (phase === "disconnected") {
    renderDisconnected(status);
  } else {
    addParagraph(els.body, "Checking the connection to MailMate's background helper…", "mm-muted");
  }
}

function renderReady(status) {
  addParagraph(els.body, "MailMate is connected and watching your mail.");
  const bits = [];
  if (status.hostVersion) {
    bits.push(`helper v${status.hostVersion}`);
  }
  if (status.protocol) {
    bits.push(`protocol ${status.protocol}`);
  }
  if (status.retention) {
    bits.push(`retention: ${status.retention}`);
  }
  if (status.drafting) {
    bits.push(status.drafting === "available" ? "drafting on" : "drafting off");
  }
  if (bits.length) {
    addParagraph(els.body, bits.join(" · "), "mm-meta");
  }
}

function renderVersionMismatch(status) {
  addParagraph(
    els.body,
    "MailMate's extension and its background helper are on different protocol versions. " +
      "Update whichever is older — your mail is unaffected in the meantime.",
  );
  addParagraph(
    els.body,
    `extension protocol 1.0 · helper protocol ${status.protocol || "?"}`,
    "mm-meta",
  );
  addRetryButton();
}

function renderDisconnected(status) {
  addParagraph(els.body, "Not connected to the background helper. New mail isn't being read.");
  addParagraph(els.body, "Your mail is unaffected.", "mm-muted");
  if (status.reason) {
    addParagraph(els.body, `Reason: ${status.reason}`, "mm-reason");
  }
  addRetryButton();
}

function addRetryButton() {
  const retry = document.createElement("button");
  retry.textContent = "↻ Retry now";
  retry.className = "mm-primary";
  retry.addEventListener("click", async () => {
    retry.disabled = true;
    retry.textContent = "Reconnecting…";
    const { status } = await browser.runtime.sendMessage({ type: "mm:reconnect" });
    render(status);
  });
  els.footer.append(retry);
}

function addParagraph(parent, text, className) {
  const p = document.createElement("p");
  p.textContent = text;
  if (className) {
    p.className = className;
  }
  parent.append(p);
}

// Live updates while the popup is open.
browser.runtime.onMessage.addListener((message) => {
  if (message && message.type === "mm:statusChanged") {
    render(message.status);
  }
});

// Initial paint.
browser.runtime
  .sendMessage({ type: "mm:getStatus" })
  .then((reply) => render(reply.status))
  .catch(() => render({ phase: "disconnected", reason: "background page not responding" }));
