// panel.js — the per-message header panel (messageDisplayAction popup).
//
// The single "what does MailMate think about THIS message, and how do I fix it" surface.
// Because Thunderbird 140 has no inbox banner/sidebar injection API, every per-message
// affordance lives here. The popup owns no native port: it resolves the displayed message,
// then drives the host entirely through the background's `mm:*` router (mm:classify,
// mm:apply, mm:dismiss, mm:undo, mm:correctLabel, mm:notJunk, mm:move, mm:folders) — keeping
// the background the single-writer of the stdout channel (interaction-design.md §Per-Message
// Header Panel).
//
// Honest Milestone-1 boundaries:
//  • Confidence is a client-side *band* (confidenceBand) — the host emits no calibrated number
//    and the spec forbids showing raw scores. A calibrated band is a later host addition.
//  • The `auto_applied` block is render-complete but unseen on a manual classify: nothing has
//    been applied (no crystallized rule has run), so M1 shows verdict + suggestions + blocked
//    + corrections. The auto bucket lights up once Milestone 2 surfaces a cached
//    classification_ready.

const PHASE_LABEL = {
  ready: "",
  connecting: "…",
  disconnected: "offline",
  version_mismatch: "version",
};

// Default correction vocabulary. The authoritative per-account category set is a host
// read-addition (get_settings category policy, Phase 12); until then these common labels plus a
// free-text "Something else" cover the wrong-category correction without fabricating a taxonomy.
const DEFAULT_CATEGORIES = [
  "Important",
  "Personal",
  "Work",
  "Receipts",
  "Finance",
  "Newsletters",
  "Promotions",
  "Social",
  "Travel",
];

// The category vocabulary the host advertises on get_settings (Phase 1b). Fetched once at boot so
// the wrong-category menu offers the host's human labels rather than a hardcoded list; falls back
// to DEFAULT_CATEGORIES when the host is older or unreachable.
let hostCategories = null;

// Kinds the panel can apply locally — applyPlannedAction (drafts.js) handles exactly these. A
// suggest action whose kind is NOT here (require_review, create_draft) is a marker, not a button:
// it gets an informational row, never an Apply that could only fail with an internal error.
const APPLYABLE_KINDS = new Set(["tag", "move", "mark_junk", "mark_read", "flag"]);

const els = {
  dot: document.getElementById("mm-dot"),
  phase: document.getElementById("mm-phase"),
  body: document.getElementById("mm-body"),
  toast: document.getElementById("mm-toast"),
};

// The message this popup is anchored to, resolved once at open.
let displayed = null;
// The last connection phase we acted on, so the live status listener reacts to TRANSITIONS only.
let lastPhase = null;
// A monotonic token per classify() call: the async-state machine's stale-result race guard. A
// reply whose token is no longer current (the user moved to another message, or re-triggered) is
// dropped, so a slow response can never overwrite the panel for a message you've moved past.
let classifyToken = 0;
// Bound the classify wait so a wedged host shows a recoverable "taking longer" state with a Retry,
// not an indefinite spinner.
const CLASSIFY_TIMEOUT_MS = 8000;

// i18n seam: resolve a message key through browser.i18n when present, else the English fallback.
// English-only v1, but the seam exists — a future locale ships `_locales/<lang>/messages.json`
// with no code change. Degrades safely in jsdom/tests where browser.i18n is absent.
function t(key, fallback) {
  try {
    if (typeof browser !== "undefined" && browser.i18n && browser.i18n.getMessage) {
      const m = browser.i18n.getMessage(key);
      if (m) {
        return m;
      }
    }
  } catch (e) {
    // fall through to the fallback
  }
  return fallback;
}

// Resolve/reject `promise`, but reject with a tagged MMTimeout if `ms` elapses first.
function withTimeout(promise, ms) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      const err = new Error("classification timed out");
      err.name = "MMTimeout";
      reject(err);
    }, ms);
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

// The status region is a live region so a screen reader announces each state transition
// (loading → verdict / timed-out / host-down). Reduced-motion is honoured in panel.css.
if (els.body) {
  els.body.setAttribute("aria-live", "polite");
  els.body.setAttribute("aria-atomic", "true");
}

