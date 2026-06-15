// options.js — the MailMate preferences page.
//
// The host owns the truth: this page reads get_settings, renders the snapshot, and on every save
// sends a write request and RE-READS get_settings to confirm the effective state (it never trusts
// its own optimistic copy). Secrets are write-only from here — an API key is shipped straight to
// the host's 0600 store via set_secret and rendered back only as "•••• set" from a boolean.
//
// Honest M4 boundaries: per-category action policy and per-account scoping (which need new host
// state) are shown as a locked, read-only note pointing at the config file; `test_provider` /
// `provider_status` are not wired (the build ships zero real providers by default).

"use strict";

const RETENTION_LEVELS = [
  ["metadata", "Metadata only", "Headers + computed features. No message body is stored. (Default, safest.)"],
  ["summaries", "Summaries", "Stores short AI summaries of bodies, not the raw text."],
  ["bodies", "Full bodies", "Stores message body text. Most capable, least private."],
];
const PROVIDER_KINDS = ["ollama", "openai_compatible", "lm_studio", "llama_cpp", "mock"];

let settings = null;

const $ = (id) => document.getElementById(id);

async function send(message) {
  try {
    const reply = await browser.runtime.sendMessage(message);
    return reply || { ok: false, error: "no response from background" };
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
}

function toast(text, isError) {
  const el = $("mm-toast");
  el.textContent = text;
  el.classList.toggle("mm-toast--err", Boolean(isError));
  el.hidden = false;
  clearTimeout(toast._t);
  toast._t = setTimeout(() => (el.hidden = true), 2600);
}

function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.type) node.type = opts.type;
  if (opts.value != null) node.value = opts.value;
  if (opts.placeholder) node.placeholder = opts.placeholder;
  if (opts.attrs) for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

// Load (or reload) the snapshot from the host and re-render everything. Called on open and after
// every successful write, so the page always shows the host's confirmed state.
async function load() {
  const reply = await send({ type: "mm:settings" });
  const banner = $("mm-status-banner");
  if (!reply.ok) {
    settings = null;
    banner.hidden = false;
    banner.textContent = `Couldn't reach the MailMate host: ${reply.error || "not connected"}. Settings are read-only until it reconnects.`;
    $("mm-sections").textContent = "";
    return;
  }
  banner.hidden = true;
  settings = reply.settings;
  render();
}

// Send a write, then re-read to confirm. Returns whether the write succeeded. CRUCIALLY it
// re-reads on FAILURE too: a native control (radio / checkbox) has already flipped its own state
// optimistically, so without a re-render it would keep showing the value the host rejected —
// the "never trust the optimistic copy" invariant must hold on the error path, not just success.
async function write(message, okText) {
  const reply = await send(message);
  if (!reply.ok) {
    toast(reply.error || "the host rejected that change", true);
    await load(); // snap the UI back to the host's real state
    return false;
  }
  toast(okText);
  await load();
  return true;
}

function render() {
  const root = $("mm-sections");
  root.textContent = "";
  root.appendChild(statusSection());
  root.appendChild(providerSection());
  root.appendChild(retentionSection());
  root.appendChild(followupSection());
  root.appendChild(lockedSection());
}

function section(title, children) {
  return el("div", { class: "mm-section" }, [el("div", { class: "mm-section__title", text: title }), ...children]);
}

function row(labelText, ...controls) {
  return el("div", { class: "mm-row" }, [el("span", { class: "mm-row__label", text: labelText }), ...controls]);
}

// --- STATUS (pause) -------------------------------------------------------------------

function statusSection() {
  const paused = Boolean(settings.paused);
  const state = el("span", {
    class: paused ? "mm-pill mm-pill--warn" : "mm-pill mm-pill--good",
    text: paused ? "PAUSED" : "ACTIVE",
  });
  const toggle = el("button", { class: paused ? "mm-primary" : "", text: paused ? "Resume MailMate" : "Pause MailMate" });
  toggle.addEventListener("click", async () => {
    toggle.disabled = true;
    await write({ type: "mm:setPause", paused: !paused }, paused ? "Resumed" : "Paused");
  });
  return section("Status", [
    row("MailMate is", state, toggle),
    el("p", { class: "mm-hint", text: "Paused stops auto-actions + follow-up drains and leaves your mail untouched. Classification still runs; everything becomes a suggestion." }),
    el("p", { class: "mm-hint", text: `Database: ${settings.database || "(default)"}` }),
  ]);
}

// --- AI PROVIDER ----------------------------------------------------------------------

function providerSection() {
  const providers = settings.providers || [];
  const children = [];
  if (!providers.length) {
    children.push(el("p", { class: "mm-hint", text: "⚠ No provider configured — reply drafting & summaries are unavailable. MailMate works fully without one; add a provider below only if you want AI drafting." }));
  }
  for (const p of providers) children.push(providerCard(p));
  children.push(addProviderForm());
  return section("AI provider (for reply drafting & summaries)", children);
}

