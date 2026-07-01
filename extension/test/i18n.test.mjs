// i18n.test.mjs — the i18n seam is real and self-consistent.
//
// English-only v1, but the seam must exist so a future locale needs only strings, no code. This
// guards the two halves: the manifest declares a default_locale, and every message key panel.js
// resolves via t("<key>", …) is actually present in _locales/en/messages.json (a missing key
// would silently fall back forever and never localize).

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const extDir = join(here, "..");

test("the manifest declares the default locale that exists on disk", () => {
  const manifest = JSON.parse(readFileSync(join(extDir, "manifest.json"), "utf8"));
  assert.equal(manifest.default_locale, "en");
  // The declared locale's messages file must parse.
  const messages = JSON.parse(
    readFileSync(join(extDir, "_locales", manifest.default_locale, "messages.json"), "utf8"),
  );
  // Each entry is a valid WebExtension message (a non-empty `message` string).
  for (const [key, entry] of Object.entries(messages)) {
    assert.ok(entry && typeof entry.message === "string" && entry.message.length > 0, `${key} has a message`);
  }
});

test("every t() key panel.js resolves is defined in the English catalogue", () => {
  const messages = JSON.parse(readFileSync(join(extDir, "_locales", "en", "messages.json"), "utf8"));
  const panel = readFileSync(join(extDir, "panel.js"), "utf8");
  // Find every t("key", …) call site and assert the key exists.
  const keys = [...panel.matchAll(/\bt\(\s*"([^"]+)"/g)].map((m) => m[1]);
  assert.ok(keys.length >= 5, `panel uses the i18n seam (found ${keys.length} keys)`);
  for (const key of keys) {
    assert.ok(messages[key], `panel.js uses t("${key}") but it is missing from _locales/en`);
  }
});

test("every __MSG_*__ placeholder in the manifest resolves to a catalogue entry", () => {
  // The manifest name/description are localized via __MSG_<key>__ (Phase 9), so they ship through
  // the same catalogue as the rest of the UI — no hardcoded, divergent copy. A placeholder with no
  // entry would render literally in Thunderbird.
  const manifestText = readFileSync(join(extDir, "manifest.json"), "utf8");
  const messages = JSON.parse(readFileSync(join(extDir, "_locales", "en", "messages.json"), "utf8"));
  const keys = [...manifestText.matchAll(/__MSG_([A-Za-z0-9_]+)__/g)].map((m) => m[1]);
  assert.ok(keys.length >= 2, `manifest localizes name + description (found ${keys.length})`);
  for (const key of keys) {
    assert.ok(messages[key], `manifest uses __MSG_${key}__ but it is missing from _locales/en`);
  }
});

test("every t() key dashboard.js resolves is defined in the English catalogue", () => {
  // The dashboard adopted the same seam in Phase 9 (a representative set of strings migrated;
  // the rest follow). Guard it the same way: a t("key") with no catalogue entry would silently
  // never localize.
  const messages = JSON.parse(readFileSync(join(extDir, "_locales", "en", "messages.json"), "utf8"));
  const dashboard = readFileSync(join(extDir, "dashboard.js"), "utf8");
  const keys = [...dashboard.matchAll(/\bt\(\s*"([^"]+)"/g)].map((m) => m[1]);
  assert.ok(keys.length >= 4, `dashboard uses the i18n seam (found ${keys.length} keys)`);
  for (const key of keys) {
    assert.ok(messages[key], `dashboard.js uses t("${key}") but it is missing from _locales/en`);
  }
});
