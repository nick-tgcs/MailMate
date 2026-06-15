-- 0006_followups (common): the sales-pipeline + follow-up workflow surface.
--
-- Seven tables land with the follow-up feature (Phase 11). `workflow_definitions` /
-- `workflow_definition_versions` mirror the rule tables (versioned, lifecycle-managed,
-- immutable content per version); `workflow_instances` is the one **mutable-state** row in
-- an otherwise append-only model — the durable temporal trigger (`next_due_at`) the
-- scheduler polls. `followup_feedback` is the sole owner of the cadence/timing/stop signal
-- (the draft body's send disposition lives in `draft_feedback`). `workflow_shadow_outcomes`
-- and `workflow_conflicts` are separate owner tables from `shadow_outcomes` / `rule_conflicts`
-- because a follow-up step is triggered by *time* (no message) and a workflow conflict is a
-- containment check (not AST overlap).
--
-- (Sequencing note: the architecture sketches an aspirational `0004_followups`, but the
-- curator took 0004 and the training layer 0005, so the follow-up surface is 0006.)
--
-- Like the other append/feedback tables, the loose cross-references (`thread_id`,
-- `anchor_message_id`, `draft_id`, the conflict's polymorphic workflow ids) carry NO SQL
-- foreign key by convention; only the workflow-internal ownership edges
-- (version→definition, instance→definition/version/item) are enforced. `current_version_id`
-- carries no FK for the same mutually-circular reason as the rule tables.

CREATE TABLE pipeline_items (
    id                 TEXT PRIMARY KEY,           -- pli_...
    account_id         TEXT NOT NULL,
    thread_id          TEXT NOT NULL,              -- the outbound quote/proposal thread
    anchor_message_id  TEXT,                       -- the sent quote, when known
    counterparty_email TEXT NOT NULL,
    counterparty_domain TEXT NOT NULL,
    title              TEXT NOT NULL,
    item_type          TEXT NOT NULL,              -- quote / proposal
    stage              TEXT NOT NULL,              -- open / engaged / won / lost / abandoned
    amount_hint        TEXT,                       -- display-only, NOT a forecast field
    last_activity_at   TEXT NOT NULL,
    created_by         TEXT NOT NULL,              -- Actor: user (never ai)
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE TABLE workflow_definitions (
    id                   TEXT PRIMARY KEY,         -- wfd_...
    stable_name          TEXT NOT NULL UNIQUE,
    scope                TEXT NOT NULL,            -- reuse RuleScope
    applies_to_item_type TEXT NOT NULL,            -- quote / proposal
    status               TEXT NOT NULL,            -- reuse the rule lifecycle statuses
    current_version_id   TEXT,                     -- no FK (mutually circular; see header)
    created_by           TEXT NOT NULL,            -- user / ai (curator may propose)
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE TABLE workflow_definition_versions (
    id                       TEXT PRIMARY KEY,     -- wfdv_...
    workflow_id              TEXT NOT NULL,
    version_number           INTEGER NOT NULL,     -- monotonic, immutable
    title                    TEXT NOT NULL,
    description              TEXT NOT NULL,
    anchor                   TEXT NOT NULL,        -- quote_sent_at / last_outbound_at / item_created_at
    enrollment_condition_json TEXT,                -- optional JSON-AST condition, opaque
    steps_json               TEXT NOT NULL,        -- ordered cadence steps, opaque
    exit_conditions_json     TEXT NOT NULL,        -- reply_received / won / lost / user_cancel / max_steps
    staleness_json           TEXT NOT NULL,        -- {coalesce, abandon_horizon_days}
    risk_level               TEXT NOT NULL,        -- low / medium / high (medium default)
    created_by               TEXT NOT NULL,
    change_reason            TEXT NOT NULL,
    created_at               TEXT NOT NULL,
    FOREIGN KEY (workflow_id) REFERENCES workflow_definitions(id)
);

CREATE TABLE workflow_instances (
    id                    TEXT PRIMARY KEY,        -- wfi_...
    pipeline_item_id      TEXT NOT NULL,
    workflow_id           TEXT NOT NULL,
    pinned_def_version_id TEXT NOT NULL,           -- canonical pin, immutable for the instance
    thread_id             TEXT NOT NULL,           -- reply-exit lookup (no FK: external id)
    anchor_at             TEXT NOT NULL,           -- resolved anchor the offsets count from
    status                TEXT NOT NULL,           -- the FSM status
    current_step_index    INTEGER NOT NULL,        -- cursor: the next step to fire
    next_due_at           TEXT,                    -- the trigger; non-NULL iff status in (active, snoozed)
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    FOREIGN KEY (pipeline_item_id) REFERENCES pipeline_items(id),
    FOREIGN KEY (workflow_id) REFERENCES workflow_definitions(id),
    FOREIGN KEY (pinned_def_version_id) REFERENCES workflow_definition_versions(id)
);

CREATE TABLE followup_feedback (
    id                        TEXT PRIMARY KEY,    -- flwfb_...
    workflow_instance_id      TEXT NOT NULL,
    pipeline_item_id          TEXT NOT NULL,
    step_index                INTEGER NOT NULL,
    draft_id                  TEXT,                -- send disposition lives in draft_feedback
    pinned_versions_json      TEXT NOT NULL,       -- provenance copy-at-event, opaque
    ai_scheduled_offset_days  INTEGER NOT NULL,
    actual_offset_days        INTEGER,             -- off-cadence signal
    reply_received_before_step INTEGER NOT NULL,   -- 0/1
    reply_latency_days        INTEGER,
    outcome                   TEXT NOT NULL,       -- cadence-only disposition
    coalesced_from_json       TEXT,                -- step indexes collapsed into this one
    human_reason_code         TEXT,
    human_reason_text         TEXT,
    polarity                  TEXT NOT NULL,       -- positive / negative
    created_at                TEXT NOT NULL
);

CREATE TABLE workflow_shadow_outcomes (
    id                                 TEXT PRIMARY KEY,  -- wsho_...
    workflow_id                        TEXT NOT NULL,     -- shadow workflow that would have fired
    workflow_version_id                TEXT NOT NULL,
    pipeline_item_id                   TEXT NOT NULL,
    thread_id                          TEXT NOT NULL,     -- no message_id (time-triggered)
    step_index                         INTEGER NOT NULL,
    would_fire_at                      TEXT NOT NULL,
    reply_before_fire                  INTEGER NOT NULL,  -- 0/1
    matched_manual_followup_within_days INTEGER,          -- cadence-fit proxy
    created_at                         TEXT NOT NULL
);

CREATE TABLE workflow_conflicts (
    id               TEXT PRIMARY KEY,             -- wcf_...
    pipeline_item_id TEXT NOT NULL,                -- the item both workflows target
    workflow_a_id    TEXT NOT NULL,                -- first workflow (or active instance)
    workflow_b_id    TEXT NOT NULL,                -- second workflow on the same item
    conflict_kind    TEXT NOT NULL,                -- concurrent_active_workflow
    status           TEXT NOT NULL,                -- open / resolved (mutable)
    detected_at      TEXT NOT NULL
);
