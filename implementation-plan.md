# MailMate — Implementation Plan (locked, rev 2)

> **Companion to:** [`interaction-design.md`](interaction-design.md) (the UX spec) and [`architecture.md`](architecture.md) (the host spine).
> **Status:** locked plan derived from a full code audit + an 8-agent UX/capability review (2026-06-21). This is the actionable, sequenced build that closes the gap between what's shipped and what the product promises.
> **Prime directive:** *quality and UX.* MailMate is a **learning decision system the user can see into and steer** — not an AI wrapper, not a black box.
> **Rev 2 changes:** provenance + capture + the privacy contract pulled into Phase 1; a thin end-to-end "Golden Path" milestone inserted; Phase 7/8 split and re-baselined; explainability hardened (rule AST on the card, a Rules manager, correctable signals); first-run backfill + cold-start bootstrap; trust/privacy made first-class.

---

## 1. Locked decisions

Decided explicitly; not to be re-litigated mid-build.

1. **Learning engine — go all-in.** Real on-device model adaptation producing a **loadable** artifact the classifier actually uses, *plus* **interpretable multi-aspect rule induction**. The "learns from you" claim must be *true*.
2. **Body retention — real toggle, full bodies the recommended default — gated by explicit consent.** The Settings control becomes live; full-body is the recommended path, but it is stored **only after an affirmative onboarding consent step** (else the runtime stays at `metadata`). This reconciles the decision with the locked UX spec and `RetentionLevel::default()==Metadata` *without reversing it* — and the disposal half (retention-purge) ships **with** the flip, not later.
3. **Build order — UX-first, explainability as the spine.**
4. **Rules use all aspects of mail, and the user sees & corrects the logic.** Rules are composed over the full feature set via the existing `all`/`any`/`not` predicate AST; every verdict shows the **actual reasoning** (correctable per-signal); a **Rules manager** exposes the logic, hit-rate, and undo-rate of every rule.
5. **Tags/labels are a first-class learning signal** (add/remove, echo-suppressed against MailMate's own tag ops, mapped onto the classifier's category vocabulary).

---

## 2. Current-state verdict (why this plan exists)

**There is a real, tested spine.** The rule engine is a full `all`/`any`/`not` predicate tree with 13 operators over any field ([condition.rs](crates/mailmate-common/src/rules/condition.rs), built so "the UI can render it"); the active-vs-shadow apply gate is real; the hard-policy guard genuinely blocks send/delete; the correction→Tier-2 online logistic-regression loop learns; reply generation is real when a provider exists (Ollama works today); privacy redaction, the workflow FSM, and all storage SQL are real; `dashboard.js`/`options.js` are working SPAs. No `todo!()`/`panic!` in request paths; no fabricated data.

**Four structural gaps undercut the thesis** (each verified in code):

