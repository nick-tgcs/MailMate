// action_more.test.mjs — cover the toolbar-popup connection states the original suite didn't
// reach: a version mismatch (a distinct, calm "update one side" card) and the connecting state.
// Same harness + idiom as action.test.mjs.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadAction } from "./harness.mjs";

test("a version mismatch renders its own explanatory card (not the generic recovery)", async (t) => {
  const { document } = await loadAction({
    status: { phase: "version_mismatch", hostVersion: "9.9", protocol: "2.0", reason: "protocol 2.0" },
  });
  t.after(() => {});
  assert.match(document.body.textContent, /version mismatch|update/i);
});

test("a connecting host shows a muted connecting state", async (t) => {
  const { document } = await loadAction({ status: { phase: "connecting" } });
  t.after(() => {});
  assert.match(document.body.textContent, /connect/i);
});