function providerCard(p) {
  const isDefault = settings.default_provider === p.id;
  const card = el("div", { class: "mm-provider" });
  card.appendChild(
    el("div", { class: "mm-provider__head" }, [
      el("span", { class: "mm-provider__name", text: p.id }),
      el("span", { class: "mm-pill", text: p.kind }),
      isDefault ? el("span", { class: "mm-pill mm-pill--good", text: "default" }) : null,
      el("span", { class: p.configured ? "mm-pill mm-pill--good" : "mm-pill mm-pill--warn", text: p.configured ? "•••• key set" : "no key" }),
    ]),
  );
  if (p.endpoint) card.appendChild(el("p", { class: "mm-hint", text: `Endpoint: ${p.endpoint}` }));

  // API key (write-only): the value is shipped straight to the 0600 store, never read back.
  const key = el("input", { type: "password", placeholder: p.configured ? "•••• set — type to replace" : "API key" });
  const saveKey = el("button", { text: "Save key" });
  saveKey.addEventListener("click", async () => {
    if (!key.value) return toast("Enter a key first", true);
    saveKey.disabled = true;
    const ok = await write({ type: "mm:setSecret", providerId: p.id, secret: key.value }, "Key saved");
    if (!ok) saveKey.disabled = false;
  });
  card.appendChild(row("API key", key, saveKey));

  const actions = el("div", { class: "mm-row" });
  if (!isDefault) {
    const mkDefault = el("button", { text: "Make default" });
    mkDefault.addEventListener("click", () => write({ type: "mm:setProvider", providerId: p.id, setDefault: true }, "Set as default"));
    actions.appendChild(mkDefault);
  }
  const remove = el("button", { class: "mm-danger", text: "Remove" });
  remove.addEventListener("click", () => write({ type: "mm:setProvider", providerId: p.id, remove: true }, "Provider removed"));
  actions.appendChild(remove);
  card.appendChild(actions);
  return card;
}

function addProviderForm() {
  const id = el("input", { type: "text", placeholder: "provider id (e.g. local-ollama)" });
  const kind = el("select");
  for (const k of PROVIDER_KINDS) kind.appendChild(el("option", { value: k, text: k }));
  const endpoint = el("input", { type: "text", placeholder: "endpoint URL (optional)" });
  const add = el("button", { class: "mm-primary", text: "Add provider" });
  add.addEventListener("click", async () => {
    if (!id.value.trim()) return toast("Enter a provider id", true);
    add.disabled = true;
    const ok = await write(
      { type: "mm:setProvider", providerId: id.value.trim(), kind: kind.value, endpoint: endpoint.value.trim() || undefined },
      "Provider added",
    );
    if (!ok) add.disabled = false;
  });
  return el("div", { class: "mm-provider" }, [
    el("div", { class: "mm-section__title", text: "Add a provider" }),
    row("Id", id),
    row("Kind", kind),
    row("Endpoint", endpoint),
    el("div", { class: "mm-row" }, [add]),
  ]);
}

// --- RETENTION ------------------------------------------------------------------------

function retentionSection() {
  const current = settings.retention_level || "metadata";
  const children = [];
  for (const [value, label, hint] of RETENTION_LEVELS) {
    const input = el("input", { type: "radio", attrs: { name: "mm-retention" } });
    input.checked = value === current;
    input.addEventListener("change", () => {
      if (input.checked) write({ type: "mm:setSettings", retentionLevel: value }, "Retention updated");
    });
    children.push(
      el("label", { class: "mm-radio" }, [input, el("span", {}, [el("strong", { text: label }), el("div", { class: "mm-hint", text: hint })])]),
    );
  }
  return section("Privacy — what MailMate stores", children);
}

// --- FOLLOW-UPS -----------------------------------------------------------------------

function followupSection() {
  const catchUp = el("input", { type: "checkbox" });
  catchUp.checked = settings.catch_up_on_launch !== false;
  catchUp.addEventListener("change", () => write({ type: "mm:setSettings", catchUpOnLaunch: catchUp.checked }, "Saved"));

  const tick = el("input", { type: "number", value: String(settings.follow_up_tick_seconds || 0), attrs: { min: "0", step: "60" } });
  const saveTick = el("button", { text: "Save" });
  saveTick.addEventListener("click", () =>
    // Floor to an integer: the host parses follow_up_tick_seconds as u64, so a fractional value
    // would be silently dropped while the UI still toasted success.
    write({ type: "mm:setSettings", followUpTickSeconds: Math.max(0, Math.floor(Number(tick.value) || 0)) }, "Cadence updated"),
  );

  return section("Follow-ups", [
    el("label", { class: "mm-check" }, [catchUp, el("span", { text: "Drain overdue follow-ups at launch (catch-up on launch)" })]),
    row("Periodic tick (seconds)", tick, saveTick),
    el("p", { class: "mm-hint", text: "0 disables the periodic in-session drain (startup catch-up only)." }),
  ]);
}

// --- Locked / deferred ----------------------------------------------------------------

function lockedSection() {
  return section("Categories & accounts", [
    el("p", { class: "mm-locked", text: "Per-category action policy (suggest / auto-when-crystallized / off) and per-account triage scope are managed in the host config file for now — wiring them onto the protocol is a tracked addition. Send and delete are never in the action vocabulary." }),
  ]);
}

load().catch((e) => {
  $("mm-status-banner").hidden = false;
  $("mm-status-banner").textContent = String(e && e.message ? e.message : e);
});