// --- Boot ----------------------------------------------------------------------------

async function boot() {
  const reply = await send({ type: "mm:getStatus" });
  const status = reply.status || {
    phase: "disconnected",
    reason: reply.error || "background not responding",
  };
  lastPhase = status.phase;
  setPhaseDot(status);
  if (status.phase !== "ready") {
    renderConnection(status);
    return;
  }
  // Fetch the host's category vocabulary once (best-effort) so corrections use real labels.
  loadCategories();
  displayed = await resolveDisplayedMessage();
  if (!displayed) {
    renderNoMessage();
    return;
  }
  await classify();
}

// Best-effort fetch of the host's category vocabulary. Never blocks boot or throws: a failure
// leaves `hostCategories` null and the menu falls back to DEFAULT_CATEGORIES.
async function loadCategories() {
  try {
    const reply = await send({ type: "mm:settings" });
    const cats = reply && reply.ok && reply.settings && reply.settings.categories;
    if (Array.isArray(cats) && cats.length) {
      hostCategories = cats.map((c) => c.label || c.key).filter(Boolean);
    }
  } catch (e) {
    // leave the fallback in place
  }
}

// Resolve the message shown in the tab this popup is anchored to.
async function resolveDisplayedMessage() {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  if (!tab) {
    return null;
  }
  try {
    const msg = await browser.messageDisplay.getDisplayedMessage(tab.id);
    if (msg) {
      return msg;
    }
  } catch (e) {
    // Not a message-display tab; fall through to the selection.
  }
  try {
    const selected = await browser.mailTabs.getSelectedMessages(tab.id);
    return selected.messages.length ? selected.messages[0] : null;
  } catch (e) {
    return null;
  }
}

async function classify() {
  const token = ++classifyToken;
  const messageId = displayed.id;
  renderLoading(t("panelReading", "Reading this message…"));

  let reply;
  try {
    reply = await withTimeout(send({ type: "mm:classify", messageId }), CLASSIFY_TIMEOUT_MS);
  } catch (e) {
    if (isStale(token, messageId)) {
      return; // a newer classify (or a message switch) superseded this one
    }
    if (e && e.name === "MMTimeout") {
      renderTimedOut();
    } else {
      renderClassifyError(e && e.message ? e.message : String(e));
    }
    return;
  }
  // Stale-result race guard: drop a reply for a message the panel has moved past.
  if (isStale(token, messageId)) {
    return;
  }
  if (!reply.ok) {
    if (reply.reason === "host_not_ready") {
      lastPhase = reply.status.phase;
      setPhaseDot(reply.status);
      renderConnection(reply.status);
    } else {
      renderClassifyError(reply.error || "classification failed");
    }
    return;
  }
  renderVerdict(reply);
}

// True when a classify reply is no longer the one the panel is waiting for.
function isStale(token, messageId) {
  return token !== classifyToken || !displayed || displayed.id !== messageId;
}

function renderTimedOut() {
  clear();
  els.body.append(para(t("panelTimedOut", "MailMate is taking longer than usual."), "mm-muted"));
  els.body.append(button(t("actionRetry", "Retry"), "mm-primary", () => classify()));
}

// --- Connection / edge states --------------------------------------------------------

function renderConnection(status) {
  clear();
  if (status.phase === "version_mismatch") {
    els.body.append(
      para("MailMate's helper is on a different protocol version. Update whichever is older."),
    );
  } else if (status.phase === "connecting") {
    els.body.append(para("Connecting to MailMate…", "mm-muted"));
  } else {
    els.body.append(para(t("panelHostUnreachable", "MailMate host unreachable. Your mail is unaffected.")));
    if (status.reason) {
      els.body.append(para(`Reason: ${status.reason}`, "mm-reason"));
    }
  }
  const retry = button(`↻ ${t("actionRetry", "Retry")}`, "mm-primary", async () => {
    retry.disabled = true;
    const reply = await send({ type: "mm:reconnect" });
    const next = reply.status || {
      phase: "disconnected",
      reason: reply.error || "background not responding",
    };
    lastPhase = next.phase;
    setPhaseDot(next);
    if (next.phase === "ready") {
      await boot();
    } else {
      renderConnection(next); // rebuilds an enabled Retry — never stuck (send never throws)
    }
  });
  els.body.append(retry);
}

