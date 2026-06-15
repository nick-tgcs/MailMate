# MailMate — Thunderbird UX & Interaction Design

> **Project:** MailMate
> **Companion to:** [`architecture.md`](architecture.md) — this document designs the Thunderbird-facing surfaces that sit on top of the native-messaging spine defined there.
> **Design stance:** local-first, provider-optional, human-in-the-loop; the extension is a thin view over the Rust host and owns no durable intelligence.

## Table of Contents

1. [Overview](#overview)
2. [Design decisions](#design-decisions)
3. [Surface map](#surface-map)
4. [Dashboard space](#the-mailmate-dashboard-space)
5. [Per-message header panel](#per-message-header-panel-messagedisplayaction-popup)
6. [Compose panel](#compose-panel--reviewing-a-mailmate-draft-reply)
7. [Configuration](#configuration)
8. [Notifications & the time-based layer](#notifications--the-time-based-layer)
9. [Action model: "crystallized-auto, rest suggest"](#action-model-crystallized-auto-rest-suggest)
10. [First-run, onboarding & connection health](#first-run-onboarding-connection-health--empty-states)
11. [Host protocol additions required](#host-protocol-additions-required)
12. [Build order / milestones](#build-order--milestones)
13. [Open questions](#open-questions)

---

## Overview

MailMate is a local-first mail assistant for Thunderbird. The intelligence — classification, learning, policy, drafting, follow-up workflows — lives in a Rust native host; the Thunderbird extension is a thin adapter that reads messages, applies safe Thunderbird-side actions when instructed, and talks to the host over a long-lived native-messaging port. The product idea is a *learning decision system*, not an AI wrapper: AI discovers principles, deterministic rules execute them, and a human approves anything risky. It is fully functional with **zero AI providers configured** — only reply/follow-up *body* generation needs a provider, and that degrades gracefully rather than failing.

**The honest current state.** Today the extension's interaction model is essentially *context-menu → console*. The host already exposes a rich protocol — `classify_message`, `draft_reply`, `record_user_action`, the full follow-up control set, `explain_decision`, `list_pending_reviews`, `get_settings`, and three host→extension notifications (`classification_ready`, `followup_draft_ready`, `followup_needs_attention`) — but almost none of it has a user-facing surface:

- `classification_ready` review-required actions are **logged to the console only** (`background.js` `surfaceReviewSuggestions()`).
- `followup_needs_attention` nudges are **logged to the console only** (`followups.js` `surfaceNeedsAttention()`).
- Draft replies open in a Thunderbird compose window with **no visual indication of `safety_notes` or context** — `drafts.js` notes a review surface "shows payload.safety_notes alongside," but no such UI exists.
- Body retention is **off by default** (`BODY_RETENTION_ALLOWED=false` in `message_reader.js`) with **no settings UI** to toggle it.
- The sales-pipeline / follow-up functions (`enroll_pipeline_item`, `update_pipeline_stage`, `review_followup`) exist but have **no UI** (Phase 12 deferred).
- Mail-command execution results are reported only to the host audit log — **no in-client feedback**.
- There is **no review UI** and **no inbox surface** for monitoring follow-up sequences or stale items.

There is no inbox banner or sidebar injection API in Thunderbird 140, so every in-place affordance must live in a `messageDisplayAction` popup, a `composeAction` popup, a `spaces` dashboard tab, the toolbar `action` popup, `options_ui`, and the desktop `notifications` channel. This document designs those surfaces to close the gaps above without granting any capability the host does not already (or is here explicitly asked to) expose.

---

## Design decisions

Two decisions are **locked** and govern every surface below.

1. **The dashboard is a first-class Thunderbird space; everything else is a satellite that deep-links into it.** MailMate's broad, cross-cutting interaction — the review queue, the follow-up pipeline, rule proposals, and the audit timeline — lives in one persistent tab inside a `spaces`-toolbar space. The per-message header popup, the compose popup, the toolbar `action` popup, and desktop notifications are **narrow satellites**: each answers a local question and then deep-links back into the dashboard space (focusing the right tab, scrolled to the right row) rather than re-implementing the queue. There is one source of truth for "what needs me," and it is the dashboard.

2. **Crystallized-auto, rest suggest.** MailMate acts on your mail by itself **only** after a behavior has been learned from you and hardened into an `active` deterministic rule through the crystallization promotion gate — and even then only for SAFE, reversible actions (file / tag / mark-read), always with Undo, and never overriding a policy veto. Everything not yet crystallized is a **suggestion** you approve in one click. The promotion gate that activates a learned rule **is** the auto-apply gate; there is no second, parallel "should I automate this?" decision. Sending and deleting are never automated — a guarantee, not a setting.

---

## Surface map

| Surface | Thunderbird API | Purpose |
|---|---|---|
| Dashboard space | `spaces.create` / `spaces.open` + `tabs` | Home: Review / Follow-ups / Proposals / Activity; the single source of truth for work waiting on the user |
| Per-message header panel | `messageDisplayAction` + popup | Reading + correcting *this* message: verdict, auto-vs-suggest actions, one-click corrections |
| Compose panel | `composeAction` + popup | The single review gate over a generated draft: rationale + commitments guard + regenerate/adjust (never sends) |
| Configuration | `options_ui` + toolbar `action` popup (quick toggles) | View/edit host settings: provider, retention, accounts, category policy, follow-up cadence, pause |
| Notifications | `notifications` API + `notifications.onClicked` | Ambient, time-driven pointers (follow-up ready / stale / proposal ready) that deep-link into the dashboard |
| Toolbar mini-hub | `action` + popup, `action.setBadgeText` | Aggregate badge (work waiting / connection state) + quick toggles + Pause |
| Onboarding / connection status | `spaces` tab (walkthrough) + `action` badge + `notifications` | First-run walkthrough, host-connection health, and empty states |

---

## The MailMate Dashboard Space

The Dashboard is MailMate's home — a first-class **space** in Thunderbird's left spaces toolbar (`browser.spaces.create`) that opens one persistent **tab** (`browser.spaces.open` / `tabs` API) rendering a custom SPA. It is the only surface broad enough to show *queues, pipelines, proposals, and history* at once; the per-message header popup, the toolbar action popup, and compose panels are all narrow satellites that **deep-link back into this tab**.

The space is local-first and provider-optional: every panel renders fully with **zero AI providers configured**. Where a feature needs a provider (only reply/follow-up *body* generation), the row degrades to a labelled "drafting needs a provider" affordance rather than failing silently.

### Spaces-toolbar button

The toolbar button carries a single **aggregate badge** = the count of things the user must act on right now: `review_queue + needs_attention_followups + pending_proposals`. It does **not** count auto-applied (crystallized) actions — those already happened with undo, so they are not work. Badge is suppressed (no dot) at zero.

`spaces.create` returns a `Space` object whose integer `id` is the handle every later call needs (`spaces.update` / `spaces.open` take that integer `spaceId`, never the name string). So we capture and persist it once at registration time. Button visuals (icon + badge) are `buttonProperties` on the `Space`, where the icon field is `defaultIcons`:

```
const space = await browser.spaces.create("mailmate", "/dashboard.html", {
  title: "MailMate",
  defaultIcons: { "16": "icons/mm-16.png", "32": "icons/mm-32.png" }
})
await persistSpaceId(space.id)   // stash space.id; every later update/open uses this integer

// later, to set/refresh the aggregate badge (buttonProperties is the 3rd arg):
await browser.spaces.update(space.id, null, {
  badgeText: "<N>",                  // aggregate actionable count, "" when 0
  badgeBackgroundColor: "#b3261e"    // amber if only follow-ups due, red if reviews waiting
})
```

```
 Thunderbird spaces toolbar (left rail)
 ┌────┐
 │ ✉  │  Mail
 │ 📅 │  Calendar
 │ 👤 │  Contacts
 │ ◆ ●│  MailMate   ← ● red dot = work waiting (aggregate badge)
 │ ⚙  │  Settings
 └────┘
```

### Overall tab layout

One header (global state + tab bar), one content pane. The four tabs are **Review / Follow-ups / Proposals / Activity**. Each tab name carries its own count badge (independent of the aggregate toolbar badge) so the user sees the breakdown without clicking.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ◆ MailMate          [ ⏸ Pause auto-apply ]   Provider: none ⚠   ⟳  ⚙ Settings │
│────────────────────────────────────────────────────────────────────────────────│
│  Review (3)   Follow-ups (5)   Proposals (2)   Activity                         │
│════════════════════════════════════════════════════════════════════════════════│
│                                                                                  │
│   « active tab content renders here »                                            │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

Header controls, fixed across all tabs:

- **Pause auto-apply** — a kill-switch for crystallized-auto. While paused, crystallized SAFE actions stop auto-applying and fall back to suggestions in the Review queue; the button reads `▶ Resume auto-apply`. (Backed by the host-side `set_pause` state — see *Configuration* — so it survives reloads and stops host-side drains, not merely a local extension flag.)
- **Provider chip** — reflects `get_settings.default_provider`. `Provider: none ⚠` when null; clicking opens the Settings (`options_ui`) provider page. This is the single global signal explaining why a draft row might say "needs a provider".
- **⟳ Refresh** — re-pulls all queue queries (see each tab).
- **⚙ Settings** — opens `options_ui` via `browser.runtime.openOptionsPage()`.

**Common row grammar.** Every row across every tab shares a layout: a left **risk/priority glyph**, a **title + one-line rationale**, a **right-aligned action cluster**, and a **caret** (`›`) that **deep-links to the source message** by selecting it in the Mail space:

```
deepLink(thunderbirdMessageId):
  // mailTabs.query queryInfo accepts only active/currentWindow/lastFocusedWindow/windowId —
  // there is no folder selector, so just find (or open) a mail tab, then select the message.
  let [tab] = await browser.mailTabs.query({ currentWindow:true })
  if (!tab) tab = await browser.mailTabs.create()        // exists since TB 121
  await browser.mailTabs.setSelectedMessages(tab.id, [Number(thunderbirdMessageId)])
  await browser.tabs.update(tab.id, { active: true })    // focus Mail, keep MailMate tab alive
```

Deep-link never destroys the dashboard tab — it focuses the Mail tab with the message selected and the per-message header popup ready, so reading + correcting happens in context and the user can return to the dashboard tab still open.

---

### Tab 1 — Review queue

The work queue of **suggestions awaiting a human**: review-required actions from background `classification_ready` notifications, plus (when paused, or never-crystallized) SAFE actions that did not auto-apply. Each card shows the classification verdict, the suggested actions partitioned by policy, and the blocked actions (shown but never actionable — they exist to make the policy guard visible).

The queue is built from `classification_ready` notification payloads the background script buffers, hydrated/re-checked via `classify_message` (alias `read_message`) for a selected card and `explain_decision` for the full timeline.

```
┌─ Review (3) ─────────────────────────────────────────────────────────────────┐
│                                                                  [ ✓ Approve all safe ] │
│──────────────────────────────────────────────────────────────────────────────│
│ ⚠ HIGH   "Q3 invoice — overdue notice"            from billing@acme-pay.co ›   │
│          Phishing 0.82 · priority high · labels: finance, suspicious           │
│          Suggested:  ⟶ move → Suspicious     [requires review]                 │
│                      🏷 tag  "phishing?"      [requires review]                 │
│          Blocked:    ✗ delete — policy never_auto_delete                       │
│          ───────────────────────────────────────────────────────────────────  │
│          [ Approve ]  [ Review each ]  [ Dismiss ]  [ ⏰ Snooze ▾ ]  [ Explain ]│
│──────────────────────────────────────────────────────────────────────────────│
│ ●  MED   "Re: design review notes"                from dana@studio.io      ›   │
│          spam 0.04 · priority normal · labels: project                         │
│          Suggested:  ⟶ file → Projects/Studio  [safe · not yet crystallized]   │
│          ───────────────────────────────────────────────────────────────────  │
│          [ Approve ]  [ Approve + always ]  [ Dismiss ]  [ ⏰ Snooze ▾ ]        │
│──────────────────────────────────────────────────────────────────────────────│
│ ○  LOW   "Your GitHub receipt"                    from receipt@github.com  ›   │
│          labels: receipt  ·  ✓ auto-filed → Receipts/Software  (crystallized)  │
│          ───────────────────────────────────────────────────────────────────  │
│          [ ↩ Undo ]   was auto-applied 12s ago by rule "Software receipts" v3  │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Per-row actions:**

| Action | Behaviour | Host call |
|---|---|---|
| **Approve** | Apply this card's suggested actions (move/tag/mark-read) via TB APIs, then record the confirmation. Row collapses to a 4-second undo toast. | TB `messages.move`/`update`, then `record_user_action` (provenance) |
| **Approve + always** | Approve *and* register this as the user-confirmed behaviour that feeds crystallization — the explicit "promote me" signal. Shown only on SAFE rows not yet crystallized. | `record_user_action` (filing/classification feedback, `user_initiated:true`) |
| **Approve all safe** (header) | Bulk-approve every SAFE+allowed row in the queue in one click; never touches `requires_review` rows. Each gets its own undo entry; the toast offers "Undo all". | per-row TB apply + `record_user_action` |
| **Review each** | Expands the card to one mini-row per suggested action with an inline ✓/✗ per action; for `requires_review` cards this is the *only* approve path (no bulk). | per-action `record_user_action` |
| **Dismiss** | Reject the suggestion(s). Recorded as a negative correction (the highest-value learning signal — "you suggested wrong"). | `record_user_action` (`suggestion_dismissed`) |
| **Snooze ▾** | Hide the card for 1h / today / tomorrow (local timer); re-surfaces unchanged. Distinct from follow-up snooze — this is just queue triage, no host workflow. | none (local) |
| **Explain** | Slides in the Activity timeline for this `decision_id` (jumps to Tab 4 pre-filtered). | `explain_decision` |
| **↩ Undo** | Reverse an auto-applied crystallized action within its undo window. | reverse TB op + `record_user_action` (`action_undone`) |

**Empty state:**

```
┌─ Review (0) ─────────────────────────────────────────────────────────────────┐
│                                                                                │
│                      ✓  Inbox triaged — nothing waiting                        │
│                                                                                │
│        Crystallized rules are auto-filing safely in the background.            │
│        New suggestions appear here when MailMate isn't yet sure.               │
│        ▸ 18 messages auto-filed today  ·  see Activity                         │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

### Tab 2 — Follow-ups pipeline

The sales-pipeline tracker: every enrolled quote/proposal as a tracked deal, with its workflow status, next due step, and any due **review-required follow-up drafts**. Two visual bands: **Needs attention** (stale items + drafts ready to review) pinned to the top, then the **live pipeline** grouped by stage. Drafts are **never auto-sent** — the row's primary action is "Open draft to review", and resolution rides `review_followup`.

```
┌─ Follow-ups (5) ─────────────────────────────────────────────────────────────┐
│ NEEDS ATTENTION ─────────────────────────────────────────────────────────────│
│ 📝 Draft ready · Acme Pty — $12k fitout quote        thread: Re: Acme quote ›  │
│    Day-14 follow-up (coalesced day-7). No reply since day 7.                    │
│    [ Open draft to review ]  [ Skip step ]  [ ⏰ Snooze ▾ ]  [ Explain ]        │
│    ⚠ drafting needs a provider — opens a blank review draft with talking points │
│ ───────────────────────────────────────────────────────────────────────────── │
│ ⏳ Stale · Bayside Cafe — coffee-cart proposal        thread: Proposal v2  ›    │
│    No activity past horizon · steps 2,3 skipped · reason: stale_past_horizon    │
│    [ Re-arm ]  [ Mark lost ]  [ Cancel sequence ]  [ Explain ]                  │
│══════════════════════════════════════════════════════════════════════════════│
│ ACTIVE PIPELINE ─────────────────────────────────────────────────────────────│
│ ▸ Awaiting reply (2)                                                           │
│   • Northwind — server refresh quote    next: day-7 nudge in 3d   [ ⏰ ][ ✕ ] › │
│   • Delos — support renewal             next: day-3 nudge in 19h  [ ⏰ ][ ✕ ] › │
│ ▸ Won (1)   ▸ Lost (0)                  collapsed — click to expand             │
│──────────────────────────────────────────────────────────────────────────────│
│ [ + Enroll selected message as a deal ]   (pick the sent quote in Mail first)  │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Per-row actions:**

| Action | Behaviour | Host call |
|---|---|---|
| **Open draft to review** | Opens the review-required follow-up draft in a compose window (composeAction panel shows safety_notes). User edits + sends themselves; on send, resolve as `send`. | open via existing `openFollowupDraft`; then `review_followup` (`send`/`edit`) |
| **Skip step** | Advance the workflow cursor without drafting. | `review_followup` (`skip`) |
| **Snooze ▾** | Push `next_due_at` out (1d / 3d / pick a date). Marks the instance snoozed. | `snooze` |
| **Re-arm** (stale) | Reschedule the next step back into the window. | `reschedule_followup` |
| **Mark won / Mark lost** | Close the deal, exit all instances. | `update_pipeline_stage` |
| **Cancel sequence** (`✕`) | Stop the workflow without won/lost outcome. | `cancel_sequence` |
| **Enroll selected as a deal** | Arm a workflow on the message currently selected in Mail. | `mailTabs.getSelectedMessages` → `enroll_pipeline_item` |
| **Explain** | Timeline for this workflow instance's `decision_id`. | `explain_decision` |

Provider-off degradation is **inline and per-row**: a draft that would need generation shows `⚠ drafting needs a provider` and the action opens a **blank review draft pre-filled with deterministic talking points** (subject + the step's rationale), preserving the never-fail contract.

**Empty state:**

```
┌─ Follow-ups (0) ─────────────────────────────────────────────────────────────┐
│                  📭  No deals tracked yet                                       │
│   Send a quote or proposal, select it in Mail, and click                       │
│   "Enroll selected message as a deal" to have MailMate time the nudges.        │
│   MailMate drafts the follow-ups for you to review — it never sends them.       │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

### Tab 3 — Proposals

Crystallized-rule candidates and curator proposals **awaiting human approval** — the materialization gate. This is the surface where the determinism-first promotion gate becomes a human decision: a proposal here is a learned principle that the back-test passed but that policy requires a human to activate (risky/conflicting proposals are *forced* to this queue and can never self-activate). Built from `list_pending_reviews`; detail and approve/reject require protocol additions (below).

```
┌─ Proposals (2) ──────────────────────────────────────────────────────────────┐
│──────────────────────────────────────────────────────────────────────────────│
│ ◆ new_rule · LOW risk      "File software receipts"                            │
│   When sender_domain ∈ {github.com, stripe.com} AND subject contains          │
│   "receipt"/"invoice"  →  move → Receipts/Software · tag "receipt"             │
│   Back-test: precision 0.97 · support 41 msgs · 0 conflicts                    │
│   Recommended: accept_for_shadow_mode                                          │
│   ─────────────────────────────────────────────────────────────────────────── │
│   [ ✓ Approve → shadow ]  [ ✓ Approve → active ]  [ ✎ Edit rule ]  [ ✗ Reject ]│
│──────────────────────────────────────────────────────────────────────────────│
│ ⚠ merge_rules · HIGH risk  "Merge 3 vendor-filing rules"        ⚑ CONFLICT     │
│   Overlaps active rule "Invoices→Finance" on 6 msgs — forced to human review.  │
│   Back-test: precision 0.88 · support 23 msgs · 1 conflict                     │
│   Recommended: pending_review (human required)                                 │
│   ─────────────────────────────────────────────────────────────────────────── │
│   [ Review conflict ]  [ ✎ Edit rule ]  [ ✗ Reject ]      (no one-click accept)│
└──────────────────────────────────────────────────────────────────────────────┘
```

**Per-row actions:**

| Action | Behaviour | Host call |
|---|---|---|
| **Approve → shadow** | Materialize into shadow mode (rule runs, logs, but does not act) to build confidence before going live. | `review_rule_proposal` (`accept_for_shadow_mode`) |
| **Approve → active** | Materialize directly active. Shown only for LOW-risk, non-conflicting proposals. | `review_rule_proposal` (`accept_active`) |
| **Edit rule** | Open the rule's JSON-AST condition/effect in an inline editor; approving sends the edited body. | `review_rule_proposal` (decision + `edited_rule`) |
| **Reject** | Decline; recorded as proposal feedback (curator learns not to re-propose). | `review_rule_proposal` (`reject`) |
| **Review conflict** | Expand the overlap detail (which active rule, which messages disagree). For HIGH-risk/conflicting rows this gates the only accept path. | `get_proposal_detail` |

Risk drives the action set: **HIGH-risk / conflicting proposals never show a one-click accept** — they force "Review conflict" first, honouring "propose, never activate; risky → forced human review."

**Empty state:**

```
┌─ Proposals (0) ──────────────────────────────────────────────────────────────┐
│              🧠  No rules waiting for approval                                  │
│   As you correct MailMate (Dismiss / "Not spam" / refile), it discovers        │
│   patterns and proposes deterministic rules here for you to approve.           │
│   Nothing activates on its own — every rule is your decision.                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

### Tab 4 — Activity / Explain

The audit trail: the unified "everything that happened" timeline. Two modes — a **global stream** of recent events across all messages, and a **focused** view filtered to one `decision_id` or `message_id` (entered by the Review/Follow-ups "Explain" actions, or by deep-link). The focused mode is powered by `explain_decision` (which is inherently per-message — it requires a `message_id` / `thunderbird_message_id`); the cross-message global stream is powered by the new `list_recent_activity` request, since `explain_decision` cannot return a global, cross-message audit stream. Read-only by design — this tab *explains*, it does not *act*.

```
┌─ Activity ───────────────────────────────────────────────────────────────────┐
│ Filter:  [ All ▾ ]  [ message / decision id … ]   Showing: "Q3 invoice…"  ✕    │
│──────────────────────────────────────────────────────────────────────────────│
│ Timeline · message "Q3 invoice — overdue notice"           decision dec_123  ›│
│                                                                                │
│  09:41:02  🔎 classified        actor:system   phishing 0.82 · priority high   │
│            matched rule "Suspicious-sender heuristics" v2                       │
│  09:41:02  🛡 policy check       actor:system   move→requires_review · delete✗  │
│  09:41:02  ⏸ surfaced for review actor:system   2 actions sent to Review queue │
│  09:43:18  ✗ blocked            actor:system   delete — never_auto_delete      │
│  09:44:50  👤 user dismissed     actor:user     rejected move→Suspicious        │
│            → recorded as classification correction (learning signal)           │
│──────────────────────────────────────────────────────────────────────────────│
│ Global stream (newest first)                                                   │
│  09:50  ✓ auto-filed   "GitHub receipt" → Receipts/Software  (rule v3)      ↩ ›│
│  09:48  📝 follow-up    Acme quote — day-14 draft surfaced for review        › │
│  09:40  🧠 proposal     "File software receipts" → pending review            › │
└──────────────────────────────────────────────────────────────────────────────┘
```

Every timeline row carries the same `›` deep-link to its source message, and auto-applied rows carry an inline `↩` undo when still within the window. The global stream is fetched via `list_recent_activity` and the filter chips map to its `event_type_filter` parameter (classified / applied / blocked / corrected / follow-up / proposal families over `audit_log.event_type`) — `explain_decision` cannot serve this because it is constrained to a single message. The global stream is the only tab with no count badge — it is history, not a work queue.

**Empty state** (fresh install):

```
┌─ Activity ───────────────────────────────────────────────────────────────────┐
│                  🗒  Nothing has happened yet                                   │
│   Once MailMate classifies a message or applies a rule, every step shows here  │
│   — classification, policy checks, what was applied or blocked, and your        │
│   corrections — fully replayable and offline.                                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

### Counts, badges, and refresh model

- **Toolbar aggregate badge** = `Review.actionable + Followups.needs_attention + Proposals.pending`. Re-derived on every notification and every tab refresh; written via `browser.spaces.update(spaceId, null, { badgeText })` using the persisted integer `space.id` (the `buttonProperties` arg is the third parameter; `tabProperties` is passed `null`).
- **Per-tab badges** = that tab's own queue length (`Review` = open cards, `Follow-ups` = needs-attention + active, `Proposals` = `list_pending_reviews.length`). Activity has none.
- **Live updates:** the background script forwards `classification_ready`, `followup_draft_ready`, `followup_needs_attention` into the dashboard tab via `runtime` messaging, so queues update without a manual refresh; `⟳` forces a full re-pull (re-runs `list_pending_reviews`, the follow-up list query, and re-reads buffered notifications). The badge is computed in the background page so it is correct even when the dashboard tab is closed.

---

## Per-Message Header Panel (`messageDisplayAction` popup)

> **Surface:** `messageDisplayAction` + its popup, anchored in the open-message header toolbar.
> **Role in the model:** the *reading + correcting* surface. Because Thunderbird 140 exposes **no inbox banner/sidebar injection API**, every per-message affordance MailMate offers — the verdict, the suggested actions, and the one-click corrections — must live in this popup. It is the single place a user sees "what MailMate thinks about *this* message" and the single place they teach it.
>
> **Implementation status (Milestone 1 — shipped).** Built as `panel.html` / `panel.css` / `panel.js` behind the `message_display_action` key, driven entirely through the background's `mm:*` router (the popup owns no native port). It renders the verdict (category + a *banded* confidence + `why`), the `apply_state`-partitioned action blocks (auto-applied · suggested · blocked), and the three one-click corrections (wrong-category → `classification_corrected`, not-junk → `junk_changed`, move → the existing `onMoved` filing-correction path). Two honest M1 deferrals: the confidence is a client-side *band* (a calibrated numeric band is a host addition), and the `auto_applied` block is render-complete but unseen until a crystallized rule exists (Milestone 2) — a manual classify applies nothing.

### Design principles for this surface

1. **The verdict is always honest about its own certainty.** Category + a calibrated confidence band + a one-line, plain-language "why" drawn straight from `explanation.summary`. No raw scores in the face of the user; the scores live behind the "why".
2. **Auto vs. suggest is visually unambiguous.** A *crystallized-auto* action that already happened is shown as a past-tense fact with **Undo**. A *pending suggestion* is shown as a future offer with **Apply / Dismiss**. The two never share a visual treatment — that distinction is the entire trust contract of "crystallized-auto, rest suggest".
3. **Correction is one click.** Wrong-category, not-spam, and filed-wrong are first-class buttons, not buried in a context menu. Each emits exactly one `record_user_action` and closes the loop. This is the learning fuel; friction here starves the system.
4. **The panel degrades, never lies.** No provider, no body retention, classification still pending — each is a labeled state, never a blank or a silent failure.

### State the panel renders from

The popup opens, reads the displayed message, and calls `classify_message` (alias `read_message`) for the message currently shown — the same payload `message_reader.js` already builds. From the response it has everything it needs *except* one thing: at classify time the host returns `policy_outcome: 'allowed' | 'requires_review'` per action, but **"allowed" is not the same as "already auto-applied"**. An `allowed` action is auto-applied *only when a crystallized rule drove it*, and only on the background `new_mail` path (where `classification_ready.applied_actions` lists what actually ran). When the user opens a message manually, nothing has been applied yet. So the panel needs the host to tell it, per action, which of three buckets it is in: **already auto-applied (crystallized)**, **suggested (apply needs a click)**, or **blocked**. That is protocol addition `apply_state` below.

```
suggested_actions[].apply_state  ∈  { auto_applied, suggest, blocked }
                                       │            │         └─ from blocked_actions[]
                                       │            └─ allowed but not yet crystallized → user approves
                                       └─ a crystallized rule already ran it (only seen post-new_mail)
```

### Mockup — the common case: a verdict + one auto-applied action + one suggestion

```
┌─ MailMate ───────────────────────────────── ✕ ─┐
│                                                 │
│  📁  Newsletters · Promotions                   │
│  ▓▓▓▓▓▓▓▓░░  High confidence                    │
│  “Bulk sender you’ve filed here 14×.”   why ▸   │
│                                                 │
│ ─────────────────────────────────────────────  │
│  ✓ Filed to  Newsletters                  ⟲ Undo│   ← crystallized-auto (past tense)
│     auto · learned rule R-118                   │
│                                                 │
│  ◻ Suggested:  Mark read                        │   ← pending suggestion (future offer)
│        [ Apply ]   [ Dismiss ]                  │
│                                                 │
│ ─────────────────────────────────────────────  │
│  Not right?                                     │
│  [ Wrong category ▾ ]  [ Not junk ]  [ Move… ▾ ]│   ← one-click corrections
│                                                 │
│  ⓘ Explain in dashboard ▸                       │
└─────────────────────────────────────────────────┘
```

- **Verdict line:** `classification.labels[0]` as the category, a 10-cell bar mapped from the *calibrated confidence band* (not the raw `spam_score`/`phishing_score`). The one-liner is `explanation.summary`; `why ▸` expands `explanation.policy_checks` + `labels` inline.
- **Auto-applied block** uses a **filled check ✓**, past-tense verb ("Filed to"), the subtext `auto · learned rule R-<id>`, and a single **⟲ Undo** affordance. This is the *only* place a crystallized action is acknowledged per-message.
- **Suggestion block** uses a **hollow box ◻**, the word "Suggested:", and **two buttons**. It can never be confused with something that already happened.

### Mockup — high-risk / review-required verdict (phishing) with a blocked action

```
┌─ MailMate ───────────────────────────────── ✕ ─┐
│                                                 │
│  ⚠  Likely phishing                             │
│  ▓▓▓▓▓▓▓▓▓░  Very high confidence               │
│  “Spoofed reply-to + credential link.”  why ▸   │
│                                                 │
│ ─────────────────────────────────────────────  │
│  ◻ Suggested:  Mark as junk                     │
│        [ Apply ]   [ Dismiss ]                  │
│                                                 │
│  ⛔ Delete — blocked by policy                   │   ← blocked_actions[], never offered
│     never_auto_delete_mail · review only        │
│                                                 │
│ ─────────────────────────────────────────────  │
│  Wrong?  [ This is legitimate ]   [ Move… ▾ ]   │
│                                                 │
│  ⓘ Explain in dashboard ▸                       │
└─────────────────────────────────────────────────┘
```

A `blocked_actions[]` entry is shown **greyed, disabled, with its `policy_id` and `reason`** — so the user sees that MailMate *considered and refused* a dangerous action (delete/send are never even offered as buttons). This is the policy guard made visible, and it is a trust feature: the system advertises its own restraint. For a phishing verdict the correction verb flips to **"This is legitimate"** (a ham correction).

### Mockup — the correction menus (one click → one `record_user_action`)

```
  [ Wrong category ▾ ]              [ Move… ▾ ]
   ┌───────────────────────┐        ┌────────────────────┐
   │ ○ Important           │        │ 🔍 type to filter…  │
   │ ○ Personal            │        │ ── recent ───────── │
   │ ○ Receipts            │        │ 📁 Clients/Acme     │
   │ ○ Newsletters  ✓ (now)│        │ 📁 Receipts         │
   │ ○ Social              │        │ 📁 Archive          │
   │ ──────────────────    │        │ ── all folders ──▸  │
   │ ○ Something else…     │        └────────────────────┘
   └───────────────────────┘
```

- **Wrong category** → pick the right label → emits `record_user_action { event_type: "classification_corrected", decision_id, thunderbird_message_id, corrected_label, user_initiated:true }` (routes into `classification_feedback`).
- **Not junk / This is legitimate** → reuses the *existing* path verbatim: `browser.messages.update(id,{junk:false})` then `record_user_action { event_type:"junk_changed", junk:false, user_initiated:true }` (already implemented in `background.js`'s `recordCorrection`).
- **Move…** → folder picker → performs the real `browser.messages.move()`; the resulting `onMoved` already lands a `message_moved` filing correction through the existing listener — **no new wire call needed**, the panel just triggers the move. This is the "filed-wrong → choose folder" correction and it is genuinely one interaction.

A small green confirmation toast replaces the buttons for ~2s after any correction ("Got it — learning from this"), then the panel reflects the new state. Every correction is reversible by re-opening the menu.

### Apply / Dismiss / Undo behavior

- **Apply (suggestion):** the panel executes the safe action *locally* via the same `applyPlannedAction()` in `drafts.js` (tag / move / mark_read / mark_junk / flag — never send/delete), then records the outcome with `record_user_action { event_type:"action_applied", … , source:"suggestion_accepted" }`. The "accepted suggestion" signal is exactly what the learning loop needs to eventually crystallize this into an auto rule — so accepting a suggestion is how a user *promotes* a behavior toward auto.
- **Dismiss (suggestion):** records `record_user_action { event_type:"suggestion_dismissed", decision_id, action_kind, user_initiated:true }`. No mail mutation. (The architecture's learning loop tracks *ignore rate* alongside accept/undo, so this signal must exist.)
- **Undo (auto-applied):** reverses the concrete mail mutation locally (move back to prior folder, remove tag, toggle read), then records `record_user_action { event_type:"action_undone", decision_id, rule_id, action_kind, user_initiated:true }`. An undo is the single strongest negative signal a crystallized rule can get — the architecture tracks **undo rate** as a rule-demotion trigger — so this discriminant must route to feedback, not merely the audit log. To undo, the panel must know the *prior state*; that prior state ships in the `apply_state` enrichment as `auto_applied.reverses_to`.

### The header-button badge / indicator

The `messageDisplayAction` button itself (always visible in the open-message header) carries a state-at-a-glance badge **before** the popup is opened, set via `messageDisplayAction.setBadgeText` / `setBadgeBackgroundColor` / `setTitle` per displayed message:

| Situation | Icon tint | Badge text | Badge color | Title (tooltip) |
|---|---|---|---|---|
| Pending suggestion(s) awaiting you | accent (blue) | count, e.g. `2` | blue | "2 suggestions — review" |
| Auto-applied, nothing pending | calm (grey-green) | `✓` | green | "Filed automatically — open to undo" |
| High-risk verdict (phishing/spam) | warning (amber) | `!` | amber | "Likely phishing — review" |
| Not yet classified / in flight | muted | (none, subtle spinner-dot) | — | "Classifying…" |
| No provider needed here / clean | neutral | (none) | — | "MailMate" |

Rule: the badge surfaces the **single highest-priority unmet thing** — a pending suggestion or a risk verdict outranks a quiet "all auto-handled" check. Counts come from `suggested_actions` with `apply_state:"suggest"`; the `!` from any `needs_review` or non-empty `blocked_actions`. The badge is set from the cached `classify_message` result the background page already holds, so opening a message paints the badge without the user clicking.

### Degraded and edge states (each labeled, never silent)

```
┌─ MailMate ─────────────────────── ✕ ─┐   ┌─ MailMate ─────────────────────── ✕ ─┐
│  📨  Not yet classified              │   │  📁  Receipts · Medium confidence    │
│  Body text is off, so MailMate is    │   │  ▓▓▓▓▓▓░░░░                          │
│  using headers only.                 │   │                                      │
│  [ Classify now ]                    │   │  ◻ Suggested: Draft a reply          │
│  [ Turn on body reading in Settings ]│   │     ⚠ Drafting needs a provider.     │
└──────────────────────────────────────┘   │     [ Set up in Settings ]           │
                                            └──────────────────────────────────────┘
```

- **Body retention off** (`BODY_RETENTION_ALLOWED=false`): a one-line notice + a deep-link to `options_ui`. The verdict still renders (headers-only is a valid classification), it just says so.
- **Drafting with no provider** (`zero_provider_by_default`): a *Draft a reply* suggestion shows the explicit message **"Drafting needs a provider"** with a Settings link — it never silently fails. (Mirrors the `get_settings.default_provider === null` check.)
- **Classification in flight / host down:** "Not yet classified" with a manual **[ Classify now ]** button (calls `classify_message` on demand). A dead host shows "MailMate host unreachable" — never a blank popup.

### Why this fits the locked model

The popup is deliberately *per-message and lightweight*: it answers "what about THIS email, and how do I fix it" in one screen, and punts everything cross-cutting — the work queue, follow-ups, proposals, the full audit timeline — to the dashboard space via the `ⓘ Explain in dashboard ▸` link (which opens the Activity/explain tab focused on this `decision_id`, powered by `explain_decision`). It introduces no send/delete affordance anywhere, makes the crystallized-auto-vs-suggest line the visual spine of the surface, and keeps every correction to a single click.

---

## Compose Panel — Reviewing a MailMate Draft Reply

The composeAction popup is the **single review gate** between a generated draft and the user's send button. It opens in the toolbar of the Thunderbird compose window that `draft_reply` populated (via `compose.beginReply` → `compose.saveMessage({mode:"draft"})`). By the time the user sees this panel, the draft body is *already* sitting in the editable compose area as a saved draft — the panel does not contain the body; it **annotates** the draft the user is looking at, supplying the rationale and the safety verdict the compose window cannot show on its own.

The panel exists because of a hard product line: **MailMate never sends.** There is no send path in the extension (`drafts.js` deliberately omits one) and a host policy guard forbids auto-send. So this surface's entire job is to make a human's decision *fast and informed*, not to act for them. Every visual element reinforces one of three things: *why* this draft, *what it might commit you to*, and *how to change it* — after which the user presses Thunderbird's own Send (or doesn't).

### Anatomy

The popup is driven by the `draft_reply` response — `{ draft_id, subject, body, safety_notes, requires_human_review }` — plus a `get_settings` snapshot read once at popup open to know whether a provider is even configured. `requires_human_review` is, by construction, always true here (drafts are advisory), so the panel treats it as a constant invariant rather than a branch.

```
┌──────────────────────────────────────────────────────┐
│  MailMate · Draft reply                      [ ✕ ]   │
├──────────────────────────────────────────────────────┤
│  ⓘ  This is a DRAFT. MailMate never sends.           │
│      Read it, adjust it, then use Thunderbird's       │
│      Send when you're ready.                          │
├──────────────────────────────────────────────────────┤
│  Why this draft                                       │
│  ──────────────                                       │
│  Reply to Dana Okafor re: "Q3 renewal pricing".       │
│  Matched your past replies to renewal threads:        │
│  brief, acknowledges the ask, defers numbers to a     │
│  call. Tone: warm-professional.                       │
│                                                       │
│  Commitments guard          ⚠ 2 flags — please read   │
│  ─────────────────                                    │
│   ⚠ Prices    Draft mentions a figure ("~$4k").       │
│               MailMate did not verify it. Confirm     │
│               before sending.                         │
│   ⚠ Dates     Proposes "next Tuesday". Check your     │
│               calendar.                                │
│   ✓ Payment   No payment terms stated.                │
│   ✓ Legal     No contractual/legal language.          │
│                                                       │
├──────────────────────────────────────────────────────┤
│  [ ↻ Regenerate ]   [ ✎ Adjust… ]                     │
│                                                       │
│  Draft is saved. Closing this keeps it in Drafts.     │
└──────────────────────────────────────────────────────┘
```

**Header invariant.** The "never sends" line is not dismissible and not conditional. It is the first thing read every time, so the mental model ("I am the sender") never erodes through familiarity.

**Why this draft.** A plain-language rationale rendered from the draft's `explanation.summary`-style text. This is the trust surface: a draft the user *understands* is a draft they can correct meaningfully. When the source content is thin (degraded / rule-based drafting), this section says so explicitly rather than inventing reasoning (see degraded state below).

### The forbidden-commitments guard

This is the panel's safety centrepiece and the part most tied to the "never auto-send / never auto-delete" posture. MailMate watches for four classes of statement that a draft should *never* make on the user's behalf without conscious sign-off: **dates, prices, payment terms, legal/contractual language.** These are exactly the commitments that turn a casual reply into a liability.

The guard is rendered as a four-row checklist with an explicit state per category, so the *absence* of a flag is as visible as its presence (an empty list would be ambiguous — "did it not check, or find nothing?"):

```
  Commitments guard                         all clear ✓
  ─────────────────
   ✓ Dates      No date/time committed.
   ✓ Prices     No figures or amounts.
   ✓ Payment    No payment terms.
   ✓ Legal      No contractual language.

  Looks safe to send as-is, but it's still your call.
```

When any category trips, the header badge flips to `⚠ N flags — please read`, the tripped rows sort to the top, and the row carries the **specific span** MailMate found ("~$4k", "next Tuesday") so the user can locate it in the compose body. Crucially the guard **never blocks** — it cannot, because the user owns the Send button. It informs. A flagged draft is still fully sendable the instant the user decides the figure is correct.

> **Guard semantics, stated plainly:** a flag means *"a human must consciously own this commitment,"* not *"this is wrong."* MailMate has no authority to assert a price is right; it only guarantees it will never let one slip past unannounced.

Today the host's `draft_reply` returns only an unstructured `safety_notes: [...]` array. To render the four-category guard deterministically (rather than regex-scraping note strings in JavaScript, which would be brittle and would re-implement guard logic client-side), the guard wants a *typed* breakdown from the host — see `commitments_guard` in the protocol additions. Until that lands, the panel degrades to listing `safety_notes` verbatim under a single "Please review" heading; the four-row layout is the target state once the structured field exists.

### Regenerate / Adjust

Two actions, deliberately distinct:

- **↻ Regenerate** — "try again, same instruction." Re-issues a draft request for the same thread; the new body replaces the compose body in place. Cheap, no typing.
- **✎ Adjust…** — "try again, *this* way." Expands an inline instruction box; the free-text steer is attached to a regenerated draft so the user can say "shorter," "drop the price," "more formal" without leaving the window.

```
┌──────────────────────────────────────────────────────┐
│  Adjust this draft                                    │
│  ─────────────────                                    │
│  Tell MailMate how to change it:                      │
│  ┌────────────────────────────────────────────────┐  │
│  │ shorter, and don't mention any dollar figure   │  │
│  └────────────────────────────────────────────────┘  │
│  Quick steers:                                        │
│   [ shorter ] [ warmer ] [ more formal ]              │
│   [ remove pricing ] [ remove dates ]                 │
│                                                       │
│  [ Cancel ]                       [ ↻ Regenerate ]    │
└──────────────────────────────────────────────────────┘
```

**Learning hook (one-click corrections).** When the user edits the compose body by hand and then sends (or just edits and closes), that divergence between MailMate's `body` and the final text is the single most valuable learning signal the product has. The panel fires a `record_user_action` capturing the `draft_id` and the user's resolution (`edit` / `regenerate` / `accepted-as-is`) so the learning loop sees it — corrections stay one interaction, never a form. The quick-steer chips ("remove pricing") double as labelled, structured corrections, which are far better training fuel than free text.

### Graceful degradation — no LLM provider configured

`zero_provider_by_default` is a first-class state, not an error. On popup open the panel reads `get_settings`; if `default_provider` is `null` **and** `providers` is empty, drafting cannot use a model, and the host's fallback produced (at best) a rule-templated draft or nothing. The panel says this **loudly and usefully** — the failure mode we are explicitly forbidden from is *failing silently*.

```
┌──────────────────────────────────────────────────────┐
│  MailMate · Draft reply                      [ ✕ ]   │
├──────────────────────────────────────────────────────┤
│  ⓘ  This is a DRAFT. MailMate never sends.           │
├──────────────────────────────────────────────────────┤
│   ⚡ Drafting needs an AI provider                     │
│   ──────────────────────────────                      │
│   No AI provider is configured, so MailMate can't     │
│   write a full reply right now. That's fine —         │
│   everything else (filing, follow-ups, learning)      │
│   keeps working without one.                          │
│                                                       │
│   What you can do:                                    │
│    • Write your reply yourself — the compose window   │
│      is ready and the recipient is filled in.         │
│    • Or add a provider to enable AI drafting.         │
│                                                       │
│   [ Open MailMate settings ]   [ Write it myself ]    │
└──────────────────────────────────────────────────────┘
```

When the host *did* return a thin rule-based draft (provider absent but a fallback template fired), the panel shows the body normally but stamps the rationale honestly and downgrades the guard's confidence language:

```
  Why this draft
  ──────────────
  ⚡ Drafted from a built-in template (no AI provider).
     This is a starting point, not a tailored reply.
     Expect to rewrite it.

  Commitments guard                    ⚠ unverified
  ─────────────────
   The template avoids dates, prices, payment and
   legal terms by design — but it wasn't AI-checked.
   Read the whole draft before sending.
```

The contract here: degraded drafting changes the *language and confidence* of the panel, never its safety guarantees. A no-provider draft is still never auto-sent, still never auto-applies anything, and still routes the user's edits back as learning signal.

### State summary

| Condition | Panel shows |
|---|---|
| Provider OK, guard all-clear | Rationale + green checklist + "your call" Send reminder |
| Provider OK, guard flags | Rationale + flagged rows (span-cited) sorted to top + ⚠ badge |
| Provider OK, user adjusting | Inline steer box + quick-steer chips → regenerated draft |
| No provider, template fallback | Honest "built-in template" rationale + "unverified" guard banner |
| No provider, nothing produced | "Drafting needs a provider" call-to-action (settings / write-it-myself) |

In every state the toolbar's own Send button is the only way mail leaves, and MailMate never touches it.

---

## Configuration

> **Surfaces:** `options_ui` (the full preferences page — configuration home) plus a thin **Quick toggles** strip mirrored into the toolbar `action` popup. **Read** path is the existing `get_settings`; **every write** crosses a *new* host request, because `get_settings` is read-only and `AppConfig`/`FileSecretStore` are off-protocol today.

### Design principles for this surface

1. **The host owns the truth; the extension owns nothing durable.** Settings live in the host's `AppConfig` (TOML) and `FileSecretStore` (0600). The extension is a *view + editor*, never a store. On open, the options page calls `get_settings` and renders the snapshot; on save, it sends a write request and **re-reads `get_settings`** to confirm the effective state (never trusts its own optimistic copy). This keeps a single source of truth and means a CLI/file edit and the UI never disagree.
2. **Secrets never touch extension storage.** The API key field is *write-only* from the UI's perspective: the user types a key, it is shipped in a `set_secret` request straight to the host's 0600 store, and the field then renders as `•••• set` from a boolean the snapshot carries (`configured: true`). The extension never persists, caches, or reads back the key — matching the secret-free contract of `get_settings`.
3. **Graceful degradation is a first-class state, not an error.** With zero providers (the default), the LLM section renders an explicit *"Drafting needs a provider — set one below"* banner and the relevant toggles are disabled-with-explanation, never silently broken. This mirrors `UnavailableProvider` host-side behavior.
4. **Pause is a host-side state, not a client flag.** "Pause MailMate" must survive an extension reload and must stop the *host* from applying auto-actions or firing follow-up drains — so it is a host setting (`paused: true`), surfaced as a snapshot field and flipped by a write request, not a `browser.storage` boolean the host can't see.
5. **Never auto-send / never auto-delete are not configurable.** No toggle in this page can enable send or delete. The per-category policy editor's strongest setting is **auto-apply** for SAFE actions (file/tag/mark-read) and only once crystallized; send/delete are absent from the action vocabulary entirely, shown as a greyed, locked footnote so the user understands it's a guarantee, not a missing feature.

### What reads via `get_settings` vs. what needs a new write

| Setting | Read source (today) | Write path (needed) |
|---|---|---|
| Retention level (`metadata`/`summaries`/`bodies`) | `get_settings.retention_level` | `set_settings` |
| Provider on/off + `default_provider` + provider list | `get_settings.default_provider`, `.providers[]` | `set_provider` |
| Provider endpoint/kind | *(snapshot has `.providers[].kind` only; `endpoint` is a read addition)* | `set_provider` |
| Provider **API key** | never read; `get_settings` surfaces only `configured` bool *(read addition)* | `set_secret` — straight to 0600 store |
| Follow-up cadence default (`follow_up_tick_seconds`) | `get_settings.follow_up_tick_seconds` | `set_settings` |
| Catch-up-on-launch | `get_settings.catch_up_on_launch` | `set_settings` |
| Per-category action policy (suggest / auto-apply-when-crystallized / off) | *(read addition — not in snapshot today)* | `set_category_policy` |
| Enable/disable categories | *(read addition — not in snapshot today)* | `set_category_policy` |
| Per-account scoping (which accounts MailMate triages) | *(read addition — not in snapshot today)* | `set_account_scope` |
| Global **Pause** | *(read addition — not in snapshot today)* | `set_pause` |
| Database path (read-only display) | `get_settings.database` | — (managed off-protocol; show, don't edit) |

The account list itself (ids + display names + addresses) is read from Thunderbird directly via `browser.accounts.list()` (the `accountsRead` permission is already in the manifest) — the host only stores the *triage decision* per account id, so the UI joins TB's account roster against the host's scope set.

### Preferences page layout (`options_ui`)

```
┌─ MailMate — Preferences ─────────────────────────────────────────────────────┐
│                                                                              │
│  ◉ STATUS                                                                    │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  MailMate is  ● ACTIVE        [ Pause MailMate ]                        │ │
│  │  Host: connected · v0.1.0     Paused stops auto-actions + follow-ups,  │ │
│  │  DB: ~/.config/mailmate/mailmate.db   leaves your mail untouched.      │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ◉ AI PROVIDER (for reply drafting & summaries)                             │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  ⚠ No provider configured — drafting & summaries are unavailable.       │ │
│  │     MailMate still classifies, files, tags and tracks follow-ups.       │ │
│  │                                                                         │ │
│  │  Provider     ( ) Off   (•) Local (Ollama/llama.cpp)   ( ) Remote      │ │
│  │  Endpoint     [ http://localhost:11434                              ]   │ │
│  │  Model / kind [ ollama                                              ]   │ │
│  │  API key      [ ••••••••  set ]   [ Replace ]   [ Clear ]               │ │
│  │               key is stored in the host's 0600 file, never the add-on. │ │
│  │  ⓘ Remote providers may send message snippets off your machine.        │ │
│  │                                            [ Test connection ]         │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ◉ PRIVACY — content retention                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  (•) Metadata only   subject/sender + structure, no body  (default)    │ │
│  │  ( ) Summaries       + redacted derived summaries                      │ │
│  │  ( ) Full bodies     + retained message bodies                         │ │
│  │  Higher levels enable better drafting/learning at more on-disk data.   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ◉ ACCOUNTS — which mailboxes MailMate triages                             │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  [x] nick@tgcs.com.au        (IMAP)      triaged                        │ │
│  │  [x] sales@tgcs.com.au       (IMAP)      triaged                        │ │
│  │  [ ] personal@gmail.com      (IMAP)      ignored — MailMate skips it    │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ◉ CATEGORIES & ACTION POLICY                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Category        On     Action when matched                            │ │
│  │  ───────────     ──     ──────────────────────────────────────────     │ │
│  │  Newsletters     [x]    ( ) Suggest only  (•) Auto-file when learned   │ │
│  │  Receipts        [x]    ( ) Suggest only  (•) Auto-file when learned   │ │
│  │  Spam / phishing [x]    (•) Suggest only  ( ) Auto-file when learned   │ │
│  │  Sales leads     [x]    (•) Suggest only  ( ) Auto-file when learned   │ │
│  │  Internal        [ ]    — disabled —                                    │ │
│  │  ────────────────────────────────────────────────────────────────────  │ │
│  │  "Auto-file when learned" only applies SAFE actions (file / tag /      │ │
│  │  mark-read) and only after the behavior crystallizes into a rule.      │ │
│  │  🔒 Sending and deleting are never automated — guaranteed, not a        │ │
│  │     setting.                                            (always with ↶) │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ◉ FOLLOW-UPS                                                               │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Default cadence    [ 3 ] days between nudges                          │ │
│  │  Scheduler tick     [ 60 ] seconds   (catch-up sweep on launch: [x])   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  [ Discard ]                                                  [ Save changes ]│
│  Settings are stored by the MailMate host. Saving re-reads the live state.  │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Quick toggles in the toolbar `action` popup

The toolbar popup is the mini-hub (counts, links). It carries only the **high-frequency, low-risk** controls — the rest deep-links into the options page. Crucially, Pause lives here so it's one click from anywhere:

```
┌─ MailMate ───────────────────────────┐
│  ● Active        12 to review  ▸      │
│  ───────────────────────────────────  │
│  [ ⏸ Pause MailMate ]                 │   → set_pause {paused:true}
│                                       │
│  Provider:  ⚠ none      [ Set up ▸ ]  │   → opens options_ui (runtime.openOptionsPage)
│  Retention: Metadata    [ Change ▸ ]  │   → opens options_ui (anchored to Privacy)
│  ───────────────────────────────────  │
│  Open dashboard ▸   ·   Preferences ▸ │
└───────────────────────────────────────┘
```

When paused, the toolbar `action` badge text changes to `⏸` and the popup's first line flips to `❚❚ Paused — [ Resume ]`. Because pause is host state, this reflects correctly even on a fresh browser session (the popup reads `get_settings.paused` on open).

### Save semantics & failure handling

- **Optimistic-then-confirm.** On Save, the page batches each changed group into its typed write (`set_settings`, `set_provider`, `set_category_policy`, `set_account_scope`, `set_pause`, `set_secret`), awaits all, then issues one `get_settings` and re-renders from the authoritative snapshot. If any write rejects (host error frame), that group reverts to the snapshot value and shows an inline error; other groups still commit. No partial-UI lies.
- **Secret writes are isolated.** `set_secret` is sent on its own (never bundled), succeeds or fails independently, and on success the page sets the field to `•••• set` from the returned `configured: true` — it never echoes the key. A `Clear` sends `set_secret` with an empty value to delete the store entry.
- **Validation is host-authoritative.** Cadence/tick bounds, retention enum, and category/policy validity are validated host-side; the UI shows ranges as hints but trusts the host's rejection reason rather than duplicating policy.

---

## Notifications & the Time-Based Layer

MailMate's work is mostly *asynchronous and time-driven*: follow-up steps fire on a cadence tick, items go stale past their abandon horizon, and the curator promotes a behavior into a proposal long after the corrections that fueled it. Thunderbird gives us no inbox banner or sidebar to surface these in place, so the desktop **`notifications` API** is the only ambient channel that reaches the user when MailMate has something time-sensitive to say. This layer is a *transport*, not a source of truth: every notification is a thin, deep-linking pointer into the dashboard tab that owns the real interaction. The host already emits the two follow-up signals we need (`followup_draft_ready`, `followup_needs_attention`); the only genuinely new wire signal is "a proposal is ready to review," because the curator runs host-side on its own schedule with no existing push.

### Design principles

1. **Notifications are pointers, never actions.** A click only ever *opens the right dashboard tab, focused on the right row*. We never auto-send, never auto-apply, and never resolve a follow-up from the toast — that all lives behind the in-dashboard review surfaces (consistent with `never_auto_send_drafts` / `never_auto_delete`).
2. **One source of truth = the dashboard; the toast is disposable.** If the user ignores or misses a toast, nothing is lost: the Review / Follow-ups / Proposals tabs and the toolbar action badge still carry the count. This lets us be aggressive about dedup and quiet-hours suppression without fear of dropping work.
3. **Quiet by default.** Three notification *classes*, each independently toggleable in `options_ui`, all defaulting to a sane low-noise posture (drafts and proposals = on but batched; needs-attention = on, immediate).
4. **Provider-off degrades, never fails.** When a `followup_draft_ready` arrives but no provider is configured, the draft body is empty/fallback — the toast says so plainly ("drafting needs a provider") and deep-links to the Follow-up row where the user can write the reply by hand.

### The three notification classes

| Class | Maps to host signal | Default cadence | Deep-link target | Click action |
|---|---|---|---|---|
| **Follow-up draft ready** | `followup_draft_ready` (existing) | Batched (coalesced, debounced ~90s) | Dashboard → **Follow-ups** tab, scrolled to `workflow_instance_id` | Open draft review row |
| **Follow-up needs attention** | `followup_needs_attention` (existing) | Immediate (it's an exception) | Dashboard → **Follow-ups** tab → *Needs attention* filter, `workflow_instance_id` | Open stale-item resolver |
| **New proposal ready** | `proposal_ready` (**NEW** — see additions) | Batched (digest, max 1/quiet-window) | Dashboard → **Proposals** tab, `proposal_id` | Open proposal review |

There is deliberately **no** desktop notification for routine `classification_ready` on background mail. That fires on essentially every inbox arrival; turning it into toasts would be a notification firehose. It is surfaced silently in the **Review** tab queue and the toolbar action badge count instead — the user pulls it when they choose. (This closes the gap where `classification_ready` results are logged to console only, without replacing one bad surface with an annoying one.)

### Notification anatomy & deep-linking

Every toast is built from a host signal, carries a stable `notification_id` (for dedup + click correlation), and stuffs its deep-link target into the click handler. Thunderbird's `notifications.onClicked` gives us the notification id; we map it back to the queued payload and call `spaces.open()` (with the persisted integer `space.id`) + `tabs` to focus the MailMate space, then post a `{focus: {tab, anchor_id}}` message to the dashboard tab.

```
                  host notification frame
                  (followup_draft_ready / _needs_attention / proposal_ready)
                            │
                            ▼
                ┌───────────────────────────┐
                │  background.js dispatcher  │
                │  • class? toggle on?       │
                │  • quiet hours?  ─► hold   │
                │  • dedup key seen? ─► drop │
                │  • batch window? ─► coalesce│
                └───────────┬───────────────┘
                            │ browser.notifications.create(id, …)
                            ▼
                    ┌──────────────────┐
                    │  desktop toast   │
                    └────────┬─────────┘
                  onClicked(id)│
                            ▼
        spaces.open(spaceId) → tabs.update(active)
        → postMessage {focus:{tab:"follow-ups", anchor:"wfi_001"}}
```

### Example copy

**Follow-up draft ready** (single):

```
┌────────────────────────────────────────────┐
│  MailMate · Follow-up ready to review       │
│                                             │
│  Day-14 nudge drafted for the Acme quote    │
│  (no reply since day 7). Review before send.│
│                                  [ click ]  │
└────────────────────────────────────────────┘
        → Follow-ups tab, row wfi_001
```

**Follow-up draft ready** (batched / coalesced — multiple fired within the debounce window):

```
┌────────────────────────────────────────────┐
│  MailMate · 3 follow-ups ready              │
│                                             │
│  Acme quote, Bryce proposal, +1 more are    │
│  drafted and waiting for your review.       │
│                                  [ click ]  │
└────────────────────────────────────────────┘
        → Follow-ups tab (default sort: just-fired)
```

**Follow-up draft ready — provider off** (graceful degrade; `default_provider == null`):

```
┌────────────────────────────────────────────┐
│  MailMate · Follow-up due — needs you       │
│                                             │
│  The Acme quote is due for a day-14 nudge.  │
│  Drafting needs an AI provider; write it    │
│  yourself or add a provider in Settings.    │
│                                  [ click ]  │
└────────────────────────────────────────────┘
        → Follow-ups tab, row wfi_001 (manual-compose mode)
```

**Follow-up needs attention** (immediate, exception path):

```
┌────────────────────────────────────────────┐
│  MailMate · Follow-up went stale            │
│                                             │
│  The Bryce proposal passed its follow-up    │
│  window with no reply. Close it won/lost,   │
│  or snooze.                                 │
│                                  [ click ]  │
└────────────────────────────────────────────┘
        → Follow-ups tab · Needs-attention filter, wfi_002
```

**New proposal ready** (batched digest):

```
┌────────────────────────────────────────────┐
│  MailMate · 2 new rule proposals            │
│                                             │
│  MailMate learned patterns it wants to      │
│  confirm before automating (e.g. "file      │
│  Stripe receipts → Receipts"). Review?      │
│                                  [ click ]  │
└────────────────────────────────────────────┘
        → Proposals tab (PendingReview queue)
```

Copy rules: name the *thing* (Acme quote), name the *because* (no reply since day 7), name the *one decision* (review / close / snooze) — and never imply the action already happened. "Drafted", "due", "wants to confirm" — never "sent", "filed", "automated".

### Dedup, quiet hours, and batching

These three behaviors live entirely in the extension's background dispatcher, keyed off fields already present in the existing payloads. No host changes are needed for dedup/quiet/batch — only for the new proposal *signal*.

**Dedup.** Each toast has a deterministic dedup key; a re-fire with the same key *replaces* the existing toast (via `notifications.create` with the same id) rather than stacking a duplicate. This matters because the host's catch-up-on-launch can re-emit `followup_draft_ready` for steps fired while offline.

| Class | Dedup key | Why |
|---|---|---|
| `followup_draft_ready` | `wfi:{workflow_instance_id}:step:{step_index}` | Coalesced re-fires (catch-up, `coalesced_from_step_indexes`) collapse to one toast for the latest step |
| `followup_needs_attention` | `wfi:{workflow_instance_id}:stale` | One stale toast per instance, ever — not per skipped step |
| `proposal_ready` | `proposal:{proposal_id}` | Curator may re-surface the same pending proposal on restart |

A 24h LRU of seen keys (in `storage.local`) survives event-page suspension so a relaunch doesn't re-toast everything the user already saw.

**Quiet hours.** A user-configured window (default **21:00–08:00**, plus an explicit *Pause MailMate* toggle from the toolbar action popup that hard-mutes all classes). During quiet hours:

- `followup_draft_ready` and `proposal_ready` are **held** — their dedup keys accumulate in a pending set; at the end of the window they fire as a **single batched digest per class** ("4 follow-ups and 2 proposals are waiting"). Nothing is lost because the dashboard counts already reflect them.
- `followup_needs_attention` is **held but not dropped**, and also fires as part of the wake-up digest — it's an exception, but a stale deal at 2am is not worth waking anyone.
- *Pause* (manual) suppresses toasts with **no** wake-up digest until un-paused; the badge counts still climb.

**Batching / debounce.** To avoid a toast storm when many steps fire on one cadence tick (or on catch-up), each batchable class has a short debounce window:

```
followup_draft_ready ── debounce 90s ──► if 1 pending: single-item copy
                                          if >1:        "N follow-ups ready"
proposal_ready ──────── debounce 5min ─► always digest copy ("N proposals")
followup_needs_attention ── no debounce ─► immediate, but ≤1 per instance (dedup)
```

The batch window resets on each new arrival (trailing debounce), so a burst of 12 fired steps yields exactly one "12 follow-ups ready" toast ~90s after the last one lands. Catch-up-on-launch (which can dump a backlog) is treated as one batch regardless of size.

### State the dispatcher owns

```
storage.local["notif.seenKeys"]   : { key: lastShownTs }  (24h LRU, dedup)
storage.local["notif.pending"]    : { followup:[…], proposal:[…], attention:[…] }  (quiet-hours hold + debounce buffer)
storage.local["notif.prefs"]      : { draftsEnabled, attentionEnabled, proposalsEnabled,
                                       quietStart, quietEnd, paused }   (mirror of options_ui)
notifications.onClicked → seenKeys[id].deepLink → spaces.open + tabs.update + postMessage(focus)
```

`notif.prefs` is the single mirror of the four notification toggles + quiet window configured in `options_ui` (the preferences home). The toolbar action popup's **Pause** writes the host-side `set_pause` state; this dispatcher mirror is kept in sync from `get_settings.paused`, so the mini-hub and the dispatcher never disagree.

---

## Action Model: "Crystallized-Auto, Rest Suggest"

MailMate takes a safe action on your mail **by itself** only after it has *learned* that action from you and the learning has hardened into a deterministic rule. Until then, the same action is offered as a **suggestion** you approve in one click. There is exactly one promotion event that flips an action from "suggest" to "auto": the **crystallization gate** described in *Learning Loop → Crystallization* (architecture.md). The gate that promotes a learned trait into an `active` rule **is** the auto-apply gate — there is no second, parallel "should I automate this?" decision to get out of sync.

This section defines the two lifecycles, the precise gate condition, the Undo contract, how a dismissal/correction feeds learning, and the visual language that keeps "MailMate did this (because rule X)" unmistakably distinct from "MailMate suggests this."

### The two lifecycles

Every safe action a message produces lands on exactly one of two tracks, decided at classification time by whether an **`active` learned rule** authored it.

**Track A — Suggested (not yet crystallized).** No active rule covers this trait yet. The action is a *proposal*. MailMate changes nothing in your mailbox; it surfaces the suggestion and waits for you.

```
suggested ──approve──▶ applied ──(no auto-undo; ordinary mail undo)
    │
    ├──dismiss────▶ dismissed   (records "ignored" → learning evidence)
    │
    └──correct────▶ corrected   (you did it differently → strong learning signal)
```

**Track B — Crystallized-auto (an active rule fired).** A learned `active` rule authored this action and the policy guard returned `allowed`. MailMate **applies it immediately**, then shows it as *already done* with a live **Undo** for a bounded window.

```
crystallized ──▶ auto-applied ──┬──undo-window-expires──▶ settled
   (rule X)                      │
                                 └──undo (within window)──▶ reverted
                                       (records "undone" → strong negative signal on rule X)
```

A **policy guard veto always wins.** Even if an active rule fires, an action the guard marks `requires_review` or `blocked` (never auto-send, never auto-delete, `financial_security_legal_move_requires_review`) is **demoted back to Track A** (a suggestion) or dropped. Crystallization earns auto-apply; it never overrides policy.

### How the rule promotion gate decides auto-apply

Auto-apply is **not** a UI threshold and **not** "policy said allowed." It is precisely the condition that an action was authored by a rule in lifecycle status `active`. That status is reached only by passing the four-step crystallization gate:

1. **Expressible deterministically** — the trait was emitted as a JSON-AST condition over deterministic features only (no model clause).
2. **Back-tested** — replayed over your recorded past decisions, it reproduced them at the historical **precision bar** with enough **support**. (Low precision ⇒ never crystallizes ⇒ stays a suggestion forever; MailMate does not fake determinism.)
3. **Promoted through the Rule Lifecycle** — `draft → pending_human_review → shadow_mode → active`, with the human-approval edge being a **Proposal** the user accepts on the dashboard's **Proposals** tab.
4. **Leaves the model path** — from activation, the trait is decided by AST evaluation, zero inference, and it **outranks any model prediction** (Rule Hierarchy).

So the user-facing promise is concrete: *MailMate starts doing a thing automatically only after (a) it watched you do it enough times that a deterministic rule reproduced your own past choices above the bar, and (b) you approved that rule on the Proposals tab.* Shadow mode is the dress rehearsal — a `shadow_mode` rule's authored action stays a **suggestion** (it records what it *would* have done in `shadow_outcomes`) and is visually badged "shadow" so the user can see a soon-to-be-automatic behavior before granting it autonomy.

This means the existing host `new_mail` path needs one tightening (see *Host protocol additions*): today it auto-applies **every** policy-`allowed` low-risk action. Under this action model, auto-apply must be additionally conditioned on **authored-by-`active`-rule**; a policy-`allowed` action with **no** active rule behind it is a *suggestion*, surfaced in `review_required_actions`, not silently applied.

### What carries this on the wire

| Moment | Host request / notification | Track |
|---|---|---|
| Background arrival classified | `new_mail` → **`classification_ready`** notification | `applied_actions[]` = Track B (auto-applied, each now carrying `rule_id`); `review_required_actions[]` = Track A (suggestions) |
| User selects a message to read | `classify_message` (alias `read_message`) | `suggested_actions[]` partitioned by `authored_by` (active rule vs model) |
| User approves a suggestion | extension applies via MailExtension APIs, then `record_user_action {event_type:"action_applied"}` | A → applied |
| User dismisses a suggestion | **`record_user_action {event_type:"suggestion_dismissed"}`** *(new)* | A → dismissed (`ignored` evidence) |
| User corrects (moves/junks differently) | `record_user_action {event_type:"message_moved"\|"junk_changed", user_initiated:true}` | A/B → corrected |
| User clicks **Undo** on an auto-applied action | extension reverses via APIs, then **`record_user_action {event_type:"action_undone", rule_id}`** *(new)* | B → reverted (`undone` evidence vs rule X) |
| Explain "why did this happen?" | `explain_decision` | renders the audit timeline behind either badge |

The `applied`/`undone`/`ignored`/`edited` outcome vocabulary already exists in the learning loop; these events feed it directly.

### What Undo does, and its window

Undo is the safety net that makes auto-apply acceptable. Because the only auto-applicable actions are **SAFE and reversible by construction** (file-to-folder, tag, mark-read — never send, never delete), Undo is always a clean inverse:

- **move** → move back to the original folder (the original `folder_id` is captured in `applied_actions[]`).
- **tag** → remove the tag MailMate added (only that key; preserves user tags).
- **mark_read / flag** → restore the prior read/flag state.

**Window.** Each auto-applied action shows Undo for a **default 30-second** live countdown in the message header panel and the toolbar action popup, and remains undoable for **24 hours** from the **Activity** tab (the longer window costs nothing because the inverse is always available and the action is in the audit timeline). The 30s figure is a setting (`undo_window_seconds`) on the options page. Undo must reverse the mail change **and** emit `action_undone` so the correction reaches learning — a silent client-only revert would let a misfiring rule keep firing.

**Undo is itself a teaching event.** An undo on a rule-authored action is the strongest negative signal a learned rule can get. It writes `undone` evidence keyed to `rule_id`; the rule's derived `RuleOutcome` view (undo rate) is what the curator watches to recommend refine/retire. A rule that gets undone enough is proposed for retirement — auto-apply is *revocable by your behavior*, not just by a settings toggle.

### How dismissal and correction feed learning

Corrections are the fuel and must be **one click** (hard constraint). Three distinct signals, three sinks (the single-writer discipline: corrections → feedback tables, provenance → audit):

- **Dismiss a suggestion** (Track A): the suggestion was wrong-enough-to-ignore. `suggestion_dismissed` → `ignored` evidence. Repeated ignores push a candidate rule's precision *down* in back-test, keeping it out of crystallization. Routed by `record_user_action` to `classification_feedback`/`filing_feedback`.
- **Correct** (either track): you did the *right* thing yourself (moved to a different folder, marked ham). This is positive evidence for the trait you *actually* want and negative for the one suggested. Rides the existing `message_moved` / `junk_changed` `record_user_action` events. (The `onMoved`-echo suppression already in `drafts.js`/`background.js` guarantees a host-applied auto-move is **not** mis-recorded as a user correction — critical so Track B's own actions don't poison the signal.)
- **Undo** (Track B): see above — `undone` against `rule_id`.

The dashboard's **Activity (explain)** tab renders `explain_decision` (focused per-message) and `list_recent_activity` (the cross-message stream) so any single auto-apply or suggestion can be opened to its full timeline (classification → which rule → policy checks → your correction), closing the "why did MailMate do that?" loop.

### Visual language: "did it" vs "suggests it"

The two tracks must never be confusable. The distinction is carried by **badge, color, verb tense, and the presence of Undo vs Approve**.

| | **Track B — auto-applied** | **Track A — suggested** |
|---|---|---|
| Badge | ● **Auto** (filled dot) | ○ **Suggested** (hollow dot) |
| Accent color | calm green/teal (done) | amber (awaiting you) |
| Verb tense | **past** — "Filed to *Receipts*" | **future/imperative** — "File to *Receipts*?" |
| Attribution | "because **rule: receipts-from-stripe**" (links to Explain) | "matched pattern (not yet a rule)" |
| Primary control | **Undo** (+ live countdown) | **Approve** / **Dismiss** |
| State in mailbox | already changed | unchanged |

#### Mockup — Track B (crystallized-auto, in the messageDisplayAction header panel)

```
┌──────────────────────────────────────────────────────────────┐
│  MailMate                                          ● Auto      │
├──────────────────────────────────────────────────────────────┤
│  ✓ Filed to  Receipts                                         │
│  ✓ Tagged    receipt                                          │
│       because rule: receipts-from-stripe  ·  why? ↗           │
│                                                                │
│   ┌─────────────────────────────────────────────────────┐    │
│   │  ⟲ Undo            auto-reverts nothing · 0:27 left  │    │
│   └─────────────────────────────────────────────────────┘    │
│                                                                │
│  Priority: normal · spam 2% · phishing 0%                      │
└──────────────────────────────────────────────────────────────┘
```

#### Mockup — Track A (suggested, same panel, different message)

```
┌──────────────────────────────────────────────────────────────┐
│  MailMate                                       ○ Suggested    │
├──────────────────────────────────────────────────────────────┤
│  MailMate suggests:                                            │
│    →  File to  Vendors/Acme ?        matched pattern           │
│    →  Tag      quote ?               (not yet a rule)          │
│                                                                │
│   ┌────────────────┐   ┌────────────────┐                     │
│   │   ✓ Approve     │   │   ✕ Dismiss     │                    │
│   └────────────────┘   └────────────────┘                     │
│                                                                │
│  ⚠ Move to Banking held for review (financial → needs you)     │
│  Priority: high · spam 1% · phishing 0%      why? ↗            │
└──────────────────────────────────────────────────────────────┘
```

The `⚠ ... held for review` line is how a policy-`requires_review`/`blocked` action shows up — never a button that could auto-fire, always an explicit "needs you." A `shadow_mode` rule's would-be action appears in the Track A panel with an extra `◐ shadow` chip and the copy "*learning — will auto-apply once approved*," giving the user a preview of an about-to-be-automatic behavior and a fast path to the Proposals tab to grant or deny it.

### State diagram

```
                         classify_message / new_mail
                                    │
                     ┌──────────────┴───────────────┐
            authored by ACTIVE rule?         no active rule
            (+ policy = allowed)             (or shadow_mode)
                     │                              │
              ┌──────▼──────┐                ┌──────▼──────┐
              │ CRYSTALLIZED│                │  SUGGESTED  │ ●○ amber
              └──────┬──────┘                └──┬───────┬──┘
   policy=allowed────┤                  approve │       │ dismiss
                     ▼                          ▼       ▼
              ┌─────────────┐            ┌─────────┐ ┌──────────┐
              │AUTO-APPLIED │ ● green    │ APPLIED │ │DISMISSED │
              │ +Undo(30s/  │            └─────────┘ └────┬─────┘
              │   24h)      │                 │           │ ignored
              └──┬───────┬──┘            (ordinary        ▼
        undo ────┤       └──── window    mail undo)   [learning:
                 ▼            expires                  suppress
          ┌──────────┐         ▼                       candidate]
          │ REVERTED │   ┌──────────┐
          └────┬─────┘   │ SETTLED  │      ── correction (either track) ──▶
   undone vs   │         └──────────┘         record_user_action
   rule_id ────┘                              (message_moved / junk_changed)
        │                                              │
        ▼                                              ▼
   [learning: undo-rate ↑ on rule X            [learning: positive evidence
    → curator: refine/retire]                   for the trait you chose]

  policy guard veto (requires_review / blocked) at any branch
     └─▶ demote to SUGGESTED  ·  never auto-send  ·  never auto-delete
```

---

## First-Run, Onboarding, Connection Health & Empty States

> **Context header:** This surface is the user's *first thirty seconds* and their *recovery path* when the spine is down. Two failure modes dominate today and both are currently invisible: (1) the native host is not connected, so nothing works and the user sees silence; (2) a fresh install has **zero crystallized rules**, so *everything* is a suggestion and a user who expected "magic auto-filing" thinks MailMate is broken. This section makes both states legible and friendly, and it grants nothing dangerous on the way in.

Design principle (Dalio-flavored, matching the rest of the doc): **surface reality, even when reality is "not connected yet" or "I haven't learned anything yet."** Silence is the bug. Every state below has an explicit, honest rendering.

### 1. The handshake is the foundation of every status surface

The toolbar badge, the dashboard banner, and the per-message panel all derive from **one piece of truth**: a periodic handshake with the host. Today `ping` exists but returns only `{ pong: true, echo }` — enough to prove the *channel* is alive, but it tells the UI nothing about *version skew*, *retention level*, or *whether drafting is available*. So onboarding and health both lean on a slightly richer handshake (`hello`, defined in the additions) plus the existing `get_settings`.

The extension maintains a single in-memory `HostStatus` that every surface reads:

```
HostStatus = {
  phase:        "connecting" | "ready" | "disconnected" | "version_mismatch",
  lastPongAt:   epoch_ms | null,     // freshness; drives "stale" detection
  protocol:     "1.0" | null,        // from hello; compared to extension's PROTOCOL_VERSION
  retention:    "metadata" | "summaries" | "bodies" | null,   // from get_settings
  drafting:     "available" | "no_provider",                  // default_provider == null → no_provider
  onboarded:    bool                  // local extension storage flag, NOT host state
}
```

`onboarded` lives in `browser.storage.local`, not the host — onboarding is a UI-completion fact, and a brand-new profile that has never run the walkthrough must show it even if the host is healthy.

**Where `HostStatus` comes from, concretely:**
- `native.js` already opens the port in its constructor and already has `port.onDisconnect`. Today that handler only `console.warn`s. We extend it to set `phase = "disconnected"` and broadcast a `status-changed` event the surfaces subscribe to.
- On connect, the extension sends `hello` (new) then `get_settings` (exists). `hello` failing/timing out within ~1.5s → `phase = "disconnected"`; protocol mismatch → `version_mismatch`.
- A lightweight heartbeat (reuse `ping` every 30s) updates `lastPongAt`. If a `ping` round-trip exceeds the timeout, the port is treated as stale and we attempt one reconnect before declaring `disconnected`.

### 2. Connection health — making the invisible failure visible

This is the load-bearing part of the section. A MailExtension native port can die for ordinary reasons (host binary missing, manifest path wrong under the Thunderbird snap confinement, host panicked, version skew after an update). Today the only evidence is a console line the user never sees.

**Toolbar `action` badge** is the always-on indicator. The badge text/color is a pure function of `HostStatus.phase`:

```
 ready           → no badge tint; count of pending reviews as badge text (e.g. "3")
 connecting      → badge "…"  (neutral grey)
 disconnected    → badge "!"  (red)   + action popup opens straight to the recovery card
 version_mismatch→ badge "!"  (amber) + popup explains "update one side"
```

Because there is **no inbox-banner API** (per the constraints), the disconnected truth is carried by (a) the red toolbar badge, (b) the action-popup recovery card, (c) a banner pinned to the top of the dashboard space, and (d) a disabled/greyed state on the per-message panel. All four read the same `HostStatus`, so they can never disagree.

**Disconnected banner (top of the MailMate dashboard space):**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ●  MailMate is not connected to its assistant                        [ Retry ]│
│  ⚠                                                                             │
│  The MailMate background helper (native host) isn't responding, so new mail    │
│  won't be classified and no suggestions will appear. Your mail is unaffected.  │
│                                                                                │
│  Last seen:  2 min ago            Reason:  host process exited (code 1)        │
│                                                                                │
│  Try this:                                                                     │
│    1. Click Retry to reconnect.                                                │
│    2. If that fails, the helper may not be installed for this profile.         │
│       → Open “How to reconnect”  (opens options_ui troubleshooting page)       │
│                                                                                │
│                              [ Retry ]   [ How to reconnect ]   [ Dismiss ]    │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes that make this honest and safe:
- **"Your mail is unaffected."** is non-negotiable copy. The user must know MailMate failing never endangers their mailbox — it just goes quiet.
- **Reason** is taken from `port.error.message` (already captured in `native.js`). We render the raw reason in a muted line; most users ignore it, but it is gold for the "How to reconnect" support flow and for diagnosing the snap-confinement path bug noted in project memory.
- **Retry** calls `connectNative()` again and re-runs the `hello`/`get_settings` handshake. A successful reconnect collapses the banner to a 3-second green "Reconnected" toast, never a silent disappearance.
- **Last seen** is `lastPongAt` rendered relative. If we never connected at all this run, it reads `never this session` and the copy shifts to first-install guidance ("the helper may not be installed yet — finish setup").

**Action-popup recovery card (mini-hub when disconnected):**

```
┌───────────────────────────────┐
│ MailMate            ● offline  │
├───────────────────────────────┤
│  Not connected to the helper. │
│  New mail isn't being read.   │
│                               │
│        [ ↻ Retry now ]        │
│                               │
│  Still stuck?                 │
│  [ Open setup & help → ]      │
└───────────────────────────────┘
```

**Version-mismatch** is a distinct, calmer state (amber, not red) because mail is safe and the fix is deterministic: "MailMate's extension (1.0) and helper (0.9) don't match. Update whichever is older." We deliberately *do not* attempt to speak a mismatched protocol — a `hello` that disagrees on `protocol_version` short-circuits before any `classify_message` is sent, preventing garbled frames on the single-writer stdout channel.

### 3. First-run walkthrough — grant nothing dangerous

Triggered when `onboarded == false` (storage flag). It opens **in the dashboard space tab**, not a modal, so the user keeps full Thunderbird around it. Four short steps; **every step is skippable and nothing in onboarding sends, deletes, or moves a single message.** Onboarding only *reads* status and *optionally writes config*.

**Step 1 — What MailMate does (and the safety floor):**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  MailMate                                                          ●  Step 1/4 │
│  ──────────────────────────────────────────────────────────────────────────── │
│                                                                                │
│        Your local-first mail assistant. Everything runs on this machine.       │
│                                                                                │
│   What it will do for you:                                                     │
│     •  Read incoming mail and suggest how to file, tag, or prioritize it.      │
│     •  Learn from your corrections — one click teaches it.                     │
│     •  Draft replies for you to review (only if you turn on a provider).       │
│                                                                                │
│   What it will NEVER do:                                                       │
│     ✕  Send a message on your behalf — drafts always wait for you.             │
│     ✕  Delete mail — ever. A hard policy guard forbids it.                     │
│                                                                                │
│   How it behaves while it's still learning:                                    │
│     →  At first, MailMate only SUGGESTS. You approve each action.              │
│     →  Once it has watched you do the same safe thing enough times, it asks    │
│        to handle that one thing automatically — and even then, with one-tap    │
│        Undo, and never for sending or deleting.                                │
│                                                                                │
│                                              [ Skip setup ]      [ Continue → ]│
└──────────────────────────────────────────────────────────────────────────────┘
```

This step is where the **"crystallized-auto, rest suggest"** model is taught in plain language. The phrase "watched you do the same safe thing enough times" is the user-facing translation of the crystallization promotion gate — we never auto-apply until a behavior crystallizes into a learned rule, and we say so up front so the *later* "MailMate can now auto-file this — OK?" prompt feels earned, not creepy.

**Step 2 — Connect the helper (the handshake, live):**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  MailMate                                                          ●  Step 2/4 │
│  ──────────────────────────────────────────────────────────────────────────── │
│                                                                                │
│   Checking the connection to MailMate's background helper…                     │
│                                                                                │
│        ┌─────────────────────────────────────────────────────────────┐        │
│        │  ●  Connected            helper v1.0   ·   protocol 1.0       │        │
│        │  ✓  Handshake OK         retention: metadata (no message      │        │
│        │                          bodies stored)                       │        │
│        └─────────────────────────────────────────────────────────────┘        │
│                                                                                │
│   Good — the helper is responding. MailMate is ready to start watching         │
│   your mail and making suggestions.                                            │
│                                                                                │
│                                              [ ← Back ]          [ Continue → ]│
└──────────────────────────────────────────────────────────────────────────────┘
```

If the handshake *fails* here, Step 2 swaps to the recovery card inline (same component as §2) — onboarding cannot proceed past a dead host into a UI that would only show "nothing happening." It shows the recovery card and a "Retry" and lets the user finish the *rest* of onboarding (which is informational/config) but flags that classification will begin once the helper connects. This is the single most important coupling in the section: **first-run is the first place a broken host gets caught, instead of failing silently after install.**

**Step 3 — Drafting & providers (graceful "off by default"):**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  MailMate                                                          ●  Step 3/4 │
│  ──────────────────────────────────────────────────────────────────────────── │
│                                                                                │
│   Reply drafting (optional)                                                    │
│                                                                                │
│   MailMate can draft replies for you to review. This needs an AI provider,     │
│   which is OFF by default — MailMate works fully without one.                  │
│                                                                                │
│        Current status:   ◌  No provider configured                            │
│                                                                                │
│   With no provider, MailMate still classifies, files, tags, prioritizes,       │
│   and tracks follow-ups. Only the “draft a reply” feature waits — when you     │
│   ask for a draft it will say “drafting needs a provider” instead of failing.  │
│                                                                                │
│     ( ) Keep drafting off for now  (recommended to start)                      │
│     ( ) I'll add a provider in Settings later                                  │
│                                                                                │
│                                              [ ← Back ]          [ Continue → ]│
└──────────────────────────────────────────────────────────────────────────────┘
```

We deliberately **do not collect an API key in the onboarding tab.** Secrets are written to the `0600` config/secret store via the dedicated `set_secret` write in `options_ui`, so onboarding points at Settings ("add a provider in Settings later") rather than handling a credential in first-run. This keeps the dangerous-credential surface out of the walkthrough entirely. The radio choice is purely informational/expectation-setting; it sets no secret.

**Step 4 — Done / what to expect next:**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  MailMate                                                          ●  Step 4/4 │
│  ──────────────────────────────────────────────────────────────────────────── │
│                                                                                │
│        ✓  You're set up. MailMate is now watching for new mail.                │
│                                                                                │
│   What happens now:                                                            │
│     •  New mail gets a small MailMate panel in its header — open it to see     │
│        the suggestion and correct it with one click.                           │
│     •  The MailMate space (left toolbar) is your dashboard: Review,            │
│        Follow-ups, Proposals, and Activity.                                    │
│     •  Right now everything is a SUGGESTION. As you correct it, MailMate       │
│        learns — and will ask before handling anything on its own.              │
│                                                                                │
│   Tip: the toolbar icon shows how many items are waiting for you.              │
│                                                                                │
│                                          [ Open the dashboard ]   [ Finish ]   │
└──────────────────────────────────────────────────────────────────────────────┘
```

On Finish we set `onboarded = true` in `storage.local`. There is **no "grant body access" toggle** in onboarding: retention starts at `metadata` (the doc default) and stays there. Turning on `summaries`/`bodies` is a deliberate, separate decision in `options_ui` with its own explanation — onboarding never nudges the user toward storing more of their mail than the safe default.

### 4. Empty states — a fresh install with no rules is *correct*, not broken

The defining first-week experience: **zero crystallized rules → everything suggests.** If the dashboard tabs render as bleak blank panes, the user concludes MailMate is dead. Each tab gets a purpose-built empty state that frames emptiness as the expected starting point and points at the one action that moves things forward. (The Review / Follow-ups / Proposals / Activity empty-state cards are specified in the *Dashboard space* section above.) Two onboarding-specific empty states deserve calling out here:

**Dashboard › Proposals tab — the "still learning" state (the most important empty state):**

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Review            Follow-ups            Proposals            Activity         │
│ ────────────────────────────────────────────────────────────────────────────  │
│                                                                                │
│                     ◌  MailMate hasn't proposed any rules yet                  │
│                                                                                │
│      This is normal for a new install. MailMate watches how you file, tag,     │
│      and prioritize. Once it's confident it has spotted a pattern (e.g.        │
│      “invoices from Acme → Finance folder”), it proposes a rule here for       │
│      your approval — and only then can it start doing that one thing for you.  │
│                                                                                │
│      Until then, everything stays a suggestion you approve. Nothing is         │
│      automatic yet.                                                            │
│                                                                                │
│              Progress:  ▱▱▱▱▱  0 patterns near the learning threshold          │
│                                                                                │
└──────────────────────────────────────────────────────────────────────────────┘
```

That progress bar is the antidote to "is this thing even learning?" It reads from the same proposal/evidence pipeline that feeds `list_pending_reviews`; when it's empty, we still show *how close* the nearest pattern is to crystallizing, so the user sees motion. (This needs a tiny read-only addition — `learning_progress`, below — because `list_pending_reviews` only returns *already-pending* proposals, not near-threshold ones.)

**Per-message panel — empty / not-yet-classified (in the `messageDisplayAction` popup):**
For a message MailMate hasn't processed (e.g. arrived while the host was down, or pre-install), the panel doesn't show a blank or an error. It shows a one-tap path back into the loop:

```
┌─────────────────────────────────────────┐
│  MailMate                       ● ready  │
├─────────────────────────────────────────┤
│  No suggestion for this message yet.     │
│                                          │
│  This one came in before MailMate saw    │
│  it (or while it was offline).           │
│                                          │
│            [ Ask MailMate now ]          │
└─────────────────────────────────────────┘
```

And the **host-down** variant of the same panel — because the per-message panel must also reflect `HostStatus`:

```
┌─────────────────────────────────────────┐
│  MailMate                     ● offline  │
├─────────────────────────────────────────┤
│  MailMate isn't connected, so it can't   │
│  suggest anything right now.             │
│  Your mail is unaffected.                │
│                                          │
│            [ ↻ Retry ]   [ Help → ]      │
└─────────────────────────────────────────┘
```

### 5. Degraded-but-connected: provider off

A subtle state that is *not* an error: host is `ready` but `drafting == no_provider`. This is the documented `zero_provider_by_default` reality and must "degrade gracefully rather than fail silently." When the user clicks **Draft reply** anywhere (context menu, per-message panel, follow-up), instead of a dead button or a silent no-op we show:

```
┌─────────────────────────────────────────┐
│  Drafting needs a provider               │
│                                          │
│  MailMate can write this draft for you,  │
│  but no AI provider is turned on. Add    │
│  one in Settings to enable drafting —    │
│  everything else keeps working.          │
│                                          │
│       [ Open Settings ]   [ Not now ]    │
└─────────────────────────────────────────┘
```

The "Draft reply" affordances stay **visible but clearly disabled** with a tooltip "Needs a provider — click to set up," rather than being hidden. Hiding them would make the feature undiscoverable; disabling-with-explanation teaches the user the off-by-default model and where to flip it. The trigger is purely `get_settings.default_provider == null`, so no new protocol is needed for this state.

---

## Host protocol additions required

This is the consolidated, de-duplicated list of everything the UX surfaces above need from the host that is **not** in today's live protocol. It collapses the per-surface "Host protocol additions needed" sections into one table. Nothing here is invented capability for its own sake — each row is the minimal wire change a specific surface genuinely hits.

Notes on shape:
- **field / behavior** rows extend an existing request or notification rather than minting a new `type`.
- **`record_user_action` discriminants** are new `event_type` values on the *existing* `record_user_action` request — they route a new learning signal to its sink, they are not new message types.
- Writing/reading settings, secrets, providers, accounts, categories, and pause is concentrated here because `get_settings` is read-only and `AppConfig`/`FileSecretStore` are off-protocol today.

> **Implementation status (Milestone 1, host side — shipped).** The host half of M1 is now live in `mailmate-native-host`: **`hello`** (handshake), the per-action **`apply_state`** field on the `classify_message` / `read_message` and `classification_ready` responses (`suggest` / `auto_applied` / `blocked`), and the three `record_user_action` discriminants — **`classification_corrected`** (→ classification feedback, via the new `UserCorrection::CorrectLabel`), **`action_undone`** (a reverted move/junk reuses the filing/not-spam corrections as negative evidence against the rule that fired; a reverted tag/draft is audited), and **`suggestion_dismissed`**. One honest deviation from the sketch below: `suggestion_dismissed` routes to the **audit** store, not a feedback table — the feedback rows key on a *chosen* label/folder, which a dismiss does not supply, and fabricating one would be a false learning signal (the same guard the junk-without-junk-state test enforces). The audit row carries `action_kind` + `authored_by` so the curator can still derive ignore/dismiss-rate. The `new_mail` auto-apply tightening + rule provenance (and therefore the `applied_by_rule_id`/`reverses_to`/`authored_by` enrichments) remain **Milestone 2** — they need rule provenance threaded onto `PlannedAction`, which the domain does not carry yet. The remaining rows below are still pending.

| New type / change | kind | Purpose | Payload (sketch) |
|---|---|---|---|
| `review_rule_proposal` | request | Approve / reject / edit a pending rule proposal — the Proposals-tab materialization gate. Specified in architecture.md but absent from the live request list, so it must be wired. | `{ proposal_id, decision: "accept_for_shadow_mode"\|"accept_active"\|"reject", edited_rule?: <rule JSON-AST> }` → `{ reviewed:true, proposal_id, resulting_status: "shadowing"\|"active"\|"rejected", rule_id? }` |
| `get_proposal_detail` | request | Full detail behind a `list_pending_reviews` summary row: condition/effect AST, back-test, and conflict overlap (which rule, which messages) for "Review conflict". | `{ proposal_id }` → `{ id, proposal_type, status, risk_level, rationale, proposal_json, backtest:{precision,support,conflicts}, conflicts:[{rule_id, stable_name, overlap_count, sample_message_ids:[…]}] }` |
| `list_followups` | request | Render the Follow-ups pipeline on dashboard open: active / needs-attention / won / lost items with next-due and any pending draft. Today only the host-initiated drain *pushes* notifications; there is no query. | `{ status_filter?: "active"\|"needs_attention"\|"won"\|"lost"\|"all", limit? }` → `{ followups:[{ pipeline_item_id, workflow_instance_id, title, thread_id, anchor_thunderbird_message_id, stage, status, next_due_at, last_event, pending_draft?:{draft_id,subject,requires_review,safety_notes}, needs_attention_reason? }] }` |
| `list_review_queue` | request | Durably rebuild the Review queue on dashboard open/reopen, since live `classification_ready` notifications are lost if the event page slept. *(Deferrable if the host persists the backlog and the background page reliably re-buffers it.)* | `{ limit? }` → `{ reviews:[{ decision_id, thunderbird_message_id, classification, suggested_actions:[…], blocked_actions:[…], explanation }] }` |
| `list_recent_activity` | request | Power the Activity tab's cross-message **global stream** and its `event_type` filter chips. `explain_decision` is per-message only (it requires a `message_id`/`thunderbird_message_id` and rejects requests without one), so it cannot return a global audit stream; this request drops that single-message constraint, reading the existing audit store (the underlying `AuditQuery` already supports `limit`). | `{ limit?, event_type_filter?: "classified"\|"applied"\|"blocked"\|"corrected"\|"follow_up"\|"proposal" }` → `{ events:[{ decision_id?, thunderbird_message_id, event_type, actor, created_at, payload }] }` |
| `classify_message` / `read_message` response — per-action `apply_state` | field | Let the per-message panel and Review queue distinguish a crystallized-auto action that already ran from a not-yet-crystallized suggestion vs a blocked action, and give Undo the prior state to reverse to. (`policy_outcome` alone is insufficient.) | each `suggested_actions[]` gains `{ apply_state:"auto_applied"\|"suggest"\|"blocked", applied_by_rule_id?, reverses_to?:{kind,to_folder} }`; suggestions/review items also carry `authored_by: {rule_id} \| "model"` |
| `new_mail` auto-apply tightening + `classification_ready` provenance | behavior | Auto-apply a policy-`allowed` action **only if** authored by an `active` rule (otherwise surface it as a suggestion in `review_required_actions`). Each `applied_actions[]` entry must carry its `rule_id` and the pre-action state (`from_folder_id`, `prior_read`, `prior_flagged`) needed for Undo. | `applied_actions[]: [{ kind, rule_id, from_folder_id?, prior_read?, prior_flagged?, … }]`; `review_required_actions[]`/`suggested_actions[]` carry `authored_by` |
| `record_user_action` — `event_type: "action_undone"` | request (discriminant) | Route an Undo of a crystallized auto-applied action into learning as strong negative evidence against the rule that fired (drives undo-rate → curator refine/retire), not merely the audit log. | `{ event_type:"action_undone", decision_id, thunderbird_message_id, rule_id, action_kind, reverted_to:{…}, user_initiated:true }` → `sink: "classification_feedback"\|"filing_feedback"` |
| `record_user_action` — `event_type: "classification_corrected"` | request (discriminant) | Route a one-click *wrong-category* label correction into classification feedback. Today `record_user_action` covers junk/move/file but has no label-correction discriminant. | `{ event_type:"classification_corrected", decision_id, thunderbird_message_id, corrected_label, prior_label, user_initiated:true }` → `sink: "classification_feedback"` |
| `record_user_action` — `event_type: "suggestion_dismissed"` | request (discriminant) | Capture the *ignore/dismiss* signal the learning loop tracks (ignore rate) so a repeatedly-dismissed suggestion is demoted rather than re-offered forever. | `{ event_type:"suggestion_dismissed", decision_id, thunderbird_message_id, action_kind, authored_by, user_initiated:true }` → **`sink: "audit"`** (shipped: a dismiss supplies no chosen label/folder, so it is recorded as audit provenance carrying `action_kind`+`authored_by`, never a fabricated feedback row) |
| `draft_reply` response — `commitments_guard` | field | Render the four-category commitments guard (dates / prices / payment / legal) deterministically instead of regex-scraping `safety_notes`. Until present, the panel falls back to listing `safety_notes` verbatim. | `commitments_guard: { dates:{status:"clear"\|"flagged", spans:[string]}, prices:{…}, payment:{…}, legal:{…} }` added to the existing `draft_reply` response |
| `regenerate_draft` | request | Regenerate a reply for a thread, optionally steered by free text and/or structured steer flags (powers Regenerate / Adjust), without overloading `draft_reply`'s semantics. *(Acceptable alternative: add the optional `steer` fields to `draft_reply`.)* | `{ thread_id, in_reply_to_message_id, previous_draft_id, steer?:string, steer_flags?:["shorter"\|"warmer"\|"more_formal"\|"remove_pricing"\|"remove_dates"] }` → same shape as `draft_reply` |
| `provider_status` | request | Cheap, drafting-specific availability signal so the compose popup picks provider-OK vs degraded layout without inferring it from `default_provider`/`providers`. *(Acceptable alternative: a field on `get_settings`.)* | `{}` → `{ drafting_available:bool, reason?:"no_provider"\|"provider_error", default_provider:string\|null }` |
| `set_settings` | request | Write the scalar config fields the snapshot already exposes (retention, follow-up cadence/tick, catch-up). Validates ranges/enums host-side; persists to `AppConfig` TOML. | `{ retention_level?, follow_up_tick_seconds?, catch_up_on_launch?, follow_up_cadence_days? }` → `{ updated:true, settings:<SettingsSnapshot> }` |
| `set_secret` | request | Write/clear a provider API key directly into the host's 0600 `FileSecretStore`, never returning the value. The only path a key reaches disk. | `{ provider_id, secret:string\|null }` → `{ stored:true, provider_id, configured:bool }` |
| `set_provider` | request | Add / update / remove a provider entry (endpoint, kind) and choose the default. Pairs with `set_secret`. | `{ provider_id, kind?, endpoint?, set_default?, remove? }` → `{ updated:true, providers:[{id,kind,endpoint?,configured}], default_provider }` |
| `test_provider` | request | Liveness/credential check for the "Test connection" button without a real generative call. | `{ provider_id }` → `{ ok:bool, detail:string }` |
| `set_category_policy` | request | Enable/disable a category and set its action mode; host enforces `auto_when_crystallized` can never widen to send/delete. | `{ category, enabled:bool, mode:"off"\|"suggest"\|"auto_when_crystallized" }` → `{ updated:true, categories:[{category,enabled,mode}] }` |
| `set_account_scope` | request | Mark an account triaged or ignored so `new_mail` intake can skip ignored accounts host-side. | `{ account_id, triaged:bool }` → `{ updated:true, scopes:[{account_id,triaged}] }` |
| `set_pause` | request | Global pause/resume as host-side state so it survives reloads and stops auto-apply + follow-up drains (backs the dashboard and toolbar Pause). | `{ paused:bool }` → `{ paused:bool }` |
| `get_settings` response — read additions | field | Surface, secret-free, the fields the config UI and status surfaces render but the snapshot lacks today. | adds `paused:bool`; per-provider `configured:bool` + `endpoint`; `categories:[{category,enabled,mode}]`; `account_scopes:[{account_id,triaged}]`; `follow_up_cadence_days:int`; `undo_window_seconds:int` |
| `hello` | request | Richer handshake than `ping`: returns host/protocol version, capabilities, retention, and drafting availability in one round-trip so the UI can render Connected / version-mismatch *before* sending any real request, and guard the single-writer stdout channel. *(Acceptable alternative: extend the `ping` response.)* | `{ extension_version, protocol_version }` → `{ host_version, protocol_version, capabilities:[…], drafting_available:bool, retention_level }` |
| `learning_progress` | request | Read-only "is it learning?" summary that powers the Proposals empty-state progress indicator — `list_pending_reviews` only returns *already-pending* proposals, not near-threshold patterns. | `{ limit?:5 }` → `{ near_threshold:[{pattern_summary,support,threshold,fraction}], crystallized_rule_count, pending_review_count }` |
| `proposal_ready` | notification | Host→extension push when the curator promotes a learned behavior to a `PendingReview` proposal on its own schedule; today the UI must poll `list_pending_reviews`. Powers the proactive "new proposals" toast. | `{ proposal_id, proposal_type, title, risk_level, recommended_status, batch_count? }` |

**Reused unchanged** (no addition required): `ping` (heartbeat/freshness), `classify_message` / `read_message`, `record_user_action` (`junk_changed`, `message_moved`, `action_applied`), `draft_reply` (draft + `safety_notes` + `requires_human_review`), the full follow-up control set (`enroll_pipeline_item`, `update_pipeline_stage`, `cancel_sequence`, `reschedule_followup`, `snooze`, `review_followup`), `explain_decision` (per-message focused timeline only), `list_pending_reviews`, `get_settings` (its existing fields), the `classification_ready` / `followup_draft_ready` / `followup_needs_attention` notifications, `browser.accounts.list()` for the account roster, the existing `browser.messages.move/update` + `drafts.js` apply path for filing corrections, and the `port.onDisconnect` / `port.error.message` the extension already captures for connection-health detection.

---

## Build order / milestones

Smallest shippable first. Each milestone is independently demoable and closes a concrete current-state gap; later milestones depend only on earlier ones plus their own additions.

**Milestone 1 — Per-message panel + connection status (the smallest end-to-end loop).**
Ship the `messageDisplayAction` popup (verdict, suggest/auto distinction, one-click corrections, blocked-action visibility) and the connection-health layer (`HostStatus`, toolbar badge, recovery card, host-down panel variants). This alone replaces "context-menu → console" with a real reading + correcting surface and makes the invisible host-down failure legible.
*Additions consumed:* `apply_state` field; the three `record_user_action` discriminants (`action_undone`, `classification_corrected`, `suggestion_dismissed`); `hello` (or `ping` extension). Reuses `classify_message`, existing `record_user_action`, `explain_decision`, `get_settings`.

**Milestone 2 — Dashboard space (Review + Activity), with onboarding & empty states.**
Stand up the `spaces` dashboard tab with the **Review** queue and **Activity/explain** tabs, the deep-link grammar the satellites point at, the first-run walkthrough, and all empty states. This consolidates background `classification_ready` results into a real work queue and gives the product its home.
*Additions consumed:* `list_review_queue` (durable reopen; deferrable), `list_recent_activity` (the Activity tab's cross-message global stream + filter chips), `new_mail` auto-apply tightening + `classification_ready` provenance (so the queue partitions Track A vs Track B correctly), `learning_progress` (Proposals empty-state progress). Reuses `explain_decision` (per-message focused timeline), `list_pending_reviews` (counts), the existing notifications fanned into the tab.

**Milestone 3 — Proposals / learning (the materialization gate) + notifications.**
Add the **Proposals** tab (approve/reject/edit, conflict review, risk-gated accept) and the desktop notification layer (dedup, quiet hours, batching, deep-linking), including the new proactive proposal push.
*Additions consumed:* `review_rule_proposal`, `get_proposal_detail`, `proposal_ready` notification. Reuses `list_pending_reviews`.

**Milestone 4 — Provider / drafting / configuration / follow-ups pipeline.**
Ship the compose review panel (commitments guard, regenerate/adjust, degraded states), the `options_ui` configuration page + toolbar quick toggles (provider, retention, accounts, categories, pause), and the **Follow-ups** pipeline tab.
*Additions consumed:* `commitments_guard` field, `regenerate_draft`, `provider_status`; the full settings write set (`set_settings`, `set_secret`, `set_provider`, `test_provider`, `set_category_policy`, `set_account_scope`, `set_pause`) + `get_settings` read additions; `list_followups`. Reuses `draft_reply`, the follow-up control set, the follow-up notifications.

Rationale for the order: Milestone 1 is the minimum viable trust loop (see a verdict, correct it, know if the host is alive) and needs the least host work. Milestone 2 gives those signals a durable home and onboards the user. Milestone 3 unlocks the learning payoff (rules the user approves). Milestone 4 — provider-dependent drafting plus the broadest write surface (settings/secrets) and the pipeline — is last because it carries the most host additions and the only credential-handling, and the product is already fully useful (classify / file / correct / learn) without it.

---

## Open questions

1. **Durable Review-queue reopen vs. notification re-buffering.** Is `list_review_queue` actually needed, or does the host already persist review-required `classification_ready` payloads such that the background page can reliably re-buffer them after an event-page sleep? This decides whether Milestone 2 ships a new query or relies on volatile buffering.
2. **Steer semantics: new `regenerate_draft` vs. extended `draft_reply`.** Mint a dedicated regenerate request, or add optional `steer`/`steer_flags`/`previous_draft_id` fields to the existing `draft_reply`? Affects how much the compose panel and the host diverge.
3. **`provider_status` vs. a field on `get_settings`.** Does drafting availability warrant its own cheap request, or should it be a field on the existing settings snapshot? The distinction matters only when a provider is *configured but failing* (which `default_provider != null` cannot express).
4. **`hello` verb vs. extending `ping`.** Add a named handshake, or extend `ping`'s response with version/capability/retention? `hello` keeps `ping`'s minimal liveness contract intact at the cost of one more verb.
5. **Undo window defaults and scope.** Is 30s (header/popup) + 24h (Activity) the right pair, and should `undo_window_seconds` be per-category or global? Longer windows are free for reversibility but blur "settled."
6. **Pause granularity.** `set_pause` is global. Do users need per-account or per-category pause, or is global pause + per-category action policy (`off`/`suggest`/`auto_when_crystallized`) sufficient?
7. **Commitments-guard authority.** The four-category guard (dates/prices/payment/legal) is presented as advisory-only. Is that the right fixed set, and should the categories themselves ever be user-configurable, or does configurability undermine the guarantee?
8. **Quiet-hours wake-up for `needs_attention`.** Stale follow-ups are held through quiet hours and surfaced in the wake-up digest. Is "never wake the user for a stale deal" always correct, or are there time-critical follow-ups that should override quiet hours?
9. **Proposal `batch_count` trust.** The `proposal_ready` notification carries an optional `batch_count` hint so the digest renders without a round-trip. If it can drift from the authoritative `list_pending_reviews`, should the toast always show a generic "new proposals" rather than a count?
10. **Settings ownership boundary.** Configuration is split between the protocol (the new `set_*` writes) and off-protocol file/secret-store management. Where exactly is the line — e.g. should the database path or follow-up horizon ever become writable on the wire, or stay file-only?
11. **`list_recent_activity` scope vs. `explain_decision`.** The global Activity stream now rides a dedicated cross-message query (`explain_decision` is per-message only). Should that query expose richer filtering (by actor, by rule, by date range) than the six `event_type` families, or is the minimal limit + event-type filter sufficient for a read-only history view?
