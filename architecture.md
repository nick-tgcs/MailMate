# MailMate Technical Architecture Plan

> **Project:** MailMate  
> **Technical name prefix:** `mailmate`  
> **Architecture stance:** local-first, Rust-first, provider-agnostic, human-curated learning system for Thunderbird

## Table of Contents

1. [Goals](#goals)
2. [Non-Goals](#non-goals)
3. [Architecture Overview](#architecture-overview)
4. [Component Diagram](#component-diagram)
5. [Module Layout](#module-layout)
6. [Core Domain Model](#core-domain-model)
7. [Data Model](#data-model)
8. [Rust Trait and Interface Definitions](#rust-trait-and-interface-definitions)
9. [Native Messaging Protocol](#native-messaging-protocol)
10. [Rule and Principle System](#rule-and-principle-system)
11. [Rule Lifecycle](#rule-lifecycle)
12. [Two Pipelines](#two-pipelines)
13. [Classification Cascade](#classification-cascade)
14. [Rule Hierarchy and Decision Order](#rule-hierarchy-and-decision-order)
15. [Policy Guard Design](#policy-guard-design)
16. [Learning Loop](#learning-loop)
17. [Self-Iteration Learning Model](#self-iteration-learning-model)
18. [Portable LoRA Training Layer](#portable-lora-training-layer)
19. [Agent Curator](#agent-curator)
20. [Provider-Abstraction Layer](#provider-abstraction-layer)
21. [Action Planning and Execution](#action-planning-and-execution)
22. [Drafting Safety](#drafting-safety)
23. [Storage Strategy](#storage-strategy)
24. [Auditability and Explainability](#auditability-and-explainability)
25. [GitHub Workflow, CI, and Releases](#github-workflow-ci-and-releases)
26. [Distribution and Installation](#distribution-and-installation)
27. [Testing Strategy](#testing-strategy)
28. [Implementation Sequence](#implementation-sequence)
29. [Risks and Mitigations](#risks-and-mitigations)
30. [Open Design Questions](#open-design-questions)

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
- Learning from user behavior through explicit rules/principles.
- Human approval, editing, rejection, disabling, and override of learned behavior.
- Safe, auditable automation.
- Test-driven development for all production code, with unit, integration, and end-to-end coverage for every feature area.

The central product idea is not “an artificial intelligence (AI) wrapper for email.” The central product idea is a **learning decision system** for email, where AI is one replaceable advisor inside a larger rule, policy, storage, and review architecture.

The system should be inspired by Ray Dalio-style principles:

- Decisions become explicit principles.
- Principles are structured rules.
- Rules are versioned.
- Rules are tested.
- Rules are reviewed.
- Rules are improved or retired.
- Humans remain able to override any decision.

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
|  | provider trait |       | engine         |       | SQLite         | |
|  +-------+--------+       +----------------+       +----------------+ |
|          |                                                            |
|          v                                                            |
|  +----------------+  +----------------------+  +--------------------+ |
|  | Ollama adapter |  | OpenAI-compatible    |  | LM Studio adapter | |
|  |                |  | adapter              |  | llama.cpp adapter | |
|  +----------------+  +----------------------+  +--------------------+ |
|          |                                                            |
|          v                                                            |
|  +----------------+                                                   |
|  | mock provider  |                                                   |
|  | for tests      |                                                   |
|  +----------------+                                                   |
+-----------------------------------------------------------------------+
```

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

    mailmate-training/
      Cargo.toml
      src/
        lib.rs
        examples.rs
        labels.rs
        datasets.rs
        export.rs
        lora.rs
        trainer.rs
        trainers/
          mod.rs
          external.rs
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

    mailmate-storage/
      Cargo.toml
      src/
        lib.rs
        sqlite.rs
        migrations.rs
        repositories/
          audit.rs
          feedback.rs
          rules.rs
          messages.rs
          threads.rs
          senders.rs
          drafts.rs
          proposals.rs
          shadow.rs

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

  migrations/
    0001_initial.sql
    0002_rule_versions.sql
    0003_audit_and_feedback.sql

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

---

## Data Model

SQLite is the initial storage engine. The schema should be migration-based and append-friendly.

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
  `summary_feedback`, `task_extraction_feedback`, `rule_proposal_feedback`),
  each the sole owner of its task's signal, capturing the AI proposal, the human
  correction, and the **reason** for it.
- **Rule performance** (the former `rule_outcomes`) and the **audit timeline**
  are *views* over those tables — a view stores nothing, so it cannot duplicate.
- Facts that are genuinely not task-feedback get their own dedicated owner table:
  `rule_conflicts` (conflicts), `shadow_outcomes` (rules that fired in shadow but
  never surfaced, so there is no feedback row), and a narrow `audit_log` for
  cross-cutting provenance with no other home.

No event type lands in two tables.

### `audit_log`

Append-only timeline for cross-cutting provenance that is *not* task feedback.

| Column | Type | Notes |
|---|---:|---|
| `id` | TEXT PRIMARY KEY | `audit_...` |
| `event_type` | TEXT | `action_applied`, `action_blocked_by_policy`, `provider_response_rejected`, rule lifecycle transitions, etc. |
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
| `message_id` | TEXT | Source message replied to |
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
Tier 2  online logistic regression + calibration table over features
        trained online from classification_feedback.   confident? -> accept
Tier 3  LLM classify task   (also the always-path for draft/summarize/extract)
```

- **Tier 2 is online logistic regression with a calibration table**, not naive Bayes:
  the gate thresholds on confidence, and the cascade is only as good as that confidence
  is calibrated. (`calibration_version` is versioned like everything else.)
- **Escalation = versioned confidence bands**, conservative at cold-start (the Tier-2
  model is untrained at install, so the band is wide → escalate often → and the
  escalation rate falls automatically as `classification_feedback` accumulates). The
  virtuous loop: more corrections → better cheap model → fewer LLM calls.
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

### Proposal thresholds

Rules should not be proposed after a single event except when the user explicitly chooses “learn this filing action.”

Possible thresholds:

- 3 similar manual moves from same domain to same folder.
- 5 similar tags across multiple senders.
- 2 high-confidence phishing corrections.
- Multiple draft edits with the same stylistic correction.

Thresholds themselves can become configurable principles.

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
  "policy_version": "policy_builtin_1"
}
```

### Local models before fine-tuning

The initial learning model should not require training neural-network weights. MailMate can get strong self-iteration from simpler, auditable local models:

- Frequency counters.
- Online statistics.
- Similarity clustering over message features.
- Online logistic regression with a calibration table over extracted features (the cascade's Tier 2 — naive Bayes was considered and rejected for poor calibration; see *Classification Cascade*).
- Contextual bandits for low-risk choices such as prompt template selection.
- Calibration tables for provider confidence.
- Rule outcome scoring.

This keeps learning transparent and testable. Later, MailMate can optionally support embeddings and portable LoRA fine-tuning, but only as an implementation of the same auditable proposal/evaluation loop. LoRA training data capture is described separately because it requires explicit positive/negative examples, dataset versioning, privacy controls, and adapter compatibility metadata.

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

### MVP self-iteration path

The first useful version should implement self-iteration in this order:

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

## Portable LoRA Training Layer

MailMate should be able to produce training data for a portable LoRA adapter that can “add on” to the user’s chosen base model. This is different from the normal rule-learning loop. Rules remain the primary, auditable source of behavior; LoRA training is an optional personalization layer that teaches the model MailMate-specific and user-specific preferences such as tone, classification style, summarization format, and task extraction conventions.

> **Built from the start, with two structural rules:**
> 1. **The full pipeline is in scope from day one** — capture, all four export views,
>    adapter metadata import, compatibility checks, eval gates, and training
>    orchestration — TDD'd against fixtures so there is no "we forgot to capture" gap.
> 2. **Training executes through a pluggable trainer backend** (external documented
>    toolchain by default, via subprocess), so the core does not hard-couple a GPU
>    training runtime and the "portable" goal stays intact. MailMate *drives* the whole
>    loop; only the weight-crunching is behind a swappable backend.
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

Some providers may support adapters directly; others may require merging outside MailMate; others may not support adapters at all. The core should expose this as capability metadata, not assume a specific backend.

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
itself sits behind a **pluggable trainer backend** (external documented toolchain by
default, invoked as a subprocess; swappable for an in-process backend later). MailMate
drives every step:

1. Per-task feedback tables capture corrections + reasons continuously (core, not training-specific).
2. At export time: derive examples from the feedback tables, redact, normalize.
3. Split deterministically into train/validation/test/holdout (a function of source-row IDs, not a stored column).
4. Export JSONL datasets.
5. Invoke the trainer backend (default: external toolchain via subprocess).
6. Import adapter metadata and artifact path.
7. Run local evaluation fixtures.
8. Activate adapter only if evaluation gates pass and the user approves.
9. Feedback capture continues; the next adapter version derives from the grown tables.

This keeps the architecture portable and avoids coupling MailMate to one trainer, GPU setup, or model host — while the orchestration, gates, and tests exist from day one.

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

---

## Provider-Abstraction Layer

AI providers are replaceable adapters behind one trait. Provider-specific details must remain inside provider modules.

Required provider implementations:

- `mailmate-ai::providers::ollama`
- `mailmate-ai::providers::openai_compatible`
- `mailmate-ai::providers::lm_studio`
- `mailmate-ai::providers::llama_cpp`
- `mailmate-ai::providers::mock`

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

[ai.providers.test]
kind = "mock"
fixture_dir = "tests/fixtures/provider_responses"
```

Only provider adapters should interpret provider-specific configuration.

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
- `CreateDraft`
- `RequireReview { target }`

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
Thunderbird event/request
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

Initial storage:

- SQLite via `rusqlite` (bundled).
- Migration-managed schema.
- Append-only audit timeline.
- Immutable rule versions.
- Readable local metadata; hashes used for identity/dedup only.

### Storage execution model

SQLite is a single-file serial writer; an async SQL driver would add ceremony
without buying write parallelism. So:

- Engine: SQLite via `rusqlite` (bundled, synchronous).
- Core traits (`RuleEngine`, `PolicyGuard`, `ActionPlanner`, `LearningEngine`,
  repositories) keep their `async` signatures, but storage is reached through an
  async facade that runs blocking `rusqlite` work via `tokio::task::spawn_blocking`,
  drawing connections from an `r2d2` pool.
- Pragmas set per connection: `journal_mode=WAL` (concurrent readers),
  `foreign_keys=ON` (FK enforcement is off by default in SQLite),
  `busy_timeout` (serializes writers without spurious `SQLITE_BUSY`).
- Only `AiProvider` performs real network async (`reqwest`). `app.rs` is the async
  orchestration seam: it awaits provider calls and the storage facade; rule/policy
  logic is pure/CPU-bound and runs inline inside those async methods.
- Writes serialize through a single logical writer path; reader/writer overlap
  relies on WAL.

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
  SQLite backups are written to disk and referenced by **filesystem path** in the
  message; they never travel through the native-messaging frame. The principle:
  native messaging carries control + small results; large artifacts move as path
  references over the shared local filesystem.

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
those rows.

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
- Storage migration tests.
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
- No LoRA/data-capture behavior is complete without tests for positive examples, negative examples, privacy filtering, dataset splits, and export formats.

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

### Storage migration tests

Required cases:

- Fresh database migration.
- Migration from each prior version.
- Rule version immutability.
- Audit-log and feedback-table append behavior.
- Foreign-key constraints.
- Privacy default: full bodies not retained.

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

Historical fixtures must be sanitized and should not require full body retention by default.

---

## Implementation Sequence

### Phase 0: Repository foundation

1. Create Rust workspace.
2. Add crates for native host, core, domain, policy, rules, learning, training, AI, storage, audit, and test support.
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
2. Add SQLite migrations.
3. Implement repository traits.
4. Add migration tests.
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

1. Add `mailmate-training` crate.
2. Implement `training_datasets`, `lora_adapters`, and `lora_eval_runs` migrations
   (no `training_examples` table — datasets are derived from the feedback tables).
3. Derive positive/negative examples on export from the per-task feedback tables.
4. Add redaction and privacy-level enforcement at export time.
5. Export SFT, preference, evaluation, and safety-counterexample JSONL datasets.
6. Add adapter metadata import for externally trained LoRA artifacts.
7. Add compatibility checks for base model family, tokenizer hash, and chat template hash.
8. Add the pluggable trainer backend (external toolchain by default) and training orchestration.
9. Add evaluation gates before an adapter can be activated.
10. Add tests proving no LoRA path bypasses rule hierarchy or policy guard.

### Phase 10: Thunderbird adapter expansion

1. Read selected message.
2. Listen to new mail events.
3. Add context-menu actions.
4. Open draft replies.
5. Apply safe actions returned by Rust host.
6. Record user actions and execution results back to Rust host.

### Phase 11: Product hardening

1. Add audit/explanation UI surfaces.
2. Add settings for providers and privacy retention.
3. Add import/export for rules.
4. Add simulation runner.
5. Add performance benchmarks.
6. Add backup/restore story for SQLite.

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Thunderbird extension API limitations | Some actions may be hard or impossible from MailExtension APIs. | Keep Thunderbird as adapter; isolate capabilities; use graceful degradation and context-menu workflows. |
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

---

## Open Design Questions

Still open:

1. Which Thunderbird APIs are sufficient for reliable folder moves, tags, junk marks, and draft creation across platforms?
2. How much message text should be sent to providers by default for each feature?
3. Should provider calls be disabled by default until the user chooses a provider?
4. How should MailMate expose rule review: Thunderbird UI page, local web UI, or native settings file first?
5. What is the minimum useful MVP: classification plus tag suggestions, or filing-learning loop first?

Resolved during this design pass:

- **Rule condition language** → committed to a declarative JSON AST (see *Condition language*).
- **Sync vs. queued classification** → background-queued with push (see *Native Messaging Protocol*).
- **Encrypted local body retention keying** → dropped; local body encryption is out of the core design (see *Storage Strategy*).

---

## Architecture Summary

MailMate should be built as a modular Rust application with Thunderbird as a thin adapter. The core value is a learning system based on explicit, versioned, auditable, testable, reversible rules. AI providers are useful advisors and curators, but they are replaceable and never bypass policy, rules, validation, or human review.

The safest path is to implement the policy guard, rule engine, feedback/audit store, and mock provider before adding real provider adapters. That keeps MailMate testable, provider-agnostic, and faithful to its core design: a local-first mail assistant that improves over time through explicit human-curated principles.
