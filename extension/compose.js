// compose.js — the composeAction review panel.
//
// Anchored in the compose window's toolbar, it annotates the draft already sitting in the
// editable compose area: it supplies the rationale and the safety verdict the compose window
// can't show on its own, and it reinforces the one hard product line — MailMate never sends.
// It owns no native port; it asks the background for this compose tab's draft context.
//
// Honest M4 boundaries: the commitments guard is shown in its degraded form (the host's
// safety_notes listed verbatim) until the typed four-category breakdown (`commitments_guard`)
// lands; Regenerate / Adjust need the `regenerate_draft` endpoint, so they are not shown rather
// than offered as buttons that can't act.

"use strict";

const body = () => document.getElementById("mm-body");

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
  const reply = await send({ type: "mm:composeContext", tabId });
  render(reply);
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
  root.appendChild(
    section("Why this draft", [
      el("p", { text: draft.rationale || "A reply drafted from your past replies on this thread. Read it before sending — it's a starting point you own." }),
    ]),
  );

  // Commitments guard (degraded): list the host's safety_notes verbatim. A non-empty list flags.
  const notes = draft.safety_notes || [];
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
  root.appendChild(section("Commitments guard", [guard]));

  appendProviderState(root, providerConfigured, /* draftingNeeded */ true);

  root.appendChild(el("p", { class: "mm-foot", text: "Draft is saved. Closing this keeps it in Drafts. MailMate never touches Send." }));
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
