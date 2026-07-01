// action.test.mjs — headless DOM tests for the toolbar mini-hub popup.
//
// Loads the REAL action.html + action.js and drives clicks: the connection header, the "what
// needs me" aggregate breakdown (with deep-links), Open dashboard, and the Pause toggle. The
// background's badge/host glue is reviewed separately; this covers the popup render + actions.

import { test } from "node:test";
import assert from "node:assert/strict";
import { loadAction, tick } from "./harness.mjs";

const text = (node) => (node ? node.textContent : "");
const bodyText = (document) => text(document.getElementById("mm-body"));

test("connected with work shows the aggregate breakdown with deep-links", async () => {
  const { window, document, state, dom } = await loadAction({
    status: { phase: "ready", hostVersion: "0.1.0", protocol: "1.0", retention: "metadata" },
    aggregate: { reviews: 2, attention: 1, proposals: 1 }, // total derived = 4
  });

  assert.equal(text(document.getElementById("mm-phase")), "Connected");
  assert.ok(bodyText(document).includes("4 things need your attention"), bodyText(document));

  const links = [...document.querySelectorAll(".mm-link")].map(text);
  assert.ok(links.some((l) => l.includes("2 suggestions to review")), links.join("|"));
  assert.ok(links.some((l) => l.includes("1 follow-up need")), links.join("|"));
  assert.ok(links.some((l) => l.includes("1 rule proposal")), links.join("|"));

  // Clicking the review line deep-links the dashboard to the review tab.
  const reviewLink = [...document.querySelectorAll(".mm-link")].find((l) => text(l).includes("review"));
  reviewLink.dispatchEvent(new window.Event("click"));
  await tick(window, 4);
  const open = state.calls.find((c) => c.type === "mm:openDashboard");
  assert.ok(open, "the breakdown link opens the dashboard");
  assert.equal(open.tab, "review", "deep-linked to the review tab");

  dom.window.close();
});

test("connected with nothing waiting shows the calm line and still offers the actions", async () => {
  const { document, dom } = await loadAction({
    aggregate: { reviews: 0, attention: 0, proposals: 0 },
  });
  assert.ok(bodyText(document).includes("Nothing needs you right now"), bodyText(document));
  assert.equal(document.querySelectorAll(".mm-link").length, 0, "no breakdown links at zero");
  // Open dashboard + Pause are always offered when connected.
  const footerButtons = [...document.querySelectorAll("#mm-footer button")].map(text);
  assert.ok(footerButtons.some((t) => t.includes("Open dashboard")), footerButtons.join("|"));
  assert.ok(footerButtons.some((t) => t.includes("Pause")), footerButtons.join("|"));
  dom.window.close();
});

test("the Pause toggle flips set_pause and repaints as paused", async () => {
  const { window, document, state, dom } = await loadAction({
    aggregate: { reviews: 0, attention: 0, proposals: 0 },
    paused: false,
  });
  const pause = [...document.querySelectorAll("#mm-footer button")].find((b) => text(b).includes("Pause"));
  assert.ok(pause, "a Pause toggle is shown when running");
  pause.dispatchEvent(new window.Event("click"));
  await tick(window, 6);

  const flip = state.calls.find((c) => c.type === "mm:setPause");
  assert.ok(flip, "Pause sends mm:setPause");
  assert.equal(flip.paused, true, "it pauses");
  // The repaint reflects the new paused state.
  assert.ok(bodyText(document).includes("paused"), bodyText(document));
  const resume = [...document.querySelectorAll("#mm-footer button")].find((b) => text(b).includes("Resume"));
  assert.ok(resume, "the toggle now offers Resume");
  dom.window.close();
});

test("Open dashboard opens the dashboard with no tab hint", async () => {
  const { window, document, state, dom } = await loadAction({
    aggregate: { reviews: 1, attention: 0, proposals: 0 },
  });
  const open = [...document.querySelectorAll("#mm-footer button")].find((b) => text(b).includes("Open dashboard"));
  open.dispatchEvent(new window.Event("click"));
  await tick(window, 4);
  const call = state.calls.find((c) => c.type === "mm:openDashboard");
  assert.ok(call, "Open dashboard sends mm:openDashboard");
  assert.equal(call.tab, null, "no tab hint from the generic Open dashboard button");
  dom.window.close();
});

test("a disconnected host falls back to the recovery card with Retry", async () => {
  const { document, state, dom } = await loadAction({
    status: { phase: "disconnected", reason: "host crashed" },
  });
  assert.equal(text(document.getElementById("mm-phase")), "Offline");
  assert.ok(bodyText(document).includes("Not connected"), bodyText(document));
  assert.ok(bodyText(document).includes("host crashed"), "the reason is surfaced");
  const retry = [...document.querySelectorAll("#mm-footer button")].find((b) => text(b).includes("Retry"));
  assert.ok(retry, "a Retry button is offered when disconnected");
  // No aggregate fetch when the host is down.
  assert.ok(!state.calls.some((c) => c.type === "mm:aggregate"), "no aggregate fetch while disconnected");
  dom.window.close();
});
