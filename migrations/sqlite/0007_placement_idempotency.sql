-- 0007_placement_idempotency (sqlite overlay): one observed placement per message.
--
-- A partial UNIQUE index over the mined `existing_placement` rows only: re-mining a message the
-- backfill already observed fails the insert (the host treats that as "already mined" and does not
-- double-count it), while deliberate corrections — which carry a different `basis` — are untouched.

CREATE UNIQUE INDEX idx_filing_feedback_placement_once
    ON filing_feedback(message_id)
    WHERE basis = 'existing_placement';
