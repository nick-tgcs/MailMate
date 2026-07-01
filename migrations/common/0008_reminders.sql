-- 0008_reminders (common): durable notify-only remind-me / snooze timers (Phase 9).
--
-- A reminder is a single-shot temporal nudge over a message or thread — distinct from the
-- sales-cadence follow-up workflow (0006), which drafts a reply when a step fires. When a
-- reminder comes due the host emits ONE notification and the reminder is `fired` (terminal), so a
-- re-drain after a crash never double-nudges. Snooze is a reschedule of `due_at`; "send-later" is
-- a reminder whose `note` points at a saved draft.
--
-- Cross-references (`message_id`, `thread_id`, `account_id`) are loose by the same convention the
-- feedback/workflow tables use — a reminder must survive the message being archived or re-indexed,
-- and forgetting a message should not cascade-delete the user's own reminder. The drain index
-- `(status, due_at)` mirrors the workflow drain's load-bearing `(status, next_due_at)`.

CREATE TABLE reminders (
    id          TEXT PRIMARY KEY,            -- rem_...
    message_id  TEXT,                        -- the message it is about (loose)
    thread_id   TEXT,                        -- or the thread (loose)
    account_id  TEXT,                        -- owning account, if known (loose)
    title       TEXT NOT NULL,               -- short label shown in the nudge
    note        TEXT,                        -- optional free-text / saved-draft pointer
    due_at      TEXT NOT NULL,               -- when to fire
    status      TEXT NOT NULL,               -- pending | fired | cancelled
    created_at  TEXT NOT NULL,
    fired_at    TEXT                         -- when it actually fired (NULL until then)
);
