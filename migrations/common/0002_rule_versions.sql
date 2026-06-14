-- 0002_rule_versions (common): the rule tables and their immutable version tables.
--
-- The two pipelines live in fully separate tables with identical schema (one shared
-- mechanism, two tables): `classification_rules`/`action_rules` hold the mutable rule
-- metadata (status, scope, current version pointer); `classification_rule_versions`/
-- `action_rule_versions` hold immutable condition→effect content, one row per revision.
--
-- The `band` column carries the hierarchy authority class (system_safety … default_fallback)
-- — rule metadata the engine needs to rank matches, not derivable from status alone.
--
-- `current_version_id` deliberately carries NO foreign key: rule↔version references are
-- mutually circular (a version FKs its rule; the rule points at its current version), and
-- the repository always writes both inside one transaction (insert rule, insert version,
-- repoint), so a single-direction FK (version→rule) is enough for integrity without the
-- deferred-constraint dance.

CREATE TABLE classification_rules (
    id                 TEXT PRIMARY KEY,
    stable_name        TEXT NOT NULL UNIQUE,
    scope              TEXT NOT NULL,
    band               TEXT NOT NULL,
    status             TEXT NOT NULL,
    current_version_id TEXT,
    created_by         TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE TABLE action_rules (
    id                 TEXT PRIMARY KEY,
    stable_name        TEXT NOT NULL UNIQUE,
    scope              TEXT NOT NULL,
    band               TEXT NOT NULL,
    status             TEXT NOT NULL,
    current_version_id TEXT,
    created_by         TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE TABLE classification_rule_versions (
    id                   TEXT PRIMARY KEY,
    rule_id              TEXT NOT NULL,
    version_number       INTEGER NOT NULL,
    title                TEXT NOT NULL,
    description          TEXT NOT NULL,
    condition_json       TEXT NOT NULL,            -- JSON-AST condition, opaque
    effect_json          TEXT NOT NULL,            -- structured effect, opaque
    priority             INTEGER NOT NULL,
    confidence_threshold REAL,
    risk_level           TEXT NOT NULL,
    created_by           TEXT NOT NULL,
    change_reason        TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    FOREIGN KEY (rule_id) REFERENCES classification_rules(id)
);

CREATE TABLE action_rule_versions (
    id                   TEXT PRIMARY KEY,
    rule_id              TEXT NOT NULL,
    version_number       INTEGER NOT NULL,
    title                TEXT NOT NULL,
    description          TEXT NOT NULL,
    condition_json       TEXT NOT NULL,            -- JSON-AST condition, opaque
    effect_json          TEXT NOT NULL,            -- structured effect, opaque
    priority             INTEGER NOT NULL,
    confidence_threshold REAL,
    risk_level           TEXT NOT NULL,
    created_by           TEXT NOT NULL,
    change_reason        TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    FOREIGN KEY (rule_id) REFERENCES action_rules(id)
);
