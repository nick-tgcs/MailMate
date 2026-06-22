// compose.js — the composeAction review panel.
//
// Anchored in the compose window's toolbar, it annotates the draft already sitting in the
// editable compose area: it supplies the rationale and the typed commitments verdict the compose
// window can't show on its own, and it reinforces the one hard product line — MailMate never
// sends. It owns no native port; it asks the background for this compose tab's draft context, and
// for a re-draft when the user steers it.
//
// The commitments guard is the typed four-category surface (`commitments_guard`): dates, prices,
// payment terms, and legal/binding language, each cited to the exact span the host's model-free
// scanner matched. When an older draft carried no typed report it degrades to the host's
// safety_notes list rather than showing nothing.

"use strict";

const body = () => document.getElementById("mm-body");

// The four commitment classes, in the order the panel presents them — must match the host's
// `CommitmentCategory` wire tokens (date/price/payment/legal).
const GUARD_CATEGORIES = [
  { key: "date", label: "Dates & deadlines", glyph: "📅" },
  { key: "price", label: "Prices & amounts", glyph: "💲" },
  { key: "payment", label: "Payment terms", glyph: "🧾" },
  { key: "legal", label: "Legal & binding", glyph: "⚖" },
];

// One-tap steer chips. Each regenerates the draft with a single "Make it <label>." adjustment.
const STEER_CHIPS = ["Shorter", "Warmer", "More formal", "More direct"];

let currentTabId = null; // the compose tab this panel annotates (set at boot)
let busy = false; // guards against overlapping regenerate round-trips

async function send(message) {
  try {
    const reply = await browser.runtime.sendMessage(message);
    return reply || { ok: false, error: "no response from background" };
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
}

function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.id) node.id = opts.id;
  if (opts.text != null) node.textContent = opts.text;
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

async function boot() {
  let tabId = null;
  try {
    const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
    tabId = tab && tab.id;
  } catch {
    /* fall through — the context call handles a null tabId */
  }
  currentTabId = tabId;
  const reply = await send({ type: "mm:composeContext", tabId });
  render(reply);
}

// Re-draft the current reply with the user's steer (quick-steer chips and/or free text). The
// background re-runs the host draft, replaces the compose body, and returns the SAME context shape
// as composeContext — so a successful regenerate just re-renders with the fresh draft.
async function regenerate(steer) {
  if (busy || currentTabId == null) return;
  busy = true;
  markWorking();
  const reply = await send({
    type: "mm:regenerateDraft",
    tabId: currentTabId,
    adjustments: steer.adjustments || [],
    steer: steer.steer || null,
  });
  busy = false;
  if (reply && reply.ok) {
    render(reply);
  } else {
    // Keep the current draft visible; surface the failure inline rather than blanking the panel.
    const note = document.getElementById("mm-regen-note");
    if (note) {
      note.textContent = `Couldn't regenerate: ${reply && reply.error ? reply.error : "the host didn't answer"}`;
      note.className = "mm-regen-note mm-regen-note--error";
    }
  }
}

// Disable the refine controls and announce the in-flight re-draft (never a silent wait).
function markWorking() {
  const note = document.getElementById("mm-regen-note");
  if (note) {
    note.textContent = "Drafting a new version…";
    note.className = "mm-regen-note mm-regen-note--busy";
  }
  for (const btn of document.querySelectorAll(".mm-refine button")) btn.disabled = true;
}

