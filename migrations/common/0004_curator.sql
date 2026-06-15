-- 0004_curator (common): the agent-curator surface.
--
-- Two tables land with the curator (Phase 8). `rule_conflicts` records contradictions found
-- between two LIVE rules of the same kind (the transient candidate-vs-existing check the
-- rule engine returns is not stored). `rule_proposal_feedback` is the single owner of "what
-- the human did with a proposal" — proposal-scoped, not message-scoped — so the curator loop
-- is itself learnable, distinct from the audit timeline's `proposal_reviewed` event.

CREATE TABLE rule_conflicts (
    id            TEXT PRIMARY KEY,
    rule_kind     TEXT NOT NULL,                 -- classification / action (same-kind only)
    rule_a_id     TEXT NOT NULL,
    rule_b_id     TEXT NOT NULL,
    conflict_kind TEXT NOT NULL,                 -- contradictory_effect / overlap / unsafe_escalation
    severity      TEXT NOT NULL,                 -- low / medium / high
    description   TEXT NOT NULL,
    status        TEXT NOT NULL,                 -- open / resolved / ignored (mutable)
    created_at    TEXT NOT NULL,
    resolved_at   TEXT
);

CREATE TABLE rule_proposal_feedback (
    id                   TEXT PRIMARY KEY,
    proposal_id          TEXT NOT NULL,          -- the agent_proposals row reviewed
    pinned_versions_json TEXT NOT NULL,          -- provenance copy-at-event, opaque
    outcome              TEXT NOT NULL,          -- accepted / accepted_with_edits / rejected / disabled_later
    human_reason_code    TEXT,
    human_reason_text    TEXT,
    polarity             TEXT NOT NULL,          -- positive / negative
    created_at           TEXT NOT NULL
);
