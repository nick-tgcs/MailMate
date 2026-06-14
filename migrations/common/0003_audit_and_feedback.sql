-- 0003_audit_and_feedback (common): the learning-loop capture surface.
--
-- Single writer per fact (see *Capture model*): corrections land in the per-task feedback
-- tables (classification_feedback, filing_feedback — the two whose AI functions are live by
-- Phase 7; the rest ship with their functions later); cross-cutting provenance with no
-- feedback home lands once in audit_log; shadow firings (no surfaced feedback) own
-- shadow_outcomes; derived evidence links live in rule_evidence; proposals in
-- agent_proposals. Rule/workflow performance is a *view*, never a table here.

CREATE TABLE audit_log (
    id              TEXT PRIMARY KEY,
    event_type      TEXT NOT NULL,
    message_id      TEXT,
    thread_id       TEXT,
    rule_kind       TEXT,
    rule_id         TEXT,
    rule_version_id TEXT,
    proposal_id     TEXT,
    actor           TEXT NOT NULL,
    payload_json    TEXT NOT NULL,                 -- event-specific data, opaque
    created_at      TEXT NOT NULL
);

CREATE TABLE classification_feedback (
    id                    TEXT PRIMARY KEY,
    message_id            TEXT NOT NULL,
    pinned_versions_json  TEXT NOT NULL,           -- provenance copy-at-event, opaque
    ai_label              TEXT,
    ai_score              REAL,
    ai_rationale          TEXT,
    human_label           TEXT NOT NULL,
    human_reason_code     TEXT,
    human_reason_text     TEXT,
    salient_features_json TEXT NOT NULL,           -- features that mattered, opaque
    polarity              TEXT NOT NULL,
    created_at            TEXT NOT NULL
);

CREATE TABLE filing_feedback (
    id                    TEXT PRIMARY KEY,
    message_id            TEXT NOT NULL,
    pinned_versions_json  TEXT NOT NULL,           -- provenance, opaque
    sender_domain         TEXT,                    -- retained for deterministic domain clustering
    ai_suggested_folder   TEXT,
    human_chosen_folder   TEXT NOT NULL,
    basis                 TEXT,
    matched_rule_id       TEXT,
    polarity              TEXT NOT NULL,
    created_at            TEXT NOT NULL
);

CREATE TABLE rule_evidence (
    id            TEXT PRIMARY KEY,
    rule_kind     TEXT,                            -- disambiguates rule_id across the two rule tables
    rule_id       TEXT,
    proposal_id   TEXT,
    source_kind   TEXT NOT NULL,                   -- which feedback table the source row lives in
    source_id     TEXT NOT NULL,                   -- the supporting per-task feedback row id
    message_id    TEXT,
    evidence_kind TEXT NOT NULL,                   -- positive / negative / counterexample / override
    weight        REAL NOT NULL,
    summary       TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE shadow_outcomes (
    id                        TEXT PRIMARY KEY,
    rule_kind                 TEXT NOT NULL,        -- classification / action
    rule_id                   TEXT NOT NULL,
    rule_version_id           TEXT NOT NULL,
    message_id                TEXT NOT NULL,        -- a shadow RULE is always message-triggered
    would_have_action_json    TEXT NOT NULL,        -- the effect it would have proposed, opaque
    would_have_policy_outcome TEXT NOT NULL,        -- the PolicyOutcome it would have hit
    matched_later_user_action INTEGER,             -- did the user later do the same? (0/1/NULL)
    created_at                TEXT NOT NULL
);

CREATE TABLE agent_proposals (
    id                 TEXT PRIMARY KEY,
    proposal_type      TEXT NOT NULL,
    status             TEXT NOT NULL,               -- the mutable review status (authoritative)
    title              TEXT NOT NULL,
    rationale          TEXT NOT NULL,
    risk_level         TEXT NOT NULL,
    recommended_status TEXT NOT NULL,
    proposal_json      TEXT NOT NULL,               -- the whole AgentProposal, opaque
    target_rule_kind   TEXT,
    target_rule_id     TEXT,
    source_provider    TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    reviewed_at        TEXT
);