function renderNoMessage() {
  clear();
  els.body.append(para(t("panelNoMessage", "Open a message to see what MailMate thinks of it."), "mm-muted"));
}

function renderClassifyError(reason) {
  clear();
  els.body.append(para(t("panelClassifyFailed", "Couldn't classify this message yet.")));
  if (reason) {
    els.body.append(para(reason, "mm-reason"));
  }
  els.body.append(button(t("actionClassifyNow", "Classify now"), "mm-primary", () => classify()));
}

function renderLoading(text) {
  clear();
  els.body.append(para(text, "mm-muted"));
}

// --- The verdict ---------------------------------------------------------------------

function renderVerdict(data) {
  clear();
  const c = data.result.classification || {};
  const actions = data.result.suggested_actions || [];
  const blocked = data.result.blocked_actions || [];
  const explanation = data.result.explanation || {};
  const decisionId = data.result.decision_id;
  const category = (c.labels && c.labels[0]) || "Unclassified";
  const risky = isRisky(c);

  // Verdict line: glyph + category.
  const verdict = div("mm-verdict" + (risky ? " mm-verdict--risk" : ""));
  verdict.append(span(risky ? "⚠" : "📁", "mm-glyph"), span(risky ? `Likely ${category}` : category));
  els.body.append(verdict);

  // Banded confidence bar (not a raw score).
  const band = confidenceBand(c);
  const bar = div("mm-bar");
  for (let i = 0; i < 10; i += 1) {
    bar.append(div("mm-cell" + (i < band.fill ? " mm-cell--on" : "")));
  }
  els.body.append(bar, para(band.text, "mm-band"));

  // Action-first: the primary things you'd want to do, on every message, before any explanation.
  renderPrimaryActions(data, decisionId);

  // The inform-only Safety block (phishing/malware heads-up) — surfaced high, takes no action.
  renderSafety(c);

  // The "why": the correctable salient-signal chips (the explainability spine), then the
  // one-line summary + the policy-checks expander.
  renderSignals(c, decisionId);
  if (explanation.summary) {
    els.body.append(para(explanation.summary, "mm-summary"));
  }
  appendWhy(explanation);

  const hr = document.createElement("hr");
  hr.className = "mm-rule";
  els.body.append(hr);

  // Host suggestions, partitioned by apply_state — the crystallized/learned offers. Within the
  // suggest bucket we split locally-applyable safe mutations (Apply / Dismiss) from non-applyable
  // markers like require_review / create_draft, which get an informational row and NEVER a
  // failing Apply.
  const auto = actions.filter((a) => a.apply_state === "auto_applied");
  const suggest = actions.filter((a) => a.apply_state === "suggest");
  const applyable = suggest.filter((a) => APPLYABLE_KINDS.has(a.kind));
  const review = suggest.filter((a) => !APPLYABLE_KINDS.has(a.kind));
  auto.forEach((a) => els.body.append(autoRow(a, decisionId)));
  applyable.forEach((a) => els.body.append(suggestRow(a, decisionId)));
  review.forEach((a) => els.body.append(reviewRow(a)));
  blocked.forEach((b) => els.body.append(blockedRow(b)));

  // Cold-start / low-confidence: when the host is still unsure and there is nothing to suggest,
  // show a designed "still learning" state instead of a bare "0 allowed, 1 need review".
  const nothingToDo = !auto.length && !applyable.length && !review.length && !blocked.length;
  if (nothingToDo && c.needs_review) {
    els.body.append(coldStartCard(c));
  } else if (nothingToDo) {
    els.body.append(para("Nothing to do — this looks handled.", "mm-muted"));
  }

  // Body-retention notice (headers-only is a valid, honest classification).
  if (data.bodyRetentionAllowed === false) {
    els.body.append(para("Using headers only — body reading is off.", "mm-muted"));
  }

  // Corrections — always available, always one click.
  appendCorrections(c, decisionId, risky);

  // Explain deep-link — opens the dashboard timeline for this message.
  const hr2 = document.createElement("hr");
  hr2.className = "mm-rule";
  els.body.append(hr2);
  const explain = button("ⓘ Explain in dashboard ▸", "mm-link", () => openExplainInDashboard());
  const footer = div("mm-footer");
  footer.append(explain);
  els.body.append(footer);
}

