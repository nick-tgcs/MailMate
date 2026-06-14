-- 0001_initial (common): the portable foundational schema for MailMate.
--
-- Engine-neutral DDL only — the ~90% shared core. Conventions (see architecture.md,
-- "Portable column conventions"):
--   * app-generated prefixed-string TEXT primary keys (no AUTOINCREMENT/SERIAL/IDENTITY),
--   * ISO-8601 TEXT timestamps,
--   * INTEGER 0/1 booleans,
--   * `*_json` columns are opaque application JSON written/read whole by Rust (their
--     per-dialect column *type* lives in the dialect overlay; here they are TEXT).
-- Engine-specific tuning (indexes, pragmas, view bodies) lives in the dialect overlay.

CREATE TABLE threads (
    id                      TEXT PRIMARY KEY,
    account_id              TEXT NOT NULL,
    subject_root_normalized TEXT NOT NULL,
    participant_domains     TEXT NOT NULL,            -- JSON array, opaque
    message_count           INTEGER NOT NULL DEFAULT 0,
    first_seen_at           TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    last_summary            TEXT,
    last_summarized_at      TEXT,
    created_at              TEXT NOT NULL
);

CREATE TABLE messages (
    id                     TEXT PRIMARY KEY,
    account_id             TEXT NOT NULL,
    folder_id              TEXT NOT NULL,
    thunderbird_message_id TEXT NOT NULL,
    rfc_message_id_hash    TEXT,
    thread_id              TEXT,
    sender_email           TEXT NOT NULL,
    sender_domain          TEXT NOT NULL,
    subject                TEXT NOT NULL,
    received_at            TEXT NOT NULL,
    classification_status  TEXT NOT NULL,
    body_hash              TEXT,
    body_retained          INTEGER NOT NULL DEFAULT 0,
    body_text              TEXT,
    created_at             TEXT NOT NULL,
    FOREIGN KEY (thread_id) REFERENCES threads(id)
);

CREATE TABLE message_features (
    id            TEXT PRIMARY KEY,
    message_id    TEXT NOT NULL,
    feature_name  TEXT NOT NULL,
    feature_value TEXT NOT NULL,                      -- JSON scalar/object, opaque
    created_at    TEXT NOT NULL,
    FOREIGN KEY (message_id) REFERENCES messages(id)
);

CREATE TABLE sender_profiles (
    id           TEXT PRIMARY KEY,
    email        TEXT NOT NULL,
    domain       TEXT NOT NULL,
    display_name TEXT,
    trust_level  TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    feature_json TEXT NOT NULL,                       -- aggregate features, opaque
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE drafts (
    id                      TEXT PRIMARY KEY,
    message_id              TEXT,                     -- NULL for a scheduled follow-up
    thread_id               TEXT,
    provider_id             TEXT NOT NULL,
    prompt_template_version TEXT NOT NULL,
    subject                 TEXT NOT NULL,
    body                    TEXT NOT NULL,
    requires_review         INTEGER NOT NULL DEFAULT 1,  -- always 1 (Drafting Safety)
    safety_flags_json       TEXT NOT NULL,            -- validator flags, opaque
    status                  TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    FOREIGN KEY (message_id) REFERENCES messages(id),
    FOREIGN KEY (thread_id) REFERENCES threads(id)
);
