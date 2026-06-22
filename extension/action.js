// action.js — the toolbar mini-hub (connection health + what-needs-me + quick actions).
//
// The popup owns no native port; it reads the single HostStatus from the background
// (`mm:getStatus`) and the work aggregate (`mm:aggregate`), renders the connection state, and —
// when connected — surfaces the aggregate breakdown (suggestions to review, follow-ups needing
// attention, rule proposals) with deep-links into the right dashboard tab, an Open dashboard
// button, and a Pause/Resume kill-switch. When the host is down it falls back to the recovery card
// (one-click Retry). It live-updates while open (`mm:statusChanged`). It never acts on mail.

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

// Render the whole popup from a status snapshot + (when ready) the work aggregate.
function render(status, aggregate) {
  const phase = status.phase || "connecting";
  els.dot.className = `mm-dot mm-dot--${phase}`;
  els.phase.textContent = PHASE_LABEL[phase] || "Connecting…";
  els.body.replaceChildren();
  els.footer.replaceChildren();

  if (phase === "ready") {
    renderReady(status, aggregate);
  } else if (phase === "version_mismatch") {
    renderVersionMismatch(status);
  } else if (phase === "disconnected") {
    renderDisconnected(status);
  } else {
    addParagraph(els.body, "Checking the connection to MailMate's background helper…", "mm-muted");
  }
}

function renderReady(status, aggregate) {
  renderAggregate(aggregate);

  const bits = [];
  if (status.hostVersion) bits.push(`helper v${status.hostVersion}`);
  if (status.protocol) bits.push(`protocol ${status.protocol}`);
  if (status.retention) bits.push(`retention: ${status.retention}`);
  if (status.drafting) bits.push(status.drafting === "available" ? "drafting on" : "drafting off");
  if (bits.length) addParagraph(els.body, bits.join(" · "), "mm-meta");

  // Footer: Open dashboard + the Pause/Resume kill-switch.
  const open = document.createElement("button");
  open.className = "mm-primary";
  open.textContent = "Open dashboard ▸";
  open.addEventListener("click", () => openDash(null));
  els.footer.append(open);

  els.footer.append(pauseToggle(aggregate));
}

// The "what needs me" breakdown — each line deep-links into the matching dashboard tab. Calm,
// affirmative line when nothing is waiting; a neutral fallback when the aggregate didn't load.
function renderAggregate(aggregate) {
  if (!aggregate || !aggregate.ok) {
    addParagraph(els.body, "MailMate is connected and watching your mail.");
    return;
  }
  if (Boolean(aggregate.paused)) {
    addParagraph(els.body, "⏸ MailMate is paused — it’s watching but applying nothing.", "mm-paused");
  }
  const total = aggregate.total || 0;
  if (total === 0) {
    addParagraph(els.body, "✓ Nothing needs you right now.", "mm-good");
    return;
  }
  addParagraph(els.body, `${total} thing${total === 1 ? "" : "s"} need your attention:`);
  const ul = document.createElement("ul");
  ul.className = "mm-agg";
  const lines = [];
  if (aggregate.reviews) lines.push([`${aggregate.reviews} suggestion${aggregate.reviews === 1 ? "" : "s"} to review`, "review"]);
  if (aggregate.attention) lines.push([`${aggregate.attention} follow-up${aggregate.attention === 1 ? "" : "s"} need attention`, "followups"]);
  if (aggregate.proposals) lines.push([`${aggregate.proposals} rule proposal${aggregate.proposals === 1 ? "" : "s"}`, "proposals"]);
  for (const [label, tab] of lines) {
    const li = document.createElement("li");
    const link = document.createElement("button");
    link.className = "mm-link";
    link.textContent = label;
    link.addEventListener("click", () => openDash(tab));
    li.append(link);
    ul.append(li);
  }
  els.body.append(ul);
}

// The Pause/Resume toggle. Pausing is the global kill-switch (set_pause): MailMate keeps watching
// but applies nothing. Re-renders from a fresh snapshot after the flip.
function pauseToggle(aggregate) {
  const paused = Boolean(aggregate && aggregate.paused);
  const btn = document.createElement("button");
  btn.className = "mm-toggle";
  btn.textContent = paused ? "▶ Resume" : "⏸ Pause";
  btn.title = paused ? "Resume — let MailMate apply actions again" : "Pause — MailMate watches but applies nothing";
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    try {
      await browser.runtime.sendMessage({ type: "mm:setPause", paused: !paused });
    } catch {
      /* leave the toggle disabled and fall through to a repaint, which restores it */
    }
    paint();
  });
  return btn;
}

// Open the dashboard (optionally deep-linked to `tab`) and close the popup — its job is done.
async function openDash(tab) {
  try {
    await browser.runtime.sendMessage({ type: "mm:openDashboard", tab: tab || null });
  } catch {
    /* the dashboard open is best-effort from the popup */
  }
  try {
    window.close();
  } catch {
    /* jsdom / already-closed — harmless */
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
    render(status, null);
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

// Fetch the status (+ aggregate when ready) and render. The single entry point, reused after a
// Pause flip so the popup always reflects the live state.
async function paint() {
  let status;
  try {
    const reply = await browser.runtime.sendMessage({ type: "mm:getStatus" });
    status = (reply && reply.status) || { phase: "disconnected", reason: "background page not responding" };
  } catch {
    render({ phase: "disconnected", reason: "background page not responding" }, null);
    return;
  }
  let aggregate = null;
  if (status.phase === "ready") {
    try {
      aggregate = await browser.runtime.sendMessage({ type: "mm:aggregate" });
    } catch {
      /* leave aggregate null — renderReady degrades to the neutral connected line */
    }
  }
  render(status, aggregate);
}

// Live updates while the popup is open: a status change repaints (and re-fetches the aggregate).
browser.runtime.onMessage.addListener((message) => {
  if (message && message.type === "mm:statusChanged") {
    paint();
  }
});

// Initial paint.
paint();
