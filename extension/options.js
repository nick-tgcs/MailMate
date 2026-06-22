// options.js — the MailMate preferences page.
//
// The host owns the truth: this page reads get_settings, renders the snapshot, and on every save
// sends a write request and RE-READS get_settings to confirm the effective state (it never trusts
// its own optimistic copy). Secrets are write-only from here — an API key is shipped straight to
// the host's 0600 store via set_secret and rendered back only as "•••• set" from a boolean.
//
// Triage tuning (per-category action policy, per-account scope, tag→category mapping) is edited
// here and written to the host (set_category_policy / set_account_scope / set_tag_mapping), then
// re-read like every other setting. "Test connection" probes a provider for a real liveness
// result via test_provider. Notification preferences are the one exception — extension-local UX,
// kept in storage.local — because they never touch the host.

"use strict";

const RETENTION_LEVELS = [
  ["metadata", "Metadata only", "Headers + computed features. No message body is stored. (Default, safest.)"],
  ["summaries", "Summaries", "Stores short AI summaries of bodies, not the raw text."],
  ["bodies", "Full bodies", "Stores message body text. Most capable, least private."],
];
// The real, network-backed provider kinds offered in the UI. The host also understands a `mock`
// kind (it degrades to "unavailable"), but it is a test/degraded sentinel with no network home, so
// it is never offered to a user adding a provider.
const PROVIDER_KINDS = ["ollama", "openai_compatible", "lm_studio", "llama_cpp"];

// The endpoint each kind serves on by default, so picking a kind pre-fills a working URL (the
// user's #2 ask). The OpenAI-shaped kinds carry `/v1` because their adapters append `/models` and
// `/chat/completions` to the base.
const KIND_DEFAULTS = {
  ollama: "http://localhost:11434",
  lm_studio: "http://localhost:1234/v1",
  llama_cpp: "http://localhost:8080",
  openai_compatible: "https://api.openai.com/v1",
};
const KIND_DEFAULT_VALUES = new Set(Object.values(KIND_DEFAULTS).filter(Boolean));
const defaultEndpoint = (kind) => KIND_DEFAULTS[kind] || "";

// Discovered model catalogs, keyed by probe identity, kept at module scope so a successful listing
// survives the full re-render every host write triggers and the same endpoint is never probed twice.
const modelCache = new Map();
const probeKey = (p) => `${p.kind}|${p.endpoint}|${p.providerId || ""}`;

// Auto-discovery is limited to loopback endpoints: hitting localhost is not egress, so the dropdown
// can populate the moment a local URL is in place; a cloud endpoint is only ever probed when the
// user explicitly clicks "List models" (no unprompted egress, and its catalog needs the key anyway).
const isLocalEndpoint = (url) =>
  /^https?:\/\/(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d+)?(?:\/|$)/i.test((url || "").trim());

// The in-progress "Add a provider" form, held OUTSIDE the DOM so it survives the full re-render
// that every host write triggers (load() → render()). Without this, flipping any other control
// — e.g. a privacy radio — wiped whatever the user had half-typed here (the user's #1 complaint).
const addDraft = { id: "", kind: "ollama", endpoint: "", model: "" };
function resetAddDraft() {
  Object.assign(addDraft, { id: "", kind: "ollama", endpoint: "", model: "" });
}

let settings = null;