| Gap | Evidence |
|---|---|
| **Explainability under-scoped** | `list_pending_reviews` returns `recommended_status`/`risk_level` but **omits `rule_draft`** ([router.rs:938](crates/mailmate-native-host/src/router.rs#L938)) → users would approve a rule they can't read. The verdict "why" renders raw policy-check IDs. No Rules-manager surface or verb → an approved rule vanishes from every UI. |
| **Learning loop unwired end-to-end** | `back_test()` has **zero production callers** ([shadow.rs:126](crates/mailmate-learning/src/shadow.rs#L126)); crystallization gates on raw count. Induction keys only on `sender_domain` ([proposals.rs:23](crates/mailmate-learning/src/proposals.rs#L23)). No negative sampling, so a domain rule that also hits kept mail scores precision 1.0. On-device "training" stamps a class-prior as a LoRA artifact at a path never written/loaded ([trainer.rs:109](crates/mailmate-ml/src/trainer.rs#L109)). |
| **Sequencing inverts risk** | Auto-apply provenance (a Phase-1 panel prerequisite) sat in Phase 6; the full-body privacy flip shipped 8 phases before its purge and with no consent gate; the highest-uncertainty Burn ML was second-to-last with no fallback. |
| **First-run delivers nothing** | `messages.query` is **unused** — only the live `onNewMailReceived` listener intakes mail, so a fresh install learns nothing from the existing inbox; zero rules + ~0.5 verdicts give a `needs_review` wall. |

Plus the residual damage from the audit: `BODY_RETENTION_ALLOWED=false` hardcoded ([message_reader.js:12](extension/message_reader.js#L12)); `resolveFolder()` stub ([drafts.js:147](extension/drafts.js#L147)); hardcoded `references:[]`/`in_reply_to:null` ([message_reader.js:51](extension/message_reader.js#L51)); missing host verbs; half-baked surfaces; unbounded workflow drain; no NDR exit.

---

## 3. The learning architecture (the spine)

**Determinism-first, two layers.** Models are the *teacher* that spots patterns; the *executor* is always a deterministic rule the user approved and can read. Nothing a model "decides" touches mail directly.

- **Layer 1 — the continuous model (invisible, immediate).** Every correction feeds an online update: the Tier-2 logistic regression today ([correction.rs:88](crates/mailmate-core/src/usecases/correction.rs#L88)); Phase 8 swaps in a real on-device adapted model behind the existing `Tier2Classifier` port. Nudges similar future mail at once; the fallback for everything not yet crystallized.
- **Layer 2 — crystallized rules (visible, approved, reversible).** Repeated, consistent corrections crystallize into deterministic rules the user **sees, edits, and approves**. Once active, a rule auto-applies only **SAFE** actions (file/tag/mark-read/mark-junk), **always with Undo**, **never** send/delete, **never** without prior human approval.

### 3.1 Rich feature capture (Phase 1a — must come first)
Every feedback row records the **full feature vector**, not just `sender_domain` — else corrections made today can't be mined for richer rules later. Read full headers via `getFull()` (already called for body):

- **Sender:** domain, address, display-name, `sender_seen_count`, `in_address_book`.
- **Relationship/thread:** real `thread_id`/`in_reply_to` (today hardcoded null), `is_reply_to_me`, `you_initiated`, thread length, last-inbound age.
- **Subject/body:** normalized tokens, urgency/money cues; body keywords, link domains, has-unsubscribe-link, image-only/HTML-only.
- **Headers:** `List-Id`/`List-Unsubscribe`, `Precedence: bulk`, **SPF/DKIM/DMARC** result, **Reply-To mismatch**, spoofed display-name.
- **Attachments:** `has_invoice_pdf`, `has_calendar`, `has_executable`, `has_archive` (derived from filename/content-type already on the wire).

### 3.2 Interpretable multi-aspect induction (Phase 7, split 7a/7b)
- **7a — cluster by EFFECT** (chosen label/folder/tag), independent of domain; **stop dropping domainless rows**; thread `account_id` into the cluster key so divergent cross-account behavior yields `RuleScope::Account` instead of a false conflict.
- **7b — candidate-predicate induction:** enumerate candidate predicates from feature values shared by ≥k cluster members; greedily add the clause that most raises **back-test precision** while support stays ≥ floor; emit the `all`/`any`/`not` AST the Rules manager renders.
- **Negative sampling:** the back-test history = cluster positives **plus** a negative pool of recent messages the candidate also matches but on which the user took no conflicting action (a `HistoricalExample.kept` discriminator), so a false positive is *visible*. Full-body retention makes the pool feasible.
- **Empty-effect guard:** reject vacuous candidates and require positive agreement on the effect's primary axis before scoring (today an empty effect scores precision 1.0 and clears the gate).
- **Overlap/subsumption + decay:** detect subsumption and co-match overlap (beyond exact contradiction); compute per-rule undo-rate from Phase-1 provenance and emit a **RetireRule/DetectStale proposal to human review** (never auto-retire) when undo-rate exceeds a floor or a rule hasn't fired in M days.

### 3.3 Explainability (Phase 1b + Phase 6), with *fidelity*
- **Verdict "why" = the actual signals, each correctable.** The host returns typed `salient_signals: [{id, label, kind, source, weight, correctable}]`, populated in the cascade from the **top-k signed logreg contributions that actually produced the score** (`logreg.probability()` already computes per-key terms) — not post-hoc IDs. Tier-3 (LLM) verdicts are labeled "AI assessment," never dressed as deterministic signals. Marking a signal wrong emits a `signal_marked_wrong` correction into `classification_feedback`.
- **Rules manager + proposal Edit-rule editor** render the same `all/any/not` AST. The proposal card shows the rule in plain English ("Sender domain is one of … **and** subject contains …") with measured **precision · support · conflicts**. The editor is a **structured clause form** (per-predicate `[field▾][operator▾][value]`, validated against the closed `Operator` enum), never a raw JSON textarea.

### 3.4 Tags as a signal (Phase 1a)
`messages.onUpdated` tag delta → a `tag_changed{added|removed, tag}` correction (add = positive, remove = negative), echo-suppressed via a `consumeHostTag` twin of `consumeHostMove` (mirrors the Phase-10 onMoved learning-poison fix). The user's tag keys join the category vocabulary (mapping surface in Settings) so induction clusters by a canonical category and a tagging rule's effect uses the same key.

### 3.5 Cold-start bootstrap (Phase 1b/2)
A fresh install must be useful in session one: (1) ship **disabled** deterministic starter rules the user one-taps (`List-Unsubscribe`+`Precedence:bulk`→newsletters; DMARC-fail+no-prior-contact→suspicious); (2) mine existing **folder placements as implicit positives** to warm clusters without fresh corrections; (3) a designed "still learning — here's what I can already see" empty state instead of a `needs_review` wall.

### 3.6 Invariants (non-negotiable)
Never send · never delete · never auto-activate a rule (human approval is the *only* materialization path) · every auto SAFE action is reversible (Undo) · **no move-effect rule promotes to active until `reverses_to` lands and Undo reverses it for real** · risky/conflicting proposals are forced to human review · never auto-junk/auto-file a reply in a thread the user joined · degrade-never-lie · single-writer native port · local-first / provider-optional.

---

## 4. Engineering principles & cross-cutting seams

- **Test gate, every phase, before & after:** full workspace `cargo test`; `node --test` in `extension/test/` (jsdom); framed-stdin probes against the real release binary; and a **real-instance exercise** of any changed user-visible flow. A green unit suite is not "done" — the zero-caller `back_test` gate and the phantom trainer both passed unit suites.
- **Falsifiable exit criteria:** every phase below has a 2–4 item checklist with concrete, harness-tied assertions (not "Demo: it works").
- **Seams established in Phase 1 (cheap now, ruinous as retrofits across 6 surfaces):** an **i18n seam** (`_locales/en/messages.json` via `browser.i18n.getMessage`; English-only v1, but the seam exists) and a **panel async-state machine** (`loading→classified|timed_out|host_down`, skeleton, bounded timeout→retry, stale-result race guard, per-message cache, `aria-live` + reduced-motion).
- **Re-baseline of learning value:** **Tier-2 + crystallized rules ARE the shipped learning value through Phase 7.** Phase 8 (on-device adapted model) is an accuracy **swap-in behind the `Tier2Classifier` port** — a Phase-8 slip degrades accuracy, it never ships fake learning. The phantom LoRA descriptor is corrected and Tier-2 weights persist/reload (pure Rust) from Phase 1a so "a loadable artifact the classifier uses" is continuously true.

---

## 5. Phased build

### Phase 1a — Foundations: integrity, provenance, capture, privacy contract, ML persistence, seams
*(backend + extension plumbing; little new UI)*
- **Integrity:** make retention real; fix `resolveFolder` → `{accountId, path}` and verify host moves land.
- **Privacy contract (ships together):** non-skippable onboarding **consent step** (full bodies only after affirmative consent, else metadata); the **retention-purge** half — write-time TTL + sweep + immediate **down-level purge** when the user lowers retention.
- **Provenance spine:** thread `rule_id`/`authored_by`/`reverses_to` onto `PlannedAction`; gate `apply_allowed` to **active-rule-authored** actions only; enrich `applied_actions[]` with `rule_id` + prior state (`from_folder_id`/`prior_read`/`prior_flagged`).
- **Rich capture (§3.1):** full headers via `getFull`; real `thread_id`/`in_reply_to`; List-*/auth-results/Reply-To/attachment-class features; `sender_seen_count`/`in_address_book`.
- **Tags as a signal (§3.4):** `tag_changed` discriminant + `onUpdated` listener + `consumeHostTag` echo-suppression.
- **ML honesty:** fix the `SmallModel` descriptor (real `adapter_type`/format); persist/reload Tier-2 logreg weights to disk; add L2/weight-decay + a per-feature confidence floor so a 2-example domain one-hot can't swing the verdict.
- **Seams:** i18n + panel async-state machine.
- **Exit:** a tag add produces a `classification_feedback` row with `polarity=positive` (probe); an auto-applied move carries `reverses_to` and an Undo reverses it (probe); lowering retention purges existing bodies (probe); Tier-2 weights survive a host restart (two-process probe); consent declined ⇒ runtime stays `metadata` (probe).

### Phase 1b — The per-message experience (explainability + action-first + safety)
- **Explainability spine:** typed `salient_signals` through the cascade → payload; render **correctable signal chips** (not raw IDs); `signal_marked_wrong` correction.
- **Action-first panel:** primary actions on every message — File to… / Junk & block / Mark read / **Draft a reply**; human category names; calibrated band; **per-message header badge** (blue count / amber `!` / green `✓` / in-flight dot, reusing the existing badge code); kill the "0 allowed, 1 need review" render → designed low-confidence + **cold-start "still learning"** state; enable the **Explain-in-dashboard** deep-link.
- **Safety surfaces:** **thread guard** (never auto-junk/file a reply you joined); a **phishing/malware Safety block** (defanged link domains, dangerous-attachment + auth warnings, inform-only); a **one-click unsubscribe** affordance from `List-Unsubscribe`.
- **Host:** calibrated confidence band; category vocabulary on `get_settings`; `salient_signals`.
- **Exit:** opening the Josh-Brown mail shows an action-first panel with readable correctable signals and a header badge; a reply-in-your-thread never shows an auto-junk action (probe); unsubscribe opens a pre-addressed compose / confirmed POST.

### Phase 2 — Golden Path (thin end-to-end learning vertical)
- **Wire the dead gate:** one narrow correction kind goes **capture → cluster → candidate predicate → `back_test` (the real, first production caller) → `AgentProposal` → existing `review_rule_proposal` → activate → auto-apply → Undo**, demoed against the live host/data dir. Single-aspect but REAL — turns the zero-caller gate load-bearing months before Phase 7.
- **Cold-start bootstrap (§3.5):** disabled starter rules + folder-history mining so there's something to approve in session one; a same-session crystallization "aha" toast after a triage batch.
- **First-run backfill:** one-tap "Triage my existing inbox" pages `browser.messages.query` through the same classify path (applies nothing; resumable/cancellable; progress chip).
- **Exit:** a scripted correction → an approved active rule → an auto-applied action → a working Undo, all asserted against the live binary; backfill populates the Review queue without mutating mail.

### Phase 3 — Compose review (the draft)
- **Host:** `commitments_guard` (typed dates/prices/payment/legal), real `rationale` on `draft_reply`, `regenerate_draft`, `provider_status`.
- **UI:** four-category guard with cited spans; real "why this draft"; Regenerate + Adjust + quick-steer chips; the **edit-divergence learning hook**; **reply-from-correct-identity** (`browser.identities` → `identityId` in `beginReply`, surfaced as "Replying from: …").
- **Scope honesty:** either pull `set_provider`/`set_secret`/`test_provider`/`provider_status` forward to land with compose, or explicitly scope this phase to the Ollama-default path so its exit criteria don't imply a config flow that doesn't exist yet.
- **Exit:** a "No thanks" reply generated via Ollama with the correct From identity; the guard renders all-clear/flags with spans; an edit-then-close records a divergence signal (probe).

### Phase 4 — Toolbar mini-hub + notifications
- **Toolbar popup:** keep connection health; add the **aggregate badge** (review+needs_attention+proposals), **Open dashboard ▸**, and Pause (once `set_pause` is surfaced).
- **Notifications:** persistent dedup (`storage.local` LRU), quiet hours, batching/digest, per-class toggles.
- **Exit:** a catch-up backlog produces one batched notification, not a storm (probe); dedup survives an event-page suspension.

### Phase 5 — Options / configuration completeness
- **Host:** `set_category_policy`, `set_account_scope`, `test_provider`; `get_settings` read additions; the **tag→category mapping** surface.
- **UI:** per-category action-policy table, per-account triage checklist, Test connection, notification prefs; drop the `mock` provider kind from prod; fix the dev-text leak.
- **Exit:** disabling a category stops its suggestions (probe); Test connection reports a real liveness result.

### Phase 6 — Dashboard completeness + Rules manager + bulk triage
- **Host:** `get_proposal_detail`, `learning_progress`, **`list_rules`/`set_rule_status`**, `get_sender_profile`, `get_metrics`; add **`rule_draft` + back-test to `list_pending_reviews`**.
- **Proposals:** Approve→active (low-risk), **structured clause Edit-rule editor**, Review conflict, the "is it learning?" progress bar, and the **rule AST + precision·support·conflicts on the card**.
- **Rules manager (5th tab):** every active+shadow rule as `all/any/not` clauses with hit-rate + undo-rate; view/enable/disable; Edit re-routes through review.
- **Bulk triage:** group queued cards by host-supplied target into collapsible bands with "Approve all N" + cross-queue multi-select; each batched apply keeps its own Undo + `record_user_action`.
- **Sender-centric view:** "About this sender ▸" + one-move treatments (Always file / Never auto-act / VIP), every write routed through review.
- **Trust receipt:** a "How MailMate is doing" card (accept-rate, auto-apply correctness = 1−undo-rate, weekly counts) over existing audit+feedback data; "not enough actions to score yet" over a fake 100%.
- **Follow-ups:** Open draft to review, Enroll selected as a deal, provider-off degradation. Remove stale "Milestone 4" toasts.
- **Exit:** an approved rule appears in the Rules manager with a readable condition + live hit-rate; a proposal card shows its rule in English with back-test numbers.

### Phase 7 — Real multi-aspect, explainable rule learning
- **7a:** cluster by effect (not domain); stop dropping domainless rows; per-account scope in the cluster key.
- **7b:** candidate-predicate induction + greedy back-test composition; **negative sampling**; **empty-effect guard**; overlap/subsumption conflict detection; **rule decay → RetireRule/DetectStale proposals** (human-gated); learn from **outbound** (Sent) mail for VIP/priority + drafter style corpus.
- **Exit:** a multi-clause rule (e.g. `auth_fail AND no_prior_contact → suspicious`) is induced, back-tested against a negative pool, and shown with honest precision; a high-undo-rate rule surfaces a retire proposal (probe).

### Phase 8 — All-in on-device ML
- A real on-device training run (Burn, CPU) that writes a **loadable** artifact; the cascade loads and uses it (`artifact_path` actually read); evaluation scores the **real** artifact on held-out data and the gate flips active only above threshold. A **feature-flagged Burn spike** lands early (alongside Phase 2) to surface integration/perf risk before this phase commits.
- **Redaction gate:** before training on bodies, expand `redact_text` to a documented coverage matrix (URL scrubbing, `sk-`/`ghp_`/`AKIA`/JWT, IBANs) with a **golden-corpus regression test** as a hard gate; document what is *not* redacted.
- **Exit:** held-out eval reports precision ≥ X from the loaded artifact (not the base model) before any activation; a Full-ceiling export cannot leave the machine without passing redaction.

### Phase 9 — Additional capabilities + robustness & cleanup
- **Capabilities:** snooze / durable remind-me (generalize the workflow FSM, notify-only); an opt-in **native folder mirror** of the queue ("MailMate/Needs review", echo-suppressed) — the one persistent home in the message-list pane the APIs otherwise forbid; **delete-my-data / export** (`forget_message`, `forget_sender`, "Reset all learning") over the Phase-1 purge primitives; keyboard triage + full a11y; i18n copy migration.
- **Robustness:** bounded workflow drain + batch cap; **NDR/bounce** workflow exit; config-file knobs for the product-significant hardcoded constants; `0700` data-dir guarantee; dead-code/stale-comment/doc sweep; fix the manifest description.

---

## 6. Capability index (where each lands)

| Capability | Phase |
|---|---|
| First-run inbox backfill · cold-start bootstrap | 2 (capture in 1a) |
| Per-message header badge · correctable signals · thread guard · phishing block · unsubscribe | 1b |
| Reply-from-correct-identity · regenerate/adjust · commitments guard | 3 |
| Bulk triage · Rules manager · sender profile · trust receipt | 6 |
| Multi-aspect induction · negative sampling · rule decay · learn-from-Sent · per-account scope | 7 |
| On-device adapted model · redaction matrix | 8 |
| Snooze/remind · folder mirror · delete/export · keyboard/a11y | 9 |

---

## 7. Considered & rejected

- **Daily digest** — marginal over badge + per-event notifications; one more thing to mute.
- **Send-later** — derivative of snooze/remind (a reminder over a saved draft); folded into Phase 9 snooze.
- **Standalone per-account scope** — folded into Phase-7 induction (`account_id` in the cluster key).
- **At-rest encryption (SQLCipher)** — not mandated; the `0700` dir + consent + purge + delete-my-data cover the substantive exposure; revisit only on a stated threat model.
- **Shadow→active live-outcome criteria** — subsumed by negative-sampling (pre-promotion FP) + rule-decay (post-active drift); human still gates every promotion.
- **Standalone "what's stored about this message" view** — folded into the `forget_message` surface as a small expander.
