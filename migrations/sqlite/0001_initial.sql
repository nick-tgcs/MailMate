-- 0001_initial (sqlite overlay): SQLite-specific tuning applied after the common DDL.
--
-- Connection pragmas (foreign_keys=ON, journal_mode=WAL, busy_timeout) are set per
-- connection in the backend, not here. This overlay carries the index set that backs the
-- background-classification queue and the common lookups. Index choices are engine-
-- specific, which is why they live in the overlay rather than the shared `common` DDL.

CREATE INDEX idx_messages_classification_status ON messages(classification_status);
CREATE INDEX idx_messages_thread_id            ON messages(thread_id);
CREATE INDEX idx_messages_account_folder       ON messages(account_id, folder_id);
CREATE INDEX idx_message_features_message_id   ON message_features(message_id);
CREATE INDEX idx_sender_profiles_email         ON sender_profiles(email);
CREATE INDEX idx_drafts_message_id             ON drafts(message_id);
CREATE INDEX idx_drafts_thread_id              ON drafts(thread_id);
CREATE INDEX idx_drafts_status                 ON drafts(status);