function render(reply) {
  const root = body();
  root.textContent = "";

  if (!reply.ok) {
    root.appendChild(section("MailMate", [el("p", { class: "mm-muted", text: reply.error || "couldn't load the draft context" })]));
    return;
  }

  const draft = reply.draft;
  const providerConfigured = reply.providerConfigured; // true | false | null(unknown)

  if (!draft) {
    // A hand-written compose (MailMate didn't draft this). Reinforce the invariant, nothing more.
    root.appendChild(
      section("Your message", [
        el("p", { class: "mm-muted", text: "MailMate didn't draft this one — you're writing it yourself. As always, MailMate never sends: Thunderbird's own Send is the only way mail leaves." }),
      ]),
    );
    appendProviderState(root, providerConfigured, /* draftingNeeded */ false);
    return;
  }

  // Why this draft — the trust surface. Degrades honestly when no rationale rode along.
  const why = section("Why this draft", [
    el("p", { text: draft.rationale || "A reply drafted from your past replies on this thread. Read it before sending — it's a starting point you own." }),
  ]);
  if (draft.from_identity) {
    why.appendChild(el("p", { class: "mm-prov", text: `Replying from: ${draft.from_identity}` }));
  }
  const provLine = providerLine(reply.provider);
  if (provLine) why.appendChild(provLine);
  root.appendChild(why);

  // The typed four-category commitments guard (cited spans), or the legacy safety_notes fallback.
  root.appendChild(section("Commitments guard", [guardBlock(draft)]));

  // Refine: regenerate with quick-steer chips / free-text Adjust (only when a provider can draft).
  const refine = refineBlock(providerConfigured);
  if (refine) root.appendChild(refine);

  appendProviderState(root, providerConfigured, /* draftingNeeded */ true);

  root.appendChild(el("p", { class: "mm-foot", text: "Draft is saved. Closing this keeps it in Drafts. MailMate never touches Send." }));
}

// A small "Drafted via ollama · llama3" provenance line, when the host told us which provider drew
// it. Quietly absent when unknown — never a guess.
function providerLine(provider) {
  if (!provider || !provider.kind) return null;
  const model = provider.model ? ` · ${provider.model}` : "";
  return el("p", { class: "mm-prov", text: `Drafted via ${provider.kind}${model}` });
}

// The typed commitments guard. `draft.commitments` is the host's report ({ findings: [...] });
// when absent (an older draft), fall back to listing the host's safety_notes verbatim.
function guardBlock(draft) {
  const report = draft.commitments;
  if (!report || !Array.isArray(report.findings)) {
    return legacyGuardBlock(draft.safety_notes || []);
  }
  const findings = report.findings;
  const flagged = findings.length > 0;
  const guard = el("div", { class: flagged ? "mm-guard mm-guard--flagged" : "mm-guard" });
  guard.appendChild(
    el("div", {
      class: flagged ? "mm-guard__badge mm-guard__badge--flagged" : "mm-guard__badge mm-guard__badge--clear",
      text: flagged ? `⚠ ${findings.length} thing${findings.length > 1 ? "s" : ""} to check before sending` : "✓ Nothing flagged — but it's still your call",
    }),
  );
  if (!flagged) {
    guard.appendChild(el("p", { class: "mm-muted", text: "MailMate watches for dates, prices, payment terms and legal language — none were flagged here." }));
    return guard;
  }
  // One row per category present, each citing the exact spans the scanner matched.
  for (const cat of GUARD_CATEGORIES) {
    const inCat = findings.filter((f) => f.category === cat.key);
    if (!inCat.length) continue;
    const row = el("div", { class: "mm-guard__cat" });
    row.appendChild(el("div", { class: "mm-guard__cat-label", text: `${cat.glyph} ${cat.label}` }));
    const spans = el("div", { class: "mm-guard__spans" });
    for (const f of inCat) {
      spans.appendChild(el("span", { class: `mm-cite mm-cite--${cat.key}`, text: f.text }));
    }
    row.appendChild(spans);
    guard.appendChild(row);
  }
  return guard;
}