// The action-first bar: the primary things to do with a message, present on every verdict.
// File to… reuses the move picker; Junk & block / Mark read / Draft reply hit their background
// handlers; Unsubscribe appears only when the host parsed a List-Unsubscribe affordance.
function renderPrimaryActions(data, decisionId) {
  const bar = div("mm-primary-actions");
  const fileTo = button("📁 File to…", "mm-pa", () => toggleMenu(fileTo, () => moveMenu()));
  const junk = button("🚫 Junk & block", "mm-pa", () =>
    runPrimary(junk, { type: "mm:junk", messageId: displayed.id }, "Marked as junk — learning from this."),
  );
  const read = button("✓ Mark read", "mm-pa", () =>
    runPrimary(read, { type: "mm:markRead", messageId: displayed.id }, "Marked read."),
  );
  const draft = button("✍ Draft reply", "mm-pa", () =>
    runPrimary(draft, { type: "mm:draftReply", messageId: displayed.id }, "Opened a reply draft."),
  );
  bar.append(fileTo, junk, read, draft);

  const u = data.result.unsubscribe;
  if (u) {
    const unsub = button("✉ Unsubscribe", "mm-pa", () =>
      runPrimary(
        unsub,
        { type: "mm:unsubscribe", unsubscribe: u, messageId: displayed.id },
        u.mailto ? "Opened an unsubscribe email — review and send." : "Unsubscribe sent.",
      ),
    );
    bar.append(unsub);
  }
  els.body.append(bar);
}

// Fire a one-shot primary action: disable the button, send, toast the outcome, re-enable on
// failure. The action either mutates mail or opens a compose; on success we re-classify so the
// verdict/badge reflect the new state.
async function runPrimary(btn, message, okText) {
  btn.disabled = true;
  const reply = await send(message);
  if (reply.ok) {
    toast(okText);
    // A compose/unsubscribe opens a window; re-classifying would race the popup closing, so only
    // mail-state mutations (junk/read) refresh the verdict.
    if (message.type === "mm:junk" || message.type === "mm:markRead") {
      await classify();
    } else {
      btn.disabled = false;
    }
  } else {
    btn.disabled = false;
    toast(reply.error || "Couldn't do that.");
  }
}

// The correctable salient-signal chips: the real reasons behind the verdict. A correctable chip
// (a deterministic model feature) carries an "✕" that records a `signal_marked_wrong` correction;
// a rule/AI signal is shown but not correctable here (you steer those elsewhere).
function renderSignals(c, decisionId) {
  const signals = c.salient_signals || [];
  if (!signals.length) {
    return;
  }
  els.body.append(para("Why MailMate thinks this:", "mm-signals-label"));
  const wrap = div("mm-signals");
  const prior = (c.labels && c.labels[0]) || null;
  signals.forEach((s) => {
    const chip = div("mm-signal" + (s.correctable ? " mm-signal--correctable" : ""));
    chip.append(span(s.label, "mm-signal__label"));
    if (s.correctable) {
      const wrong = button("✕", "mm-signal__wrong", async () => {
        wrong.disabled = true;
        const reply = await send({
          type: "mm:signalWrong",
          decisionId,
          messageId: displayed.id,
          signalId: s.id,
          priorLabel: prior,
        });
        if (reply.ok) {
          toast("Thanks — I'll weigh that less.");
          chip.classList.add("mm-signal--rejected");
          wrong.remove();
        } else {
          wrong.disabled = false;
          toast(reply.error || "Couldn't record that.");
        }
      });
      wrong.title = "This reason is wrong";
      chip.append(wrong);
    }
    wrap.append(chip);
  });
  els.body.append(wrap);
}