// The user's mail accounts (id + name), read from the MailExtension accounts API for the
// per-account triage checklist. Held at module scope so the host-driven re-render reuses them
// without re-querying. Empty/absent API ⇒ no checklist (the section degrades to a note).
let accounts = [];
async function loadAccounts() {
  try {
    if (!browser.accounts || typeof browser.accounts.list !== "function") return [];
    const list = await browser.accounts.list();
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

// Desktop-notification preferences live in storage.local (extension-local UX, NOT host config) —
// the SAME key + shape notifications.js reads. Held at module scope across the host-driven re-render.
let notifPrefs = null;
const NOTIF_PREFS_KEY = "mm:notifPrefs";
const NOTIF_CLASS_LABELS = [
  ["proposal_ready", "New rule proposals to approve"],
  ["followup_draft_ready", "Follow-up drafts ready to review"],
  ["followup_needs_attention", "Tracked deals that go stale"],
];
function defaultNotifPrefs() {
  return {
    classes: { proposal_ready: true, followup_draft_ready: true, followup_needs_attention: true },
    quietHours: { enabled: false, start: "22:00", end: "07:00" },
  };
}
async function loadNotifPrefs() {
  try {
    const got = await browser.storage.local.get(NOTIF_PREFS_KEY);
    const p = got[NOTIF_PREFS_KEY];
    const d = defaultNotifPrefs();
    if (!p) return d;
    return { classes: { ...d.classes, ...(p.classes || {}) }, quietHours: { ...d.quietHours, ...(p.quietHours || {}) } };
  } catch {
    return defaultNotifPrefs();
  }
}
async function saveNotifPrefs() {
  try {
    await browser.storage.local.set({ [NOTIF_PREFS_KEY]: notifPrefs });
  } catch {
    /* best-effort — a write failure leaves the last-saved prefs in effect */
  }
}

const $ = (id) => document.getElementById(id);

async function send(message) {
  try {
    const reply = await browser.runtime.sendMessage(message);
    return reply || { ok: false, error: "no response from background" };
  } catch (e) {
    return { ok: false, error: String(e && e.message ? e.message : e) };
  }
}

// Turn a raw host/messaging error into an actionable toast. "no response from background" means the
// background event page didn't answer — almost always because it is still running pre-reload code
// (re-opening Settings reloads THIS page but not the background page) — so point at the actual fix.
function friendlyError(err) {
  if (/no response from background/i.test(err || "")) {
    return "the MailMate background didn't respond — reload the add-on (about:debugging → Reload)";
  }
  return err || "couldn't list models — type the name instead";
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
  notifPrefs = await loadNotifPrefs(); // extension-local; independent of the host snapshot
  accounts = await loadAccounts(); // from the MailExtension API, not the host snapshot
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
  root.appendChild(notificationsSection());
  root.appendChild(categoriesSection());
  root.appendChild(accountsSection());
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

// modelField — the Model control. Renders a <select> (dropdown) once a non-empty model catalog is
// known for the current endpoint, and a free-text <input> otherwise (server down, empty catalog, or
// a cloud endpoint not yet probed). LOCAL endpoints (loopback) auto-discover as soon as the URL is
// in place — loopback isn't egress, so probing Ollama / LM Studio / llama.cpp is free and the user
// expects the list to appear without a button. CLOUD endpoints never auto-probe (no unprompted
// egress, and the catalog needs the key anyway): the user clicks "List models" after saving a key.
// A "type manually" escape always exists; the chosen value is pushed back through onChange(value).
//
// `probe()` yields the live { kind, endpoint, providerId }. Returns { node, refresh }: call
// refresh() after the kind/endpoint changes so the control re-evaluates the new endpoint. Catalogs
// live in the module-level modelCache, so a successful listing survives the full page re-render
// every host write triggers (the same persistence reason as addDraft) and is never re-fetched.
function modelField(probe, initialValue, onChange) {
  const node = el("div", { class: "mm-modelfield" });
  let value = initialValue || "";
  let manual = false;
  let listing = false;

  const set = (v) => {
    value = v;
    onChange(v);
  };

  async function discover(explicit) {
    const p = probe();
    if (listing || !p.endpoint || p.kind === "mock") return;
    const key = probeKey(p);
    if (!explicit && modelCache.has(key)) return; // already probed (success or empty) — don't repeat
    listing = true;
    paint();
    const reply = await send({ type: "mm:listModels", kind: p.kind, endpoint: p.endpoint, providerId: p.providerId });
    listing = false;
    const models = reply.ok ? reply.models || [] : [];
    modelCache.set(key, models);
    // Only the explicit button is chatty; auto-discovery stays silent so a stopped local server
    // doesn't toast an error on every re-render.
    if (explicit) {
      if (!reply.ok) toast(friendlyError(reply.error), true);
      else if (!models.length) toast(`No models found at ${p.endpoint}`, true);
      else toast(`${models.length} model${models.length === 1 ? "" : "s"} found`);
    }
    if (models.length && !value) set(models[0]); // default to something usable (the user's #2 ask)
    manual = false;
    paint();
  }

  function paint() {
    node.textContent = "";
    const models = modelCache.get(probeKey(probe())) || [];
    if (models.length && !manual) {
      const select = el("select");
      // Keep a saved-but-unlisted model selectable rather than silently switching it to models[0].
      const opts = value && !models.includes(value) ? [value, ...models] : models;
      for (const m of opts) select.appendChild(el("option", { value: m, text: m }));
      select.value = value || models[0];
      if (select.value !== value) set(select.value); // capture the shown default so Add/Save sees it
      select.addEventListener("change", () => set(select.value));
      const refresh = el("button", { text: "↻", attrs: { title: "Refresh model list" } });
      refresh.disabled = listing;
      refresh.addEventListener("click", () => discover(true));
      const manualBtn = el("button", { class: "mm-link", text: "type manually" });
      manualBtn.addEventListener("click", () => {
        manual = true;
        paint();
      });
      node.append(select, refresh, manualBtn);
    } else {
      const input = el("input", { type: "text", value, placeholder: "model (e.g. llama3) — required for chat models" });
      input.addEventListener("input", () => set(input.value));
      const list = el("button", { text: listing ? "Listing…" : "List models" });
      list.disabled = listing;
      list.addEventListener("click", () => discover(true));
      node.append(input, list);
    }
  }

  function refresh() {
    manual = false;
    paint();
    maybeAuto();
  }

  // Auto-discover, but only for a local endpoint we haven't probed yet (cloud stays manual).
  function maybeAuto() {
    const p = probe();
    if (p.endpoint && p.kind !== "mock" && isLocalEndpoint(p.endpoint) && !modelCache.has(probeKey(p))) {
      discover(false);
    }
  }

  paint();
  maybeAuto();
  return { node, refresh };
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

  // Model (editable): the chat adapters (Ollama / LM Studio / OpenAI-compatible) require it;
  // llama.cpp serves whatever model the server has loaded and ignores this. A local endpoint
  // auto-lists into the dropdown; the saved provider's stored key is attached for a cloud catalog.
  let pickedModel = p.model || "";
  const model = modelField(
    () => ({ kind: p.kind, endpoint: p.endpoint || "", providerId: p.id }),
    pickedModel,
    (v) => (pickedModel = v),
  );
  const saveModel = el("button", { text: "Save model" });
  saveModel.addEventListener("click", async () => {
    if (!pickedModel.trim()) return toast("Enter a model name", true);
    saveModel.disabled = true;
    const ok = await write({ type: "mm:setProvider", providerId: p.id, model: pickedModel.trim() }, "Model saved");
    if (!ok) saveModel.disabled = false;
  });
  card.appendChild(row("Model", model.node, saveModel));

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
  // Test connection: a REAL liveness probe (test_provider). A reachable endpoint reports its
  // model count; an unreachable one reports the transport error — both are data, not a failure,
  // so neither re-renders the page (it is a read-only probe, it changes no host state).
  const test = el("button", { text: "Test connection" });
  test.addEventListener("click", async () => {
    const label = test.textContent;
    test.disabled = true;
    test.textContent = "Testing…";
    const reply = await send({ type: "mm:testProvider", kind: p.kind, endpoint: p.endpoint || "", providerId: p.id });
    test.disabled = false;
    test.textContent = label;
    if (!reply.ok) return toast(reply.error || "couldn't reach the MailMate host", true);
    if (reply.reachable) {
      const n = reply.model_count;
      toast(typeof n === "number" ? `Connected — ${n} model${n === 1 ? "" : "s"} available` : "Connected");
    } else {
      toast(`Not reachable: ${reply.error || "no response from the endpoint"}`, true);
    }
  });
  actions.appendChild(test);
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
  // Every field is bound to `addDraft` so it survives the re-render a host write triggers.
  const id = el("input", { type: "text", value: addDraft.id, placeholder: "provider id (e.g. local-ollama)" });
  id.addEventListener("input", () => (addDraft.id = id.value));

  const kind = el("select");
  for (const k of PROVIDER_KINDS) kind.appendChild(el("option", { value: k, text: k }));
  kind.value = addDraft.kind;

  // Show the typed endpoint, or the current kind's default when the user hasn't typed one.
  const endpoint = el("input", {
    type: "text",
    value: addDraft.endpoint || defaultEndpoint(addDraft.kind),
    placeholder: "endpoint URL (e.g. http://localhost:11434 for Ollama)",
  });
  endpoint.addEventListener("input", () => (addDraft.endpoint = endpoint.value));

  const model = modelField(
    () => ({ kind: kind.value, endpoint: endpoint.value.trim(), providerId: null }),
    addDraft.model,
    (v) => (addDraft.model = v),
  );
  // A settled endpoint (blur) re-evaluates the model control: a local URL populates the dropdown,
  // a cloud one falls back to free text.
  endpoint.addEventListener("change", () => model.refresh());

  // Picking a kind swaps in that kind's default endpoint — but never clobbers an endpoint the user
  // actually typed (we only replace a blank field or another kind's untouched default) — then
  // re-evaluates the model control against the new endpoint.
  kind.addEventListener("change", () => {
    addDraft.kind = kind.value;
    const current = endpoint.value.trim();
    if (!current || KIND_DEFAULT_VALUES.has(current)) {
      endpoint.value = defaultEndpoint(kind.value);
      addDraft.endpoint = endpoint.value;
    }
    model.refresh();
  });

  const add = el("button", { class: "mm-primary", text: "Add provider" });
  add.addEventListener("click", async () => {
    if (!id.value.trim()) return toast("Enter a provider id", true);
    add.disabled = true;
    const ok = await write(
      {
        type: "mm:setProvider",
        providerId: id.value.trim(),
        kind: kind.value,
        endpoint: endpoint.value.trim() || undefined,
        model: (addDraft.model || "").trim() || undefined,
      },
      "Provider added",
    );
    // Clear the draft only once it has landed; on failure keep what the user typed.
    if (ok) resetAddDraft();
    else add.disabled = false;
  });
  return el("div", { class: "mm-provider" }, [
    el("div", { class: "mm-section__title", text: "Add a provider" }),
    row("Id", id),
    row("Kind", kind),
    row("Endpoint", endpoint),
    row("Model", model.node),
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

// --- NOTIFICATIONS (extension-local, storage.local) -----------------------------------

function notificationsSection() {
  const prefs = notifPrefs || defaultNotifPrefs();
  const children = [
    el("p", { class: "mm-hint", text: "Desktop notifications point you back into MailMate; they never act on your mail. The dashboard badge always shows what needs you — these only control the OS pings." }),
  ];

  // Per-class toggles.
  for (const [key, label] of NOTIF_CLASS_LABELS) {
    const cb = el("input", { type: "checkbox" });
    cb.checked = prefs.classes[key] !== false;
    cb.addEventListener("change", async () => {
      notifPrefs.classes[key] = cb.checked;
      await saveNotifPrefs();
      toast("Notification preferences saved");
    });
    children.push(el("label", { class: "mm-check" }, [cb, el("span", { text: label })]));
  }

  // Quiet hours.
  const quiet = prefs.quietHours || {};
  const enabled = el("input", { type: "checkbox" });
  enabled.checked = Boolean(quiet.enabled);
  const start = el("input", { type: "time", value: quiet.start || "22:00" });
  const end = el("input", { type: "time", value: quiet.end || "07:00" });
  const persistQuiet = async () => {
    notifPrefs.quietHours = {
      enabled: enabled.checked,
      start: start.value || "22:00",
      end: end.value || "07:00",
    };
    await saveNotifPrefs();
    toast("Notification preferences saved");
  };
  enabled.addEventListener("change", persistQuiet);
  start.addEventListener("change", persistQuiet);
  end.addEventListener("change", persistQuiet);
  children.push(
    el("label", { class: "mm-check" }, [enabled, el("span", { text: "Quiet hours — hold desktop pings during this window" })]),
    row("From", start, el("span", { class: "mm-row__label", text: "to" }), end),
    el("p", { class: "mm-hint", text: "During quiet hours the badge still updates and the dashboard still shows everything — only the OS pings are silenced. A still-pending item will ping again after the window if it recurs." }),
  );
  return section("Notifications", children);
}

// --- CATEGORIES — per-category action policy + tag→category mapping --------------------

// The three policies a category can carry, with the human copy each radio/option shows.
const POLICY_OPTIONS = [
  ["auto", "Auto", "Apply crystallized rules automatically; suggest the rest."],
  ["suggest", "Suggest", "Never act automatically — every action becomes a suggestion."],
  ["off", "Off", "Ignore this category — no actions, no suggestions."],
];

function categoriesSection() {
  const policies = settings.category_policies || {};
  const cats = settings.categories || [];
  const children = [
    el("p", { class: "mm-hint", text: "How MailMate treats each category. ‘Auto’ lets crystallized rules act on their own; ‘Suggest’ keeps you in the loop; ‘Off’ silences the category entirely. Send and delete are never automated." }),
  ];

  for (const cat of cats) {
    const current = policies[cat.key] || "auto";
    const select = el("select");
    for (const [value, label] of POLICY_OPTIONS) select.appendChild(el("option", { value, text: label }));
    select.value = current;
    select.addEventListener("change", () =>
      write({ type: "mm:setCategoryPolicy", category: cat.key, policy: select.value }, `‘${cat.label}’ set to ${select.value}`),
    );
    children.push(el("div", { class: "mm-cat-row" }, [el("span", { class: "mm-cat-row__name", text: cat.label }), select]));
  }

  children.push(tagMappingEditor());
  return section("Categories", children);
}

// The tag→category mapping editor: existing mappings (each removable) plus an add row. Mapping a
// Thunderbird tag to a category surfaces that tag as a first-class category — including a brand-new
// category key, which then appears in the policy table above on the next re-render.
function tagMappingEditor() {
  const mappings = settings.tag_mappings || {};
  const children = [
    el("div", { class: "mm-section__title", text: "Tag → category" }),
    el("p", { class: "mm-hint", text: "Map a Thunderbird tag to a category so your own tags drive triage. Mapping to a new name creates that category." }),
  ];

  const keys = Object.keys(mappings).sort();
  if (!keys.length) {
    children.push(el("p", { class: "mm-hint", text: "No tag mappings yet." }));
  }
  for (const tag of keys) {
    const remove = el("button", { class: "mm-link", text: "remove" });
    remove.addEventListener("click", () => write({ type: "mm:setTagMapping", tag, category: "" }, "Mapping removed"));
    children.push(
      el("div", { class: "mm-cat-row" }, [
        el("span", { class: "mm-cat-row__name", text: `${tag} → ${mappings[tag]}` }),
        remove,
      ]),
    );
  }

  // Add row — bound to nothing (cleared on the re-render a successful write triggers).
  const tagInput = el("input", { type: "text", placeholder: "tag (e.g. $label1 or Important)" });
  const catInput = el("input", { type: "text", placeholder: "category (e.g. work)" });
  const add = el("button", { text: "Add mapping" });
  add.addEventListener("click", async () => {
    const tag = tagInput.value.trim();
    const category = catInput.value.trim();
    if (!tag) return toast("Enter a tag", true);
    if (!category) return toast("Enter a category", true);
    add.disabled = true;
    const ok = await write({ type: "mm:setTagMapping", tag, category }, "Mapping added");
    if (!ok) add.disabled = false;
  });
  children.push(row("New mapping", tagInput, catInput, add));
  return el("div", { class: "mm-subsection" }, children);
}

// --- ACCOUNTS — per-account triage scope ----------------------------------------------

function accountsSection() {
  const scopes = settings.account_scopes || {};
  const children = [
    el("p", { class: "mm-hint", text: "Which accounts MailMate triages. An unchecked account is still classified, but MailMate never auto-acts on or suggests anything for it." }),
  ];

  if (!accounts.length) {
    children.push(el("p", { class: "mm-hint", text: "No mail accounts were reported by Thunderbird, so there is nothing to scope here yet." }));
    return section("Accounts", children);
  }

  for (const acct of accounts) {
    const id = acct.id;
    const inScope = scopes[id] !== false;
    const cb = el("input", { type: "checkbox" });
    cb.checked = inScope;
    cb.addEventListener("change", () =>
      write({ type: "mm:setAccountScope", accountId: id, enabled: cb.checked }, cb.checked ? "Account included" : "Account excluded"),
    );
    children.push(el("label", { class: "mm-check" }, [cb, el("span", { text: acct.name || id })]));
  }
  return section("Accounts", children);
}

load().catch((e) => {
  $("mm-status-banner").hidden = false;
  $("mm-status-banner").textContent = String(e && e.message ? e.message : e);
});
