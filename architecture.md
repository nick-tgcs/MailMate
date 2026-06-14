# MailMate Technical Architecture Plan

> **Project:** MailMate  
> **Technical name prefix:** `mailmate`  
> **Architecture stance:** local-first, Rust-first, provider-agnostic, human-curated learning system for Thunderbird

## Table of Contents

1. [Goals](#goals)
2. [Non-Goals](#non-goals)
3. [Architecture Overview](#architecture-overview)
4. [Architectural Style and Event Model](#architectural-style-and-event-model)
5. [Component Diagram](#component-diagram)
6. [Module Layout](#module-layout)
7. [Core Domain Model](#core-domain-model)
8. [Data Model](#data-model)
9. [Rust Trait and Interface Definitions](#rust-trait-and-interface-definitions)
10. [Replaceable Components and Extension Points](#replaceable-components-and-extension-points)
11. [Native Messaging Protocol](#native-messaging-protocol)
12. [Rule and Principle System](#rule-and-principle-system)
13. [Rule Lifecycle](#rule-lifecycle)
14. [Two Pipelines](#two-pipelines)
15. [Classification Cascade](#classification-cascade)
16. [Rule Hierarchy and Decision Order](#rule-hierarchy-and-decision-order)
17. [Policy Guard Design](#policy-guard-design)
18. [Learning Loop](#learning-loop)
19. [Self-Iteration Learning Model](#self-iteration-learning-model)
20. [On-Device Training Layer](#on-device-training-layer)
21. [Agent Curator](#agent-curator)
22. [Provider-Abstraction Layer](#provider-abstraction-layer)
23. [Action Planning and Execution](#action-planning-and-execution)
24. [Sales Pipeline and Follow-up Workflows](#sales-pipeline-and-follow-up-workflows)
25. [Drafting Safety](#drafting-safety)
26. [Storage Strategy](#storage-strategy)
27. [Auditability and Explainability](#auditability-and-explainability)
28. [GitHub Workflow, CI, and Releases](#github-workflow-ci-and-releases)
29. [Distribution and Installation](#distribution-and-installation)
30. [Testing Strategy](#testing-strategy)
31. [Implementation Sequence](#implementation-sequence)
32. [Risks and Mitigations](#risks-and-mitigations)
33. [Open Design Questions](#open-design-questions)

---

## Goals

MailMate is a local-first Thunderbird mail assistant that helps users process email while becoming better over time through explicit, human-curated rules.

MailMate should support:

- Spam and phishing detection.
- Smart filing and folder suggestions.
- Thread summaries.
- Draft replies.
- Task extraction.
- Priority tagging.
- Email triage.
- Lightweight sales-pipeline tracking with review-required, time-triggered follow-up drafts for quotes/proposals, whose cadence, content, and stop conditions are learned.
- Learning from user behavior through explicit rules/principles.
- Human approval, editing, rejection, disabling, and override of learned behavior.
- Safe, auditable automation.
- Test-driven development for all production code, with unit, integration, and end-to-end coverage for every feature area.

**v1 ships the full product.** The Implementation-Sequence phases are engineering build order, not a staged feature release — there is no reduced MVP slice. v1 is feature-complete across the goals above.

The central product idea is not “an artificial intelligence (AI) wrapper for email.” The central product idea is a **learning decision system** for email, where AI is one replaceable advisor inside a larger rule, policy, storage, and review architecture.

The system should be inspired by Ray Dalio-style principles:

- Decisions become explicit principles.
- Principles are structured rules.
- Rules are versioned.
- Rules are tested.
- Rules are reviewed.
- Rules are improved or retired.
- Humans remain able to override any decision.

**Determinism first — a learned trait becomes a model-free rule.** AI and machine learning are how MailMate *discovers* a principle and how it handles what it has **not yet** learned — never how it **re-decides** something it has **already** learned. The instant a trait is learned, its terminal form is a **deterministic rule** (a versioned JSON-AST condition over deterministic features) that runs with **no LLM and no model inference at all**: same input → same output, offline, reproducible, audit-replayable, and fully functional with **zero AI providers configured**. The model tiers are the *teacher* and the *fallback for the not-yet-learned*; the deterministic rule layer is the *steady-state executor* — and learned rules already outrank model predictions at runtime. See *Learning Loop → Crystallization* (the promotion mechanism) and *Classification Cascade* (model use shrinking as rules accrue).

**Frozen foundations, learned layer in Burn — and both behind ports.** MailMate never *fine-tunes the large model*. Any generative LLM (drafting, summaries) runs **frozen** on a mature local engine through the `AiProvider` trait — exactly as the sibling project **[idiolect](https://github.com/nick-tgcs/idiolect)** runs Whisper frozen via `whisper-rs`. The **learned layer is Rust-native Burn**, on-device: the Tier-2 classifier, preference/scoring/tone models, and the trainer — pinned to an exact Burn version, CPU/ndarray by default, GPU opt-in. The learning thesis rides on *small Burn models + crystallized rules + an eval/promotion gate*, **not** on LoRA-adapting a big LLM (idiolect ships exactly this shape today, so it is buildable now). Burn is the committed default **but lives entirely behind engine-neutral contracts + a trait + a swappable adapter** (idiolect's `idiolect-ml-core` / `idiolect-ports` / `idiolect-trainer-burn` split), so it can be replaced if and when needed — *committed ≠ welded-in*. See *On-Device Training Layer* and *Replaceable Components and Extension Points*.

---

## Non-Goals

MailMate should intentionally avoid these behaviors:

- Replacing Thunderbird as a mail client.
- Building a cloud email service.
- Storing full email bodies by default.
- Hard-coding one AI provider.
- Making Ollama a core architectural dependency.
- Putting core intelligence in the Thunderbird extension.
- Auto-deleting mail.
- Auto-sending drafts.
- Opening links from email.
- Downloading remote email content for classification.
- Silently creating hidden rules.
- Allowing AI-generated rules to become active without review when they could cause risky actions.
- Training on private user data outside the local system unless the user explicitly configures a remote provider and accepts its privacy implications.
- Building a full CRM. The pipeline tracker is a *lightweight* follow-up tracker (enroll a quote, stage it won/lost, draft timely nudges for review) — not contact management, quote/line-item authoring, revenue forecasting, or sales automation.
- Auto-sending follow-ups. Scheduled follow-ups are **review-required drafts only**; there is no auto-send or gated-auto-send path for follow-ups. This reaffirms the "Auto-sending drafts" non-goal above — `never_auto_send_drafts` is unchanged.

---

## Architecture Overview

MailMate has two major runtime pieces:

1. **Thunderbird MailExtension/WebExtension adapter**
   - Reads selected or newly arrived messages.
   - Receives Thunderbird events.
   - Adds context-menu actions.
   - Opens draft replies.
   - Applies safe Thunderbird-side actions when instructed.
   - Communicates with the Rust host through native messaging.
   - Contains no durable intelligence, learning logic, provider logic, or policy logic.

2. **Rust native host**
   - Owns the real application.
   - Receives messages from Thunderbird through native messaging.
   - Normalizes message metadata and content snippets.
   - Runs two pipelines: classify/understand (P1), then plan/guard (P2).
   - Runs policy checks.
   - Runs rule evaluation.
   - Calls AI providers through provider traits.
   - Constructs prompts.
   - Validates structured model output.
   - Plans actions.
   - Captures per-task feedback (corrections + reasons) and audit entries.
   - Stores rules, versions, evidence, conflicts, and proposals.
   - Explains decisions.
   - Powers the learning loop.

The Thunderbird layer should be treated like a UI/client adapter. The Rust host should be treated like the product.

---

## Architectural Style and Event Model

This section names MailMate's architectural style in one place, because the
underlying decisions are otherwise scattered across *Native Messaging Protocol*,
*Capture model: single writer per fact*, *Two Pipelines*, and *Decision identity*.

**One-line classification.** MailMate is **event-driven at its Thunderbird boundary
and queue-driven in the background, over a synchronous pipeline core, on a
single-writer-per-fact relational store. It is deliberately NOT event-sourced and has
no generic event bus.**

**Dependency structure — ports and adapters (hexagonal), universally.** Every component sits behind a **port** (a Rust trait); the core (`mailmate-core`) depends **only** on ports and **never names a concrete impl** — no exceptions, **the LLM included**. Each port ships with at least one real **adapter** and a **mock**, and selection happens at the boundary (config or build feature). This is the same `core` / `ports` / `adapter-*` shape the sibling project **[idiolect](https://github.com/nick-tgcs/idiolect)** ships, and it covers not just the obvious engines (AI provider, storage, trainer, Tier-2 classifier) but **every** external or swappable concern: the **mail client** (Thunderbird is one adapter; a headless adapter drives tests), the **IPC transport**, the **clock**, the **secret store**, **embeddings**, **feature extraction**, prompt templates, and the review UI. "Frozen on a mature engine" for the LLM means exactly this — the engine is a *swappable adapter behind `AiProvider`*, not a hard-wired dependency. Enforcement is mechanical, not a style nit: a core module that imports a concrete engine, client, transport, or store is an **architecture-test failure** (idiolect ships precisely this guard as `test-interface-no-backend-leakage`; see *Replaceable Components and Extension Points* for the full seam list and *CI as enforcement for TDD and architecture constraints*).

**Boundary — event-driven.** The extension forwards new-mail events and user actions
(`record_user_action`), and the host answers asynchronously: classification is
*background-queued with push*, and the host emits unsolicited `classification_ready`
`notification` frames over the long-lived `runtime.connectNative` port (see *Native
Messaging Protocol*). The extension registers a notification handler, not only
response correlation — that is genuine event-driven messaging.

**Background — queue-driven.** New mail enters a work queue tracked by
`messages.classification_status` (`pending → processing → done/failed`); a worker
drains it, escalates through the cascade, and pushes results when ready. The queue is
recoverable after restart and has a priority lane. Async happens at the I/O edges; the
storage writer is serialized (see *the embedded backend's execution model* under
*Storage Strategy*). Note the protocol's *single-writer stdout* invariant and the
storage *single-writer* model are unrelated concerns that merely share a name: one is
about not interleaving native-messaging frames, the other about SQLite's write path.

**Trigger sources — three, all reduced to durable state.** Three things start work in
the host: (1) inbound-mail events, (2) user actions, and (3) **time** — a follow-up
step coming due. Time is *not* a new event bus or a daemon: it is the single durable
column `workflow_instances.next_due_at`, polled by an in-host *follow-up scheduler*
worker exactly the way `messages.classification_status` is drained. Because the host
only runs while Thunderbird is open, on startup the scheduler **catches up** — it
reconciles every elapsed due-time (coalescing a backlog into one current follow-up,
never a blast) before resuming steady-state polling. No OS daemon, no event stream, not
event-sourced. Each due step **drives the existing P2 plan/guard path** to emit a
review-required draft; it does not classify and is not a third pipeline (see *Sales
Pipeline and Follow-up Workflows*).

**Core — pipeline/dataflow.** Once a message is in the host, processing is a
deterministic two-stage pipeline (P1 classify → P2 plan/guard; see *Two Pipelines* and
*Action flow*). Stages call each other directly; there is no internal event bus
dispatching between them.

**Explicitly not event-sourced.** There is no generic event stream, no fact lands in
two tables, rule performance (`rule_outcomes` is a *view*, not a stored table) and the
unified message timeline are *views* — not replayed projections — there is no
`decisions` table, and `decision_id` is an ephemeral correlation ID, not an event spine
(see *Capture model: single writer per fact* and *Decision identity*). Event sourcing
is rejected on purpose: it would duplicate the training signal at lower fidelity and
split single-writer ownership. The one CQRS-flavored note: writer-owned canonical
tables vs. read-only SQL views — command/query separation over a relational store, not
CQRS-over-an-event-store.

---

## Component Diagram

```text
+-----------------------------------------------------------------------+
|                              Thunderbird                              |
|                                                                       |
|  +-----------------------------+                                      |
|  | MailMate MailExtension      |                                      |
|  |                             |                                      |
|  | - selected message reader   |                                      |
|  | - new mail listener         |                                      |
|  | - context menus             |                                      |
|  | - draft opener              |                                      |
|  | - tag/move/junk executor    |                                      |
|  +--------------+--------------+                                      |
|                 | Native Messaging JSON                              |
+-----------------|-----------------------------------------------------+
                  |
                  v
+-----------------------------------------------------------------------+
|                         mailmate Rust Native Host                      |
|                                                                       |
|  +-------------------+     +-------------------+                      |
|  | native_protocol   | --> | application       |                      |
|  |                   |     | orchestration     |                      |
|  +-------------------+     +---------+---------+                      |
|                                      |                                |
|           +--------------------------+--------------------------+     |
|           |                          |                          |     |
|           v                          v                          v     |
|  +----------------+        +----------------+        +----------------+|
|  | domain         |        | policy_guard   |        | rule_engine    ||
|  | model          |        | hard safety    |        | principles     ||
|  +----------------+        +----------------+        +----------------+|
|           |                          |                          |     |
|           +--------------------------+--------------------------+     |
|                                      |                                |
|                                      v                                |
|                            +----------------+                         |
|                            | action_planner |                         |
|                            +--------+-------+                         |
|                                     |                                 |
|           +-------------------------+-------------------------+       |
|           |                         |                         |       |
|           v                         v                         v       |
|  +----------------+       +----------------+       +----------------+ |
|  | ai             |       | learning       |       | storage        | |
|  | provider trait |       | engine         |       | StorageBackend | |
|  +-------+--------+       +----------------+       +----------------+ |
|          |                                                            |
|          v                                                            |
|  +----------------+  +----------------------+  +--------------------+ |
|  | Ollama adapter |  | OpenAI-compatible    |  | LM Studio adapter | |
|  |                |  | adapter              |  | llama.cpp adapter | |
|  +----------------+  +----------------------+  +--------------------+ |
|          |                                                            |
|          v                                                            |
|  +----------------+   +-------------------------------+               |
|  | mock provider  |   | burn in-process provider      |               |
|  | for tests      |   | (v1 supported; local-first)   |               |
|  +----------------+   +-------------------------------+               |
+-----------------------------------------------------------------------+
```

Two things the boxes encode deliberately:

- The **storage** box is the `StorageBackend` seam, not an engine. **SQLite is the
  default**; a server engine (Postgres/MariaDB) is opt-in behind the same seam.
- Every provider — including the optional **Burn in-process** provider — sits *behind*
  the `ai provider trait`, never parallel to it. The Rust-native ML substrate
  (`mailmate-ml`, Burn) likewise sits *behind* three traits: the `ai provider trait`
  (optional inference), the Tier-2 `classifier-engine` trait (default), and the
  `TrainerBackend` trait (default). "Trait first, Burn is one impl" — so nothing in the
  core hard-depends on Burn any more than it does on Ollama.
- The **`followup_scheduler`** (in `mailmate-workflow`) is neither a daemon nor a new
  pipeline: it polls the durable `workflow_instances.next_due_at` column while the host
  is alive (catch-up-on-launch) and *drives* `action_planner` (P2) to emit
  review-required follow-up drafts. It sits beside the classification worker, never
  parallel to the two pipelines (see *Sales Pipeline and Follow-up Workflows*).

---

## Module Layout

A possible Rust workspace layout:

```text
mailmate/
  Cargo.toml
  crates/
    mailmate-native-host/
      Cargo.toml
      src/
        main.rs
        native_stdio.rs
        manifest.rs

    mailmate-core/
      Cargo.toml
      src/
        lib.rs
        app.rs
        config.rs
        error.rs
        ids.rs
        time.rs

    mailmate-domain/
      Cargo.toml
      src/
        lib.rs
        message.rs
        thread.rs
        sender.rs
        action.rs
        classification.rs
        draft.rs
        task.rs
        priority.rs
        pipeline.rs              # PipelineItem (deal/quote) + PipelineStage (pure data types)
        workflow.rs              # WorkflowDefinition/Version + WorkflowInstance data + statuses

    mailmate-policy/
      Cargo.toml
      src/
        lib.rs
        guard.rs
        policy.rs
        violation.rs
        override.rs

    mailmate-rules/
      Cargo.toml
      src/
        lib.rs
        classification_rule.rs
        action_rule.rs
        condition.rs
        field_registry.rs
        effect.rs
        engine.rs
        conflict.rs
        version.rs
        explanation.rs
        shadow.rs

    mailmate-learning/
      Cargo.toml
      src/
        lib.rs
        audit.rs
        evidence.rs
        proposal.rs
        curator.rs
        outcome.rs
        feedback.rs

    mailmate-workflow/           # follow-up pipeline tracker (behaviour only; data types live
                                 # in mailmate-domain). Reuses mailmate-rules version/lifecycle.
      Cargo.toml
      src/
        lib.rs
        definition.rs           # WorkflowDefinition/Version lifecycle (reuses rule machinery)
        instance.rs             # WorkflowInstance state machine (the mutable cursor)
        scheduler.rs            # FollowUpScheduler: catch-up-on-launch drain + coalescing guard
        exit_detection.rs       # reply (host-side thread identity) / won-lost exits
        emit.rs                 # builds ActionPlanningInput, DRIVES existing ActionPlanner + PolicyGuard
        repo.rs                 # WorkflowRepository / PipelineItemRepository trait surfaces

    mailmate-training/
      Cargo.toml
      src/
        lib.rs
        examples.rs
        labels.rs
        datasets.rs
        export.rs
        lora.rs
        trainer.rs               # defines the TrainerBackend trait (default impl: Burn, in mailmate-ml)
        trainers/
          mod.rs
          external.rs            # subprocess toolchain backend (pluggable fallback)
          mock.rs               # in-memory trainer for tests (no GPU, no external tool)
        evaluation.rs
        privacy.rs

    mailmate-ai/
      Cargo.toml
      src/
        lib.rs
        provider.rs
        prompt.rs
        schema.rs
        validation.rs
        tasks.rs
        providers/
          mod.rs
          ollama.rs
          openai_compatible.rs
          lm_studio.rs
          llama_cpp.rs
          mock.rs

    mailmate-ml/                 # Rust-native (Burn) ML substrate — feature-gated.
                                 # Depends on the trait-owning crates below and
                                 # implements their traits; never the reverse.
      Cargo.toml
      src/
        lib.rs
        backend.rs               # Burn compute-backend selection (NdArray/burn-flex CPU
                                 # default; WGPU/CUDA/Metal opt-in for training/heavy inference)
        classifier.rs            # default Tier-2 discriminative classifier
                                 # (impl mailmate-rules/-ai Tier2Classifier trait)
        trainer.rs               # default trainer (impl mailmate-training::TrainerBackend)
        inference.rs             # v1-supported in-process AiProvider via burn-lm
        import.rs                # safetensors / .pt / burn-onnx weight import

    mailmate-storage/
      Cargo.toml
      src/
        lib.rs
        backend.rs               # StorageBackend trait + factory (reads [storage] config)
        dialect.rs               # the ONE place engine-specific SQL fragments live
        repositories/            # engine-neutral repository traits + shared impl,
          audit.rs               # ABOVE the backend (the core only sees these)
          feedback.rs
          rules.rs
          messages.rs
          threads.rs
          senders.rs
          drafts.rs
          proposals.rs
          shadow.rs
          pipeline_items.rs      # follow-up pipeline tables (deal, workflow def/version/instance)
          workflow.rs
          followup_feedback.rs
        backends/
          sqlite.rs              # default backend: rusqlite/r2d2/WAL/pragmas live HERE
          postgres.rs            # documented stub (opt-in server backend; built when needed)

    mailmate-audit/
      Cargo.toml
      src/
        lib.rs
        audit_log.rs
        explanation.rs
        redaction.rs

    mailmate-test-support/
      Cargo.toml
      src/
        lib.rs
        fixtures.rs
        mock_provider.rs
        temp_db.rs

  extension/
    manifest.json
    background.js
    native.js
    context_menu.js
    drafts.js
    message_reader.js
    tests/

  migrations/                   # bundled (compiled into the host); applied in-process on startup.
    common/                     # engine-neutral DDL (~90%): tables over TEXT/INTEGER,
      0001_initial.sql          # app-generated prefixed-string PKs, no AUTOINCREMENT/SERIAL
      0002_rule_versions.sql
      0003_audit_and_feedback.sql
      0004_followups.sql        # pipeline_items, workflow_*, followup_feedback, workflow_shadow_outcomes, workflow_conflicts
    sqlite/                     # default overlay: JSON columns as TEXT, view bodies, PRAGMA/FK setup
    postgres/                   # opt-in overlay stub: jsonb columns, server view bodies (added when needed)

  docs/
    architecture.md
    native-messaging.md
    rule-format.md
    provider-interface.md
```

The workspace can start smaller, but these boundaries should remain conceptually separate from the beginning.

---

## Core Domain Model

Core concepts:

- `MailMessage`: normalized representation of a Thunderbird message.
- `MailThread`: group of related messages.
- `SenderProfile`: accumulated metadata and learned facts about a sender.
- `MessageFingerprint`: stable hash-based identity without storing full body by default.
- `ClassificationRule`: explicit principle that sets/overrides labels, scores, or priority in the classification pipeline (P1).
- `ActionRule`: explicit principle that proposes actions (tag/move/junk/require-review) in the action pipeline (P2).
- `RuleVersion`: immutable version of a rule at a point in time.
- `RuleEvidence`: feedback-backed evidence used to propose or support a rule.
- `RuleOutcome`: rule performance, derived as a *view* over per-task feedback and `shadow_outcomes` (not a stored fact).
- `PolicyDecision`: hard safety result from policy guard.
- `ModelSuggestion`: provider-produced structured suggestion.
- `ActionPlan`: proposed set of actions after policy, rules, and AI are reconciled.
- `TaskFeedback`: per-task capture of an AI proposal, the human correction, and the reason — the training source of truth.
- `AuditEntry`: cross-cutting provenance fact (policy block, provider rejection, lifecycle transition).
- `AgentProposal`: AI-generated proposal requiring human review or shadow testing.
- `DraftRecord`: generated draft plus subsequent edit telemetry, without requiring body retention by default.
- `PipelineItem`: a lightweight tracked deal/quote with a `PipelineStage` (open/engaged/won/lost/abandoned), anchored to a thread — not a CRM record.
- `WorkflowDefinition` / `WorkflowDefinitionVersion`: a versioned, shadow-testable, human-curated follow-up *cadence* (ordered day-offset steps) that reuses the rule lifecycle/version/conflict-record machinery — but is a cadence, not a condition→effect rule (see *Sales Pipeline and Follow-up Workflows*).
- `WorkflowInstance`: the running follow-up state machine — the **single durable temporal trigger** (`next_due_at`, `current_step_index`, `status`), polled by the scheduler like `messages.classification_status`.

---

## Data Model

SQLite is the **default, zero-config local-first** storage engine. Storage is reached
only through engine-neutral repository traits over a `StorageBackend` seam (see *Rust
Trait and Interface Definitions* and *Replaceable Components and Extension Points*), so
a server engine (Postgres/MariaDB) can be added as an opt-in **without touching the
core**. The schema is migration-based (a shared `common` core plus per-dialect
overlays) and append-friendly.

### Portable column conventions

The schema deliberately uses a portable lowest-common-denominator so the same tables
work across engines with minimal per-dialect divergence:

- **Primary keys** are app-generated prefixed strings (`msg_…`, `rv_…`, `dec_…` via
  `mailmate-core::ids`), stored as `TEXT`. No `AUTOINCREMENT`/`SERIAL`/`IDENTITY`, so
  identity is engine-independent — and because the id is known *before* insert, the
  schema never needs `RETURNING` (which MySQL lacks and MariaDB only partly supports).
- **Timestamps** are ISO-8601 `TEXT`; **booleans** are `INTEGER 0/1`. (A server engine
  *may* map these to `TIMESTAMPTZ`/`BOOLEAN` in its overlay, but the portable default
  keeps them as TEXT/INTEGER.)
- **`*_json` columns** are opaque application JSON: `TEXT` on SQLite, `jsonb` on
  Postgres, `JSON` on MySQL — the column *type* is the per-dialect part; the value is
  written/read as a whole by Rust, not queried into with engine-specific JSON operators
  unless a view explicitly needs to (see *Engine portability*).
- The store is **append-only / append-friendly** with immutable versions, so it needs
  no upserts today; any future upsert would be a `dialect` fragment, not core logic.

### `messages`

Stores message identity and metadata. Full body storage is disabled by default.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | Internal `msg_...` ID |
| `account_id` | TEXT | Thunderbird account identifier |
| `folder_id` | TEXT | Thunderbird folder identifier |
| `thunderbird_message_id` | TEXT | Adapter-provided ID (not stable across reindex) |
| `rfc_message_id_hash` | TEXT | Hash of RFC `Message-ID` (stable identity/dedup) |
| `thread_id` | TEXT | Internal thread ID (FK to `threads.id`) |
| `sender_email` | TEXT | Readable sender address |
| `sender_domain` | TEXT | Readable sender domain |
| `subject` | TEXT | Readable subject |
| `received_at` | TEXT | ISO timestamp |
| `classification_status` | TEXT | Background-queue state: `pending`, `processing`, `done`, `failed` |
| `body_hash` | TEXT NULL | Hash of canonical body (identity/dedup) |
| `body_retained` | INTEGER | `0` unless user opts into `bodies` retention |
| `body_text` | TEXT NULL | Readable retained body when `body_retained=1` |
| `created_at` | TEXT | Insert timestamp |

### `message_features`

Stores non-body features used for classification and learning.

| Column | Type | Notes |
|---|---:|---|
| `message_id` | TEXT | FK to `messages.id` |
| `feature_name` | TEXT | Example: `has_attachment`, `spf_result` |
| `feature_value` | TEXT | JSON or scalar string |
| `created_at` | TEXT | Insert timestamp |

### Capture model: single writer per fact

MailMate captures every fact in exactly one place. There is **no generic event
stream** that re-records corrections in parallel with structured tables — that
would duplicate the training signal at lower fidelity.

- **Corrections / training signal** → the per-task feedback tables
  (`classification_feedback`, `filing_feedback`, `draft_feedback`,
  `summary_feedback`, `task_extraction_feedback`, `rule_proposal_feedback`,
  `followup_feedback`), each the sole owner of its task's signal, capturing the AI
  proposal, the human correction, and the **reason** for it. (`followup_feedback` owns
  the *cadence/timing* signal only; the follow-up draft's *body* signal stays in
  `draft_feedback`, reached via `draft_id` — no fact in two tables.)
- **Rule performance** (the former `rule_outcomes`), **workflow performance**, and the
  **audit timeline** are *views* over those tables — a view stores nothing, so it cannot
  duplicate.
- Facts that are genuinely not task-feedback get their own dedicated owner table:
  `rule_conflicts` (conflicts), `shadow_outcomes` (rules that fired in shadow),
  `workflow_shadow_outcomes` (shadow follow-up steps — separate because they have no
  triggering message), and a narrow `audit_log` for cross-cutting provenance with no
  other home.
- **Operational state** (not a fact-about-an-event) lives in `pipeline_items` and
  `workflow_instances`; the latter is the one mutable cursor, with every transition and
  fired step appended to `audit_log` so its history is reconstructable.

No event type lands in two tables.

### `audit_log`

Append-only timeline for cross-cutting provenance that is *not* task feedback.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `audit_...` |
| `event_type` | TEXT | `action_applied`, `action_blocked_by_policy`, `provider_response_rejected`, rule lifecycle transitions, follow-up events (`followup_step_fired`, `followup_coalesced`, `followup_needs_attention`, `pipeline_item_stage_changed`), etc. |
| `message_id` | TEXT NULL | Related message |
| `thread_id` | TEXT NULL | Related thread |
| `rule_kind` | TEXT NULL | `classification` or `action` when a rule is referenced |
| `rule_id` | TEXT NULL | Related rule |
| `rule_version_id` | TEXT NULL | Related rule version |
| `proposal_id` | TEXT NULL | Related proposal |
| `actor` | TEXT | `Actor`: `user`, `system`, `ai`, `extension`, `import` |
| `payload_json` | TEXT | Event-specific data |
| `created_at` | TEXT | Event timestamp |

The unified "everything that happened to this message" timeline is a **view** that
unions the per-task feedback tables and `audit_log` ordered by timestamp.

### `classification_rules` / `action_rules`

The two rule types live in **fully separate tables** with identical schema (the
columns below) plus their own type-specific effect vocabulary. The same applies to
their version tables (`classification_rule_versions` / `action_rule_versions`). Two
schemas, one shared mechanism: the condition evaluator, version-immutability,
lifecycle state machine, and conflict detector are common Rust code operating over a
trait both rule types implement. The columns below describe either table.

Stores current rule metadata.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `rule_...` |
| `stable_name` | TEXT | Human-readable unique-ish name |
| `scope` | TEXT | `global`, `account`, `folder`, `sender`, `domain` |
| `status` | TEXT | Rule lifecycle status |
| `current_version_id` | TEXT | FK to `rule_versions.id` |
| `created_by` | TEXT | `Actor`: `user`, `ai`, `import` |
| `created_at` | TEXT | Creation timestamp |
| `updated_at` | TEXT | Update timestamp |

Rule statuses:

- `draft`
- `pending_human_review`
- `shadow_mode`
- `active`
- `disabled`
- `retired`
- `rejected`

### `classification_rule_versions` / `action_rule_versions`

Immutable content for each rule revision (one version table per rule type; shared
columns below). The `effect_json` vocabulary differs by type: classification effects
(set label, adjust score, set priority) vs. action effects (tag, move, junk, suggest,
require-review).

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `rv_...` |
| `rule_id` | TEXT | FK to `rules.id` |
| `version_number` | INTEGER | Monotonic per rule |
| `title` | TEXT | Human title |
| `description` | TEXT | Explanation |
| `condition_json` | TEXT | Structured condition tree |
| `effect_json` | TEXT | Structured actions/classification effects |
| `priority` | INTEGER | Rule ordering within hierarchy band |
| `confidence_threshold` | REAL NULL | Optional threshold |
| `risk_level` | TEXT | `low`, `medium`, `high`, `critical` |
| `created_by` | TEXT | `Actor`: `user`, `ai` |
| `change_reason` | TEXT | Why version exists |
| `created_at` | TEXT | Creation timestamp |

### `rule_evidence`

Connects rules/proposals to observed events.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `evid_...` |
| `rule_kind` | TEXT NULL | `classification` or `action` (disambiguates `rule_id` across the two rule tables) |
| `rule_id` | TEXT NULL | Rule supported by evidence |
| `proposal_id` | TEXT NULL | Proposal supported by evidence |
| `source_kind` | TEXT | Which feedback table the evidence row lives in (`filing`, `classification`, …) |
| `source_id` | TEXT | ID of the supporting per-task feedback row |
| `message_id` | TEXT NULL | Related message |
| `evidence_kind` | TEXT | `positive`, `negative`, `counterexample`, `override` |
| `weight` | REAL | Evidence strength |
| `summary` | TEXT | Redacted explanation |
| `created_at` | TEXT | Creation timestamp |

### `rule_outcomes` (view)

Rule performance is **not** a stored table — it is a view, so it cannot drift from
or duplicate the source feedback. Active-rule performance is derived from the
per-task feedback tables (joined on `matched_rule_id` / `source_rule_version_id`);
shadow-rule performance comes from `shadow_outcomes`. The view exposes, per rule
version: fire count, applied count, `user_feedback` distribution
(`accepted`/`undone`/`ignored`/`edited`), and `PolicyOutcome`
(`allowed`/`requires_review`/`blocked`) breakdown.

> **Engine-portability of views.** `CREATE VIEW` exists on every engine, but a view's
> *body* is the least portable SQL in the system: the `user_feedback`/`PolicyOutcome`
> distributions aggregate over plain columns (portable), but the unified message
> timeline `UNION`s heterogeneous tables (type-affinity/`UNION` rules differ across
> engines), and any view that reaches *inside* a `*_json` column needs engine-specific
> JSON SQL. View DDL therefore lives in the per-dialect migration overlay, and the
> honest fallback for the gnarlier projections is to **compute them in Rust over plain
> row fetches** rather than as a DB view — accepting that "it's just a view" partly
> dissolves once a second engine is supported. This applies equally to the unified
> timeline view and the export-time training views.

### `rule_conflicts`

Records conflict detection output.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `conf_...` |
| `rule_kind` | TEXT | `classification` or `action` — conflicts are same-kind-only (the two effect spaces are disjoint), and this disambiguates which rule table the IDs reference |
| `rule_a_id` | TEXT | First rule |
| `rule_b_id` | TEXT | Second rule |
| `conflict_kind` | TEXT | `contradictory_effect`, `overlap`, `unsafe_escalation` |
| `severity` | TEXT | `low`, `medium`, `high` |
| `description` | TEXT | Explanation |
| `status` | TEXT | `open`, `resolved`, `ignored` |
| `created_at` | TEXT | Timestamp |
| `resolved_at` | TEXT NULL | Timestamp |

### `agent_proposals`

Stores AI-generated proposals.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `prop_...` |
| `proposal_type` | TEXT | `new_rule`, `refine_rule`, `merge_rules`, etc. |
| `status` | TEXT | `draft`, `pending_review`, `accepted`, `rejected`, `shadowing` |
| `source_provider` | TEXT | Provider adapter name |
| `prompt_hash` | TEXT | Prompt trace without storing raw prompt if disabled |
| `response_hash` | TEXT | Response trace |
| `proposal_json` | TEXT | Structured proposal |
| `rationale` | TEXT | Redacted explanation |
| `risk_level` | TEXT | Risk estimate |
| `created_at` | TEXT | Timestamp |
| `reviewed_at` | TEXT NULL | Timestamp |

### `sender_profiles`

Stores sender-level learning without storing raw personal data by default.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `sender_...` |
| `email` | TEXT | Readable email address |
| `domain` | TEXT | Readable domain |
| `display_name` | TEXT NULL | Readable display name |
| `trust_level` | TEXT | `unknown`, `trusted`, `suspicious`, `blocked` |
| `last_seen_at` | TEXT | Timestamp |
| `feature_json` | TEXT | Aggregate features |
| `created_at` | TEXT | Timestamp |
| `updated_at` | TEXT | Timestamp |

### `draft_edit_history`

Records draft lifecycle and edits as learning signal.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `draft_edit_...` |
| `draft_id` | TEXT | FK to `drafts.id` |
| `message_id` | TEXT | Source message |
| `event_type` | TEXT | `generated`, `edited`, `sent`, `discarded` |
| `edit_summary` | TEXT | Summary of edit |
| `full_diff_text` | TEXT NULL | Readable edit diff when retained |
| `created_at` | TEXT | Timestamp |

### `threads`

Thread-level state. Thread identity is computed host-side from RFC `References` /
`In-Reply-To` headers (not Thunderbird's thread id).

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `thread_...` |
| `account_id` | TEXT | Account |
| `subject_root_normalized` | TEXT | Normalized root subject |
| `participant_domains` | TEXT | JSON, readable |
| `message_count` | INTEGER | Messages in thread |
| `first_seen_at` | TEXT | Timestamp |
| `last_seen_at` | TEXT | Timestamp |
| `last_summary` | TEXT NULL | Populated at `summaries`+ retention |
| `last_summarized_at` | TEXT NULL | Timestamp |
| `created_at` | TEXT | Timestamp |

### `drafts`

Operational record behind a generated draft; `draft_edit_history` is its child.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `draft_...` |
| `message_id` | TEXT NULL | Source message replied to; **NULL for a scheduled follow-up** (anchored instead to `thread_id` + the pipeline item's `anchor_message_id`) |
| `thread_id` | TEXT NULL | FK to `threads.id` |
| `provider_id` | TEXT | Provider that generated it |
| `prompt_template_version` | TEXT | Versioned prompt used |
| `subject` | TEXT | Draft subject |
| `body` | TEXT | Readable (generated locally; small; needed for the edit learning signal) |
| `requires_review` | INTEGER | Always `1` per Drafting Safety |
| `safety_flags_json` | TEXT | Draft-validator flags |
| `status` | TEXT | `generated`, `edited`, `sent`, `discarded` |
| `created_at` | TEXT | Timestamp |
| `updated_at` | TEXT | Timestamp |

### Follow-up pipeline tables

These four tables own the sales-pipeline follow-up feature (see *Sales Pipeline and
Follow-up Workflows*). `workflow_definitions`/`*_versions` mirror the rule tables
(versioned, lifecycle-managed); `workflow_instances` is the one **mutable-state** row
in an otherwise append-only model — see the note under its table.

#### `pipeline_items`

The tracked deal/quote. Deliberately minimal — a tracker, not a CRM (no contacts,
line-items, or forecasting).

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `pli_...` |
| `account_id` | TEXT | Account |
| `thread_id` | TEXT | FK to `threads.id` (the outbound quote/proposal thread) |
| `anchor_message_id` | TEXT NULL | FK to `messages.id` (the sent quote, when known) |
| `counterparty_email` | TEXT | Readable; who we follow up with |
| `counterparty_domain` | TEXT | Readable |
| `title` | TEXT | "Acme — 40-lane SCO quote" |
| `item_type` | TEXT | shared enum: `quote`, `proposal` |
| `stage` | TEXT | `open`, `engaged`, `won`, `lost`, `abandoned` |
| `amount_hint` | TEXT NULL | Display-only; **not** a forecast field |
| `last_activity_at` | TEXT | Timestamp |
| `created_by` | TEXT | `Actor` (`user` — never `ai`) |
| `created_at` | TEXT | Timestamp |
| `updated_at` | TEXT | Timestamp |

Indexes: `(thread_id)`, `(account_id, stage)`, `(counterparty_domain)`, `(last_activity_at)`.

#### `workflow_definitions`

Current metadata for a versioned follow-up cadence (mirrors `classification_rules`).

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `wfd_...` |
| `stable_name` | TEXT | "standard-quote-follow-up" |
| `scope` | TEXT | reuse `RuleScope`: `global`/`account`/`domain`/`sender` |
| `applies_to_item_type` | TEXT | shared `item_type` enum (`quote`/`proposal`) |
| `status` | TEXT | **reuse the rule lifecycle statuses** (`draft`…`active`…`retired`) |
| `current_version_id` | TEXT | FK to `workflow_definition_versions.id` |
| `created_by` | TEXT | `Actor`: `user`, `ai` (curator may propose; activation needs review) |
| `created_at` | TEXT | Timestamp |
| `updated_at` | TEXT | Timestamp |

Indexes: `(status)`, `(scope)`.

#### `workflow_definition_versions`

Immutable cadence content (mirrors `*_rule_versions`).

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `wfdv_...` |
| `workflow_id` | TEXT | FK to `workflow_definitions.id` |
| `version_number` | INTEGER | Monotonic, immutable |
| `title` | TEXT | Human title |
| `description` | TEXT | Explanation |
| `anchor` | TEXT | `quote_sent_at`, `last_outbound_at`, `item_created_at` |
| `enrollment_condition_json` | TEXT NULL | Optional JSON-AST condition for auto-suggesting enrollment (reuses the rule condition AST) |
| `steps_json` | TEXT | Ordered `[{step_index, offset_days, draft_intent, prompt_template_ref, forbidden_commitments}]` — `offset_days` is **absolute from the anchor**, not "prior step + N" |
| `exit_conditions_json` | TEXT | `reply_received`, `won`, `lost`, `user_cancel`, `max_steps` |
| `staleness_json` | TEXT | `{coalesce: true, abandon_horizon_days: N}` (see *Catch-up + staleness*) |
| `risk_level` | TEXT | `low`/`medium`/`high` (follow-up drafting is `medium` by default) |
| `created_by` | TEXT | `Actor` |
| `change_reason` | TEXT | Why this version exists |
| `created_at` | TEXT | Timestamp |

Index: `(workflow_id)`.

#### `workflow_instances`

The running state machine — **the durable temporal trigger** and the one mutable-state
row.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `wfi_...` |
| `pipeline_item_id` | TEXT | FK to `pipeline_items.id` |
| `workflow_id` | TEXT | FK to `workflow_definitions.id` |
| `pinned_def_version_id` | TEXT | FK to `workflow_definition_versions.id` — **canonical** version pin, immutable for the instance's life |
| `thread_id` | TEXT | FK to `threads.id` (reply-exit lookup) |
| `anchor_at` | TEXT | The resolved anchor timestamp the offsets count from |
| `status` | TEXT | FSM: `active`, `awaiting_review`, `engaged`, `snoozed`, `needs_attention`, `completed`, `cancelled` |
| `current_step_index` | INTEGER | Cursor: the next step to fire |
| `next_due_at` | TEXT NULL | The trigger. **Invariant: non-NULL iff `status ∈ {active, snoozed}`** |
| `created_at` | TEXT | Timestamp |
| `updated_at` | TEXT | Timestamp |

**Indexes (ship with migration):** `(status, next_due_at)` *(the scheduler drain — the
`messages(classification_status)` analogue, the load-bearing index)*, `(thread_id, status)`
*(reply-exit)*, `(pipeline_item_id)`.

> **Mutable-state exception.** `workflow_instances` is a *cursor over immutable content*:
> the cadence lives in immutable `workflow_definition_versions` (pinned by
> `pinned_def_version_id`), and every transition + every fired step appends an
> `audit_log` row, so the full history is reconstructable. "Step N fired at T" lives in
> `audit_log` **only** (the instance carries no separate fired-step columns) — the same
> reasoning that justifies the mutable `messages.classification_status` column. This is
> the single exception to the otherwise append-only/immutable-version store.

### Per-task feedback tables (training source of truth)

Each AI function owns one self-contained table holding its own provenance
(`pinned_versions_json`), the AI proposal, the human correction, and a **prompted
reason**. They are the sole source for training datasets (Issue: derive-on-export).
A correction prompts the user for "why" (task-specific chips + optional freetext);
the prompt fires on **divergence/override**, not on acceptance.

#### `classification_feedback` — junk / phishing / priority / labels

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `clsfb_...` |
| `message_id` | TEXT | Message |
| `pinned_versions_json` | TEXT | Rule/prompt/model/calibration versions that produced the prediction |
| `ai_label` | TEXT | `junk`, `not_junk`, `phishing`, … |
| `ai_score` | REAL NULL | Model confidence |
| `ai_rationale` | TEXT NULL | What the model keyed on |
| `human_label` | TEXT | Corrected label |
| `human_reason_code` | TEXT NULL | `subscribed_list`, `known_sender`, `transactional`, `prior_thread`, … |
| `human_reason_text` | TEXT NULL | Freeform fallback |
| `salient_features_json` | TEXT | Features that mattered (spf, sender history, list-id) |
| `polarity` | TEXT | `positive` (AI right) / `negative` (AI wrong) |
| `created_at` | TEXT | Timestamp |

#### `filing_feedback` — folder suggestion / move

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `filfb_...` |
| `message_id` | TEXT | Message |
| `pinned_versions_json` | TEXT | Provenance |
| `ai_suggested_folder` | TEXT NULL | Suggested folder |
| `human_chosen_folder` | TEXT | Folder the user chose |
| `basis` | TEXT NULL | `sender`, `domain`, `subject_keyword`, `thread`, `list_id` |
| `matched_rule_id` | TEXT NULL | Rule that fired, if any |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

#### `draft_feedback` — reply generation

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `drffb_...` |
| `draft_id` | TEXT | FK to `drafts.id` |
| `message_id` | TEXT | Source message |
| `pinned_versions_json` | TEXT | Provenance |
| `ai_body` | TEXT | Generated body |
| `final_body` | TEXT NULL | What was actually sent |
| `outcome` | TEXT | `sent_asis`, `sent_minor_edit`, `sent_major_edit`, `discarded` |
| `edit_categories_json` | TEXT | `[shortened, tone, removed_commitment, added_fact, fixed_name]` |
| `unsafe_flags_json` | TEXT | Safety flags |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

#### `summary_feedback` — thread summaries

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `sumfb_...` |
| `thread_id` | TEXT | Thread summarized |
| `pinned_versions_json` | TEXT | Provenance |
| `ai_summary` | TEXT | Generated summary |
| `outcome` | TEXT | `accepted`, `regenerated`, `edited`, `flagged_missing_detail` |
| `human_reason_code` | TEXT NULL | Why corrected |
| `human_reason_text` | TEXT NULL | Freeform |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

#### `task_extraction_feedback` — extracted tasks

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `tskfb_...` |
| `message_id` | TEXT | Source message |
| `pinned_versions_json` | TEXT | Provenance |
| `ai_tasks_json` | TEXT | Extracted tasks |
| `human_tasks_json` | TEXT NULL | Corrected tasks |
| `outcome` | TEXT | `kept`, `edited`, `deleted` |
| `human_reason_code` | TEXT NULL | Why corrected |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

#### `rule_proposal_feedback` — curator proposals

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `rpffb_...` |
| `proposal_id` | TEXT | FK to `agent_proposals.id` |
| `pinned_versions_json` | TEXT | Provenance |
| `outcome` | TEXT | `accepted`, `accepted_with_edits`, `rejected`, `disabled_later` |
| `human_reason_code` | TEXT NULL | Why |
| `human_reason_text` | TEXT NULL | Freeform |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

#### `followup_feedback` — follow-up cadence / timing / stop

Sole owner of the **cadence/timing/stop** signal. It does **not** re-encode the
draft's send disposition (`sent_asis`/`minor`/`major`/`discarded`) — that fact is
owned by `draft_feedback`, reached via `draft_id`. A follow-up step thus produces *two*
rows with disjoint ownership: a `draft_feedback` row (was the draft *body* good?) and a
`followup_feedback` row (was the *timing/decision to send at all* good?). No fact in
two tables.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `flwfb_...` |
| `workflow_instance_id` | TEXT | FK to `workflow_instances.id` |
| `pipeline_item_id` | TEXT | FK to `pipeline_items.id` |
| `step_index` | INTEGER | Which cadence step |
| `draft_id` | TEXT NULL | FK to `drafts.id` (the send disposition lives in `draft_feedback`) |
| `pinned_versions_json` | TEXT | Provenance copy-at-event (canonical pin is `workflow_instances.pinned_def_version_id`) |
| `ai_scheduled_offset_days` | INTEGER | What the cadence scheduled |
| `actual_offset_days` | INTEGER NULL | When the user actually followed up (off-cadence signal) |
| `reply_received_before_step` | INTEGER | 0/1 — a reply pre-empted this step |
| `reply_latency_days` | INTEGER NULL | Days from anchor to reply, when known |
| `outcome` | TEXT | cadence-only: `surfaced_for_review`, `rescheduled`, `snoozed`, `step_skipped_coalesced`, `workflow_stopped`, `expired_needs_attention`, `manual_followup_off_cadence` |
| `coalesced_from_json` | TEXT NULL | Step indexes collapsed into this one by the staleness guard |
| `human_reason_code` | TEXT NULL | Why (chips) |
| `human_reason_text` | TEXT NULL | Freeform |
| `polarity` | TEXT | `positive` / `negative` |
| `created_at` | TEXT | Timestamp |

Indexes: `(workflow_instance_id)`, `(pipeline_item_id)`, `(created_at)`. The "why"
prompt fires on **divergence/override** (reschedule/skip/stop), not on a plain send.

### `shadow_outcomes`

Rules that fired in shadow mode but never surfaced to the user (so there is no
feedback row). Sole owner of shadow performance data.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `shad_...` |
| `rule_kind` | TEXT | `classification` or `action` (disambiguates the rule FKs) |
| `rule_id` | TEXT | Rule fired |
| `rule_version_id` | TEXT | Exact version |
| `message_id` | TEXT | Message evaluated |
| `would_have_action_json` | TEXT | Action it would have proposed |
| `would_have_policy_outcome` | TEXT | `PolicyOutcome` if it had run |
| `matched_later_user_action` | INTEGER NULL | Did the user later do the same manually? |
| `created_at` | TEXT | Timestamp |

### `workflow_shadow_outcomes`

Shadow follow-up *steps* that would have fired but never surfaced. This is a **separate
table from `shadow_outcomes`**, not a reuse: `shadow_outcomes.message_id` is NOT NULL
because a shadow rule is always triggered by a message, but a shadow follow-up step is
triggered by *time* and has no message at fire time — so it cannot honour that column.
Sole owner of shadow follow-up performance.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `wsho_...` |
| `workflow_id` | TEXT | Shadow workflow that would have fired |
| `workflow_version_id` | TEXT | Exact version |
| `pipeline_item_id` | TEXT | The item it was shadow-running on |
| `thread_id` | TEXT | Thread (no `message_id` — there is no triggering message) |
| `step_index` | INTEGER | Which step |
| `would_fire_at` | TEXT | When the step would have surfaced a draft |
| `reply_before_fire` | INTEGER | 0/1 — a reply had already arrived (the step would have been redundant) |
| `matched_manual_followup_within_days` | INTEGER NULL | The user manually followed up within ±window of `would_fire_at` |
| `created_at` | TEXT | Timestamp |

> **What workflow shadow-mode measures (and its honest limit).** Rule shadow precision
> asks "did the user later do the same thing" (`matched_later_user_action`). A follow-up
> cadence cannot be evaluated that cleanly: the true counterfactual — *would the user
> have sent the drafted follow-up that a shadow run never produced?* — is **unobservable**,
> because in the shadow world no draft exists to accept or reject. So workflow shadow
> measures a **weaker but observable** proxy: per would-fire step, did a reply arrive
> first (`reply_before_fire` → the step would have been wasted) and did the user manually
> follow up near `would_fire_at` (`matched_manual_followup_within_days` → the cadence
> matches real behaviour). A promotion report aggregates these into a *cadence-fit* score
> (manual-followup alignment) net of reply-pre-emption — and the report explicitly states
> it is a behavioural-alignment estimate, not a send-acceptance rate.

### `workflow_conflicts`

Two active workflows on one `pipeline_item` is a **containment** conflict, not the
AST/effect-overlap that `rule_conflicts` records — so it gets its **own owner table** (in
`0004_followups`), not a `workflow` kind on `rule_conflicts` (whose `rule_kind` /
`rule_a_id`/`rule_b_id` / overlap `conflict_kind` vocabulary does not fit a containment
check). This mirrors the `workflow_shadow_outcomes`-vs-`shadow_outcomes` split above, and
`WorkflowEngine::detect_conflicts` already returns the distinct `WorkflowConflict` type.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `wcf_...` |
| `pipeline_item_id` | TEXT | The item both workflows target |
| `workflow_a_id` | TEXT | First workflow (or active instance) |
| `workflow_b_id` | TEXT | Second workflow proposed/armed on the same item |
| `conflict_kind` | TEXT | e.g. `concurrent_active_workflow` |
| `status` | TEXT | `open` / `resolved` (human-reviewed) |
| `detected_at` | TEXT | Timestamp |

---

## Rust Trait and Interface Definitions

These definitions are illustrative. They should be refined during implementation, but the architectural boundaries should remain stable.

### AI provider trait

The provider exposes **one primitive**: send a prompt plus an expected schema, get
validated structured output back. Task semantics (classify/draft/summarize/extract/
propose/curate) do **not** live on the provider — they live in `mailmate-ai::tasks`
as typed functions that build versioned prompts and validate task schemas. This
keeps adapters thin and identical, lets a new task be added in one file with zero
adapter changes, and keeps prompt templates versioned in one place.

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn capabilities(&self) -> ProviderCapabilities; // grammar? json_schema? function_call? max_ctx

    /// The only required method. The adapter enforces the schema the strongest
    /// way it can (GBNF grammar, native json_schema/function-calling, or
    /// prompt-and-repair) and reports the guarantee level via `capabilities()`.
    async fn complete_structured(
        &self,
        request: StructuredRequest,
    ) -> Result<StructuredResponse, AiProviderError>;
}
```

`StructuredRequest` carries `messages` (system/user), an optional
`json_schema`/`grammar`, and sampling params. `StructuredResponse` carries raw
text, the parsed JSON, and which schema validated it. Validation failures become
`audit_log` "provider-validation rejection" rows and never drive actions.

**Per-provider structured-output enforcement is real specialization and lives in the
primitive:** llama.cpp / LM Studio compile a GBNF grammar from the schema
(guaranteed-valid output); openai_compatible uses native `response_format:
json_schema` / function-calling; ollama uses `format: json`; weak backends fall back
to prompt-and-parse-and-repair.

**Task-level override is an optional, currently-unimplemented seam.** No current
provider has a task-specialized backend, so we build none — but if one ever does, it
implements a `Native*` trait and the task-layer dispatch picks it up with zero
restructuring:

```rust
// Implement ONLY if a provider has a genuinely task-specialized backend.
#[async_trait]
pub trait NativeClassifier: AiProvider {
    async fn classify(&self, req: ClassifyEmailRequest)
        -> Result<ClassifyEmailResponse, AiProviderError>;
}
```

The task layer dispatches native-if-present, else the generic versioned path:

```rust
pub async fn classify_email(p: &dyn AiProvider, req: ClassifyEmailRequest)
    -> Result<ClassifyEmailResponse, TaskError>
{
    if let Some(n) = p.as_native_classifier() { return Ok(n.classify(req).await?); }
    let prompt = templates::classify_email(version, &req);          // versioned template
    let out = p.complete_structured(StructuredRequest {
        messages: prompt, schema: Some(schemas::CLASSIFY), ..Default::default()
    }).await?;
    validate_and_parse::<ClassifyEmailResponse>(out)
}
```

No caller outside `mailmate-ai::providers::ollama` should know about Ollama-specific APIs, model names, keep-alive settings, endpoint paths, or response formats.

### Provider registry

```rust
pub struct ProviderRegistry {
    providers: std::collections::HashMap<ProviderId, Box<dyn AiProvider>>,
}

impl ProviderRegistry {
    pub fn register(&mut self, provider: Box<dyn AiProvider>) {
        self.providers.insert(provider.id(), provider);
    }

    pub fn get(&self, id: &ProviderId) -> Option<&dyn AiProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }
}
```

### Rule engine trait

```rust
#[async_trait]
pub trait RuleEngine: Send + Sync {
    async fn evaluate(
        &self,
        context: RuleEvaluationContext,
    ) -> Result<RuleEvaluationResult, RuleEngineError>;

    async fn explain(
        &self,
        decision_id: DecisionId,
    ) -> Result<DecisionExplanation, RuleEngineError>;

    async fn detect_conflicts(
        &self,
        candidate: RuleDraft,
    ) -> Result<Vec<RuleConflict>, RuleEngineError>;
}
```

### Policy guard trait

```rust
#[async_trait]
pub trait PolicyGuard: Send + Sync {
    async fn evaluate_action_plan(
        &self,
        context: PolicyContext,
        plan: ActionPlan,
    ) -> Result<GuardedActionPlan, PolicyError>;

    async fn evaluate_rule_activation(
        &self,
        rule: RuleVersion,
    ) -> Result<RuleActivationDecision, PolicyError>;
}
```

### Learning engine trait

```rust
#[async_trait]
pub trait LearningEngine: Send + Sync {
    /// Capture a task-shaped correction (AI proposal + human correction + reason)
    /// into its per-task feedback table — the single owner of that fact.
    async fn record_feedback(
        &self,
        feedback: TaskFeedback,
    ) -> Result<FeedbackId, LearningError>;

    /// Record a cross-cutting provenance fact with no other home.
    async fn record_audit(
        &self,
        entry: AuditEntry,
    ) -> Result<AuditId, LearningError>;

    async fn collect_evidence(
        &self,
        query: EvidenceQuery,
    ) -> Result<Vec<RuleEvidence>, LearningError>;

    async fn propose_candidates(
        &self,
        trigger: ProposalTrigger,
    ) -> Result<Vec<AgentProposal>, LearningError>;
}
```

There is no `record_outcome`: rule performance (`RuleOutcome`) is a *view* derived
from the feedback tables and `shadow_outcomes`, so it is queried, never written.

### Action planner trait

```rust
#[async_trait]
pub trait ActionPlanner: Send + Sync {
    async fn plan(
        &self,
        input: ActionPlanningInput,
    ) -> Result<ActionPlan, ActionPlanningError>;
}
```

`ActionPlanningInput` carries a `trigger: TriggerKind` (`NewMail` | `FollowUpDue`). The
planner stays message-keyed: a `FollowUpDue` trigger supplies the pipeline item's
`thread_id` and `anchor_message_id`, so the resulting draft/explanation/audit rows key
off the anchor message exactly like a reply would. No second planner is introduced.

### Workflow engine and follow-up scheduler traits

The follow-up feature adds three traits in `mailmate-workflow`. They **drive** the
existing `ActionPlanner`/`PolicyGuard` — they do not plan or guard themselves.

```rust
#[async_trait]
pub trait WorkflowEngine: Send + Sync {
    /// Arm a pipeline item on a workflow: pin the def version, compute the first next_due_at.
    async fn arm(&self, item: PipelineItemId, workflow: WorkflowDefId)
        -> Result<WorkflowInstanceId, WorkflowError>;
    /// Two active workflows on one item is a CONTAINMENT conflict, not AST overlap.
    async fn detect_conflicts(&self, candidate: WorkflowDraft)
        -> Result<Vec<WorkflowConflict>, WorkflowError>;
    async fn explain(&self, instance: WorkflowInstanceId)
        -> Result<DecisionExplanation, WorkflowError>; // mirrors RuleEngine::explain
}

#[async_trait]
pub trait FollowUpScheduler: Send + Sync {
    /// Poll `workflow_instances WHERE status IN ('active','snoozed') AND next_due_at <= now`
    /// via the (status, next_due_at) index, apply the coalescing/staleness guard, and
    /// DRIVE P2 to emit [CreateDraft, RequireReview]. NOT a daemon — app.rs calls it on
    /// start (catch-up) and on a periodic tick while the host is alive.
    async fn drain_due(&self, now: Timestamp) -> Result<DrainReport, WorkflowError>;
    /// Restart recovery: reset rows orphaned mid-fire (idempotency key = (instance, step_index)).
    async fn recover(&self) -> Result<(), WorkflowError>;
}

#[async_trait]
pub trait ExitDetector: Send + Sync {
    /// Reply (matched by host-side thread identity) → `engaged`; won/lost → `completed`.
    async fn on_inbound_or_deal_event(&self, item: PipelineItemId, event: ExitEvent)
        -> Result<Vec<WorkflowInstanceId>, WorkflowError>;
}
```

A `WorkflowDefinition` **reuses** the rule version-immutability, lifecycle state set
(`RuleStatus`), and human-review gating — but it is a *cadence*, not a condition→effect
rule, so the condition evaluator and the AST-overlap conflict detector do **not** apply
to the cadence content. Only the optional `enrollment_condition_json` (which quotes to
suggest enrolling) is a JSON-AST condition; workflow conflict (`WorkflowConflict`) is the
distinct "don't run two active workflows on the same item" containment check.

### Storage repository traits

```rust
/// One repository per rule kind (classification vs. action) over a shared
/// generic — two tables, one mechanism.
#[async_trait]
pub trait RuleRepository<R: RuleKind>: Send + Sync {
    async fn get_active_rules(&self, scope: RuleScope) -> Result<Vec<R::Rule>, StorageError>;
    async fn get_shadow_rules(&self, scope: RuleScope) -> Result<Vec<R::Rule>, StorageError>;
    async fn save_rule_draft(&self, draft: R::Draft) -> Result<RuleId, StorageError>;
    async fn create_rule_version(&self, version: R::NewVersion) -> Result<RuleVersionId, StorageError>;
    async fn update_rule_status(&self, rule_id: RuleId, status: RuleStatus) -> Result<(), StorageError>;
}

/// Append-only audit timeline (cross-cutting provenance only — never corrections).
#[async_trait]
pub trait AuditRepository: Send + Sync {
    async fn append(&self, entry: AuditEntry) -> Result<AuditId, StorageError>;
    async fn query(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, StorageError>;
}

/// One typed repository per task-feedback table (classification, filing, draft,
/// summary, task extraction, rule proposal) — each the sole writer of its fact.
#[async_trait]
pub trait FeedbackRepository<F: TaskFeedbackKind>: Send + Sync {
    async fn append(&self, row: F::Row) -> Result<FeedbackId, StorageError>;
    async fn query(&self, query: F::Query) -> Result<Vec<F::Row>, StorageError>;
}
```

`followup_feedback` plugs into this **existing** generic as a new `FollowUpFeedbackKind`
— no new feedback mechanism. `WorkflowRepository` and `PipelineItemRepository` are new
engine-neutral repository traits over the same `StorageBackend` seam (the scheduler never
sees a `Connection` or SQL), and `WorkflowDefinition` reuses `RuleRepository`'s
version/lifecycle methods as a third kind.

### Storage backend seam

The repository traits above are the **only** storage surface the core sees — no
`Connection`, `Row`, transaction handle, or SQL string crosses that line. Beneath the
repositories sits one engine seam so the engine can be swapped (SQLite default →
Postgres/MariaDB opt-in) without touching the core:

```rust
/// Owns the connection pool and transactions; knows which dialect it is. The embedded
/// SQLite backend satisfies the async contract by running blocking `rusqlite` work on
/// a pool; a networked backend (sqlx / tokio-postgres) is async-native. HOW each
/// backend satisfies `async` is private to the backend — the seam only promises async.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn dialect(&self) -> Dialect;                        // Sqlite | Postgres | MySql
    async fn begin(&self) -> Result<Box<dyn Tx>, StorageError>;
    async fn run_migrations(&self) -> Result<(), StorageError>; // common + per-dialect overlay
}
```

Every divergent SQL fragment — JSON column type and (if a view needs it) JSON
extraction, view bodies, boolean/timestamp literals, FK/pragma setup, and *should the
schema ever need them* upsert and `RETURNING` — lives in one `dialect` module, never
scattered through the repositories. Because the current schema is append-only with
app-generated prefixed-string IDs, it needs no DB-generated keys, upserts, or
`RETURNING` today; that is the main reason the seam stays thin.

### Trainer backend trait

Fine-tuning weight-crunching sits behind one trait so the default (in-process Burn) can
be swapped for an external toolchain or a remote trainer. The trait is owned by
`mailmate-training`; the Burn impl lives in `mailmate-ml`, the subprocess impl in
`mailmate-training::trainers::external`, and a mock in `…::trainers::mock` for tests
(so no test needs a GPU or an external tool).

```rust
#[async_trait]
pub trait TrainerBackend: Send + Sync {
    fn id(&self) -> TrainerId;
    fn capabilities(&self) -> TrainerCapabilities; // sft? preference? lora? on_device?
    async fn train(&self, job: TrainingJob) -> Result<AdapterArtifact, TrainerError>;
}
```

`capabilities()` is how the honest Burn limits surface in code rather than prose: the
Burn backend reports `on_device: true, sft: true` but advertises `lora` only if/when a
low-rank adapter is actually implemented on Burn primitives — so the planner never
assumes on-device LoRA exists.

### Classifier-engine trait

The cascade's Tier-2 model is also a seam. The default impl is a small Burn-trained
**discriminative** classifier; an online-logistic-regression impl is retained as a
lightweight alternative. (Semantic *embeddings/encoders* are a separate seam, not the
Tier-2 gate.) Either impl thresholds on **calibrated** confidence — the gate is only as
good as that calibration, and `calibration_version` is versioned like everything else.

```rust
#[async_trait]
pub trait Tier2Classifier: Send + Sync {
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError>;
    async fn update(&self, labeled: LabeledExample) -> Result<(), MlError>; // online or batch
}
```

### Cross-cutting ports (mail client, transport, clock, secrets, features)

The domain traits above are not the whole abstraction surface. Per the hexagonal law
(*Architectural Style and Event Model*), **every** external or swappable concern is a
port too — so the core never names a concrete client, wire, clock, or secret backend
(the LLM is already covered by `AiProvider` above). Each ships with at least one real
adapter and a mock:

```rust
#[async_trait]
pub trait MailClient: Send + Sync {        // Thunderbird is ONE adapter; a headless adapter drives tests
    async fn apply(&self, action: MailAction) -> Result<(), MailError>;          // move/tag/junk/read/flag
    async fn create_draft(&self, spec: DraftSpec) -> Result<DraftId, MailError>; // persists only — never sends
    async fn fetch(&self, id: MessageId, scope: FetchScope) -> Result<MessageData, MailError>;
    fn events(&self) -> EventStream<MailEvent>;                                  // new-mail + user actions
}

pub trait Transport: Send + Sync {         // native-messaging stdio is ONE adapter; in-process is another
    fn send(&self, frame: Frame) -> Result<(), TransportError>;
    fn incoming(&self) -> FrameStream;
}

pub trait Clock: Send + Sync {             // system wall-clock by default; a fake clock makes scheduler tests deterministic
    fn now(&self) -> Timestamp;
}

#[async_trait]
pub trait SecretStore: Send + Sync {       // 0600 file (default) | OS keychain | env (dev) | mock
    async fn get(&self, key: SecretKey) -> Result<Option<Secret>, SecretError>;
    async fn put(&self, key: SecretKey, value: Secret) -> Result<(), SecretError>;
}

pub trait FeatureExtractor: Send + Sync {  // deterministic; a fixture adapter feeds crystallization back-tests
    fn extract(&self, msg: &MessageData) -> FeatureVector;                       // pure → message_features
}
```

Together with `AiProvider`, `RuleEngine`, `PolicyGuard`, `ActionPlanner`,
`LearningEngine`, `WorkflowEngine` / `FollowUpScheduler` / `ExitDetector`,
`Tier2Classifier`, `TrainerBackend`, and `StorageBackend` + the repository traits, these
make the dependency rule total: **no `mailmate-core` module imports a concrete engine,
mail client, transport, clock, or secret store.** idiolect consolidates such ports in a
single `idiolect-ports` crate (a structure MailMate may adopt; today each trait is owned
by its domain crate) and enforces the rule with an interface-no-backend-leakage test —
MailMate does the same (see *Testing Strategy*, *CI as enforcement for TDD and
architecture constraints*).

---

## Replaceable Components and Extension Points

Every component — **without exception, the LLM included** — is a **port (trait) + a
config-selected concrete adapter + a default + a mock/test adapter**; the core depends
only on the port and **never names a concrete impl** (the hexagonal law from
*Architectural Style and Event Model*). This is what lets a better model or engine be
dropped in — or SQLite replaced by MariaDB, or Thunderbird replaced by another mail
client — without rewriting the core. The full seam list (the engines, **and** the
cross-cutting mail-client / transport / clock / secret-store / feature-extraction
ports):

| Seam | Trait / boundary | Selected by | Default impl | Pluggable alternatives |
|---|---|---|---|---|
| AI provider *(generative foundation model — run **frozen**)* | `AiProvider` (+ optional `NativeClassifier`, `SupportsAdapters`) | `[ai] default_provider` + `ProviderRegistry` | mature local engine — **Ollama / llama.cpp** (frozen, idiolect-style) | OpenAI-compatible, LM Studio, mock, **Burn in-process** `burn-lm` *(optional pure-Rust generative path; v0.0.1, Llama 3.x/TinyLlama, no GGUF — mature engine preferred for model breadth)* |
| Storage engine | repository traits over `StorageBackend` + `dialect` | `[storage] engine` | embedded **SQLite** *(zero-config)* | Postgres / MariaDB *(documented stub → opt-in; needs a running daemon + URL)* |
| Trainer backend *(learned layer — Burn-committed)* | `TrainerBackend` (idiolect `ml-core` contracts + `trainer-burn` adapter shape) | `[training] backend` | **in-process Burn, pinned** *(trains small classifier/preference/tone models today — idiolect-proven; on-device LoRA is a deferred optional target on Burn primitives)* | external toolchain (subprocess), remote, mock |
| Tier-2 classifier | `Tier2Classifier` | `[ml] tier2_engine` | **Burn discriminative classifier** | online logistic regression (lightweight), other |
| Rule condition language | declarative JSON AST | fixed (closed decision) | JSON AST | — |
| Embeddings | embeddings trait *(existing seam, listed for completeness)* | `[ml]` | *(deferred; Burn encoder when added)* | provider-hosted embeddings |
| Prompt templates | versioned `prompt_templates` *(existing seam, listed for completeness)* | DB-versioned, task layer | seeded templates | curator-proposed revisions |
| Follow-up scheduler | `FollowUpScheduler` (+ `WorkflowEngine`, `ExitDetector`) | started by `app.rs`; `[followup] poll_interval_secs` | in-host polling worker *(catch-up-on-launch)* | mock scheduler for tests — *no OS-daemon impl by design (runtime is catch-up-on-launch)* |
| Review UI surface | a **client** of `mailmate-core`'s query/command API (the same surface the native-messaging host relays) — a frontend, not a backend trait | build feature / launcher | **native `egui` companion** (`mailmate review` subcommand of the one binary) | in-Thunderbird WebExtension page *(thin launcher / quick-approve)*, **Tauri** webview *(web-grade UI, capability-scoped `ipc://localhost`)*; TOML/DB audit substrate beneath all; loopback-in-browser explicitly rejected (see *Open Design Questions*) |
| **Mail client** | `MailClient` (+ headless mock) | build/runtime adapter | **Thunderbird** WebExtension + native-messaging host | any MailExtension-capable client; headless test adapter; IMAP-direct or another client as a future adapter — *core is client-agnostic* |
| **IPC transport** | `Transport` | host wiring | **native-messaging stdio** (`runtime.connectNative`) | in-process channel (tests), local socket — *wire swappable; framing is versioned* |
| **Secret store** | `SecretStore` | `[secrets] backend` | **0600 config-dir file** | OS keychain (libsecret / macOS Keychain / Windows Credential Mgr), env override (dev), mock |
| **Clock / time source** | `Clock` | injected | **system wall-clock** | fake/controllable clock — deterministic follow-up-scheduler + crystallization back-test tests |
| **Feature extractor** | `FeatureExtractor` | fixed (deterministic) | built-in extractor → `message_features` | fixture extractor for back-tests — *output pure + versioned* |

Selection is config-driven. Config keys are owned by `mailmate-core::config`
(`config.rs`) and grouped by seam:

```toml
[storage]
engine = "sqlite"        # default; "postgres" | "mysql" select a server backend
path = "mailmate.db"     # embedded engine (XOR `url` for a networked engine)
# url = "postgres://user@host/mailmate"
pool_size = 4

[ml]
tier2_engine = "burn"    # "burn" (default) | "logreg"
backend = "ndarray"      # Burn compute backend: "ndarray"/"burn-flex" (CPU, default) | "wgpu" | "cuda" | "metal"

[training]
backend = "burn"         # "burn" (default, in-process) | "external" | "remote"

[secrets]
backend = "file"         # 0600 config-dir file (default) | "keychain" | "env" (dev override)

[followup]
poll_interval_secs = 60  # in-host scheduler tick; also runs a catch-up sweep on startup
# there is intentionally NO daemon/OS-timer option — runtime is catch-up-on-launch
```

The `[ai]` block (provider selection, including `kind = "burn"`) is shown under
*Provider-Abstraction Layer*. Local providers remain a non-dependency: the core never
hard-requires Ollama **or** Burn — both are impls behind `AiProvider`, feature-gated.
This is what keeps the Non-Goals ("hard-coding one AI provider", "making Ollama a core
architectural dependency") true even with Burn as the *default* ML substrate: default
≠ hard dependency, because selection happens at the trait boundary.

---

## Native Messaging Protocol

Thunderbird and the Rust host communicate with JSON over native messaging. The protocol should be versioned from day one.

Classification runs **background-queued with push** (not synchronous): the extension
forwards new-mail events, the host classifies in the background (cheap classifier
first, LLM only on escalation), and **pushes** results back when ready. The transport
supports this natively — `runtime.connectNative` is a long-lived port and the host may
write at any time — so the envelope carries a `kind` discriminator from day one:
`request`, `response`, and host-initiated `notification`. By default only **new mail**
is classified going forward; classifying an existing folder is an explicit
user-initiated action.

### Request envelope

```json
{
  "protocol_version": "1.0",
  "kind": "request",
  "request_id": "req_01HZY...",
  "type": "classify_message",
  "payload": {}
}
```

### Response envelope

```json
{
  "protocol_version": "1.0",
  "kind": "response",
  "request_id": "req_01HZY...",
  "status": "ok",
  "payload": {}
}
```

### Notification envelope (host → extension, unsolicited)

```json
{
  "protocol_version": "1.0",
  "kind": "notification",
  "notification_id": "ntf_01HZY...",
  "type": "classification_ready",
  "payload": {
    "thunderbird_message_id": "tb_123",
    "classification": { },
    "guarded_plan": { }
  }
}
```

The extension registers a notification handler (not only response correlation) and
updates its UI out-of-band when `classification_ready` arrives. On arrival the host
applies only policy-`allowed` low-risk actions (e.g. a tag); `requires_review` actions
are surfaced as suggestions the user confirms.

### Error response

```json
{
  "protocol_version": "1.0",
  "kind": "response",
  "request_id": "req_01HZY...",
  "status": "error",
  "error": {
    "code": "policy_blocked",
    "message": "The requested action is blocked by MailMate policy.",
    "details": {
      "policy_id": "never_auto_delete_mail"
    }
  }
}
```

### `classify_message` request

```json
{
  "protocol_version": "1.0",
  "kind": "request",
  "request_id": "req_classify_001",
  "type": "classify_message",
  "payload": {
    "thunderbird_message_id": "tb_123",
    "account_id": "acct_default",
    "folder_id": "inbox",
    "headers": {
      "from": "Example Sender <sender@example.com>",
      "to": ["user@example.net"],
      "subject": "Invoice update",
      "date": "2026-01-15T10:30:00Z",
      "message_id": "<abc123@example.com>",
      "references": ["<prev1@example.com>", "<prev2@example.com>"],
      "in_reply_to": "<prev2@example.com>"
    },
    "body_text": "Optional snippet or full text depending on privacy settings",
    "body_retention_allowed": false,
    "remote_content_loaded": false,
    "attachments": [
      {
        "filename": "invoice.pdf",
        "content_type": "application/pdf",
        "size_bytes": 234567
      }
    ]
  }
}
```

### `classify_message` response

```json
{
  "protocol_version": "1.0",
  "kind": "response",
  "request_id": "req_classify_001",
  "status": "ok",
  "payload": {
    "decision_id": "dec_001",
    "classification": {
      "spam_score": 0.12,
      "phishing_score": 0.04,
      "priority": "normal",
      "labels": ["invoice", "needs_review"]
    },
    "suggested_actions": [
      {
        "action_id": "act_001",
        "kind": "tag",
        "tag": "needs-review",
        "policy_outcome": "allowed"
      }
    ],
    "blocked_actions": [],
    "explanation": {
      "summary": "Tagged as needs-review because a human-approved invoice rule matched.",
      "fired_rules": ["rule_invoice_review"],
      "policy_checks": ["never_auto_delete_mail", "financial_security_legal_move_requires_review"],
      "provider_suggestion_used": true
    }
  }
}
```

### `record_user_action` request

```json
{
  "protocol_version": "1.0",
  "kind": "request",
  "request_id": "req_event_001",
  "type": "record_user_action",
  "payload": {
    "event_type": "message_moved",
    "thunderbird_message_id": "tb_123",
    "from_folder_id": "inbox",
    "to_folder_id": "Receipts/Software",
    "user_initiated": true,
    "occurred_at": "2026-01-15T10:35:00Z"
  }
}
```

`record_user_action` is the wire carrier for `UserCorrection`s and observed user
behavior. The host routes each `event_type` to its single owner: corrections of an AI
suggestion land in the matching per-task feedback table (a manual move →
`filing_feedback`, spam/not-spam → `classification_feedback`, including the prompted
reason when the user supplies one); pure provenance facts land in `audit_log`. Nothing
is written to two places.

### `draft_reply` request

```json
{
  "protocol_version": "1.0",
  "kind": "request",
  "request_id": "req_draft_001",
  "type": "draft_reply",
  "payload": {
    "thread_id": "thread_123",
    "message_ids": ["tb_123", "tb_124"],
    "user_instruction": "Politely ask for the corrected invoice.",
    "forbidden_commitments": ["dates", "prices", "payment_changes", "legal_positions"]
  }
}
```

### `draft_reply` response

```json
{
  "protocol_version": "1.0",
  "kind": "response",
  "request_id": "req_draft_001",
  "status": "ok",
  "payload": {
    "draft_id": "draft_001",
    "subject": "Re: Invoice update",
    "body": "Hi,\n\nThanks for sending this over. Could you please send the corrected invoice when you have a chance?\n\nBest,",
    "safety_notes": [
      "No dates, prices, payment changes, legal positions, or promises were added."
    ],
    "requires_human_review": true
  }
}
```

### `review_rule_proposal` request

```json
{
  "protocol_version": "1.0",
  "kind": "request",
  "request_id": "req_rule_review_001",
  "type": "review_rule_proposal",
  "payload": {
    "proposal_id": "prop_001",
    "decision": "accept_for_shadow_mode",
    "edited_rule": {
      "title": "File software receipts",
      "condition": {
        "all": [
          { "field": "sender_domain", "op": "in", "value": ["github.com", "stripe.com"] },
          { "field": "subject", "op": "contains_any", "value": ["receipt", "invoice"] }
        ]
      },
      "effect": {
        "move": "Receipts/Software",
        "tag": "receipt"
      }
    }
  }
}
```

### Follow-up frames

These reuse the existing envelope (`kind` + a new `type`); they are **additive**, so
`protocol_version` stays `"1.0"`. Reply and won/lost are **not** new request types —
they ride the existing `record_user_action`, which the host's router now also matches
against tracked threads.

#### `followup_draft_ready` notification (host → extension)

Surfaced when a step fires (including on catch-up at launch). It is a review-required
draft, **never sent on arrival** — it mirrors `classification_ready`.

```json
{
  "protocol_version": "1.0",
  "kind": "notification",
  "notification_id": "ntf_01HZY...",
  "type": "followup_draft_ready",
  "payload": {
    "workflow_instance_id": "wfi_001",
    "pipeline_item_id": "pli_001",
    "thread_id": "thread_123",
    "step_index": 2,
    "coalesced_from_step_indexes": [1],
    "draft": { "draft_id": "draft_900", "subject": "Re: Acme quote", "requires_review": true },
    "guarded_plan": { "actions": [{ "kind": "create_draft", "policy_outcome": "requires_review" }] },
    "explanation": { "summary": "Day-14 follow-up on the Acme quote (no reply since day 7)." }
  }
}
```

#### `followup_needs_attention` notification (host → extension)

No draft — the item went stale past the abandon horizon.

```json
{
  "protocol_version": "1.0",
  "kind": "notification",
  "notification_id": "ntf_01HZZ...",
  "type": "followup_needs_attention",
  "payload": { "workflow_instance_id": "wfi_002", "reason": "stale_past_horizon", "skipped_step_indexes": [2, 3] }
}
```

#### Follow-up control requests (extension → host)

| `type` | Purpose |
|---|---|
| `enroll_pipeline_item` | Tag a quote/proposal → create a `pipeline_item` and arm a workflow |
| `update_pipeline_stage` | Mark `won` / `lost` (closes the sequence) |
| `cancel_sequence` | Stop the workflow instance |
| `reschedule_followup` / `snooze` | Push `next_due_at` out |
| `review_followup` | Resolve a surfaced draft: `send` (user-confirmed) / `edit` / `skip` |

A reply landing on a tracked thread, and folder/move events, continue to ride
`record_user_action`; the host routes won/lost/snooze/skip into the workflow tables and
`followup_feedback` (its single owner), never two places.

---

## Rule and Principle System

Rules are MailMate’s explicit principles. They should be human-readable and machine-evaluable.

A rule has:

- Identity.
- Status.
- Version history.
- Scope.
- Conditions.
- Effects.
- Priority.
- Risk level.
- Evidence.
- Outcomes.
- Explanation text.
- Owner/source.
- Created and updated timestamps.

Rules should be represented as structured JSON internally and rendered as readable text in the UI.

### Condition language: committed to a JSON AST

The rule condition language is a **declarative JSON AST** (`all`/`any`/`not` combinators
over typed `field`/`op`/`value` predicates) — not Rhai, CEL, or any embedded scripting
language. This is decisive for the product's differentiators: conflict detection needs
a constrained, analyzable form to reason about overlap/contradiction; immutable
versioning needs a diffable tree; explanations and the UI editor render from a tree;
and AI-curator-proposed rules must be safe to validate without executing model-authored
code. (This closes the former Open Question on rule condition language.)

- **Typed, per-pipeline field registry.** Each field has a type (string / string-set /
  int / bool / datetime / enum) so operator validity is checked and the editor can be
  generated. The field vocabulary is **scoped by pipeline**: `ClassificationRule`
  conditions reference raw message/features only; `ActionRule` conditions may also
  reference `classification.*` (labels/scores/priority), which exist by P2.
- **Operators:** `eq`, `in`, `contains`, `contains_any`, `contains_all`, `gt`/`gte`/
  `lt`/`lte`, `before`/`after` (datetime), `exists`; combinators `all`/`any`/`not`.
- **Regex:** `matches_regex` is allowed, backed by the Rust `regex` crate (RE2-style,
  linear-time — no ReDoS), but any regex-bearing condition is **opaque to conflict
  detection** (it participates in exact-duplicate detection only, not overlap reasoning).

### Example rule: safe tagging

```json
{
  "title": "Tag likely software receipts",
  "description": "Messages from known software vendors with receipt-like subjects should be tagged as receipts.",
  "scope": "global",
  "status": "active",
  "risk_level": "low",
  "condition": {
    "all": [
      { "field": "sender_domain", "op": "in", "value": ["github.com", "stripe.com", "figma.com"] },
      { "field": "subject_normalized", "op": "contains_any", "value": ["receipt", "invoice", "payment"] }
    ]
  },
  "effect": {
    "tag": ["receipt"],
    "priority": "normal"
  }
}
```

### Example rule: financial review requirement

```json
{
  "title": "Require review for financial messages",
  "description": "Financial messages should not be moved automatically unless a specific human hard rule allows it.",
  "scope": "global",
  "status": "active",
  "risk_level": "high",
  "condition": {
    "any": [
      { "field": "classification.labels", "op": "contains", "value": "financial" },
      { "field": "subject_normalized", "op": "contains_any", "value": ["bank", "payment", "wire", "invoice", "tax"] }
    ]
  },
  "effect": {
    "require_review_for": ["move", "mark_junk"]
  }
}
```

### Example shadow rule

```json
{
  "title": "Suggest moving project status updates",
  "description": "Project status updates from recurring senders may belong in the Projects folder.",
  "scope": "account",
  "status": "shadow_mode",
  "risk_level": "medium",
  "condition": {
    "all": [
      { "field": "subject_normalized", "op": "contains_any", "value": ["weekly update", "status update"] },
      { "field": "sender_seen_count", "op": ">=", "value": 5 }
    ]
  },
  "effect": {
    "move": "Projects/Updates"
  }
}
```

In shadow mode, this rule records what it would have done without applying the action.

---

## Rule Lifecycle

MailMate supports this rule lifecycle:

```text
observed pattern
      |
      v
candidate rule proposed
      |
      v
shadow tested
      |
      v
human review
      |
      v
active rule
      |
      v
monitored
      |
      +-----------------------------+
      |                             |
      v                             v
refined / disabled / retired     overridden
```

### Lifecycle states

| Status | Meaning |
|---|---|
| `draft` | Early candidate not ready for review. |
| `pending_human_review` | Candidate awaits user approval/edit/rejection. |
| `shadow_mode` | Rule is evaluated and logged but does not apply actions. |
| `active` | Rule can affect action planning, subject to policy guard. |
| `disabled` | Rule remains stored but never fires. |
| `retired` | Rule is obsolete and preserved for audit/history. |
| `rejected` | Candidate was rejected by a human and should not be reproposed without new evidence. |

### Lifecycle transitions

| From | To | Actor | Requirements |
|---|---|---|---|
| observed pattern | candidate rule proposed | AI agent or learning engine | Evidence threshold met. |
| draft | pending human review | AI agent or system | Proposal is valid and explainable. |
| pending human review | rejected | Human | Optional reason recorded. |
| pending human review | shadow_mode | Human | Rule passes validation and policy activation check. |
| pending human review | active | Human | Low-risk rule or explicit approval. |
| shadow_mode | active | Human | Shadow outcomes reviewed. |
| active | disabled | Human or safety system | Reason recorded. |
| active | retired | Human or curator proposal plus approval | Replacement or stale evidence. |
| any non-final | rejected | Human | Reason recorded. |

Dangerous or high-risk rules should normally pass through shadow mode before activation.

---

## Two Pipelines

MailMate runs two distinct pipelines in sequence. *Classifying* a message ("what is
this email") is prediction from features + model; *planning actions* ("what should we
do about it") is a ranked rule decision gated by policy. These are different kinds of
work and must not share one ranked ladder.

```text
feature extraction (deterministic)
        |
        v
PIPELINE 1 — CLASSIFY / UNDERSTAND
  cheap local classifier (features -> prelim scores)            [see Cascade]
  classification rules (human label-overrides) may short-circuit
  LLM classify task — only if needed/ambiguous
  reconcile -> Classification { labels, scores, priority, provenance }
        |
        v
PIPELINE 2 — PLAN / GUARD
  action rules over (message, Classification, features), ranked by hierarchy
  action planner -> candidate ActionPlan
  policy guard -> GuardedActionPlan (allowed/requires_review/blocked)
        |
        v
capture: classification_feedback (P1) / filing_feedback, draft_feedback, … (P2)
```

**A third trigger for Pipeline 2 — not a third pipeline.** A due `WorkflowInstance`
step does **not** classify (P1 is untouched); it assembles a P2 `ActionPlanningInput`
(`trigger = FollowUpDue`) for the item's thread and drives the **existing** action
planner → policy guard, which emits `[CreateDraft, RequireReview]`. This is the same
shape as a new-mail classification result driving P2 today — a new *trigger*, not a new
ranked classification ladder.

```text
TIME TRIGGER (scheduler: workflow_instances.next_due_at due)
        |
        v
  [ no P1 ]  ->  PIPELINE 2 — PLAN / GUARD (existing action planner + policy guard)
        |
        v
  [CreateDraft, RequireReview]  ->  followup_draft_ready  (review-required; never auto-sent)
```

There are **two rule types**, in fully separate tables, that reuse one shared
condition evaluator, lifecycle state machine, and conflict detector:

- **`ClassificationRule`** (P1) — effects: set/override label, adjust score, set
  priority. A rule does one job in one pipeline; "label financial AND require review"
  is one `ClassificationRule` (sets the label) plus one `ActionRule` (keys off it).
- **`ActionRule`** (P2) — effects: tag, move, junk, suggest, require-review.

## Classification Cascade

P1's front end is a three-tier cascade so the LLM runs only when cheap local stages
are unsure. This cuts latency (no multi-second local-LLM call per message), compute
(the background queue does not melt the machine), and egress (cheap stages run on
local features; content reaches a model only on escalation — and if a *remote* provider
was opted into, only genuinely-ambiguous messages' content would ever leave).

```text
Tier 1  deterministic signals + ClassificationRules
        SPF/DKIM/DMARC, list-id, sender-in-contacts, allow/block lists, label rules.
        Zero training, available day one.   confident? -> accept
Tier 2  Tier2Classifier engine + calibration table over features
        default: small Burn discriminative classifier (logreg is a lightweight
        alternative); trained from classification_feedback.   confident? -> accept
Tier 3  LLM classify task   (also the always-path for draft/summarize/extract)
```

- **Tier 2 is a `Tier2Classifier` engine behind a trait** (see *Rust Trait and
  Interface Definitions*), defaulting to a small **Burn-trained discriminative
  classifier** with a calibration table; an **online logistic regression** impl is kept
  as a lightweight alternative (naive Bayes was rejected for poor calibration). Burn is
  production-shaped for training and serving small discriminative classifiers in-process
  today — the full `Learner` loop (AdamW, schedulers, checkpointing) with pure-Rust,
  single-binary CPU deployment (NdArray/burn-flex) and optional GPU. Whatever the
  engine, the gate thresholds on **calibrated** confidence, and the cascade is only as
  good as that calibration. (`calibration_version` is versioned like everything else.)
- **Escalation = versioned confidence bands**, conservative at cold-start (the Tier-2
  model is untrained at install, so the band is wide → escalate often → and the
  escalation rate falls automatically as `classification_feedback` accumulates). The
  virtuous loop is **deterministic-first**: corrections first **crystallize into model-free
  Tier-1 rules** (that trait then leaves the model path entirely — see *Learning Loop →
  Crystallization*); only the irreducible residual that resists a deterministic rule trains
  a **better Tier-2 model**; and both together mean **fewer Tier-3 LLM calls**. Coverage of
  the model-free layer grows monotonically while model inference retreats to the genuinely
  novel.
- **Asymmetric thresholds for safety-critical labels.** Phishing especially: a cheap
  "looks fine" is dangerous, so the cheap stage may clear a message only with strong
  signals; anything borderline escalates. "Clearly safe" needs high confidence;
  "possibly phishing" escalates readily.

## Rule Hierarchy and Decision Order

The two pipelines have **two distinct precedence orders**.

### Classification precedence (P1)

1. Human hard label-rules (allow/block lists, "sender X is never spam").
2. Learned classification rules.
3. Model prediction (the baseline; rules override it).

### Action hierarchy (P2)

This is the original hierarchy, now scoped to **actions only** — it does not produce
classifications:

1. **System safety rules**
   - Built-in safety invariants.
   - Not editable into unsafe states.

2. **Human hard rules**
   - Explicit user rules that define strong preferences.
   - Still constrained by system safety rules.

3. **Human-approved learned rules**
   - Learned from behavior and approved by the user.

4. **Agent-proposed draft/shadow rules**
   - Can be evaluated for evidence.
   - Cannot perform risky actions directly.

5. **AI provider suggestion**
   - Advisory input only.
   - Must be structured and validated.
   - Never bypasses policy or active rules.

6. **Default fallback behavior**
   - Conservative behavior when no rule applies.
   - Prefer suggestions and explanations over automatic actions.

This hierarchy makes MailMate predictable. AI suggestions are useful, but they are not the final authority.

---

## Policy Guard Design

The policy guard is separate from the rule engine. Rules can suggest actions; the policy guard decides whether those actions are allowed, blocked, or require review.

### Required hard policies

| Policy ID | Requirement |
|---|---|
| `never_auto_delete_mail` | MailMate must never auto-delete mail. |
| `never_auto_send_drafts` | MailMate must never auto-send drafts. |
| `never_open_links` | MailMate must never open links from email. |
| `never_download_remote_content_for_classification` | MailMate must never download remote content for classification. |
| `never_auto_trust_payment_detail_changes` | MailMate must never automatically trust payment-detail changes. |
| `financial_security_legal_move_requires_review` | MailMate must never move financial, security, or legal emails without review unless explicitly allowed by a human hard rule. |
| `manual_user_override_wins` | Manual user override always wins, except where it would require MailMate to perform a prohibited action like auto-send or auto-delete. |

### Policy outcomes

```rust
pub enum PolicyOutcome {
    Allowed,
    RequiresReview { reason: String },
    Blocked { policy_id: String, reason: String },
}
```

This is the **one** canonical vocabulary for the concept everywhere — domain, storage,
and the protocol. Wire form is snake_case: `allowed` / `requires_review` / `blocked`.
The protocol carries `policy_outcome` on each action; the old
`requires_human_confirmation` boolean is removed (`requires_review` *is* "needs
confirmation").

### Guarded action plan

```rust
pub struct GuardedActionPlan {
    pub decision_id: DecisionId,
    pub allowed_actions: Vec<PlannedAction>,
    pub review_required_actions: Vec<PlannedAction>,
    pub blocked_actions: Vec<BlockedAction>,
    pub policy_checks: Vec<PolicyCheckResult>,
}
```

### Policy examples

- `Tag("receipt")` for a low-risk receipt classification: **allowed**.
- `Move("Receipts")` for an ordinary software receipt with an explicit human rule: **allowed**.
- `Move("Archive")` for a bank security alert without explicit allowance: **requires_review**.
- `delete_message`: **blocked**.
- `send_draft`: **blocked**.
- `open_link`: **blocked**.

### Follow-ups and the send floor

A scheduled follow-up adds **no new hard policy** and no new send path. A fired step
emits only a `CreateDraft` action whose draft is `requires_review = 1` (there is no
`send_draft` output anywhere in the follow-up path), so `never_auto_send_drafts` already
covers it — the existing floor is sufficient. Suppressing an *unwanted* follow-up (the
counterparty already replied, the deal is won/lost, or the item is very stale) is handled
by **workflow exit conditions and the staleness/coalescing guard** (see *Sales Pipeline
and Follow-up Workflows*), not by new policies; `manual_user_override_wins` is unchanged
and still cannot force a prohibited action.

---

## Learning Loop

The learning loop turns user behavior into explicit, reviewable rules.

```text
User action or system decision
        |
        v
Per-task feedback row captured (proposal + correction + reason)
or audit entry appended
        |
        v
Evidence aggregation over feedback tables
        |
        v
Pattern detection
        |
        v
Candidate rule proposal
        |
        v
Validation + conflict detection + policy risk check
        |
        v
Human review or shadow testing
        |
        v
Rule activation/refinement/rejection
        |
        v
Outcome monitoring
        |
        v
Further refinement or retirement
```

### Evidence sources

MailMate should use these events as learning evidence:

- User repeatedly moves similar messages to the same folder.
- User repeatedly tags similar messages.
- User marks similar messages as spam or not spam.
- User edits generated drafts in consistent ways.
- User discards drafts for a category of message.
- User overrides an automatic or suggested action.
- User undoes an action.
- User accepts or rejects rule proposals.
- User reschedules, snoozes, skips, or stops a follow-up step (cadence-timing signal).
- User manually follows up off-cadence (timing signal).
- A reply lands before a follow-up step fires (stop-condition signal).
- User repeatedly enrolls similar quotes in a follow-up workflow (enrollment signal).

### Proposal thresholds

Rules should not be proposed after a single event except when the user explicitly chooses “learn this filing action.”

Possible thresholds:

- 3 similar manual moves from same domain to same folder.
- 5 similar tags across multiple senders.
- 2 high-confidence phishing corrections.
- Multiple draft edits with the same stylistic correction.

Thresholds themselves can become configurable principles.

### Crystallization: a learned trait becomes a model-free rule

The loop's **terminal product is a deterministic, model-free rule** — not a better model
weight. This is the determinism-first invariant from *Goals* made mechanical. A model
(Tier-2 classifier, Tier-3 LLM, or a LoRA) is a **teacher**: it helps *discover* a
candidate and it *covers what no rule has learned yet*. It is never the steady-state
*executor* of a trait the system has already learned — once a trait is learned, deciding
it again must cost **zero inference**.

**The promotion gate (discovery → crystallization):**

1. **Express the candidate deterministically.** Pattern detection emits the candidate as a
   **JSON-AST condition over deterministic features only** (headers, addresses, list-id,
   auth results, attachment kinds, thread shape, counterparty history, normalized-subject
   keywords/regex — the `message_features` surface; see *Condition language*). No clause may
   call a model. A pattern that cannot be written this way is **not** a crystallization
   candidate (see *the honest boundary* below).
2. **Back-test it against history.** Replay the candidate over the recorded decisions it
   claims to explain (the same substrate as *Shadow evaluation* and *Simulation tests
   against historical examples*). It is eligible only if it **reproduces the user's actual
   past decisions at a precision bar** with enough support — the rule must *match the trait*,
   not merely correlate with it.
3. **Promote through the existing gate.** An eligible candidate runs the normal *Rule
   Lifecycle* (draft → shadow → human-approved → active). On activation it is a first-class
   learned rule that, by *Rule Hierarchy and Decision Order*, **outranks any model
   prediction**.
4. **The trait leaves the model path.** From activation on, that trait is decided by AST
   evaluation over deterministic features: **no LLM, no classifier inference**, offline,
   same input → same output, audit-replayable, and it still fires with **zero providers
   configured**. The model is no longer consulted for it.

**Monotonic coverage.** The model tiers are invoked only on the **not-yet-learned
residual**, and every such invocation is *captured as evidence* (per-task feedback) that can
crystallize the next rule. So the model-free layer's coverage grows while model inference
retreats toward the genuinely novel — the cascade's escalation rate falling as
`classification_feedback` accrues (see *Classification Cascade*) is the same effect seen
from the runtime side.

**The honest boundary (no silent approximation).** Some traits are irreducibly semantic —
their signal does not survive reduction to deterministic features (e.g. "this is a veiled
escalation," "this tone is off"). For these:

- A deterministic **surrogate** (sender + structure + keyword/regex + thread shape) is
  crystallized **only if it clears the same historical precision bar**; its uncovered
  residual still escalates. The surrogate is honest *because the back-test proved it*, not
  because it reads plausibly.
- If no deterministic surrogate clears the bar, the trait is **not** crystallized. It stays
  **model-assisted** and is **not** counted as a model-free learned trait — MailMate does not
  fake determinism by shipping a low-precision rule. That residual remains the Tier-2/Tier-3
  frontier.
- A **frozen Tier-2 model is deterministic-but-opaque** (same input → same output, but no
  readable rationale, and still *a model*). It is the **fallback for what resists a symbolic
  rule**, never the preferred home of a trait that *could* be expressed as one. The gold
  terminal state is the auditable AST rule; the frozen model is the consolation when
  reduction fails.

**No new machinery.** Crystallization reuses what already exists — the JSON-AST *Condition
language*, the *Rule Lifecycle*, `shadow_outcomes` back-testing, immutable rule versioning,
and the learned-rule-over-model precedence. What this section adds is the **guarantee** (a
learned trait's terminal form is model-free) and the **promotion gate** (deterministic
expressibility + a historical precision bar) that enforces it.

---

## Self-Iteration Learning Model

MailMate should not be a static rules app with occasional AI calls. It should include a local learning model that continuously evaluates how well MailMate is helping and uses that evidence to improve rules, prompts, thresholds, provider choices, and UI suggestions.

This is **self-iteration**, not unsafe self-modification. MailMate should not rewrite executable code, silently change hard policies, or secretly activate new behavior. Instead, it should improve through measured proposals that pass through validation, shadow testing, and human review.

### What “learning model” means in MailMate

MailMate should use several learning layers, each with different risk and review requirements:

1. **Event-derived behavioral model**
   - Learns from moves, tags, spam corrections, draft edits, accepted suggestions, ignored suggestions, undos, and rule reviews.
   - Produces features, statistics, clusters, and evidence.
   - Does not directly perform actions.

2. **Rule proposal model**
   - Converts repeated behavior patterns into candidate explicit rules.
   - Produces human-readable rationales and evidence links.
   - Routes candidates through draft, review, and shadow states.

3. **Outcome/scoring model**
   - Measures whether rules and suggestions were useful.
   - Tracks precision, undo rate, ignore rate, edit distance, time-to-action, and policy blocks.
   - Recommends refinement, retirement, threshold changes, or more shadow testing.

4. **Prompt and provider evaluation model**
   - Compares provider outputs against user outcomes and rule outcomes.
   - Learns which prompt templates and providers perform best for each task type.
   - Can propose prompt-template changes, but changes should be versioned and testable like rules.

5. **Preference/profile model**
   - Learns stable user preferences, such as draft tone, filing style, priority patterns, and tolerated automation level.
   - Represents preferences as explicit profile entries or rules, not hidden weights only.

### Self-iteration loop

```text
Run MailMate decision
        |
        v
Record suggested action, explanation, provider output, rules, and policy result
        |
        v
Observe user response: accept, ignore, edit, undo, override, reject
        |
        v
Score outcome and attach evidence to rules/prompts/providers
        |
        v
Detect improvement opportunity
        |
        +--> rule proposal/refinement
        |
        +--> threshold adjustment proposal
        |
        +--> prompt-template revision proposal
        |
        +--> provider routing recommendation
        |
        +--> UI/automation-level recommendation
        |
        v
Validate, simulate, and shadow test
        |
        v
Human review for behavior-changing updates
        |
        v
Activate new version or reject/retire
```

### Learning targets

MailMate should explicitly score and improve these targets:

| Target | Example metric | Possible self-iteration | Requires human approval? |
|---|---:|---|---|
| Filing suggestions | Accepted move rate, undo rate | Refine folder rule or threshold | Yes for automatic moves |
| Spam/phishing classification | User correction rate | Adjust sender/domain features or propose rule | Yes for active rule changes |
| Priority tagging | Tag acceptance/ignore rate | Tune priority threshold | Yes if automatic tagging changes materially |
| Draft quality | Edit distance, discard rate, sent-after-edit rate | Adjust tone profile or draft prompt | Usually yes for prompt/profile changes |
| Task extraction | Completion/deletion rate | Refine task extraction prompt or rule | Yes if creating persistent tasks |
| Thread summaries | User regeneration rate, thumbs feedback | Adjust summary style prompt | Optional for low-risk prompt revisions |
| Provider routing | Latency, validation failures, accepted output rate | Prefer provider A for summaries, provider B for classification | User setting or explicit opt-in |
| Rule health | Override rate, stale fire rate | Split, merge, retire, or shadow-test rules | Yes |
| Follow-up workflows | Reschedule rate, reply-before-step rate, stop-early rate, expiry-to-needs-attention rate | Refine cadence offsets, tighten stop/exit condition, propose/suspend a workflow, suggest enrollment, revise the follow-up prompt | **Yes** for any cadence/stop/enrollment change; every surfaced step is review-required regardless |

### Rule and model versioning

Self-iteration requires versioning more than rules. MailMate should version:

- Rules.
- Rule thresholds.
- Prompt templates.
- Provider routing policies.
- Feature extractors.
- Draft style profiles.
- Classifier calibration settings.
- Simulation datasets.

A MailMate decision should be explainable against exact versions:

```json
{
  "decision_id": "dec_123",
  "rule_versions": ["rv_software_receipts_3"],
  "prompt_template_versions": ["pt_classify_email_5"],
  "provider_routing_version": "route_2",
  "feature_extractor_version": "features_4",
  "calibration_version": "cal_spam_7",
  "policy_version": "policy_builtin_1",
  "workflow_definition_version": "wfdv_quote_followup_2"
}
```

(A follow-up decision additionally pins `workflow_definition_version`; the follow-up
draft's *body* signal still flows to `draft_feedback` → the existing **derive-on-export**
path, and `followup_feedback` is itself a derive-on-export source for a workflow-timing
dataset (`task = "workflow_followup"`) — no new training storage, and LoRA stays advisory
and gated, never enabling auto-send.)

### Local models before fine-tuning

The initial learning model should not require training neural-network weights. MailMate can get strong self-iteration from simpler, auditable local models:

- Frequency counters.
- Online statistics.
- Similarity clustering over message features.
- The cascade's Tier-2 classifier behind the `Tier2Classifier` trait — defaulting to a small Burn-trained discriminative classifier, with online logistic regression + a calibration table as the lightweight alternative (naive Bayes was considered and rejected for poor calibration; see *Classification Cascade*).
- Contextual bandits for low-risk choices such as prompt template selection.
- Calibration tables for provider confidence.
- Rule outcome scoring.

This keeps learning transparent and testable. The **committed default substrate** for
these small models and for the trainer is Rust-native **Burn** — pinned to an exact
version, CPU/ndarray by default — built as the same engine-neutral-contracts + port +
Burn-adapter split the sibling project **idiolect** ships (`idiolect-ml-core` /
`idiolect-ports` / `idiolect-trainer-burn`), so Burn is the default *and* swappable
(external tooling and a mock stay pluggable behind the same traits). It is still just one
implementation of the same auditable proposal/evaluation loop. The foundation/generative
model is **frozen** (run via a mature engine behind `AiProvider`, idiolect-style), so none
of this fine-tunes the big model. Embeddings/encoders via Burn are a ready-now capability
when that seam is built; **batteries-included LoRA is NOT shipped in Burn** (it is DIY on
Burn primitives), so an on-device LoRA adapter stays a *deferred, optional* target behind
the `TrainerBackend` seam — the small-model trainer above is what ships and what the
learning thesis needs (see *On-Device Training Layer*).

### Shadow evaluation as the core of self-improvement

Every behavior-changing improvement should be tested in shadow mode first when risk is non-trivial. Shadow evaluation should answer:

- Would the new rule have fired?
- What action would it have proposed?
- Would policy have allowed, reviewed, or blocked it?
- Did the user later do something similar manually?
- Would the new behavior have reduced work or caused an error?

Shadow results should produce a promotion report:

```json
{
  "candidate_id": "prop_123",
  "shadow_messages_seen": 42,
  "would_have_fired": 11,
  "matched_later_user_action": 9,
  "conflicts": 0,
  "policy_blocks": 0,
  "estimated_precision": 0.82,
  "recommendation": "promote_to_active_with_review",
  "rationale": "The user manually moved 9 of 11 matching messages to the same folder."
}
```

### Prompt evolution

Prompts are part of the product and should be treated like rules:

- Stored as named templates.
- Versioned.
- Tested against fixtures.
- Evaluated against user outcomes.
- Rolled back if they regress.
- Provider-neutral where possible.

The AI curator may propose prompt changes such as:

- “The draft reply prompt is producing responses that the user shortens by 40%; propose a more concise template.”
- “The task extractor misses due dates when they appear in bullet lists; add a fixture and revise the extraction prompt.”
- “The phishing classifier overweights marketing language; adjust prompt guidance and run simulation.”

Prompt changes that affect high-risk behavior should require review. Low-risk wording improvements can be staged behind feature flags or shadow evaluation.

### Provider routing as a learning problem

MailMate can learn which provider works best for each task without coupling the core to any provider. The provider router can track:

- Latency.
- Cost, if remote providers are used.
- Structured-output validity rate.
- User acceptance rate.
- Policy violation rate.
- Task-specific quality metrics.

Example routing policy:

```json
{
  "task": "draft_reply",
  "routing_strategy": "scored_preference",
  "candidates": [
    { "provider_id": "local_lm_studio", "score": 0.87 },
    { "provider_id": "local_ollama", "score": 0.81 },
    { "provider_id": "openai_compatible", "score": 0.78 }
  ],
  "constraints": {
    "prefer_local": true,
    "remote_requires_user_opt_in": true
  }
}
```

Provider routing changes should be visible in settings and should never send content to a remote provider unless the user explicitly opted in.

### Additional storage for self-iteration

Add tables for model/prompt/provider iteration:

#### `learning_metrics`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `metric_...` |
| `subject_type` | TEXT | `rule`, `prompt_template`, `provider`, `threshold`, `feature_extractor` |
| `subject_id` | TEXT | ID of measured thing |
| `metric_name` | TEXT | Example: `undo_rate`, `acceptance_rate` |
| `metric_value` | REAL | Numeric value |
| `window_start` | TEXT | Measurement window |
| `window_end` | TEXT | Measurement window |
| `sample_size` | INTEGER | Number of observations |
| `created_at` | TEXT | Timestamp |

#### `prompt_templates`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `pt_...` |
| `task` | TEXT | `classify_email`, `draft_reply`, etc. |
| `version_number` | INTEGER | Monotonic per template |
| `template_text` | TEXT | Template, may include placeholders |
| `schema_id` | TEXT | Expected response schema |
| `status` | TEXT | `draft`, `shadow_mode`, `active`, `retired` |
| `created_by` | TEXT | `Actor`: `user`, `ai`, `system` |
| `change_reason` | TEXT | Why this version exists |
| `created_at` | TEXT | Timestamp |

#### `provider_scores`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `ps_...` |
| `provider_id` | TEXT | Configured provider |
| `task` | TEXT | Task type |
| `score` | REAL | Learned routing score |
| `latency_ms_p50` | INTEGER | Median latency |
| `valid_response_rate` | REAL | Schema-valid response rate |
| `acceptance_rate` | REAL | User acceptance rate |
| `policy_violation_rate` | REAL | Block/review rate |
| `sample_size` | INTEGER | Observations |
| `updated_at` | TEXT | Timestamp |

#### `experiments`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `exp_...` |
| `experiment_type` | TEXT | `rule_shadow`, `prompt_shadow`, `provider_routing`, `threshold_test` |
| `candidate_id` | TEXT | Proposal or version being tested |
| `status` | TEXT | `running`, `completed`, `promoted`, `rejected`, `cancelled` |
| `start_at` | TEXT | Timestamp |
| `end_at` | TEXT NULL | Timestamp |
| `success_criteria_json` | TEXT | Required promotion criteria |
| `result_json` | TEXT NULL | Measured outcome |

### Safety constraints for self-iteration

Self-iteration must obey strict limits:

- It may propose changes to rules, prompts, thresholds, and provider routing.
- It may run candidates in shadow mode.
- It may update low-risk local metrics automatically.
- It may not weaken hard policies.
- It may not activate dangerous behavior without human approval.
- It may not silently increase automation level.
- It may not send more private data to providers than the user allowed.
- It may not treat its own generated labels as ground truth without user feedback or later behavioral confirmation.

### Self-iteration delivery order (within full v1)

v1 is feature-complete; this is the *delivery order* in which self-iteration comes online within it (a feature-delivery sequence, not a reduced scope):

1. Record decisions and user outcomes.
2. Compute simple metrics per rule and action type.
3. Generate rule proposals from repeated manual behavior.
4. Run proposed rules in shadow mode.
5. Show a promotion report to the user.
6. Version and activate approved rules.
7. Track undo/override rates for active rules.
8. Propose refinements or retirement when rules degrade.
9. Add prompt-template versioning and fixture-based prompt evaluation.
10. Add provider scoring and explicit user-approved provider routing.

This gives MailMate a practical learning model immediately while keeping behavior inspectable and reversible.

---

## On-Device Training Layer

MailMate should be able to produce training data for a portable LoRA adapter that can “add on” to the user’s chosen base model. This is an **optional, deferred** capability — *not* the core of MailMate's training layer. The committed v1 trainer is Rust-native **Burn** training **small learned models** on-device (the Tier-2 discriminative classifier and preference/scoring/tone models) from the per-task feedback tables, gated into service by an eval/promotion check — the exact shape the sibling project **[idiolect](https://github.com/nick-tgcs/idiolect)** ships (`idiolect-ml-core` contracts + `idiolect-ports` traits + an `idiolect-trainer-burn` adapter + an `idiolect-trainerctl` orchestrator whose `evaluate_promotion(policy, report, compatibility) -> Promote` gate is MailMate's crystallization gate). Two things this layer is **not**: it never fine-tunes the foundation model (any generative LLM runs **frozen** behind `AiProvider`, as idiolect runs Whisper frozen via `whisper-rs`), and it does not depend on LoRA. Rules remain the primary, auditable source of behavior; a portable LoRA adapter is a real but optional target of the same `TrainerBackend` seam — useful for tone/style personalization, never the source of truth.

> **Built from the start, with two structural rules:**
> 1. **Capture is day-one; LoRA export rides behind the port.** Capture (the per-task
>    feedback tables) and **small-model training** ship first; the LoRA-export views,
>    adapter metadata import, compatibility checks, eval gates, and training
>    orchestration — TDD'd against fixtures so there is no "we forgot to capture" gap.
> 2. **Training executes through a pluggable `TrainerBackend` trait.** The **default
>    backend is in-process Burn** (Rust-native: reverse-mode autodiff via `Autodiff<B>`,
>    AdamW/SGD, LR schedulers, gradient accumulation, and checkpointing of model +
>    optimizer state — all production-shaped in Burn today). An **external documented
>    toolchain** (subprocess) and a **remote** backend remain fully pluggable
>    alternatives, and a **mock** backend exists for tests. MailMate *drives* the whole
>    loop; only the weight-crunching is behind the swappable backend, and the "portable"
>    goal is *strengthened* — the default path is a single Rust binary (CPU via
>    NdArray/burn-flex; GPU via CUDA/Metal/WGPU when available), with no hard GPU or
>    Python dependency.
>
> **Honest scope of the Burn default — LoRA is DIY, not first-class.** Burn ships the
> *primitives* for parameter-efficient fine-tuning (custom `#[derive(Module)]` layers,
> per-parameter `set_require_grad(false)` to freeze base weights, full autodiff, AdamW),
> so a low-rank adapter on `Linear` layers is idiomatic and feasible — but there is **no
> batteries-included LoRA module in Burn core**, and `burn-lm` does not expose one. So:
> the committed v1 Burn trainer trains **small models** (classifier / preference / tone)
> now; an on-device **LoRA** adapter on these primitives is a **deferred, optional
> target — not a v1 commitment**, with the external/`peft`-style toolchain as the
> **proven pluggable fallback** when LoRA is wanted sooner; `TrainerBackend.capabilities()`
> advertises `lora` only once the primitive-built adapter actually works. Burn quantization is INT8/INT4 PTQ only (no
> QAT/mixed-precision), and Burn has **no GGUF interop** — it is not a path to running
> community GGUF weights.
>
> **Training data is not stored separately.** There is no `training_examples` table.
> The per-task feedback tables (`classification_feedback`, `filing_feedback`,
> `draft_feedback`, `summary_feedback`, `task_extraction_feedback`,
> `rule_proposal_feedback`) are the single source of truth; datasets are **derived on
> export** as views over them. Their fields map straight across: `polarity` →
> chosen/rejected, `human_reason_code` → labels, `ai_body`/`final_body` → SFT
> input/target.
>
> **Capture timing:** the feedback tables ship *with each AI function* (the moment
> classification goes live, `classification_feedback` exists), because a correction +
> reason not captured at the instant it happens is gone forever.
>
> Draft *tone* is learnable at the **default** retention level (`draft_feedback` retains
> the small, locally-generated draft bodies); classification training that depends on
> email *body* content needs `summaries`/`bodies` opt-in.

The architecture must capture both **positive examples** and **negative examples**. Positive examples show what MailMate should do. Negative examples show what MailMate should avoid. Without negative examples, the adapter will only imitate accepted outputs and will not learn the boundary between useful and harmful behavior.

### Core principle: LoRA is advisory, rules and policy still win

A LoRA-adapted model is still only an AI provider implementation detail. Its outputs must pass through the same layers as every other provider output:

```text
LoRA-adapted provider output
        |
        v
structured response validation
        |
        v
rule hierarchy
        |
        v
policy guard
        |
        v
human review where required
```

The LoRA must not:

- Weaken hard policies.
- Become the source of truth for filing or spam decisions.
- Silently activate rules.
- Store or export full private email bodies without explicit user consent.
- Be treated as portable across incompatible base models without compatibility metadata.

### What the LoRA should learn

Good LoRA targets:

- Draft reply tone and structure.
- Preferred summary format.
- Task extraction style.
- Classification explanation style.
- Repeated distinctions the user makes when triaging email.
- Spam/phishing reasoning patterns from confirmed corrections.
- Rule proposal wording and granularity.

Poor LoRA targets:

- Absolute safety rules. These belong in policy guard code.
- Exact folder moves when a deterministic rule can express them.
- Secrets, credentials, private facts, or raw payment data.
- Unreviewed model guesses.
- Provider-specific API behavior.

### Training signal taxonomy

MailMate should label training signals explicitly.

| Signal kind | Positive example | Negative example | Notes |
|---|---|---|---|
| `classification` | User confirms “not spam” or accepts classification | User marks classification wrong, undo, override | Include message features and final label. |
| `filing` | User accepts suggested folder or repeatedly moves similar mail | User undoes move or rejects suggestion | Prefer rule training over LoRA for deterministic filing. |
| `draft_reply` | User sends lightly edited generated draft | User heavily edits, discards, or marks unsafe | Store redacted diff by default. |
| `summary` | User accepts summary or does not regenerate | User regenerates, edits, or flags missing detail | Include preferred summary style. |
| `task_extraction` | User keeps extracted task | User deletes/edits extracted task | Include task fields and correction. |
| `rule_proposal` | User accepts proposal or edits lightly | User rejects proposal or disables derived rule | Useful for curator behavior. |
| `safety` | Policy correctly blocks unsafe model output | Model suggested unsafe action | Negative examples should strongly mark forbidden behavior. |

### Positive and negative labels

Every training example should have an outcome label, not just raw text.

Suggested labels:

- `accepted` — user accepted as-is.
- `accepted_with_minor_edits` — user made small edits.
- `corrected` — user fixed the output.
- `rejected` — user rejected the output.
- `discarded` — user discarded draft/summary/task.
- `undone` — user undid the action.
- `blocked_by_policy` — policy rejected output.
- `unsafe` — output violated safety constraints.
- `counterexample` — example demonstrates when not to apply a pattern.

These labels support supervised fine-tuning datasets, preference datasets, and evaluation fixtures.

### Training example object

Training examples are **not durable rows** — they are derived at export time from the
per-task feedback tables. The export-time representation should be provider-neutral
and task-specific:

```json
{
  "id": "trn_001",
  "task": "draft_reply",
  "source_feedback": { "kind": "draft", "id": "drffb_001" },
  "privacy_level": "redacted",
  "base_model_family": "llama",
  "input": {
    "system": "You are MailMate, a safe local-first email assistant.",
    "instruction": "Draft a concise reply asking for the corrected invoice.",
    "context_features": {
      "sender_domain": "example.com",
      "thread_summary": "Vendor sent an invoice update and user needs corrected copy.",
      "forbidden_commitments": ["dates", "prices", "payment_changes", "legal_positions"]
    }
  },
  "candidate_output": {
    "body": "Hi, please send the corrected invoice when available. Best,"
  },
  "user_corrected_output": {
    "body": "Hi, thanks for sending this over. Could you please send the corrected invoice when you have a chance? Best,"
  },
  "label": "accepted_with_minor_edits",
  "polarity": "positive",
  "quality_score": 0.86,
  "safety_flags": [],
  "created_at": "2026-01-15T10:40:00Z"
}
```

Negative example:

```json
{
  "id": "trn_002",
  "task": "draft_reply",
  "source_feedback": { "kind": "draft", "id": "drffb_002" },
  "privacy_level": "redacted",
  "input": {
    "instruction": "Reply to the vendor about payment details.",
    "context_features": {
      "sender_domain": "unknown-example.com",
      "thread_summary": "Sender claims payment details changed.",
      "forbidden_commitments": ["payment_changes"]
    }
  },
  "candidate_output": {
    "body": "Thanks, I will update the payment details and send payment today."
  },
  "user_corrected_output": null,
  "label": "unsafe",
  "polarity": "negative",
  "quality_score": 0.0,
  "safety_flags": ["payment_detail_change", "unsupported_commitment"],
  "created_at": "2026-01-15T11:10:00Z"
}
```

### Dataset formats

MailMate should be able to export multiple dataset views from the same internal examples.

1. **Supervised fine-tuning JSONL**
   - Input: instruction + redacted context.
   - Output: corrected/accepted target.
   - Uses positive examples only or positive plus safe corrected outputs.

2. **Preference JSONL**
   - Input: instruction + context.
   - Chosen: accepted/corrected output.
   - Rejected: discarded/unsafe/overridden output.
   - Useful for DPO/ORPO-style training if supported later.

3. **Evaluation JSONL**
   - Frozen test cases not used for training.
   - Measures whether a new LoRA regresses safety or quality.

4. **Safety counterexample JSONL**
   - Negative examples of forbidden behavior.
   - Used for evaluation and, when supported, preference training.

Example preference export:

```jsonl
{"task":"draft_reply","messages":[{"role":"system","content":"You are MailMate..."},{"role":"user","content":"Context: ... Draft a reply..."}],"chosen":"Hi, thanks for sending this over...","rejected":"Thanks, I will update the payment details and send payment today.","metadata":{"safety_flags":["payment_detail_change"],"privacy_level":"redacted"}}
```

### Portability requirements

A LoRA adapter is not universally portable across every model. It is portable only across compatible base model families, tokenizers, chat templates, and architectures. MailMate must store compatibility metadata with every dataset and adapter.

Required adapter metadata:

```json
{
  "adapter_id": "lora_001",
  "name": "mailmate-draft-style-v1",
  "format": "safetensors",
  "adapter_type": "lora",
  "base_model_family": "llama",
  "base_model_name": "Meta-Llama-3.1-8B-Instruct",
  "base_model_revision": "...",
  "tokenizer_hash": "tok_abc",
  "chat_template_hash": "tmpl_def",
  "training_dataset_id": "ds_001",
  "training_example_count": 1200,
  "positive_count": 850,
  "negative_count": 350,
  "created_at": "2026-01-20T12:00:00Z",
  "eval_report_id": "eval_001"
}
```

A LoRA-adapted model is not a special case — it is a configured provider whose output
flows through the same `complete_structured` → validation → task layer → rules →
policy path as any other. Adapter loading is therefore an **optional provider
capability** (the same opt-in-trait pattern as the `Native*` task seam), expressed as
capability metadata rather than assuming any backend:

```rust
pub struct AdapterSpec {
    pub adapter_id: AdapterId,
    pub path: std::path::PathBuf,
    pub adapter_type: AdapterType,
    pub base_model_family: String,
    pub tokenizer_hash: Option<String>,
    pub chat_template_hash: Option<String>,
}

#[async_trait]
pub trait SupportsAdapters: AiProvider {
    fn can_load_adapter(&self, adapter: &AdapterSpec) -> AdapterCompatibility;
    async fn load_adapter(&self, adapter: AdapterSpec) -> Result<(), AiProviderError>;
    async fn unload_adapter(&self, adapter_id: AdapterId) -> Result<(), AiProviderError>;
}
```

Some providers may support adapters directly; others may require merging outside MailMate; others may not support adapters at all. The core should expose this as capability metadata, not assume a specific backend. In particular, the optional in-process **Burn** provider *may* implement `SupportsAdapters` for a DIY low-rank adapter once one exists on Burn primitives — but this is an **optional capability advertised via metadata**, not an assumed feature, and "others may not support adapters at all" remains the default expectation.

### Additional storage for LoRA training

Add these tables in addition to the self-iteration tables.

#### `training_examples` — removed

There is **no** `training_examples` table. It would duplicate the per-task feedback
tables, which are the single source of truth. Training examples are **derived on
export** as views over those tables; the train/validation/test/holdout split is a
deterministic function of the source row IDs computed at export time, not a stored
column.

#### `training_datasets`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `ds_...` |
| `name` | TEXT | Human-readable dataset name |
| `dataset_type` | TEXT | `sft`, `preference`, `evaluation`, `safety_counterexample` |
| `base_model_family` | TEXT | Intended model family |
| `example_ids_hash` | TEXT | Stable hash of included examples |
| `positive_count` | INTEGER | Positive examples |
| `negative_count` | INTEGER | Negative examples |
| `validation_count` | INTEGER | Validation examples |
| `test_count` | INTEGER | Held-out tests |
| `privacy_level` | TEXT | Highest included privacy level |
| `export_format` | TEXT | `jsonl_chat`, `alpaca`, `preference_jsonl`, etc. |
| `artifact_path` | TEXT NULL | Local exported dataset path |
| `created_at` | TEXT | Timestamp |

#### `lora_adapters`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `lora_...` |
| `name` | TEXT | Adapter name |
| `adapter_type` | TEXT | `lora`, `qlora`, etc. |
| `format` | TEXT | `safetensors`, etc. |
| `base_model_family` | TEXT | Compatible family |
| `base_model_name` | TEXT | Exact training base model |
| `base_model_revision` | TEXT NULL | Revision/hash |
| `tokenizer_hash` | TEXT NULL | Tokenizer compatibility |
| `chat_template_hash` | TEXT NULL | Chat template compatibility |
| `training_dataset_id` | TEXT | FK to `training_datasets.id` |
| `artifact_path` | TEXT | Local adapter path |
| `status` | TEXT | `candidate`, `active`, `retired`, `failed_eval` |
| `created_at` | TEXT | Timestamp |

#### `lora_eval_runs`

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `eval_...` |
| `adapter_id` | TEXT | FK to `lora_adapters.id` |
| `dataset_id` | TEXT | Evaluation dataset |
| `base_provider_id` | TEXT | Provider used for evaluation |
| `metrics_json` | TEXT | Accuracy, win rate, safety failures, etc. |
| `safety_failures` | INTEGER | Count of safety regressions |
| `quality_score` | REAL | Aggregate score |
| `approved_for_use` | INTEGER | Human/system approval |
| `created_at` | TEXT | Timestamp |

### Data capture rules

Per-task feedback rows should be created (and flagged as training-eligible) only when
the signal is clear.

Positive capture examples:

- Generated draft was sent after minor edits.
- Summary was accepted or copied.
- Extracted task was kept.
- Suggested classification was accepted.
- User manually performed the same filing action repeatedly.
- Rule proposal was accepted with no or minor edits.

Negative capture examples:

- Generated draft was discarded.
- User removed invented facts from a draft.
- User rejected a classification.
- User undid a move/tag/junk action.
- Policy blocked a provider output.
- Rule proposal was rejected or later disabled due to bad behavior.

Ambiguous behavior, such as ignoring a suggestion, should not automatically become a negative example unless repeated or paired with later contradictory action.

### Training pipeline boundary

The **whole pipeline is built and TDD'd from the start**; only the weight-crunching
itself sits behind the **pluggable `TrainerBackend` trait** (**default: in-process
Burn**; an external documented toolchain via subprocess, a remote backend, and a mock
remain selectable via `[training] backend`). MailMate drives every step:

1. Per-task feedback tables capture corrections + reasons continuously (core, not training-specific).
2. At export time: derive examples from the feedback tables, redact, normalize.
3. Split deterministically into train/validation/test/holdout (a function of source-row IDs, not a stored column).
4. Export JSONL datasets.
5. Invoke the `TrainerBackend` (default: **in-process Burn**; external toolchain subprocess and remote remain selectable via `[training] backend`).
6. Import adapter metadata and artifact path.
7. Run local evaluation fixtures.
8. Activate adapter only if evaluation gates pass and the user approves.
9. Feedback capture continues; the next adapter version derives from the grown tables.

This keeps the architecture portable and avoids coupling MailMate to one trainer, GPU setup, or model host. The Burn default is pure-Rust/single-binary on CPU (NdArray/burn-flex) and can train on GPU (CUDA/Metal/WGPU) when available — nothing in the core hard-depends on a GPU — while the orchestration, gates, and tests exist from day one.

### Evaluation gates before activating a LoRA

A candidate LoRA adapter must pass gates before it can be selected by provider routing:

- Zero hard-policy regressions on safety fixtures.
- No increase in unsafe draft commitments.
- Better or equal structured-output validity rate.
- Better task-specific quality score than the base model or previous adapter.
- Acceptable latency and memory overhead.
- Compatibility metadata matches the loaded base model.
- User explicitly approves activation.

If an adapter fails, it should remain stored as `failed_eval` with an explanation, not silently deleted.

### Privacy and consent

Training data is more sensitive than ordinary metrics because it can encode user behavior and private content. MailMate should provide separate controls for:

- Capturing training examples.
- Including redacted text.
- Including full body text.
- Exporting datasets.
- Using remote trainers.
- Importing trained adapters.
- Deleting all training examples and artifacts.

Default should be conservative:

- Capture metadata/features and labels.
- Redact text by default.
- Do not export automatically.
- Do not train remotely without explicit consent.
- Do not include full bodies unless explicitly enabled.

---

## Agent Curator

The AI agent curator is responsible for improving the explicit rule system. It is not allowed to silently take over.

The agent curator can:

- Propose new rules.
- Refine existing rules.
- Merge duplicate rules.
- Split over-broad rules.
- Detect stale rules.
- Detect conflicting rules.
- Explain why a rule fired.
- Suggest threshold changes.
- Recommend shadow testing.
- Summarize user feedback patterns.
- Propose, refine, or retire follow-up workflows (cadence and stop/exit conditions) and suggest enrollment.

The agent curator must not:

- Directly activate dangerous rules without human approval.
- Bypass the policy guard.
- Hide generated rules from the user.
- Rewrite rule history.
- Treat provider output as ground truth.

### Curator proposal examples

```json
{
  "proposal_type": "new_rule",
  "risk_level": "low",
  "title": "Tag recurring newsletter as reading",
  "rationale": "The user moved 6 messages from the same sender to Reading and tagged 4 of them as newsletter.",
  "recommended_status": "pending_human_review",
  "evidence_refs": [
    { "kind": "filing", "id": "filfb_001" },
    { "kind": "filing", "id": "filfb_004" },
    { "kind": "filing", "id": "filfb_009" }
  ],
  "rule_draft": {
    "condition": {
      "all": [
        { "field": "sender_email", "op": "eq", "value": "sender@example.com" },
        { "field": "headers.list_id_present", "op": "equals", "value": true }
      ]
    },
    "effect": {
      "tag": ["newsletter"],
      "move": "Reading"
    }
  }
}
```

```json
{
  "proposal_type": "split_rule",
  "risk_level": "medium",
  "title": "Split broad invoice rule by sender domain",
  "rationale": "The current invoice rule has 3 recent overrides for bank emails but performs well for software vendors.",
  "target_rule_kind": "action",
  "target_rule_id": "rule_invoice_review",
  "recommended_status": "pending_human_review"
}
```

```json
{
  "proposal_type": "new_workflow",
  "risk_level": "medium",
  "title": "Standard quote follow-up: 3 / 7 / 14",
  "rationale": "You manually followed up on 6 quotes at ~3 and ~7 days; 4 of those got a reply by day 14.",
  "recommended_status": "shadow_mode",
  "evidence_refs": [
    { "kind": "followup", "id": "flwfb_010" },
    { "kind": "followup", "id": "flwfb_014" }
  ],
  "workflow_draft": {
    "anchor": "quote_sent_at",
    "applies_to_item_type": "quote",
    "steps": [
      { "step_index": 0, "offset_days": 3, "draft_intent": "gentle_check_in" },
      { "step_index": 1, "offset_days": 7, "draft_intent": "value_add" },
      { "step_index": 2, "offset_days": 14, "draft_intent": "last_call" }
    ],
    "exit_conditions": ["reply_received", "won", "lost"]
  }
}
```

New follow-up proposal types: `new_workflow`, `refine_workflow_cadence`,
`refine_workflow_stop_condition`, `suggest_enrollment`, `retire_workflow`. The curator
constraints above are unchanged — it may *propose*, but activation always needs human
review and every surfaced step is review-required.

---

## Provider-Abstraction Layer

AI providers are replaceable adapters behind one trait. Provider-specific details must remain inside provider modules.

Required provider implementations:

- `mailmate-ai::providers::ollama`
- `mailmate-ai::providers::openai_compatible`
- `mailmate-ai::providers::lm_studio`
- `mailmate-ai::providers::llama_cpp`
- `mailmate-ai::providers::mock`

Supported v1 provider implementation (feature-gated, local-first):

- **`mailmate-ml` Burn in-process provider** (registered into the `ProviderRegistry`
  under `kind = "burn"`) — a **v1-supported** pure-Rust, single-binary local-inference
  option. Honest operational notes (current upstream maturity, not reasons to defer):
  `burn-lm` is **v0.0.1** (Llama 3.x / TinyLlama only, run via its own `InferenceServer`),
  there is **no GGUF interop**, and it lags llama.cpp/Ollama in **model breadth** — so
  users who need a wider model roster should pick Ollama/llama.cpp (the providers are
  co-first-class; the registry ships empty and the user chooses at setup). Weights are
  imported via safetensors / `.pt` into a Burn-defined architecture, or via `burn-onnx`
  (verify operator coverage). It implements the same `AiProvider` primitive as every
  other provider, so it inherits validation, the task layer, rules, and policy unchanged.

### Provider tasks

Tasks are **not** provider methods — they are typed functions in `mailmate-ai::tasks`
built on the single `complete_structured` primitive, each owning a versioned prompt
template and an output schema:

- Classify email.
- Draft reply.
- Summarize thread.
- Extract tasks.
- Propose rules.
- Curate/refine rules.

Adding a task touches one file in the task layer and zero adapters. `propose_rules`
and `curate_rules` are ordinary task functions (they were always orchestration, not a
model primitive).

### Structured output validation

Every provider response should be validated before use.

Validation should check:

- JSON schema compliance.
- Required fields.
- Enum values.
- Confidence ranges.
- No unsupported action kinds.
- No policy-prohibited direct instructions.
- No invented provider-specific fields leaking into core logic.

Invalid responses should become audit events and should not drive actions.

### Provider configuration model

```toml
[ai]
default_provider = "local_ollama"

[ai.providers.local_ollama]
kind = "ollama"
endpoint = "http://localhost:11434"
model = "configured-by-user"

[ai.providers.local_lm_studio]
kind = "lm_studio"
endpoint = "http://localhost:1234/v1"
model = "configured-by-user"

[ai.providers.openai_compatible]
kind = "openai_compatible"
endpoint = "https://api.example.com/v1"
model = "configured-by-user"
# Baseline: key in a 0600-permission file in MailMate's config dir (survives
# GUI-launched Thunderbird, which does not inherit shell exports).
api_key_file = "secrets/openai_compatible.key"
# Optional upgrade: OS keychain. Env var is a dev-only override:
# api_key_env = "MAILMATE_OPENAI_COMPATIBLE_API_KEY"

[ai.providers.local_burn]
# v1-supported, in-process; requires the `burn` cargo feature.
kind = "burn"
model = "configured-by-user"   # e.g. a burn-lm Llama 3.2 / TinyLlama checkpoint
# backend selection is shared with [ml].backend (ndarray/burn-flex/wgpu/cuda/metal).
# Weights load via safetensors/.pt into a Burn-defined architecture, or burn-onnx
# (verify ops). No GGUF.

[ai.providers.test]
kind = "mock"
fixture_dir = "tests/fixtures/provider_responses"
```

Only provider adapters should interpret provider-specific configuration. The
non-`[ai]` selection keys (`[storage]`, `[ml]`, `[training]`) live under *Replaceable
Components and Extension Points* and are owned by `mailmate-core::config`.

---

## Action Planning and Execution

MailMate should separate action planning from execution.

### Action vocabularies

The old flat "supported actions" list actually mixed three different categories, which
are now three typed enums:

**`PlannedAction`** — P2 outbound, MailMate proposes/applies; each carries a `PolicyOutcome`:

- `Tag`
- `Move` (suggested vs. applied is the `PolicyOutcome`, not a separate kind — this
  replaces the old `suggest_move` + `move_to_folder` pair)
- `MarkJunk`
- `CreateDraft` *(may be authored by a scheduled follow-up trigger as well as an on-demand `DraftReply`; in both cases the draft is `requires_review = 1`)*
- `RequireReview { target }`

The three enums are **unchanged** by the follow-up feature — a scheduled follow-up
reuses `CreateDraft`; the enroll/stage/cancel/snooze/review verbs are protocol requests
+ state transitions on the pipeline tables, not new `PlannedAction`/`TaskRequest`/
`UserCorrection` kinds.

**`TaskRequest`** — on-demand AI tasks the user triggers:

- `SummarizeThread`
- `ExtractTasks`
- `ExplainClassification`
- `DraftReply`

**`UserCorrection`** — inbound, the user teaching the system; populates the per-task feedback tables:

- `MarkSpam`
- `MarkNotSpam`
- `LearnFiling`

### Action flow

```text
Thunderbird event/request  — OR a FollowUpDue trigger from the scheduler (carries a pipeline_item)
        |
        v
Rust host builds context
        |
        v
Policy pre-checks
        |
        v
Rule engine evaluation
        |
        v
AI provider advisory call if needed
        |
        v
Action planner creates candidate plan
        |
        v
Policy guard evaluates candidate plan
        |
        v
Allowed/review/blocked plan returned to Thunderbird
        |
        v
Thunderbird executes only allowed or user-confirmed actions
        |
        v
Execution result recorded as learning/audit event
```

### Action plan example

```json
{
  "decision_id": "dec_123",
  "message_id": "msg_123",
  "actions": [
    {
      "kind": "tag",
      "tag": "receipt",
      "source": "rule",
      "rule_id": "rule_receipts",
      "policy_outcome": "allowed"
    },
    {
      "kind": "move",
      "folder_id": "Receipts/Software",
      "source": "rule",
      "rule_id": "rule_receipts",
      "policy_outcome": "requires_review"
    }
  ]
}
```

---

## Sales Pipeline and Follow-up Workflows

MailMate tracks quotes/proposals through a lightweight pipeline and drafts timely
follow-ups for review. It is a **tracker, not a CRM**, and it changes no safety
invariant: a follow-up is a review-required draft, produced by the *existing* P2 path on
a *time* trigger.

### Where it fits

```text
PipelineItem (deal/quote, has a stage)
   └─ runs through ─▶ WorkflowDefinition v_N   (steps: day 3 / 7 / 14 / 28)
                        └─ instantiated as ─▶ WorkflowInstance (state machine)
                                                 ├─ current_step_index
                                                 ├─ next_due_at  ◀── the ONLY temporal trigger
                                                 └─ status (FSM below)
```

You tag a sent quote → a `PipelineItem` is created and **armed** on a
`WorkflowDefinition` → its `WorkflowInstance` carries the durable `next_due_at`. When a
step is due, the scheduler drives P2 to emit `[CreateDraft, RequireReview]`, surfaced as
`followup_draft_ready`. A reply (inbound mail, matched by host-side thread identity) or a
won/lost mark **exits** the sequence. Domain types live in `mailmate-domain`; the engine
in `mailmate-workflow`; the tables in *Data Model* (not repeated here).

### The durable temporal trigger

Time is the only new trigger, and it is modelled 1:1 on the existing classification
queue — durable state polled by a worker, **never** an event bus or a daemon:

| Classification queue | Follow-up scheduler |
|---|---|
| `messages.classification_status` (`pending`…) | `workflow_instances.next_due_at` + `status` |
| worker drains `pending` rows | worker drains `status IN ('active','snoozed') AND next_due_at <= now` |
| index `messages(classification_status)` | index `workflow_instances(status, next_due_at)` |
| pushes `classification_ready` | pushes `followup_draft_ready` |

`app.rs` runs the scheduler on startup (catch-up sweep) and on a periodic tick
(`[followup] poll_interval_secs`) while the host is alive. **Invariant:** `next_due_at`
is non-NULL **iff** `status ∈ {active, snoozed}` — those are exactly the selectable rows.

### Catch-up-on-launch + staleness / coalescing guard

Because the host runs only while Thunderbird is open, a step may be overdue (the client
was closed for days). On each drain pass for an instance, with `H = abandon_horizon_days`
(from `staleness_json`, config default):

1. **Due set** = steps with `step_index ≥ current_step_index` whose absolute due-time (`anchor_at + offset_days`) `≤ now`.
2. A due step is **fresh** if `now − due_time ≤ H`, else **stale**.
3. **If any fresh due step exists** → let `latest` = the highest-`step_index` fresh due step. Emit **one** follow-up draft for `latest` (drive P2). Record each due step with index `< latest` as `step_skipped_coalesced` (a `followup_coalesced` audit row lists them; the emitted step's `followup_feedback.coalesced_from_json` carries them). Move to `awaiting_review`, clear `next_due_at`.
4. **Else (all due steps stale past H)** → emit **no draft**; move to `needs_attention`; push `followup_needs_attention`.
5. **`awaiting_review` hold (the frequency cap):** while a follow-up draft is pending review, `next_due_at` is NULL so nothing new fires — **at most one pending follow-up draft per instance**. On `review_followup`, advance `current_step_index` to `latest + 1` and set `next_due_at` to the next future step (or `completed` if none).

So a 30-day absence never produces three nagging drafts: it produces one current
follow-up (or a single needs-attention nudge).

### Exit / stop detection

- **Reply:** new inbound mail on the item's thread (host-side `References`/`In-Reply-To` identity, reusing `threads`) → `engaged`, `next_due_at` cleared. (The user may resume.)
- **Won/lost:** a `record_user_action` `update_pipeline_stage` → `completed`.
- **Cancel:** `cancel_sequence` → `cancelled`.

Exit always means clearing `next_due_at`, so the scheduler simply stops selecting the row.

### Instance state machine

`next_due_at` non-NULL iff `status ∈ {active, snoozed}`:

| From | Event | To |
|---|---|---|
| *(arm)* | user enrolls a quote | `active` |
| `active` / `snoozed` | step due → draft emitted (drives P2) | `awaiting_review` |
| `awaiting_review` | `review_followup` (send/edit/skip), more steps remain | `active` |
| `awaiting_review` | `review_followup`, last step | `completed` |
| `active` / `awaiting_review` / `snoozed` | reply received | `engaged` |
| `engaged` | user resumes (optional) | `active` |
| `active` | user snoozes | `snoozed` |
| `active` / `awaiting_review` | all due steps stale past horizon | `needs_attention` |
| any non-terminal | won / lost | `completed` |
| any non-terminal | cancel | `cancelled` |

### How a due step drives P2 (no logic of its own)

```text
TriggerKind::FollowUpDue { pipeline_item, thread_id, anchor_message_id }
   → existing ActionPlanner  → draft-safety validator  → PolicyGuard
   → [CreateDraft (allowed), RequireReview (requires_review)]  (requires_review = 1)
   → followup_draft_ready
```

The scheduler contributes the *trigger* and the *step's prompt template + forbidden
commitments*; the drafting, validation, and policy decisions are 100% the existing
machinery. There is no follow-up-specific send path, draft validator, or policy.

### Reusing the versioned-principle machinery

A `WorkflowDefinition` is a Ray-Dalio-style principle: versioned, immutable per version,
lifecycle-managed (`draft → pending_human_review → shadow_mode → active → …`), and
human-curated — reusing the rule version/lifecycle/status code. It is **not** a
condition→effect rule, so the condition evaluator and AST-overlap conflict detector do
not apply to the cadence; only `enrollment_condition_json` is an AST condition, and a
workflow *conflict* is the containment check "don't run two active workflows on one
item". Shadow-mode, learning, and curation all reuse existing paths:

- **Shadow:** a `shadow_mode` workflow records would-fire steps in `workflow_shadow_outcomes` (no draft); the promotion report scores *cadence-fit* (manual-followup alignment net of reply-pre-emption) — an honestly weaker signal than rule shadow, since the send-acceptance counterfactual is unobservable.
- **Learning:** `followup_feedback` (cadence/timing) + `draft_feedback` (body) feed evidence → curator proposals (`new_workflow`, `refine_workflow_cadence`, …) → review → shadow → activate. Cadence offsets, stop conditions, and the follow-up prompt template are all learned/versioned.
- **Curation:** the curator may *propose* but never activate; every surfaced step is review-required regardless.

### Worked example

Day 0: you send Acme a quote and tag it → `pli_acme` (`open`), armed on
`standard-quote-follow-up` v2 (3/7/14) → `wfi_acme` `active`, `next_due_at = day 3`.
Day 3: scheduler fires step 0 → review-required draft → `awaiting_review`; you edit and
send → `active`, `next_due_at = day 7`. You then close Thunderbird for 12 days. Day 19
you reopen: catch-up sees steps 1 (day 7) and 2 (day 14) overdue, both fresh within a
14-day horizon → coalesce to **one** draft for step 2, step 1 recorded as
`step_skipped_coalesced` → `awaiting_review`. If instead Acme had replied on day 9, the
day-19 catch-up would find the instance already `engaged` (exited) and surface nothing.

### Invariant-compliance checklist

| Resolved decision / invariant | How this honours it |
|---|---|
| Review-required only (`never_auto_send_drafts`) | Output is `CreateDraft + RequireReview`, `requires_review = 1`; no `send_draft` exists in the path. |
| Catch-up-on-launch, no daemon | In-host scheduler polls `next_due_at`; startup catch-up sweep; no OS timer. |
| No draft blast | Coalesce to one fresh step; stale→`needs_attention`; `awaiting_review` caps to one pending draft. |
| Not a third pipeline | A `FollowUpDue` trigger drives the existing P2 planner; P1 untouched. |
| No event bus / not event-sourced | The trigger is one durable column polled by a worker. |
| Single writer per fact | `followup_feedback` (cadence) vs `draft_feedback` (body) vs `audit_log` (provenance) vs `workflow_shadow_outcomes` (shadow) — disjoint owners. |
| Lightweight, not a CRM | `pipeline_items` is stage + thread anchor + counterparty + opaque amount hint only. |

---

## Drafting Safety

Drafting must be designed around human review.

Hard requirements:

- Never auto-send drafts.
- Do not invent facts.
- Do not commit the user to dates unless present in the thread or explicitly provided.
- Do not commit the user to prices unless present in the thread or explicitly provided.
- Do not commit the user to payments unless present in the thread or explicitly provided.
- Do not commit the user to legal positions unless present in the thread or explicitly provided.
- Do not make promises unless present in the thread or explicitly provided.
- Drafts must be created for human review only.
- Edits to drafts must be captured in `draft_feedback` / `draft_edit_history`.
- A scheduled follow-up draft is an ordinary draft: it passes the **same** draft-safety validator, is created `requires_review = 1`, and its step's `forbidden_commitments` (e.g. `["dates","prices","payment_changes","legal_positions"]`) are enforced by that existing validator — a follow-up cannot smuggle a price, date, or commitment.

### Draft validation

Before returning a generated draft to Thunderbird, MailMate should run a draft-safety validator:

- Detect unsupported commitments.
- Detect invented dates or amounts.
- Detect payment-detail language.
- Detect legal-position language.
- Detect overconfident claims.
- Confirm the draft is marked as requiring human review.

If validation fails, MailMate can either:

- Ask the provider for a safer revision.
- Return a blocked draft response with explanation.
- Return a minimal safe template.

---

## Storage Strategy

MailMate is local-first.

### Default storage (SQLite)

The default engine, behind the `StorageBackend` seam:

- SQLite via `rusqlite` (bundled — statically compiled, no system library, single binary).
- Zero-config: on startup the host creates the DB file if absent and applies the bundled `common` + `sqlite` migrations in-process; the default user never sees a migration step.
- Migration-managed schema (shared `common` core + per-dialect overlay).
- Append-only audit timeline.
- Immutable rule versions.
- Readable local metadata; hashes used for identity/dedup only.

### Storage execution model

The execution model has two layers: an **engine-neutral contract** that the core
depends on, and **backend-private** details that differ per engine.

**Engine-neutral contract (true for every backend).**

- The core reaches storage only through the async repository traits (`RuleRepository`,
  `AuditRepository`, `FeedbackRepository`, …) over a `StorageBackend` — returning domain
  types and `StorageError`. No `Connection`, `Row`, transaction handle, or SQL string
  crosses the seam. The async contract is what the seam promises; *how* a backend
  satisfies it is private.
- Core traits (`RuleEngine`, `PolicyGuard`, `ActionPlanner`, `LearningEngine`,
  repositories) keep their `async` signatures.
- Only `AiProvider` performs real network async (`reqwest`). `app.rs` is the async
  orchestration seam: it awaits provider calls and the storage facade; rule/policy logic
  is pure/CPU-bound and runs inline inside those async methods.

**Default backend: embedded SQLite (these details are SQLite-backend-private, NOT
cross-engine invariants).**

- Engine: SQLite via `rusqlite` (bundled, synchronous).
- The SQLite backend satisfies the async contract by running blocking `rusqlite` work
  via `tokio::task::spawn_blocking`, drawing connections from an `r2d2` pool.
- Pragmas set per connection: `journal_mode=WAL` (concurrent readers),
  `foreign_keys=ON` (FK enforcement is off by default in SQLite),
  `busy_timeout` (serializes writers without spurious `SQLITE_BUSY`).
- Writes serialize through a single logical writer path; reader/writer overlap relies on
  WAL.
- A **networked backend** (sqlx / tokio-postgres) is **async-native**: it does **not**
  use `spawn_blocking`, has no WAL/`busy_timeout`/pragma setup, enforces FKs by default,
  and gains real write concurrency via MVCC. So for the embedded default a blocking
  driver on a pool is sufficient; a server engine simply awaits its driver — the
  single-writer/WAL reasoning above is *not* imposed on it.

### Engine portability

The repository traits keep the core engine-agnostic; the unavoidable per-engine SQL is
quarantined in one `dialect` module so 90% of queries (plain INSERT/SELECT/UPDATE over
TEXT/INTEGER columns with prefixed-string PKs) stay shared. The surfaces that genuinely
diverge and must route through `dialect`:

- **JSON column type** (`TEXT` / `jsonb` / `JSON`) and, only where a view reaches inside
  a `*_json` column, the JSON-extraction expression (`json_extract` vs `->>` vs
  `JSON_UNQUOTE(JSON_EXTRACT(...))`). The portable default is to treat `*_json` as opaque
  and do structured work in Rust.
- **View bodies** and cross-table `UNION`s (type-affinity rules differ) — see the
  engine-portability note under `rule_outcomes (view)`.
- **Boolean/timestamp column types** (LCD `INTEGER`/`TEXT` by default; a server overlay
  may use `BOOLEAN`/`TIMESTAMPTZ`).
- **FK enforcement / pragmas** (SQLite needs `PRAGMA foreign_keys=ON`; servers default-on).
- **Upsert and `RETURNING`** — *anticipated* surfaces for a server backend only; the
  current append-only, prefixed-string-ID schema uses neither.

SQLite stays the **zero-config default that ships**; a server engine (Postgres/MariaDB)
is an **opt-in for users who already run a database**, selected purely via
`[storage] engine` + a connection URL, with the second backend kept as a documented
stub until a real need exists (the seam is built thin now; the engine is not). The
driver choice underpinning the seam is **resolved**: `rusqlite` (bundled) for the
embedded default, `sqlx` for the opt-in server backend — a two-driver split behind one
`StorageBackend` trait (sea-orm rejected as a redundant entity layer; diesel excluded as
portability-hostile; see *Open Design Questions*). "Swappable" means swappable in code, not
free in operations: a server engine adds a daemon, credentials, connection lifecycle,
and its own backup tooling.

### Threat model and what we do (and do not) protect

Thunderbird's own profile already stores every subject and sender in clear on the
same disk (the message list and header caches), even for online IMAP. Hashing
MailMate's copy of that metadata protects against essentially nothing while
disabling the learning loop (you cannot cluster, substring-match, or compute
"similar" over hashes). So MailMate does **not** hash or encrypt local-already-readable
metadata. The privacy investments that actually matter live at the boundaries that
genuinely leak:

- Remote-provider egress is opt-in (local providers send nothing off-machine).
- Body retention is off by default (for size/necessity, not as theatre).
- Redaction is applied at **export/sharing** time (datasets, bug reports), not on
  the live operational database.
- Hashes are used only for **identity/dedup** (`rfc_message_id_hash`, body
  fingerprint) — values we never need to read or display.

Privacy defaults:

- Full email body storage disabled by default (size/necessity).
- Draft diff retention disabled by default.
- Readable subject/sender/domain + structural features; hashes for identity only.
- Store provider prompts/responses only as hashes by default unless debug retention is enabled.
- Allow users to configure retention windows.
- Allow users to purge event classes or provider traces (purge is a tombstone/redaction,
  not a hard delete that breaks the audit trail).

### Content retention levels

| Level | Behavior |
|---|---|
| `metadata` *(default)* | Readable subject/sender/domain + structural features + identity hashes. No body. |
| `summaries` *(opt-in)* | + redacted/derived body summaries and features (smaller than full body). |
| `bodies` *(opt-in)* | + full readable retained bodies. |
| `debug_full_trace` *(temporary opt-in)* | + raw prompts/responses for debugging. |

> Note: local body encryption is intentionally out of the core design (it defends
> against a local-disk threat Thunderbird's own store already exposes). It may
> return later as an optional capability.

### Storage and native-messaging hygiene

These are implementation invariants, not optional polish:

- **Indexes** ship with every migration. Minimum set: `messages(classification_status)`
  (background-queue drain), `messages(rfc_message_id_hash)` (identity lookups),
  `messages(thread_id)`, `messages(sender_domain)`, `messages(received_at)`; on each
  per-task feedback table `(message_id)`, `(created_at)`, and `(matched_rule_id)` where
  present; on each rule table `(status)` and `(scope)`; on `audit_log`/`shadow_outcomes`
  `(created_at)` plus `shadow_outcomes(rule_id)`.
- **Single-writer stdout.** Native messaging is 4-byte native-endian length-prefixed
  frames. Because the background classification worker pushes notifications
  concurrently with request/response replies, two writers can race on stdout and
  interleaved frames corrupt the channel. All stdout writes go through a single writer
  (a dedicated writer task fed by an mpsc channel, or a stdout mutex).
- **Message-size limits.** Native messaging caps host→extension messages at **1 MB**
  (the browser *rejects* — and tears down the port on — oversize frames);
  extension→host is up to 4 GB. See
  [MDN: Native messaging](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_messaging)
  (confirmed: a Gecko platform constant Thunderbird inherits). Per-message
  results are KBs and never approach this; the single stdout writer carries one
  length check that emits a structured `error` instead of writing an oversize frame,
  so a bug fails loudly rather than wedging the channel.
- **Large artifacts go out-of-band.** Dataset/rule exports, LoRA adapter files, and
  database backups/exports **for the embedded engine** are written to disk and
  referenced by **filesystem path** in the message; they never travel through the
  native-messaging frame. (A networked engine is backed up by its own tooling — `pg_dump`
  etc. — not via the frame.) The principle: native messaging carries control + small
  results; large artifacts move as path references over the shared local filesystem.

---

## Auditability and Explainability

Every automatic or suggested action must be traceable to:

- Input message fingerprint.
- Matching rules.
- Rule versions.
- Rule hierarchy band.
- AI provider suggestion, if used.
- Policy guard checks.
- User feedback history, if relevant.
- Final action plan.
- Thunderbird execution result.

### Decision identity

There is **no `decisions` table**. A `decision_id` (`dec_...`) is an **ephemeral
correlation ID** minted per evaluation run and stamped into the rows that record the
run's facts — `audit_log` entries and any per-task feedback rows it produces — plus
the protocol payloads. Provenance (which rule/prompt/model/calibration versions
produced a result) lives in each feedback row's own `pinned_versions_json`, not in a
shared spine; the correlation ID only lets the audit view stitch one run's rows back
together. `RuleEngine::explain(decision_id)` reconstructs an explanation by querying
those rows. A fired follow-up step works identically: it mints an ephemeral `decision_id`
stamped into its `audit_log` (`followup_step_fired`) row, its `followup_feedback` row, and
the `followup_draft_ready` payload, so `explain` can reconstruct "step N of workflow
`wfd_…` v_K, prompt `pt_followup_…`, policy checks passed, surfaced for review" — still
no `decisions` table.

### Explanation example

```json
{
  "decision_id": "dec_123",
  "summary": "MailMate suggested moving this message to Receipts/Software because it matched your approved software receipt rule.",
  "matched_rules": [
    {
      "rule_id": "rule_receipts",
      "version": 3,
      "status": "active",
      "matched_conditions": [
        "sender_domain is github.com",
        "subject contains receipt"
      ]
    }
  ],
  "ai_suggestion": {
    "provider_id": "local_ollama",
    "used": true,
    "classification": "receipt",
    "confidence": 0.91
  },
  "policy": {
    "allowed": ["tag"],
    "requires_review": ["move"],
    "blocked": []
  },
  "user_controls": [
    "Apply move once",
    "Always allow this rule",
    "Edit rule",
    "Disable rule",
    "Not a receipt"
  ]
}
```

---

## GitHub Workflow, CI, and Releases

MailMate development should use GitHub Actions as the required validation and release automation system.

### Branch strategy

MailMate should use a two-long-lived-branch model:

| Branch | Purpose | Rules |
|---|---|---|
| `develop` | Active integration branch for normal development. | All feature/fix/docs/ci branches merge into `develop` by pull request only. PR validation must pass before merge. |
| `main` | Stable release branch. | Only release PRs from `develop` merge into `main`. Merges to `main` trigger release workflows. |

Short-lived branches should follow conventional prefixes:

- `feat/...`
- `fix/...`
- `docs/...`
- `test/...`
- `ci/...`
- `refactor/...`
- `chore/...`

Default developer workflow:

```text
feature/fix branch
        |
        v
pull request into develop
        |
        v
GitHub Actions PR validation
        |
        v
squash merge into develop
        |
        v
release PR from develop into main
        |
        v
GitHub Actions release workflow
```

### Required pull request validation

Every PR into `develop` must run GitHub Actions checks. A PR is not mergeable unless all required checks pass.

**Full validation is wired from day one as one comprehensive, always-running workflow.**
The naive trap is marking a *separate named required check per subsystem* before that
check's job exists — GitHub then deadlocks merges waiting on a status that never
reports. The fix is not fewer checks: it is structuring validation as jobs that
**always run**, where areas not yet built have empty test sets that **pass trivially**.
The TDD mandate guarantees the PR that *introduces* a subsystem also introduces its
tests, so the suite is genuinely full as the code grows — required from day one, no
deadlock.

Required PR checks (all run from day one; empty until their subsystem lands):

- Rust formatting: `cargo fmt --check`.
- Rust linting: `cargo clippy --workspace --all-targets -- -D warnings`.
- Unit tests.
- Integration tests.
- End-to-end or harness-based e2e tests.
- Storage migration tests (matrix across enabled engines; the SQLite leg is required, Postgres/MariaDB legs are opt-in and non-blocking so a server engine never becomes a required-check dependency).
- Native messaging protocol tests.
- Thunderbird adapter tests/harness tests.
- Provider contract tests using mock provider.
- LoRA/training-data capture tests.
- Security/dependency audit where practical.
- Documentation/link checks for Markdown architecture and user docs.

The PR template should require authors to identify:

- Failing test observed first for TDD.
- Unit tests added/updated.
- Integration tests added/updated.
- End-to-end tests added/updated.
- Privacy impact.
- Policy-guard impact.
- Migration impact.
- Provider impact.
- LoRA/training-data impact, if relevant.

### Required GitHub Actions workflows

Initial workflows should include:

```text
.github/
  dependabot.yml
  workflows/
    pr-validation.yml
    release.yml
    docs.yml
    security.yml
    dependabot-automerge.yml
```

#### `pr-validation.yml`

Triggers:

```yaml
on:
  pull_request:
    branches: [develop]
  push:
    branches: [develop]
```

Responsibilities:

- Validate formatting.
- Run clippy with warnings denied.
- Run all unit tests.
- Run all integration tests.
- Run e2e/harness tests.
- Run migration tests.
- Run provider mock/contract tests.
- Run LoRA dataset/export tests.
- Upload test artifacts and coverage reports where useful.

#### `release.yml`

Triggers:

```yaml
on:
  push:
    branches: [main]
    tags:
      - "v*"
  workflow_dispatch: {}
```

Responsibilities:

- Re-run the full validation suite on `main`.
- Build release binaries for supported platforms.
- Package the Thunderbird extension.
- Generate checksums and signatures where possible.
- Create or update a GitHub Release.
- Attach native host binaries, extension package, checksums, and release notes.
- Publish a release manifest if MailMate uses one later.

Main should be considered releasable at all times. A merge to `main` should either create a release candidate or final release artifact, depending on tagging/version policy.

#### `docs.yml`

Triggers:

```yaml
on:
  pull_request:
    branches: [develop]
  push:
    branches: [develop, main]
```

Responsibilities:

- Check Markdown formatting where practical.
- Check internal links.
- Validate examples in docs where possible.
- Ensure architecture docs mention required safety/policy constraints.

#### `security.yml`

Triggers:

```yaml
on:
  pull_request:
    branches: [develop]
  schedule:
    - cron: "0 6 * * 1"
```

Responsibilities:

- Run dependency audit for Rust crates.
- Check JavaScript extension dependencies if any are added.
- Run secret scanning patterns against the repo.
- Flag unsafe dependency or license changes.

### Automated dependency updates and security scans

MailMate should use automated dependency maintenance from the beginning. Dependency updates should be handled as isolated, testable pull requests.

Required automation:

- Enable Dependabot for Rust/Cargo dependencies.
- Enable Dependabot for GitHub Actions versions.
- Enable Dependabot for JavaScript/Node dependencies if the Thunderbird extension gains a package manifest.
- Enable GitHub code scanning where available.
- Enable secret scanning where available.
- Run scheduled security scans at least weekly.
- Create **one PR per dependency update or security issue** so failures are easy to diagnose and revert.

Recommended `.github/dependabot.yml` policy:

```yaml
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "weekly"
    target-branch: "develop"
    open-pull-requests-limit: 10
    groups: {}

  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
    target-branch: "develop"
    open-pull-requests-limit: 10
    groups: {}

  - package-ecosystem: "npm"
    directory: "/extension"
    schedule:
      interval: "weekly"
    target-branch: "develop"
    open-pull-requests-limit: 10
    groups: {}
```

The empty `groups` policy is intentional: MailMate should prefer one PR per issue/update rather than broad grouped bumps. Grouping can be reconsidered later only for low-risk patch updates after CI history is stable.

### Automated dependency bump validation and merge

Dependency PRs should go through the same PR validation as human-authored PRs. Automated merging is allowed only after all required checks pass. Auto-merge is enabled from day one, scoped by the eligibility table below (patch/low-risk auto-merge; majors and runtime/provider/security-sensitive updates require human review) — its safety rests on the always-running full-validation workflow gating every merge.

Dependency automation flow:

```text
Dependabot opens one PR for one dependency/security issue
        |
        v
GitHub Actions PR validation runs against develop
        |
        v
unit + integration + e2e + migration + provider + training-data tests pass
        |
        v
security workflow confirms no new known vulnerability
        |
        v
auto-merge patch/minor safe updates, or request review for risky updates
        |
        v
squash merge into develop
```

Auto-merge eligibility:

| Update type | Auto-merge? | Requirements |
|---|---|---|
| Patch dependency update | Yes | All PR validation and security checks pass. |
| Minor dependency update | Yes for low-risk crates/actions | All checks pass; no migration/security/policy/provider behavior changes. |
| Major dependency update | No by default | Human review required. |
| Security patch | Yes when non-breaking | All checks pass; security scan confirms fix. |
| Runtime/provider/security-sensitive dependency | No by default | Human review required even for minor updates. |
| GitHub Actions version bump | Yes for patch/minor pinned action updates | All checks pass. |

Automation should use GitHub-native Dependabot auto-merge or a small GitHub Actions workflow with least-privilege permissions. It must not bypass branch protection.

Auto-merge workflow requirements:

- Only operate on Dependabot-authored PRs.
- Only target `develop`.
- Wait for required PR validation checks to complete successfully.
- Use squash merge.
- Delete the branch after merge where supported.
- Leave a comment or label when human review is required.
- Never auto-merge into `main`; releases still go through release PRs.

Security scan requirements:

- Dependency audit runs on every PR and weekly schedule.
- Code scanning runs on every PR where practical.
- Secret scanning patterns run on every PR.
- Vulnerability alerts should result in one Dependabot/security PR per issue.
- Failed security checks block merge.

### Branch protection requirements

Protect `develop`:

- Require pull request before merging.
- Require required status checks from `pr-validation.yml`.
- Require branch to be up to date before merging.
- Require conversation resolution.
- Prefer squash merges.
- Disallow direct pushes except emergency administrator override.

Protect `main`:

- Require pull request before merging.
- Accept PRs only from `develop` for normal releases.
- Require full release validation.
- Require signed or approved release commits/tags where possible.
- Disallow force pushes.
- Disallow direct pushes except emergency administrator override.

### Release policy

MailMate releases should be produced from `main` only.

Recommended release flow:

1. Stabilize work on `develop`.
2. Open release PR from `develop` to `main`.
3. Ensure all PR validation passes.
4. Update version and changelog in the release PR.
5. Merge release PR into `main`.
6. Tag the release as `vX.Y.Z`.
7. GitHub Actions builds release artifacts and creates the GitHub Release.
8. Keep `develop` moving after release.

Versioning should use semantic versioning once public releases begin:

- `MAJOR` for incompatible storage/protocol changes.
- `MINOR` for new backward-compatible features.
- `PATCH` for backward-compatible fixes.

Release artifacts should include:

- Rust native host binary or archives by platform.
- Thunderbird extension package.
- Checksums.
- Release notes.
- Migration notes if storage changes.
- Provider/adapter compatibility notes where relevant.
- LoRA adapter compatibility notes if any official adapters are shipped later.

### CI as enforcement for TDD and architecture constraints

GitHub Actions cannot prove every developer truly wrote the test first, but it can enforce outcomes and review evidence:

- Require tests for changed production areas.
- Require PR checklist confirmation of RED-GREEN-REFACTOR.
- Run coverage and fail if coverage drops below agreed thresholds.
- Run architecture boundary checks when practical.
- Run provider tests without real providers.
- Run policy guard tests on every PR.
- Run privacy tests for storage and training-data capture.

CI should make the correct development path the easiest path.

---

## Distribution and Installation

MailMate ships **two artifacts with different channels**, distributed **GitHub-Releases-first**:

1. **Thunderbird extension** (`.xpi`)
2. **Rust native host** (binary + native-messaging manifest)

The host's manifest must name the **stable extension ID**, but the two artifacts can
ship via different channels as long as that ID matches. Version skew between them is
handled by `protocol_version`.

### Native host

- **GitHub Releases binaries + self-install subcommand.** `release.yml` builds per-OS
  binaries (+ checksums/signatures); the user runs `mailmate-native-host install`
  (`--uninstall`), which writes the manifest to the correct per-OS location
  (Linux `~/.mozilla/native-messaging-hosts/` + Thunderbird's dir; macOS
  `~/Library/.../NativeMessagingHosts/`; Windows registry keys). This subcommand is the
  primitive any later installer calls.
- Later layers: an install script (`curl | sh`, `.ps1`), package managers (Homebrew,
  AUR, apt, winget, Flatpak), and code-signing/notarization (Gatekeeper/SmartScreen).

### Thunderbird extension

- **Self-hosted `.xpi` on GitHub Releases**, self-updating via an `update_url` we host.
  An add-on ID is mandatory; **signing is not required** (see below). Hosting and
  updates stay under our control.
- Later: an ATN listing for discoverability/auto-update (an ATN *listing* does go
  through ATN, which signs the package — but that is optional, for reach).

> **Confirmed:** Thunderbird does **not** enforce extension signing — it ships with
> `xpinstall.signatures.required = false` by default, an intentional, documented
> decision ([Bugzilla 1549562](https://bugzilla.mozilla.org/show_bug.cgi?id=1549562),
> RESOLVED WONTFIX: "We do not currently sign add-ons … the attack surface … is much
> lower than Firefox"). So a plain **unsigned** `.xpi` installs in release Thunderbird,
> and "direct from GitHub" needs no signing step. (Re-verify the pref default against
> the specific targeted Thunderbird ESR before a public release, since defaults can
> change.)

### Provider API-key storage

Relevant only once a **remote** provider is opted into (local providers need no key).
A native host is spawned by Thunderbird and inherits *its* environment — and a
GUI-launched Thunderbird often does not carry shell exports — so `api_key_env` is
fragile. Therefore:

- **Baseline: a `0600` config file** in MailMate's config dir (cross-platform, dodges
  the env-inheritance problem; same threat model as Issue 2's local-readable stance).
- **Optional upgrade:** OS keychain (libsecret / macOS Keychain / Windows Credential Manager).
- **Env var:** demoted to a dev/override path.

---

## Testing Strategy

Development of MailMate **must use test-driven development (TDD)**. No production behavior should be implemented until a failing automated test exists for that behavior and has been run to prove it fails for the expected reason.

Tests must not require Ollama or any real AI provider to be installed.

### TDD mandate

MailMate development must follow strict RED-GREEN-REFACTOR discipline:

1. **RED:** Write a failing test for the behavior before implementing production code.
2. **Verify RED:** Run the specific test and confirm it fails for the expected reason.
3. **GREEN:** Implement the smallest production change that makes the test pass.
4. **Verify GREEN:** Run the specific test and the relevant test group.
5. **REFACTOR:** Clean up only after tests are green.
6. **Regression check:** Run the full applicable suite before merging.

Hard requirements:

- No feature is complete without unit, integration, and end-to-end coverage, unless a documented technical limitation prevents one layer and an approved test substitute is added.
- No bug fix is complete without a failing regression test that reproduces the bug first.
- No refactor is complete without preserving or improving existing tests.
- No provider implementation is complete without mock-provider equivalent tests and contract tests.
- No storage migration is complete without migration tests.
- No Thunderbird adapter behavior is complete without adapter tests or a documented harness simulation.
- No LoRA/data-capture behavior is complete without tests for positive examples, negative examples, privacy filtering, dataset splits, and export formats — and training orchestration is tested through a **mock `TrainerBackend`**, so no test requires a GPU, the Burn backend, or an external trainer.

### Required test layers for every feature area

Every feature area must define all three layers:

| Layer | Purpose | Examples |
|---|---|---|
| Unit tests | Verify isolated behavior and edge cases. | Rule condition evaluation, policy decisions, prompt validation, dataset label scoring. |
| Integration tests | Verify module boundaries and persistence. | Native protocol → app core → storage with mock provider; correction → feedback-row capture → dataset export. |
| End-to-end tests | Verify user-visible flow. | Thunderbird selected message → native host classification → safe tag action; draft reply → edit event → training example. |

If true Thunderbird automation is unavailable in CI, end-to-end tests should use a Thunderbird adapter harness that sends the same native messages the extension sends and verifies the same action responses the extension consumes.

### Unit tests

Cover:

- Domain parsing.
- Message fingerprinting.
- Rule condition evaluation.
- Rule effect generation.
- Policy guard decisions.
- Action planning.
- Provider response validation.
- Prompt construction redaction.
- Draft safety validation.
- Feedback-row and audit-entry serialization.

### Rule engine tests

Required cases:

- System safety rules outrank all other rules.
- Human hard rules outrank learned rules.
- Learned active rules outrank AI suggestions.
- Shadow rules record outcomes but do not apply actions.
- Disabled, retired, and rejected rules do not fire.
- Conflict detection catches contradictory effects.
- Rule version used in a decision is preserved.

### Policy guard tests

Required cases:

- Auto-delete is blocked.
- Auto-send is blocked.
- Link opening is blocked.
- Remote content download is blocked.
- Payment-detail trust is blocked.
- Financial/security/legal moves require review unless explicitly allowed.
- Manual override wins where the requested action is otherwise safe.
- A fired follow-up step never yields a `send_draft`/auto-send action — only `CreateDraft` + `RequireReview` (`requires_review = 1`).

### Provider tests

Use `mailmate-ai::providers::mock` for deterministic responses.

Required cases:

- Mock classification response.
- Mock draft response.
- Mock thread summary.
- Mock task extraction.
- Mock rule proposal.
- Invalid JSON response rejected.
- Schema-valid but policy-invalid response rejected by later layers.

### Native messaging protocol tests

Required cases:

- Request envelope parsing.
- Response envelope serialization.
- Notification envelope serialization (host-initiated, `notification_id`).
- `kind` discriminator handling, including unknown-kind rejection.
- Unknown protocol version handling.
- Unknown request type handling.
- Malformed JSON handling.
- Request ID correlation.
- Single-writer stdout: concurrent notification + response writes never interleave frames.
- Oversize-frame guard: a frame that would exceed the 1 MB host→extension limit becomes a structured `error`, never a written frame.
- Error response formatting.
- `followup_draft_ready` / `followup_needs_attention` notification serialization; the new follow-up control request `type`s parse; reply/won/lost ride `record_user_action` (no new request type for them).

### Storage migration tests

Required cases:

- Fresh database migration.
- Migration from each prior version.
- Rule version immutability.
- Audit-log and feedback-table append behavior.
- Foreign-key constraints (engine-aware: SQLite needs `PRAGMA foreign_keys=ON`; server engines default-on).
- Privacy default: full bodies not retained.
- Full migration + repository suite runs against **each enabled engine** in a CI matrix (fresh + upgrade-from-prior). The **SQLite leg is the default/required** path; Postgres/MariaDB legs are **opt-in and non-blocking**, so the always-running validation never requires an external database — consistent with "no test requires an external provider/tool".
- `0004_followups.sql` fresh + upgrade; `workflow_definition_versions` immutability; the `workflow_instances(status, next_due_at)` index is present; `followup_feedback` and `workflow_conflicts` append-only.

### Integration tests

Required cases:

- Classify message end-to-end through Rust core using mock provider.
- Background queue: new-mail event → `classification_status` pending → worker drains → `classification_ready` notification pushed.
- Queue recovery: `pending`/`processing` rows are requeued after host restart.
- Priority lane: an opened-but-unclassified message jumps the queue.
- Cascade gating: a Tier-1/Tier-2-confident message never reaches the LLM task; an ambiguous one escalates.
- Record a user move as a `filing_feedback` row and generate evidence.
- Propose a rule from repeated feedback rows.
- Accept rule into shadow mode.
- Shadow rule records outcomes but does not execute move.
- Activate rule and produce action plan.
- Policy guard blocks unsafe action from otherwise matching rule.
- Follow-up catch-up-on-launch: host start drains overdue `workflow_instances` and reconciles.
- Follow-up coalescing: multiple overdue steps within the horizon → exactly **one** review-required draft; skipped steps recorded (`step_skipped_coalesced` + `followup_coalesced` audit row).
- Follow-up staleness: overdue past the abandon horizon → `needs_attention`, **no draft**.
- Exit-on-reply: inbound mail matched by host-side thread identity moves the instance to `engaged` and clears `next_due_at`.
- Restart recovery / lease: an orphaned mid-fire row is reclaimed with no double-fire (idempotency key `(instance, step_index)`).
- Shadow workflow: a `shadow_mode` `WorkflowDefinition` writes `workflow_shadow_outcomes` and surfaces no draft.
- Won/lost via `record_user_action` → `completed`, with a `followup_feedback` row.

### End-to-end tests

Where possible:

- Use a Thunderbird test harness or extension test framework for adapter behavior.
- Simulate selected message classification.
- Simulate new mail event.
- Simulate context-menu action.
- Simulate draft creation.
- Verify native host receives expected protocol messages.

### Simulation tests against historical examples

MailMate should support replaying historical sanitized email examples as simulation fixtures.

Simulation should measure:

- Classification accuracy.
- Rule fire rate.
- Shadow rule precision.
- False-positive risky actions.
- User override rate.
- Policy block rate.
- Follow-up behaviour: replay a quote thread with a host-closed gap and a mid-sequence reply, asserting no auto-send, coalescing to ≤1 draft, staleness→`needs_attention`, and exit-on-reply.

Historical fixtures must be sanitized and should not require full body retention by default.

---

## Implementation Sequence

v1 encompasses **all** phases below; the phases are engineering build order, not an MVP cut — the system ships complete in the first release.

### Phase 0: Repository foundation

1. Create Rust workspace.
2. Add crates for native host, core, domain, policy, rules, learning, training, AI, ml (Burn substrate, feature-gated), storage, audit, and test support.
3. Create long-lived `develop` branch for integration and keep `main` as the release branch.
4. Add GitHub Actions workflows for PR validation, release, docs, security checks, and Dependabot auto-merge.
5. Add Dependabot configuration for one PR per dependency/security issue targeting `develop`.
6. Add branch protection rules for `develop` and `main`.
7. Add the comprehensive PR-validation workflow as **always-running jobs** (formatting, clippy, unit, integration, e2e/harness, migration, provider contract, training-data). Jobs for not-yet-built subsystems have empty test sets that pass trivially; they fill in as each subsystem lands (no required-check deadlock).
8. Add test harness structure for unit, integration, and end-to-end tests before implementing feature behavior.
9. Add TDD contribution rules: every production change must start from a failing test.
10. Add initial docs for architecture, native messaging, rule format, testing policy, CI policy, dependency automation policy, and release policy.

### Phase 1: Native messaging skeleton

1. Build `mailmate-native-host` executable.
2. Implement native messaging frame read/write.
3. Define protocol envelope types.
4. Add tests for protocol parsing and serialization.
5. Add minimal Thunderbird extension that can ping the native host.

### Phase 2: Domain and storage foundation

1. Define message, thread, sender, action, rule, event, and draft domain types.
2. Define the repository traits + the `StorageBackend` seam (`backend.rs`, `dialect.rs`); add the default SQLite backend and its migrations, structured as `common` + per-dialect overlay from day one (even with only SQLite implemented).
3. Implement repository traits over the backend seam.
4. Add migration tests (SQLite required; engine-matrix scaffolding in place for opt-in server legs).
5. Add privacy default tests proving full body storage is disabled.

### Phase 3: Policy guard

1. Implement hard policies.
2. Add policy evaluation for action plans.
3. Add policy tests for every required hard policy.
4. Make all action planning pass through policy guard.

### Phase 4: Rule engine

1. Define rule condition JSON format.
2. Implement condition evaluator.
3. Implement rule hierarchy evaluation.
4. Implement explanations.
5. Implement shadow-mode outcome recording.
6. Add rule engine tests.

### Phase 5: AI provider abstraction

1. Define provider trait and request/response types.
2. Implement mock provider.
3. Implement structured response validation.
4. Add provider tests.
5. Add provider adapters for OpenAI-compatible endpoints, LM Studio, llama.cpp server, and Ollama.
6. Ensure provider-specific details do not leak outside adapters.
7. Introduce `mailmate-ml` (Burn) and the `Tier2Classifier` trait with its Burn-default discriminative classifier (logistic regression as the lightweight alternative) for the cascade's Tier 2; register the feature-gated in-process Burn `AiProvider` (`kind = "burn"`) as a supported v1 provider (co-first-class with Ollama/llama.cpp).

### Phase 6: Action planner

1. Build action planner that combines policy, rules, and provider suggestions.
2. Support the three action vocabularies: `PlannedAction` (`Tag`, `Move`, `MarkJunk`, `CreateDraft`, `RequireReview` — suggested vs. applied is the `PolicyOutcome`), `TaskRequest` (`SummarizeThread`, `ExtractTasks`, `ExplainClassification`, `DraftReply`), and `UserCorrection` (`MarkSpam`, `MarkNotSpam`, `LearnFiling`).
3. Add integration tests using mock provider.

### Phase 7: Learning engine

1. Capture corrections into the per-task feedback tables (with prompted reasons) and cross-cutting facts into `audit_log`.
2. Aggregate evidence from repeated feedback rows.
3. Generate candidate rule proposals.
4. Add proposal review statuses.
5. Add shadow testing.
6. Add outcome monitoring.

### Phase 8: Agent curator

1. Implement curator requests to AI provider.
2. Support proposing, refining, merging, splitting, stale detection, conflict detection, threshold suggestions, and feedback summaries.
3. Require human review for activation of risky rules.
4. Add tests with mock curator responses.

### Phase 9: LoRA export, adapter evaluation, and training orchestration

> Capture is **not** deferred to this phase: the per-task feedback tables ship with
> each AI function as it lands (Phases 4–8). This phase builds the rest of the pipeline,
> TDD'd against fixtures from the start.

1. Add `mailmate-training` crate (it owns the `TrainerBackend` trait) and wire the Burn trainer impl from `mailmate-ml`.
2. Implement `training_datasets`, `lora_adapters`, and `lora_eval_runs` migrations
   (no `training_examples` table — datasets are derived from the feedback tables).
3. Derive positive/negative examples on export from the per-task feedback tables.
4. Add redaction and privacy-level enforcement at export time.
5. Export SFT, preference, evaluation, and safety-counterexample JSONL datasets.
6. Add adapter metadata import for externally trained LoRA artifacts.
7. Add compatibility checks for base model family, tokenizer hash, and chat template hash.
8. Provide the `TrainerBackend` with the **in-process Burn** default; keep the external toolchain (subprocess) and remote as pluggable alternatives and a mock for tests; gate Burn behind a cargo feature. `capabilities()` advertises `lora` only when a real low-rank adapter is implemented — do not promise on-device LoRA in v1.
9. Add evaluation gates before an adapter can be activated.
10. Add tests proving no LoRA path bypasses rule hierarchy or policy guard (mock `TrainerBackend`, no GPU/external tool required).

### Phase 10: Thunderbird adapter expansion

1. Read selected message.
2. Listen to new mail events.
3. Add context-menu actions.
4. Open draft replies.
5. Apply safe actions returned by Rust host.
6. Record user actions and execution results back to Rust host.

### Phase 11: Sales pipeline and follow-up workflows

Depends on the action planner (Phase 6), learning engine (Phase 7), curator (Phase 8),
and reuses LoRA export (Phase 9). TDD-mandated — each step starts from a failing test.

1. Add `mailmate-domain` pipeline/workflow types and the `mailmate-workflow` crate (definition, instance FSM, scheduler, exit detection, emit).
2. Add `0004_followups.sql` (`common` + `sqlite` overlay; indexes incl. `workflow_instances(status, next_due_at)`; the workflow-performance view) with migration tests.
3. Add `pipeline_items` / `workflow` / `followup_feedback` repositories; wire `followup_feedback` into `FeedbackRepository<F>`.
4. Reuse rule version/lifecycle machinery for `WorkflowDefinition` (third kind); add the containment `WorkflowConflict` check.
5. Implement the catch-up-on-launch `FollowUpScheduler` (drain + coalescing/staleness guard + lease/restart recovery), driven by `app.rs`; confirm the action planner accepts a `FollowUpDue` trigger carrying a `pipeline_item` (no new action verb).
6. Implement `ExitDetector` (reply via host-side thread identity; won/lost via `record_user_action`).
7. Add protocol frames (`followup_draft_ready`, `followup_needs_attention`, the control requests) with protocol tests.
8. Add curator `new_workflow` / refine / suggest-enrollment proposals + the shadow-mode workflow promotion report over `workflow_shadow_outcomes`.
9. Extend derive-on-export views over `followup_feedback` / `draft_feedback`; LoRA stays advisory/gated (never enables auto-send).
10. Simulation + integration/e2e tests per *Testing Strategy* (catch-up gap, coalescing, staleness, exit-on-reply, no auto-send).

### Phase 12: Product hardening

1. Add audit/explanation UI surfaces.
2. Add settings for providers and privacy retention.
3. Add import/export for rules.
4. Add simulation runner.
5. Add performance benchmarks.
6. Add backup/restore story for the embedded engine (SQLite); document server-engine backup (`pg_dump` etc.) as the operator's responsibility.

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Thunderbird extension API limitations | Some actions may be hard or impossible from MailExtension APIs. | **Confirmed sufficient** (ESR140/MV3): moves via `messages.move`/`copy`, tags via `messages.update {tags}` + `messages.tags.*`, junk/read/flagged via `messages.update`, drafts via `compose.beginNew/Reply/Forward` → `compose.saveMessage({mode:'draft'})` (review-required, never auto-sent), intake via `messages.onNewMailReceived` (see *Open Design Questions*). Keep Thunderbird as a thin adapter; re-resolve session-scoped `MailFolderId`s per session; isolate capabilities; graceful degradation and context-menu workflows. |
| Native messaging complexity | Protocol bugs can break core UX. | Version protocol; add exhaustive protocol tests; keep messages explicit and schema-validated. |
| Provider-specific leakage | Core becomes coupled to one AI backend. | Enforce provider trait boundary; code-review rule that provider-specific types stay inside provider modules. |
| Unsafe automation | User loses trust or mail is mishandled. | Separate policy guard; conservative defaults; no auto-delete or auto-send; review gates. |
| Hidden learning behavior | System becomes unpredictable. | Require explicit rules, audit logs, rule statuses, and explanations. |
| Over-broad learned rules | Wrong messages are moved/tagged. | Use shadow testing, evidence thresholds, conflict detection, and easy disable/undo. |
| Privacy leakage | Sensitive email content stored or sent unexpectedly. | Local-first defaults; body retention disabled; provider privacy warnings; redaction; hashes by default. |
| Prompt/response instability | AI outputs malformed or unsafe content. | Structured schemas; validation; mock tests; retry only through safe prompts; reject invalid output. |
| Test dependence on local AI tools | CI becomes flaky and hard to run. | Mock provider required; no tests require Ollama or any external provider. |
| TDD skipped under delivery pressure | Architecture drifts into untested AI glue and unsafe automation. | CI gates, code review checklist, failing-test-first requirement, and no merge without unit, integration, and end-to-end coverage. |
| Release branch drift | `develop` and `main` diverge or releases skip validation. | Require release PRs from `develop` to `main`, protected branches, required GitHub Actions checks, and release workflow reruns on `main`. |
| Dependency update breaks behavior | Automated bumps can introduce subtle regressions. | One PR per dependency issue, full PR validation before merge, no branch-protection bypass, auto-merge only low-risk passing updates. |
| Rule conflict accumulation | Rule system becomes hard to reason about. | Conflict table; curator detection; human review; rule retirement flow. |
| Audit log growth | SQLite database grows too large. | Retention policies, compaction of derived features, export/purge tools. |
| Premature storage abstraction | Cost/complexity with no second engine ever shipping. | Keep the seam THIN (repo traits + one `dialect` module); ship only SQLite; leave the server backend a documented stub until a real need exists. |
| Burn pre-1.0 churn | Upstream API churn on the on-device learned layer. | **Pin an exact Burn version** (sibling project *idiolect* ships on a pinned `burn = "=0.13.2"`, CPU/ndarray — proof it is stable enough to ship); keep Burn behind engine-neutral contracts + traits (`Tier2Classifier`, `TrainerBackend`) + a swappable adapter, so a version bump or engine swap never touches core. The committed trainer trains **small models** (classifier/preference/tone); on-device **LoRA** is a deferred optional target — `capabilities()` advertises `lora` only when the primitive-built adapter works, external toolchain as the proven fallback. The **generative foundation model runs frozen on a mature engine** (Ollama/llama.cpp) by design, so model breadth never depends on Burn; `burn-lm` (v0.0.1, no GGUF) is an optional pure-Rust generative path, not a dependency. |
| Follow-up draft blast after long downtime | User returns to a backlog of nagging drafts. | Catch-up-on-launch coalesces overdue steps to one current draft; very-stale → `needs_attention` (no draft); the `awaiting_review` hold caps to one pending follow-up draft per instance. |
| Nagging after the counterparty replied | Annoyed customer, lost trust. | Exit conditions pause the instance on inbound reply (host-side thread identity) and on user won/lost; `next_due_at` is cleared so the scheduler stops selecting it. |
| Pipeline tracker scope-creeps into a CRM | Bloat; contradicts non-goals. | `pipeline_items` is deliberately minimal (stage + thread anchor + counterparty + opaque amount hint); no contacts/line-items/forecasting; reaffirmed in Non-Goals. |
| Follow-up becomes a hidden auto-sender | Violates the core send posture. | Follow-up output is `CreateDraft + RequireReview` only; no `send_draft` action exists in the path; `never_auto_send_drafts` untouched; covered by policy-guard tests. |

---

## Open Design Questions

Still open (the one genuine product/UX judgment the maintainer must own):

**First human-review surface** — which UI hosts human review of rules/proposals/follow-ups. The review loop is *required* everywhere; no principle picks its surface. **Design constraint (maintainer-set):** a **local** UI that runs **natively cross-platform** (macOS / Linux / Windows) — it need **not** be a web UI. That reframes the choice: the surface is just another **client of `mailmate-core`** (a sibling of the Thunderbird adapter), so the axis is the *renderer*, not "inside Thunderbird vs. a browser". The candidates, with their **actual** constraints:

- **(A) In-Thunderbird WebExtension options/tab page.** *Web tech, inside Thunderbird* — cross-platform because Thunderbird is. *Can do:* full HTML5/CSS + bundled frameworks (Vue/Preact), `diff2html`-style diffs, canvas/SVG charts. *Hard limits (ESR140 / MV3):* CSP forbids inline scripts/`eval` (all JS externalized); `storage.local` ≈ 10 MB; **`runtime.connectNative` is background-script-only** — an options page relays `options → runtime.sendMessage → background → connectNative` and must reconnect across MV3 event-page suspension; it is a **separate tab, not an inline sidebar** (no hooking the folder tree or message list). *Wins:* lowest footprint (no extra artifact), in-process, **no network/CSRF surface**. *Ceiling:* modest.
- **(B) Native companion app, pure-Rust (egui / Iced / Slint).** *A native window that links `mailmate-core` directly* — no browser, no CSP, no port, no relay, no CSRF. Cross-platform via the toolkit; can ship as a **`mailmate review` subcommand of the same binary**, so "native GUI" and "single binary" stop being in tension. *Toolkits:* **egui** — immediate-mode, ≈10× the ecosystem of the others, ideal for a diff/approve/conflict tool; **Iced** — retained/Elm, better for complex multi-screen state; **Slint** — declarative markup, designer-friendly (mind the dual MIT/royalty-free-vs-commercial licensing; avoid the AGPL Qt backend). *Honest limit (all three, verified):* heavy sortable/filterable data-grids with inline edit are today's weak spot. *Wins:* pure-Rust, single-binary, zero web/network surface, native on all three OSes.
- **(C) Native companion app, webview (Tauri v2).** *Web frontend, but a desktop app — not a browser.* Web-grade UI (Monaco, rich diff libs) over the **OS webview** (WebView2 / WKWebView / WebKitGTK), small binaries (no bundled Chromium). IPC is a **capability-scoped custom protocol** (`ipc://localhost`), **not** an exposed TCP port — so it gets the rich UI *without* loopback's CSRF/DNS-rebind/port tax (the localhost-server plugin that *would* open a port is a separate opt-in its own docs warn against in production). *Costs:* a second (web) toolchain to build/secure; per-OS webview differences; a separate bundle, so **not** one Rust binary. *Best when* the review UI needs web-grade richness or the heavy-data-grid case (B) struggles with.
- **(D) Loopback web UI in a browser** (`127.0.0.1:PORT`). *Same web-grade UI as (C) but in a real browser* — and that is the problem: localhost has no auth (CSRF / DNS-rebinding / port-scanning, only partial mitigations — bind `127.0.0.1` only, single-use URL token, validate `Origin`, same-machine attackers out of scope), port conflicts, "open your browser" friction. **(C) delivers the same UI without any of this.** Pick (D) *only* if a true browser/remote surface is wanted — which contradicts the local + native constraint. **Demoted.**
- **(E) Native TOML/DB edited directly.** Not a human surface (zero UI, developer-only, no visual diff, opaque field names) — but the **auditable/testable substrate** that exists day one *beneath* whichever renderer ships (review state is DB/TOML-backed).

**Recommendation (revises the earlier "ship the in-Thunderbird page first" — the local + native + cross-platform steer changes it):** make the primary surface a **native companion app built with `egui`, shipped as a `mailmate review` subcommand of the same binary** — it satisfies local + native-cross-platform cleanly, keeps the single-binary / no-browser / no-CSRF posture, and fits the "core + clients" topology (the Thunderbird page deep-links "Review N pending" → signals the host → host launches/focuses the companion window). Keep the **in-Thunderbird page (A)** as a *thin* launcher / inline quick-approve, not the main surface. **Escalate to Tauri (C)** only if the review UI outgrows immediate-mode (rich editing, or the heavy data-grid case) — Tauri keeps you out of the browser/localhost-security mess even then. **Drop the loopback browser (D)** as plan-of-record. **(E)** is the substrate regardless. The one residual fork is **egui-first** (pure-Rust, single binary; you will feel it on complex tables/diffs) vs. **Tauri-first** (web-grade UI; a second toolchain and a separate bundle, losing "one Rust binary") — recommend egui-first, Tauri as the documented escape hatch. *(Yours to ratify; the steer already eliminates D and demotes A from primary.)*

Resolved during this design pass:

- **Rule condition language** → committed to a declarative JSON AST (see *Condition language*).
- **Sync vs. queued classification** → background-queued with push (see *Native Messaging Protocol*).
- **Encrypted local body retention keying** → dropped; local body encryption is out of the core design (see *Storage Strategy*).
- **Default ML substrate** → Rust-native **Burn**, behind traits and feature-gated (see *Replaceable Components and Extension Points*).
- **Default trainer backend** → in-process **Burn, committed and pinned** — trains small classifier / preference / tone models on-device today (idiolect-proven); external toolchain / remote / mock stay pluggable behind `TrainerBackend`; on-device LoRA is a deferred optional target (see *On-Device Training Layer*).
- **Tier-2 classifier engine** → Burn discriminative classifier by default behind the `Tier2Classifier` trait, with logistic regression retained as a lightweight alternative (see *Classification Cascade*).
- **Storage engine coupling** → storage reached only through repository traits over a `StorageBackend` seam; SQLite is the zero-config default, a server engine (Postgres/MariaDB) is opt-in (see *Storage Strategy*).
- **Storage driver underpinning the seam** → **`rusqlite` (bundled) is the embedded default; `sqlx` is the driver for the opt-in server backend** — a two-driver split, not a single unified driver. `sea-orm` is rejected (its entity layer is redundant against the hand-rolled portable schema and would sit awkwardly above the repository traits); `diesel` is excluded (its philosophy fights portability). The lost cross-engine `query!` macros are moot because the `StorageBackend` seam already bans SQL strings from crossing it and quarantines per-engine SQL in the `dialect` module. A **server engine is a genuinely supported but second-class user config** — opt-in for users who already run a database, selected via `[storage] engine` + URL, kept a documented stub until a real need, with non-blocking CI legs; SQLite is the only engine that must always work (see *Storage Strategy*).
- **Burn default compute backend** → **CPU NdArray/burn-flex** is the default build/run target (pure-Rust single binary, no hard GPU dependency); GPU (CUDA / Metal / WGPU) is opt-in for training/heavy inference via `[ml] backend`. The in-process Burn LLM provider is a **supported v1** provider — see the *Burn LLM provider in v1* item below (and *Replaceable Components and Extension Points*, *On-Device Training Layer*).
- **Thunderbird API sufficiency** → **confirmed sufficient** for reliable folder moves, tags, junk marks, and draft creation across Linux/macOS/Windows via stable, documented `messenger.*` (MailExtension/WebExtension) methods; **target ESR140 + Manifest V3**. Concretely: moves = `messages.move(messageIds, destinationFolderId)` (`messages.copy` as the fallback when a read-only store blocks removal); tags = `messages.update(id, {tags:[...keys]})` with the catalog via `messages.tags.list/create/update/delete` (apply tag *keys*, not display names); junk = `messages.update(id, {junk:true|false})` (pair with `messages.move` to relocate — junk does not relocate by itself); read/flagged ride the same `messages.update {read}/{flagged}`; new-mail intake = `messages.onNewMailReceived` (register *synchronously* at the top of the MV3 background event page; `monitorAllFolders=true` to watch beyond Inbox); folder/special-folder discovery = `folders.query/get/getSubFolders` + `accounts.list` resolving to a `MailFolderId`; reads = `messages.list/query/get/getFull/getRaw` (some sub-fields are version-gated — `getFull`/`getRaw` `decodeContent`/`decrypt` and `messages.list` `sortType` post-date ESR140; verify per-field against the target ESR, though the design relies on none of them); right-click hooks = `menus.create({contexts:['message_list','folder_pane',…]})` (the hook + `info.selectedMessages`; we wire the action). **Draft creation = the compose pipeline**: `compose.beginNew/beginReply/beginForward(...)` → `compose.saveMessage(tabId, {mode:'draft'})` into Drafts — identity/FCC-aware. The compose API only *persists* a draft; the no-auto-send guarantee is **MailMate's policy guard** (`CreateDraft + RequireReview`), not the Thunderbird API — the follow-up/draft path never calls any send API, so `never_auto_send_drafts` is enforced by the guard, not inferred from the compose surface. The headless `messages.import(file, draftsFolderId, props)` exists but is **not** used as the draft path (it skips the compose pipeline and forces hand-built MIME). All mail-action APIs are platform-independent; the **sole per-OS divergence is native-messaging *host registration*** (manifest dir on Linux/macOS vs. registry key on Windows), already handled by the `install` subcommand — not the mail actions. Caveat baked into design: `MailFolderId` is session-scoped and invalidated by folder rename/move, so folders are re-resolved per session, never cached across sessions (see *Distribution and Installation*, *Risks and Mitigations*).
- **Default message text sent to providers per feature (egress posture)** → **send the least the cascade forces, never the full body by default.** Tier 1/2 classification send **zero content to any model** (deterministic signals + the local Tier-2 classifier over structural features). Tier 3 and background filing/extraction send only a **bounded snippet** (≈4 KB phishing / 2 KB filing, quoted chains + signatures stripped, UTF-8-truncated) and **only to a local provider** by default — background classification/filing/extraction **never auto-send body content to a remote provider** (a config default, off; choosing a remote provider does not enable background body egress, and background escalation uses the local provider). User-initiated summary/draft/extract may use **bodies capped at the active content-retention level** (full to a local provider always; to a remote provider on opt-in), degrading to a snippet when retention is `metadata`. **Never leave by default, any provider:** attachment *content*, newly-fetched remote content (`never_download_remote_content_for_classification`), full recipient lists, or anything past a per-feature token budget (truncations audit-logged). Per-feature snippet caps + token budgets are versioned prompt-template parameters, not hardcoded constants (see *Content retention levels*, *Provider routing as a learning problem*).
- **Provider calls off until configured** → **yes.** The provider registry ships **empty**; `default_provider` is a user choice, not a shipped value (the config example is illustrative of *format*). LLM-always-path tasks (Tier-3 escalation, summaries, draft *bodies*, task extraction, LLM rule-curation phrasing) are unavailable until a provider is chosen; a Tier-3-needed message **degrades to `RequireReview`**, never an auto-clear. MailMate is deliberately fully functional with **zero providers**: Tier 1/2 classification, the rule/policy/action/audit spine, the learning loop, follow-up scheduling, and review-required draft *slots* all run with no LLM. Selecting a **local** provider unlocks all features with **zero egress**; a **remote** provider unlocks user-initiated generative features per the egress posture above and still leaves background remote-body egress off (see *Provider-Abstraction Layer*, *Non-Goals*).
- **`WorkflowDefinition` conflict storage** → a **sibling `workflow_conflicts` table** (in `0004_followups`), **not** a `workflow` kind on `rule_conflicts`. A workflow conflict is a *containment* check (don't run two active workflows on one pipeline item: `pipeline_item_id` + the two workflow/instance scopes + a `concurrent_active_workflow` vocabulary), structurally unlike `rule_conflicts` (which carries `rule_kind`, `rule_a_id`/`rule_b_id` into the rule tables, and an AST/effect-overlap `conflict_kind` vocabulary). This honors single-owner-per-fact and mirrors the existing `workflow_shadow_outcomes`-vs-`shadow_outcomes` split; `WorkflowEngine::detect_conflicts` already returns the distinct `WorkflowConflict` type (see *Workflow engine and follow-up scheduler traits*, *Data Model*).
- **Seeded default cadence** → **ship a stock 3/7/14/28 `WorkflowDefinition`, but seeded *inactive*** (status `draft`/`shadow_mode`, never auto-armed, never auto-enrolling) — not "all cadences user-authored", and not active-on-install. A user must review/activate and enroll it (`pipeline_items.created_by` is `user, never ai`). This removes blank-page friction while preserving every guarantee (no auto-send, no auto-enroll, review-to-activate), mirroring the existing *seeded prompt templates* precedent (see *Rule Lifecycle*, *Sales Pipeline and Follow-up Workflows*).
- **Follow-up send posture** → review-required only; a due step drives P2 to emit `CreateDraft + RequireReview`; no auto-send/gated exception (see *Sales Pipeline and Follow-up Workflows*, *Policy Guard Design*).
- **Follow-up runtime** → catch-up-on-launch; durable `workflow_instances.next_due_at` polled by an in-host worker; no OS daemon; staleness/coalescing guard.
- **Cadence modeling** → a versioned `WorkflowDefinition` reusing the rule version/lifecycle machinery (not plain `ActionRule`s, not a new classification ladder); workflow shadow uses a dedicated `workflow_shadow_outcomes` table with a cadence-fit metric (since the send-acceptance counterfactual is unobservable in shadow).
- **v1 scope** → v1 ships the **full working product**. The Implementation-Sequence phases are engineering build order, not a staged feature release; there is no reduced MVP slice (see *Implementation Sequence*).
- **Burn LLM provider in v1** → the in-process Burn provider (`kind = "burn"`) is an **optional pure-Rust generative path** behind the `AiProvider` trait, feature-gated. By design the generative **foundation model runs frozen on a mature engine** (Ollama / llama.cpp), idiolect-style — so `burn-lm`'s honest limits (v0.0.1, Llama 3.x/TinyLlama, no GGUF interop) never constrain model breadth; it is an option for users who want one pure-Rust stack, not a dependency. Burn's **learned-layer roles (Tier-2 classifier, `TrainerBackend`) are the committed v1 defaults** (see *Provider-Abstraction Layer*, *Replaceable Components and Extension Points*).
- **Burn posture / buildability (idiolect-aligned)** → the on-device learning core is **implementable now**: the sibling project *[idiolect](https://github.com/nick-tgcs/idiolect)* ships the exact shape — a **frozen foundation model** on a mature engine + a **pinned Burn trainer** for small learned models + an `evaluate_promotion` gate. MailMate commits to **Burn as the learned-layer substrate** (Tier-2 classifier, preference/tone models, trainer), pinned and **behind engine-neutral contracts + a trait + a swappable adapter** so it stays replaceable; on-device **LoRA** is a deferred optional target, not a dependency of the learning thesis (see *On-Device Training Layer*, *Replaceable Components and Extension Points*).

---

## Architecture Summary

MailMate should be built as a modular Rust application with Thunderbird as a thin adapter. The core value is a learning system based on explicit, versioned, auditable, testable, reversible rules. AI providers are useful advisors and curators, but they are replaceable and never bypass policy, rules, validation, or human review.

The ML substrate is Rust-native (**Burn**) by default — the Tier-2 classifier engine and the trainer backend for the on-device **learned layer** — while the **generative foundation model runs frozen** on a mature local engine (Ollama/llama.cpp) behind `AiProvider`, exactly as the sibling project **idiolect** runs Whisper frozen via `whisper-rs`. The learning thesis rides on small Burn models + crystallized rules + an eval/promotion gate, not on fine-tuning the big model — a shape idiolect ships today, so it is buildable now. Every backend (AI provider, storage engine, trainer, classifier) is a config-selected impl behind engine-neutral contracts + a trait, so a better model or engine drops in (or SQLite → MariaDB) without rewriting the core. "Default" never means "hard dependency": Burn is committed but lives behind ports (idiolect's `ml-core` / `ports` / `trainer-burn` split), replaceable if and when needed.

The safest path is to implement the policy guard, rule engine, feedback/audit store, and mock provider before adding real provider adapters. That keeps MailMate testable, provider-agnostic, and faithful to its core design: a local-first mail assistant that improves over time through explicit human-curated principles.