// The inform-only Safety block: phishing/malware findings the host surfaced. Renders a severity
// glyph + title (+ detail); it never offers an action — it is a heads-up, not policy.
function renderSafety(c) {
  const findings = c.safety_findings || [];
  if (!findings.length) {
    return;
  }
  const box = div("mm-safety");
  box.append(para("⚠ Safety", "mm-safety__title"));
  findings.forEach((f) => {
    const item = div(`mm-safety__item mm-safety--${f.severity || "info"}`);
    item.append(span(`${severityGlyph(f.severity)} ${f.title}`, "mm-safety__h"));
    if (f.detail) {
      item.append(para(f.detail, "mm-safety__d"));
    }
    box.append(item);
  });
  els.body.append(box);
}

function severityGlyph(severity) {
  switch (severity) {
    case "danger":
      return "⛔";
    case "warning":
      return "⚠";
    default:
      return "ⓘ";
  }
}

// The cold-start / still-learning card: shown when the host is unsure and there is nothing to
// suggest, instead of a bare "0 allowed, 1 need review". Honest and encouraging.
function coldStartCard(c) {
  const card = div("mm-coldstart");
  card.append(para("Still learning your mail", "mm-coldstart__title"));
  const hasSignals = (c.salient_signals || []).length > 0;
  card.append(
    para(
      hasSignals
        ? "Here's what I can already see (above). Correct anything that's off and I'll get sharper fast."
        : "I don't have a confident read yet. File or correct a few like this and I'll learn your preferences quickly.",
      "mm-muted",
    ),
  );
  return card;
}

// Open the dashboard timeline for the displayed message (the Explain deep-link). The dashboard
// reads the `explain` hash to focus the message; absent that handler it still opens the space.
function openExplainInDashboard() {
  const id = displayed && displayed.id != null ? String(displayed.id) : "";
  send({ type: "mm:openDashboard", explain: id });
}

function appendWhy(explanation) {
  const checks = explanation.policy_checks || [];
  const labels = explanation.labels || [];
  if (!checks.length && !labels.length) {
    return;
  }
  const toggle = button("why ▸", "mm-why-toggle", null);
  const why = div("mm-why");
  why.hidden = true;
  if (labels.length) {
    why.append(para(`Labels: ${labels.join(", ")}`, "mm-meta"));
  }
  if (checks.length) {
    const heading = para("Policy checks:", "mm-meta");
    const ul = document.createElement("ul");
    checks.forEach((id) => {
      const li = document.createElement("li");
      li.textContent = id;
      ul.append(li);
    });
    why.append(heading, ul);
  }
  toggle.addEventListener("click", () => {
    why.hidden = !why.hidden;
    toggle.textContent = why.hidden ? "why ▸" : "why ▾";
  });
  els.body.append(toggle, why);
}

// An already-applied crystallized action: past tense, with Undo.
function autoRow(action, decisionId) {
  const row = div("mm-action mm-action--auto");
  const text = div("mm-action__text");
  text.append(span(actionText(action, true)));
  const sub = document.createElement("span");
  sub.className = "mm-action__sub";
  sub.textContent = action.rule_id ? `auto · learned rule ${action.rule_id}` : "auto · applied automatically";
  text.append(sub);
  const undo = button("⟲ Undo", null, async () => {
    undo.disabled = true;
    const reply = await send({
      type: "mm:undo",
      action,
      decisionId,
      messageId: displayed.id,
    });
    if (reply.ok) {
      toast("Undone — I'll remember that.");
      await classify();
    } else {
      undo.disabled = false;
      toast(reply.error || "Couldn't undo automatically.");
    }
  });
  const buttons = div("mm-action__buttons");
  buttons.append(undo);
  text.append(buttons);
  row.append(span("✓", "mm-action__mark"), text);
  return row;
}

