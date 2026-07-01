-- 0008_reminders (sqlite overlay): the drain index.
--
-- The reminder drain selects `WHERE status='pending' AND due_at <= now ORDER BY due_at LIMIT cap`.
-- A composite index on (status, due_at) makes that a bounded range scan over only the pending,
-- soonest-due rows — the same shape as the workflow_instances (status, next_due_at) drain index.

CREATE INDEX idx_reminders_status_due ON reminders(status, due_at);