// The degraded guard for a draft that carried no typed report: the host's safety_notes verbatim.
function legacyGuardBlock(notes) {
  const guard = el("div", { class: notes.length ? "mm-guard mm-guard--flagged" : "mm-guard" });
  guard.appendChild(
    el("div", {
      class: notes.length ? "mm-guard__badge mm-guard__badge--flagged" : "mm-guard__badge mm-guard__badge--clear",
      text: notes.length ? `⚠ ${notes.length} thing${notes.length > 1 ? "s" : ""} to check before sending` : "✓ Nothing flagged — but it's still your call",
    }),
  );
  if (notes.length) {
    const ul = el("ul");
    for (const n of notes) ul.appendChild(el("li", { text: String(n) }));
    guard.appendChild(ul);
  } else {
    guard.appendChild(el("p", { class: "mm-muted", text: "MailMate watches for dates, prices, payment terms and legal language — none were flagged here." }));
  }
  return guard;
}

// The refine controls: one-tap quick-steer chips, a free-text Adjust box, and a plain Regenerate.
// Shown only when a provider can actually draft — otherwise the degraded provider state speaks for
// itself and we never offer a button that can't act.
function refineBlock(providerConfigured) {
  if (providerConfigured !== true) return null;
  const box = el("div", { class: "mm-refine" });
  box.appendChild(el("div", { class: "mm-refine__label", text: "Refine this draft" }));

  const chips = el("div", { class: "mm-chips" });
  for (const label of STEER_CHIPS) {
    const chip = el("button", { class: "mm-chip", text: label });
    chip.addEventListener("click", () => regenerate({ adjustments: [label] }));
    chips.appendChild(chip);
  }
  box.appendChild(chips);

  const adjust = el("div", { class: "mm-adjust" });
  const input = el("input", { class: "mm-adjust__input" });
  input.type = "text";
  input.placeholder = "Tell MailMate how to change it…";
  const apply = el("button", { class: "mm-secondary", text: "Apply" });
  apply.addEventListener("click", () => {
    const v = input.value.trim();
    if (v) regenerate({ steer: v });
  });
  const regen = el("button", { class: "mm-secondary", text: "Regenerate" });
  regen.addEventListener("click", () => regenerate({}));
  adjust.appendChild(input);
  adjust.appendChild(apply);
  adjust.appendChild(regen);
  box.appendChild(adjust);

  box.appendChild(el("p", { class: "mm-regen-note", id: "mm-regen-note", text: "" }));
  return box;
}

// Show the provider posture. When drafting needs a provider and none is configured, this is a
// loud, useful call-to-action (never a silent failure) per the zero-provider-by-default contract.
function appendProviderState(root, providerConfigured, draftingNeeded) {
  if (providerConfigured === true) return; // nothing to say — drafting is available
  if (providerConfigured === null) return; // unknown (host not ready) — don't guess

  const box = el("div", { class: "mm-degraded" }, [
    el("div", { class: "mm-degraded__title", text: "⚡ Drafting needs an AI provider" }),
    el("p", {
      class: "mm-muted",
      text: draftingNeeded
        ? "No AI provider is configured, so MailMate can't write a full reply. Everything else (filing, follow-ups, learning) keeps working — write the reply yourself, or add a provider in Settings."
        : "No AI provider is configured. MailMate works fully without one; add a provider in Settings only if you want AI-drafted replies.",
    }),
  ]);
  const actions = el("div", { class: "mm-actions" });
  const open = el("button", { class: "mm-primary", text: "Open MailMate settings" });
  open.addEventListener("click", async () => {
    try {
      await browser.runtime.openOptionsPage();
    } catch {
      // Never a silent dead end: if the page can't be focused, tell the user the other route.
      open.textContent = "Open MailMate's preferences from the add-on menu";
      open.disabled = true;
    }
  });
  actions.appendChild(open);
  box.appendChild(actions);
  root.appendChild(box);
}

function section(title, children) {
  return el("div", { class: "mm-section" }, [el("div", { class: "mm-section__title", text: title }), ...children]);
}

boot().catch((e) => {
  body().textContent = "";
  body().appendChild(el("p", { class: "mm-muted", text: String(e && e.message ? e.message : e) }));
});