// A pending suggestion: future offer, Apply / Dismiss.
function suggestRow(action, decisionId) {
  const row = div("mm-action mm-action--suggest");
  const text = div("mm-action__text");
  text.append(span(`Suggested: ${actionText(action, false)}`));
  const apply = button("Apply", "mm-primary", async () => {
    setBusy([apply, dismiss], true);
    const reply = await send({ type: "mm:apply", action, decisionId, messageId: displayed.id });
    if (reply.ok) {
      toast("Applied — that helps me learn.");
      await classify();
    } else {
      setBusy([apply, dismiss], false);
      toast(reply.error || "Couldn't apply that action.");
    }
  });
  const dismiss = button("Dismiss", null, async () => {
    setBusy([apply, dismiss], true);
    const reply = await send({
      type: "mm:dismiss",
      decisionId,
      actionKind: action.kind,
      messageId: displayed.id,
    });
    if (reply.ok) {
      toast("Dismissed — I'll remember that.");
      row.remove();
    } else {
      setBusy([apply, dismiss], false);
      toast(reply.error || "Couldn't record that.");
    }
  });
  const buttons = div("mm-action__buttons");
  buttons.append(apply, dismiss);
  text.append(buttons);
  row.append(span("◻", "mm-action__mark"), text);
  return row;
}

// A non-applyable suggest marker (require_review / create_draft): informational, no Apply — the
// panel can't perform it locally, so offering an Apply that could only fail would be a lie.
function reviewRow(action) {
  const row = div("mm-action mm-action--review");
  const text = div("mm-action__text");
  text.append(span(actionText(action, false)));
  if (action.kind === "require_review" && action.target) {
    const sub = document.createElement("span");
    sub.className = "mm-action__sub";
    sub.textContent = action.target;
    text.append(sub);
  }
  row.append(span("⚑", "mm-action__mark"), text);
  return row;
}

// A blocked candidate: greyed, inert, policy made visible.
function blockedRow(blocked) {
  const row = div("mm-action mm-action--blocked");
  const text = div("mm-action__text");
  const kind = blocked.action && blocked.action.kind ? blocked.action.kind : "action";
  text.append(span(`${cap(kind)} — blocked by policy`));
  const sub = document.createElement("span");
  sub.className = "mm-action__sub";
  sub.textContent = [blocked.policy_id, blocked.reason].filter(Boolean).join(" · ");
  text.append(sub);
  row.append(span("⛔", "mm-action__mark"), text);
  return row;
}

// --- Corrections (one click → one record_user_action) --------------------------------

function appendCorrections(classification, decisionId, risky) {
  els.body.append(para(risky ? "Wrong?" : "Not right?", "mm-correct-label"));
  const cluster = div("mm-correct");
  const current = (classification.labels && classification.labels[0]) || null;

  const wrongCat = button("Wrong category ▾", null, () =>
    toggleMenu(wrongCat, () => categoryMenu(decisionId, current)),
  );
  // For a spam/phishing verdict the ham correction reads "This is legitimate"; otherwise
  // "Not junk". Both reuse the existing junk_changed path (junk:false).
  const notJunk = button(risky ? "This is legitimate" : "Not junk", null, async () => {
    notJunk.disabled = true;
    const reply = await send({ type: "mm:notJunk", messageId: displayed.id });
    if (reply.ok) {
      toast("Got it — learning from this.");
      await classify();
    } else {
      notJunk.disabled = false;
      toast(reply.error || "Couldn't record that.");
    }
  });
  const move = button("Move… ▾", null, () => toggleMenu(move, () => moveMenu()));

  cluster.append(wrongCat, notJunk, move);
  els.body.append(cluster);
}

// Render (and remember) at most one open correction menu under the cluster.
let openMenu = null;
function toggleMenu(anchor, build) {
  if (openMenu) {
    openMenu.remove();
    const wasMine = openMenu.dataset.owner === anchor.textContent;
    openMenu = null;
    if (wasMine) {
      return;
    }
  }
  const menu = build();
  menu.dataset.owner = anchor.textContent;
  openMenu = menu;
  els.body.append(menu);
}

