// message_reader.test.mjs — exercise the REAL readMessageForHost mapping headlessly.
//
// message_reader.js is a classic background script (it talks to `browser.messages.*`), so it
// can't be imported as a module. We load its actual source into a vm context with a mocked
// `browser`, then call the genuine readMessageForHost and assert the wire payload it produces —
// the same payload the Rust host's ClassifyMessagePayload deserializes. This is the layer that
// proves the rich header capture (References / In-Reply-To / Reply-To / List-* / Precedence /
// Authentication-Results) and the sender context actually reach the host, not just that the
// Rust extractor would handle them.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const srcPath = join(here, "..", "message_reader.js");
const src = readFileSync(srcPath, "utf8");

// Run the real source in a fresh context with `browser` mocked; return its exported functions.
function loadReader(browser) {
  const sandbox = { browser, console };
  vm.createContext(sandbox);
  // `this` in a vm script is the context global, so this guarantees the functions are reachable
  // regardless of how the engine binds top-level declarations.
  vm.runInContext(
    `${src}\nthis.readMessageForHost = readMessageForHost;\nthis.setBodyRetention = setBodyRetention;`,
    sandbox,
    // Attribute V8/c8 coverage (and stack traces) to the real file, not "evalmachine.<anonymous>".
    { filename: srcPath },
  );
  return { readMessageForHost: sandbox.readMessageForHost, setBodyRetention: sandbox.setBodyRetention };
}

function fakeBrowser(overrides = {}) {
  return {
    messages: {
      getFull: async () => ({
        headers: {
          "message-id": ["<root@list.test>"],
          references: ["<root@list.test> <parent@list.test>"],
          "in-reply-to": ["<parent@list.test>"],
          "reply-to": ["collector@elsewhere.test"],
          "list-id": ["<newsletter.list.test>"],
          "list-unsubscribe": ["<mailto:unsub@list.test>"],
          precedence: ["bulk"],
          "authentication-results": ["mx.test; spf=pass; dkim=fail; dmarc=fail"],
        },
        parts: [{ contentType: "text/plain", body: "a secret body" }],
      }),
      listAttachments: async () => [
        { name: "invoice.pdf", contentType: "application/pdf", size: 2048 },
      ],
      query: async () => ({ messages: [{}, {}, {}] }),
      ...(overrides.messages || {}),
    },
    contacts: {
      quickSearch: async () => [{ id: "c1" }],
      ...(overrides.contacts || {}),
    },
  };
}

function fakeHeader() {
  return {
    id: 42,
    author: "Jane Doe <jane@list.test>",
    recipients: ["me@x.test"],
    subject: "Re: hello",
    date: new Date("2026-01-01T00:00:00Z"),
    headerMessageId: "<root@list.test>",
    folder: { accountId: "acct_1", path: "/Inbox" },
  };
}

// The payload is built inside the vm realm; round-trip it through JSON exactly as it travels to
// the host, which also normalizes nested array/object prototypes for deepStrictEqual.
const wire = (p) => JSON.parse(JSON.stringify(p));

test("readMessageForHost lifts the rich header set into the wire payload", async () => {
  const { readMessageForHost } = loadReader(fakeBrowser());
  const payload = wire(await readMessageForHost(fakeHeader()));

  assert.equal(payload.thunderbird_message_id, "42");
  assert.equal(payload.account_id, "acct_1");
  assert.equal(payload.folder_id, "/Inbox");
  assert.deepEqual(payload.headers.references, ["<root@list.test>", "<parent@list.test>"]);
  assert.equal(payload.headers.in_reply_to, "<parent@list.test>");
  assert.equal(payload.headers.reply_to, "collector@elsewhere.test");
  assert.equal(payload.headers.list_id, "<newsletter.list.test>");
  assert.equal(payload.headers.list_unsubscribe, "<mailto:unsub@list.test>");
  assert.equal(payload.headers.precedence, "bulk");
  assert.match(payload.headers.authentication_results, /dmarc=fail/);
  // The thread key is the root of the References chain (no native thread id in MV3).
  assert.equal(payload.thread_id, "<root@list.test>");
  // Sender context captured from the address book + a bounded query.
  assert.equal(payload.sender_in_address_book, true);
  assert.equal(payload.sender_seen_count, 3);
  assert.equal(payload.attachments[0].filename, "invoice.pdf");
});

test("the body is withheld unless retention allows, but rich headers still flow", async () => {
  const { readMessageForHost } = loadReader(fakeBrowser());
  const payload = await readMessageForHost(fakeHeader());
  // Body retention defaults off → the body read from getFull is NOT forwarded …
  assert.equal(payload.body_text, null);
  assert.equal(payload.body_retention_allowed, false);
  // … yet the headers read from the SAME getFull are present (metadata, always allowed).
  assert.equal(payload.headers.reply_to, "collector@elsewhere.test");
});

test("setBodyRetention drives whether the body is forwarded", async () => {
  const { readMessageForHost, setBodyRetention } = loadReader(fakeBrowser());

  // The host reports a body-retaining effective level → the body IS forwarded.
  setBodyRetention("bodies");
  let payload = await readMessageForHost(fakeHeader());
  assert.equal(payload.body_text, "a secret body");
  assert.equal(payload.body_retention_allowed, true);

  // Lowering the effective level back to metadata withholds it again.
  setBodyRetention("metadata");
  payload = await readMessageForHost(fakeHeader());
  assert.equal(payload.body_text, null);
  assert.equal(payload.body_retention_allowed, false);
});

test("sender context degrades to null when the address-book/query APIs are absent", async () => {
  const b = fakeBrowser();
  delete b.contacts;
  b.messages.query = undefined;
  const { readMessageForHost } = loadReader(b);
  const payload = await readMessageForHost(fakeHeader());
  // null (not false / 0) so the host treats it as "unknown" rather than a real negative signal.
  assert.equal(payload.sender_in_address_book, null);
  assert.equal(payload.sender_seen_count, null);
});