function categoryMenu(decisionId, current) {
  const menu = div("mm-menu");
  const chips = div("mm-chips");
  const choose = async (label) => {
    const reply = await send({
      type: "mm:correctLabel",
      decisionId,
      messageId: displayed.id,
      label,
      priorLabel: current,
    });
    if (reply.ok) {
      toast("Got it — learning from this.");
      closeMenu();
      await classify();
    } else {
      toast(reply.error || "Couldn't record that.");
    }
  };
  (hostCategories || DEFAULT_CATEGORIES).forEach((label) => {
    const isNow = current && label.toLowerCase() === current.toLowerCase();
    const chip = button(isNow ? `${label} ✓` : label, "mm-chip", () => choose(label));
    if (isNow) {
      chip.disabled = true;
    }
    chips.append(chip);
  });
  menu.append(chips);
  // "Something else" — a free-text label, since the vocabulary is open.
  const row = div("mm-row");
  const input = document.createElement("input");
  input.type = "text";
  input.placeholder = "Something else…";
  const ok = button("Set", "mm-primary", () => {
    const label = input.value.trim();
    if (label) {
      choose(label);
    }
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && input.value.trim()) {
      choose(input.value.trim());
    }
  });
  row.append(input, ok);
  menu.append(row);
  return menu;
}

function moveMenu() {
  const menu = div("mm-menu");
  const row = div("mm-row");
  const select = document.createElement("select");
  const placeholder = document.createElement("option");
  placeholder.textContent = "Loading folders…";
  placeholder.value = "";
  select.append(placeholder);
  const go = button("Move", "mm-primary", async () => {
    const value = select.value;
    if (!value) {
      return;
    }
    const { accountId, path } = JSON.parse(value);
    go.disabled = true;
    const reply = await send({ type: "mm:move", messageId: displayed.id, folder: { accountId, path } });
    if (reply.ok) {
      toast("Moved — I'll learn this filing.");
      closeMenu();
      await classify();
    } else {
      go.disabled = false;
      toast(reply.error || "Couldn't move the message.");
    }
  });
  go.disabled = true;
  row.append(select, go);
  menu.append(row);
  // Populate asynchronously; the menu degrades to a notice if folders can't be read.
  send({ type: "mm:folders" }).then((reply) => {
    select.replaceChildren();
    const folders = (reply && reply.folders) || [];
    if (!folders.length) {
      const opt = document.createElement("option");
      opt.textContent = "No folders available";
      opt.value = "";
      select.append(opt);
      return;
    }
    const pick = document.createElement("option");
    pick.textContent = "Pick a folder…";
    pick.value = "";
    select.append(pick);
    folders.forEach((f) => {
      const opt = document.createElement("option");
      opt.textContent = f.accountName ? `${f.accountName} · ${f.path}` : f.path;
      opt.value = JSON.stringify({ accountId: f.accountId, path: f.path });
      select.append(opt);
    });
    go.disabled = false;
  });
  return menu;
}

function closeMenu() {
  if (openMenu) {
    openMenu.remove();
    openMenu = null;
  }
}

// --- Derivations ---------------------------------------------------------------------

function isRisky(c) {
  const labels = (c.labels || []).map((l) => String(l).toLowerCase());
  return (
    labels.some((l) => /spam|phish|junk|suspicious|malware/.test(l)) ||
    Math.max(c.spam_score || 0, c.phishing_score || 0) >= 0.6
  );
}

// A *banded* confidence — never the raw score. The host now emits a calibrated `confidence_band`
// (high / medium / low); the panel renders that. `needs_review` always overrides to the honest
// "Needs review" label, whatever the band. The pre-host-band client derivation is kept as a
// fallback for an older host that emits no band (degrade, never lie).
function confidenceBand(c) {
  if (c.needs_review) {
    return { text: "Needs review", fill: 4 };
  }
  const fillByBand = { high: 8, medium: 5, low: 3 };
  if (c.confidence_band && fillByBand[c.confidence_band] !== undefined) {
    return { text: `${cap(c.confidence_band)} confidence`, fill: fillByBand[c.confidence_band] };
  }
  const score = Math.max(c.spam_score || 0, c.phishing_score || 0);
  if (isRisky(c)) {
    if (score >= 0.85) return { text: "Very high confidence", fill: 9 };
    if (score >= 0.65) return { text: "High confidence", fill: 7 };
    if (score >= 0.45) return { text: "Medium confidence", fill: 5 };
    return { text: "Low confidence", fill: 3 };
  }
  return { text: "High confidence", fill: 8 };
}

function actionText(a, past) {
  switch (a.kind) {
    case "move":
      return `${past ? "Filed to" : "File to"} ${folderLeaf(a.to_folder)}`;
    case "tag":
      return `${past ? "Tagged" : "Add tag"} “${a.tag}”`;
    case "mark_read":
      return past ? "Marked read" : "Mark read";
    case "mark_junk":
      return past ? "Marked as junk" : "Mark as junk";
    case "flag":
      return past ? "Flagged" : "Flag";
    case "require_review":
      return "Flagged for your review";
    case "create_draft":
      return past ? "Drafted a reply" : "Draft a reply";
    default:
      return `${past ? "Did" : "Do"} ${a.kind}`;
  }
}

function folderLeaf(path) {
  if (!path) {
    return "a folder";
  }
  const parts = String(path).split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : String(path);
}

// --- Small DOM + messaging helpers ---------------------------------------------------

// Drive the host through the background. This NEVER rejects: a dropped port, a host error, or an
// asleep MV3 event page (no listener yet) is normalized to { ok:false, error } so no caller's
// await throws and no button is left stuck-disabled. The background mirrors this on its side.
async function send(message) {
  try {
    const reply = await browser.runtime.sendMessage(message);
    return reply == null ? { ok: false, error: "no response from the background page" } : reply;
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
}

function setPhaseDot(status) {
  const phase = status.phase || "connecting";
  els.dot.className = `mm-dot mm-dot--${phase}`;
  els.phase.textContent = PHASE_LABEL[phase] !== undefined ? PHASE_LABEL[phase] : "";
}

function clear() {
  els.body.replaceChildren();
  openMenu = null;
}

function div(className) {
  const el = document.createElement("div");
  if (className) {
    el.className = className;
  }
  return el;
}

function span(text, className) {
  const el = document.createElement("span");
  el.textContent = text;
  if (className) {
    el.className = className;
  }
  return el;
}

function para(text, className) {
  const p = document.createElement("p");
  p.textContent = text;
  if (className) {
    p.className = className;
  }
  return p;
}

function button(label, className, onClick) {
  const b = document.createElement("button");
  b.textContent = label;
  if (className) {
    b.className = className;
  }
  if (onClick) {
    b.addEventListener("click", onClick);
  }
  return b;
}

function setBusy(buttons, busy) {
  buttons.forEach((b) => {
    b.disabled = busy;
  });
}

function cap(text) {
  return text ? text.charAt(0).toUpperCase() + text.slice(1) : text;
}

let toastTimer = null;
function toast(text) {
  els.toast.textContent = text;
  els.toast.hidden = false;
  if (toastTimer) {
    clearTimeout(toastTimer);
  }
  toastTimer = setTimeout(() => {
    els.toast.hidden = true;
  }, 2200);
}

// If the host drops or recovers while the popup is open, reflect it honestly. React to phase
// TRANSITIONS only — the heartbeat re-broadcasts "ready" every 30s, which must not wipe a verdict
// or an open correction menu. A drop replaces the now-stale, no-longer-actionable verdict with the
// recovery card ("degrade, never lie"); a recovery re-resolves the message and re-classifies.
browser.runtime.onMessage.addListener((message) => {
  if (!message || message.type !== "mm:statusChanged") {
    return;
  }
  setPhaseDot(message.status);
  const phase = message.status.phase;
  if (phase === lastPhase) {
    return;
  }
  const previous = lastPhase;
  lastPhase = phase;
  if (phase === "ready") {
    if (previous && previous !== "ready") {
      boot(); // recovered — re-resolve the message and re-classify
    }
  } else {
    renderConnection(message.status); // host gone — never leave a stale, actionable verdict
  }
});

boot().catch((e) => {
  renderClassifyError(e && e.message ? e.message : String(e));
});
