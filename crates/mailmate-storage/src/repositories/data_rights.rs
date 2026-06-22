//! SQLite implementation of [`DataRightsRepository`]: transactional erasure (forget a message,
//! forget a sender, reset all learning) and a portability export.
//!
//! Every erasure runs inside one `rusqlite` transaction so it is all-or-nothing. Foreign keys
//! are enforced (`PRAGMA foreign_keys=ON`), so message-scoped children (`message_features`,
//! `drafts`) are deleted before the `messages` row, and learned `*_rule_versions` before their
//! `*_rules`. Every other cross-reference is loose by the schema's convention, so its order is
//! free. The per-message audit rows are erased too (this is "forget my data"), and the host
//! leaves a single `data_forgotten` tombstone afterwards so the *act* stays accountable without
//! resurrecting the erased content.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Transaction};

use mailmate_common::data_rights::{
    DataExport, ErasureReport, ExportedFeedback, ExportedFiling, ExportedMessage, ExportedRule,
};
use mailmate_common::error::StorageError;
use mailmate_common::ids::MessageId;
use mailmate_ports::storage::data_rights::DataRightsRepository;

use crate::backend::{map_rusqlite, SqliteBackend};

/// The hierarchy bands MailMate *learned* (everything else is built-in safety, an explicit
/// human rule, an AI suggestion, or the default fallback — kept across a learning reset).
const LEARNED_BANDS: &[&str] = &["learned_active", "agent_shadow"];

/// SQLite implementation of [`DataRightsRepository`].
pub struct SqliteDataRightsRepository {
    backend: Arc<SqliteBackend>,
}

impl SqliteDataRightsRepository {
    /// Build a repository over `backend`.
    #[must_use]
    pub fn new(backend: Arc<SqliteBackend>) -> Self {
        Self { backend }
    }
}

/// Delete everything *derived from* a single message (everything but the `messages` row itself),
/// recording the per-table tally. Shared by [`forget_message`] and [`forget_sender`] so the two
/// erasure paths can never drift in what they consider "about this message".
fn purge_message_children(
    tx: &Transaction<'_>,
    message_id: &str,
    report: &mut ErasureReport,
) -> Result<(), StorageError> {
    // FK children of `messages` — must go before the row they reference.
    for (table, sql) in [
        (
            "message_features",
            "DELETE FROM message_features WHERE message_id = ?1",
        ),
        ("drafts", "DELETE FROM drafts WHERE message_id = ?1"),
        // Loose (no FK) but message-scoped — the corrections, audit trail, learning by-products,
        // and the notify-only reminder (its user-authored title/note is per-message PII) that
        // name this message.
        (
            "classification_feedback",
            "DELETE FROM classification_feedback WHERE message_id = ?1",
        ),
        (
            "filing_feedback",
            "DELETE FROM filing_feedback WHERE message_id = ?1",
        ),
        ("audit_log", "DELETE FROM audit_log WHERE message_id = ?1"),
        (
            "rule_evidence",
            "DELETE FROM rule_evidence WHERE message_id = ?1",
        ),
        (
            "shadow_outcomes",
            "DELETE FROM shadow_outcomes WHERE message_id = ?1",
        ),
        ("reminders", "DELETE FROM reminders WHERE message_id = ?1"),
    ] {
        let n = tx.execute(sql, params![message_id]).map_err(map_rusqlite)? as u64;
        report.record(table, n);
    }
    Ok(())
}

/// Garbage-collect the `threads` rows in `thread_ids` that no longer have any message — their
/// `last_summary` / participant metadata is message-derived PII that must not outlive the last
/// message of the thread. A thread still holding other messages is left intact.
fn gc_orphaned_threads(
    tx: &Transaction<'_>,
    thread_ids: &[String],
    report: &mut ErasureReport,
) -> Result<(), StorageError> {
    for thread_id in thread_ids {
        let remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .map_err(map_rusqlite)?;
        if remaining == 0 {
            let n = tx
                .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
                .map_err(map_rusqlite)? as u64;
            report.record("threads", n);
        }
    }
    Ok(())
}

/// The distinct, non-null thread ids of the messages matched by `where_clause` (bound to `param`)
/// — captured *before* the messages are deleted so the now-orphaned threads can be GC'd after.
fn thread_ids_for(
    tx: &Transaction<'_>,
    where_clause: &str,
    param: &str,
) -> Result<Vec<String>, StorageError> {
    let mut stmt = tx
        .prepare(&format!(
            "SELECT DISTINCT thread_id FROM messages WHERE thread_id IS NOT NULL AND {where_clause}"
        ))
        .map_err(map_rusqlite)?;
    let rows = stmt
        .query_map(params![param], |row| row.get::<_, String>(0))
        .map_err(map_rusqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)
}

#[async_trait]
impl DataRightsRepository for SqliteDataRightsRepository {
    async fn forget_message(&self, id: &MessageId) -> Result<ErasureReport, StorageError> {
        let id = id.as_str().to_owned();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            let mut report = ErasureReport::new();
            let threads = thread_ids_for(&tx, "id = ?1", &id)?;
            purge_message_children(&tx, &id, &mut report)?;
            let n = tx
                .execute("DELETE FROM messages WHERE id = ?1", params![id])
                .map_err(map_rusqlite)? as u64;
            report.record("messages", n);
            gc_orphaned_threads(&tx, &threads, &mut report)?;
            tx.commit().map_err(map_rusqlite)?;
            Ok(report)
        })
    }

    async fn forget_sender(&self, sender_email: &str) -> Result<ErasureReport, StorageError> {
        let email = sender_email.to_owned();
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            let mut report = ErasureReport::new();

            // Gather this exact sender's messages first (a shared domain's other senders are
            // untouched), then erase each message's derived data, then the messages themselves.
            let ids: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT id FROM messages WHERE sender_email = ?1")
                    .map_err(map_rusqlite)?;
                let rows = stmt
                    .query_map(params![email], |row| row.get::<_, String>(0))
                    .map_err(map_rusqlite)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)?
            };
            let threads = thread_ids_for(&tx, "sender_email = ?1", &email)?;
            for mid in &ids {
                purge_message_children(&tx, mid, &mut report)?;
            }
            let n = tx
                .execute(
                    "DELETE FROM messages WHERE sender_email = ?1",
                    params![email],
                )
                .map_err(map_rusqlite)? as u64;
            report.record("messages", n);
            let n = tx
                .execute(
                    "DELETE FROM sender_profiles WHERE email = ?1",
                    params![email],
                )
                .map_err(map_rusqlite)? as u64;
            report.record("sender_profiles", n);
            // GC threads that lost their last message (their summary is message-derived PII).
            gc_orphaned_threads(&tx, &threads, &mut report)?;

            tx.commit().map_err(map_rusqlite)?;
            Ok(report)
        })
    }

    async fn reset_learning(&self) -> Result<ErasureReport, StorageError> {
        // The IN-list of learned bands, as a SQL fragment of quoted literals (the values are
        // compile-time constants, never user input — no injection surface).
        let bands = LEARNED_BANDS
            .iter()
            .map(|b| format!("'{b}'"))
            .collect::<Vec<_>>()
            .join(",");
        self.backend.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(map_rusqlite)?;
            let mut report = ErasureReport::new();

            // Learned rule versions BEFORE their rules (the only FK among these tables).
            for (versions, rules) in [
                ("classification_rule_versions", "classification_rules"),
                ("action_rule_versions", "action_rules"),
            ] {
                let n = tx
                    .execute(
                        &format!(
                            "DELETE FROM {versions} WHERE rule_id IN \
                             (SELECT id FROM {rules} WHERE band IN ({bands}))"
                        ),
                        [],
                    )
                    .map_err(map_rusqlite)? as u64;
                report.record(versions, n);
                let n = tx
                    .execute(&format!("DELETE FROM {rules} WHERE band IN ({bands})"), [])
                    .map_err(map_rusqlite)? as u64;
                report.record(rules, n);
            }

            // The correction corpus and learning by-products (all loose — order is free).
            for table in [
                "classification_feedback",
                "filing_feedback",
                "followup_feedback",
                "rule_evidence",
                "shadow_outcomes",
                "agent_proposals",
                "rule_conflicts",
                "rule_proposal_feedback",
                "sender_profiles",
            ] {
                let n = tx
                    .execute(&format!("DELETE FROM {table}"), [])
                    .map_err(map_rusqlite)? as u64;
                report.record(table, n);
            }

            // The learn-from-Sent VIP signal lives in `audit_log` as `mail_sent` rows (Phase 7):
            // `propose_vip_rules` counts them per recipient domain. They are pure learning
            // evidence (not an accountability event), so a full learning reset must drop them too
            // — otherwise the same VIP proposals re-derive after the reset. Other audit rows (the
            // accountability trail) are deliberately kept.
            let n = tx
                .execute("DELETE FROM audit_log WHERE event_type = 'mail_sent'", [])
                .map_err(map_rusqlite)? as u64;
            report.record("audit_log", n);

            tx.commit().map_err(map_rusqlite)?;
            Ok(report)
        })
    }

    async fn export(&self) -> Result<DataExport, StorageError> {
        let bands = LEARNED_BANDS
            .iter()
            .map(|b| format!("'{b}'"))
            .collect::<Vec<_>>()
            .join(",");
        self.backend.with_conn(|conn| {
            let mut export = DataExport::default();

            let mut stmt = conn
                .prepare(
                    "SELECT id, sender_email, sender_domain, subject, received_at, \
                     body_retained, body_text FROM messages ORDER BY received_at ASC, id ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    let body_retained: i64 = row.get(5)?;
                    let retained = body_retained != 0;
                    let body_text: Option<String> = row.get(6)?;
                    Ok(ExportedMessage {
                        id: MessageId::from(row.get::<_, String>(0)?),
                        sender_email: row.get(1)?,
                        sender_domain: row.get(2)?,
                        subject: row.get(3)?,
                        received_at: row.get(4)?,
                        body_retained: retained,
                        // Never surface a body the dial said not to keep, even if a stale value
                        // somehow lingered in the column.
                        body_text: if retained { body_text } else { None },
                    })
                })
                .map_err(map_rusqlite)?;
            export.messages = rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)?;

            let mut stmt = conn
                .prepare(
                    "SELECT message_id, human_label, created_at FROM classification_feedback \
                     ORDER BY created_at ASC, id ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ExportedFeedback {
                        message_id: MessageId::from(row.get::<_, String>(0)?),
                        human_label: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                })
                .map_err(map_rusqlite)?;
            export.classification_feedback =
                rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)?;

            let mut stmt = conn
                .prepare(
                    "SELECT message_id, human_chosen_folder, sender_domain, created_at \
                     FROM filing_feedback ORDER BY created_at ASC, id ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ExportedFiling {
                        message_id: MessageId::from(row.get::<_, String>(0)?),
                        human_chosen_folder: row.get(1)?,
                        sender_domain: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                })
                .map_err(map_rusqlite)?;
            export.filing_feedback = rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)?;

            let mut stmt = conn
                .prepare(&format!(
                    "SELECT stable_name, band, status FROM classification_rules \
                     WHERE band IN ({bands}) \
                     UNION ALL \
                     SELECT stable_name, band, status FROM action_rules WHERE band IN ({bands}) \
                     ORDER BY stable_name ASC"
                ))
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ExportedRule {
                        stable_name: row.get(0)?,
                        band: row.get(1)?,
                        status: row.get(2)?,
                    })
                })
                .map_err(map_rusqlite)?;
            export.rules = rows.collect::<Result<Vec<_>, _>>().map_err(map_rusqlite)?;

            Ok(export)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    use crate::backend::{open_and_migrate, StorageConfig};

    fn db() -> Arc<SqliteBackend> {
        open_and_migrate(&StorageConfig::sqlite_in_memory()).unwrap()
    }

    fn exec(backend: &SqliteBackend, sql: &str) {
        backend
            .with_conn(|conn| conn.execute_batch(sql).map_err(map_rusqlite))
            .unwrap();
    }

    fn count(backend: &SqliteBackend, sql: &str) -> i64 {
        backend
            .with_conn(|conn| {
                conn.query_row(sql, [], |row| row.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap()
    }

    /// Seed one message `id` from `sender` plus a full spread of derived rows that name it.
    fn seed_message(backend: &SqliteBackend, id: &str, sender: &str) {
        let domain = sender.split('@').nth(1).unwrap_or("x.test");
        exec(
            backend,
            &format!(
                "INSERT INTO messages (id, account_id, folder_id, thunderbird_message_id, \
                   sender_email, sender_domain, subject, received_at, classification_status, \
                   body_retained, body_text, created_at) \
                 VALUES ('{id}','acct','inbox','tb_{id}','{sender}','{domain}','subj {id}', \
                   '2026-06-22T00:00:00Z','classified',1,'BODY {id}','2026-06-22T00:00:00Z');
                 INSERT INTO message_features (id, message_id, feature_name, feature_value, created_at) \
                 VALUES ('mf_{id}','{id}','has_link','1','2026-06-22T00:00:00Z');
                 INSERT INTO drafts (id, message_id, provider_id, prompt_template_version, subject, \
                   body, requires_review, safety_flags_json, status, created_at, updated_at) \
                 VALUES ('drf_{id}','{id}','prov','v1','re','b',1,'[]','draft', \
                   '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z');
                 INSERT INTO classification_feedback (id, message_id, pinned_versions_json, \
                   human_label, salient_features_json, polarity, created_at) \
                 VALUES ('cfb_{id}','{id}','{{}}','spam','{{}}','positive','2026-06-22T00:00:00Z');
                 INSERT INTO filing_feedback (id, message_id, pinned_versions_json, sender_domain, \
                   human_chosen_folder, polarity, created_at) \
                 VALUES ('ffb_{id}','{id}','{{}}','{domain}','Archive','positive','2026-06-22T00:00:00Z');
                 INSERT INTO audit_log (id, event_type, message_id, actor, payload_json, created_at) \
                 VALUES ('aud_{id}','action_applied','{id}','system','{{}}','2026-06-22T00:00:00Z');
                 INSERT INTO rule_evidence (id, source_kind, source_id, message_id, evidence_kind, \
                   weight, summary, created_at) \
                 VALUES ('rev_{id}','classification','cfb_{id}','{id}','positive',1.0,'s','2026-06-22T00:00:00Z');
                 INSERT INTO shadow_outcomes (id, rule_kind, rule_id, rule_version_id, message_id, \
                   would_have_action_json, would_have_policy_outcome, created_at) \
                 VALUES ('sho_{id}','classification','r','v','{id}','{{}}','allow','2026-06-22T00:00:00Z');"
            ),
        );
    }

    #[test]
    fn forget_message_erases_only_that_messages_derived_data() {
        let backend = db();
        seed_message(&backend, "msg_1", "a@keep.test");
        seed_message(&backend, "msg_2", "b@keep.test");
        let repo = SqliteDataRightsRepository::new(backend.clone());

        let report = block_on(repo.forget_message(&MessageId::from("msg_1"))).unwrap();

        // msg_1 is gone everywhere; msg_2 is untouched.
        for (table, col) in [
            ("messages", "id"),
            ("message_features", "message_id"),
            ("drafts", "message_id"),
            ("classification_feedback", "message_id"),
            ("filing_feedback", "message_id"),
            ("audit_log", "message_id"),
            ("rule_evidence", "message_id"),
            ("shadow_outcomes", "message_id"),
        ] {
            assert_eq!(
                count(
                    &backend,
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col}='msg_1'")
                ),
                0,
                "{table} still has msg_1 rows"
            );
            assert_eq!(
                count(
                    &backend,
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col}='msg_2'")
                ),
                1,
                "{table} wrongly lost msg_2"
            );
        }
        // The report tallies every touched table once.
        assert_eq!(report.removed.get("messages"), Some(&1));
        assert_eq!(report.removed.get("classification_feedback"), Some(&1));
        assert_eq!(
            report.total(),
            8,
            "8 rows removed across 8 tables: {report:?}"
        );
    }

    #[test]
    fn forgetting_an_unknown_message_is_an_empty_report_not_an_error() {
        let backend = db();
        let repo = SqliteDataRightsRepository::new(backend);
        let report = block_on(repo.forget_message(&MessageId::from("nope"))).unwrap();
        assert!(
            report.is_empty(),
            "nothing stored ⇒ empty report: {report:?}"
        );
    }

    #[test]
    fn forget_sender_erases_only_that_exact_address_not_the_shared_domain() {
        let backend = db();
        seed_message(&backend, "s1", "spammer@shared.test");
        seed_message(&backend, "s2", "spammer@shared.test");
        seed_message(&backend, "f1", "friend@shared.test"); // same domain, different person
        exec(
            &backend,
            "INSERT INTO sender_profiles (id, email, domain, trust_level, last_seen_at, \
               feature_json, created_at, updated_at) VALUES \
             ('sp1','spammer@shared.test','shared.test','low','2026-06-22T00:00:00Z','{}', \
               '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z'), \
             ('sp2','friend@shared.test','shared.test','high','2026-06-22T00:00:00Z','{}', \
               '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z');",
        );
        let repo = SqliteDataRightsRepository::new(backend.clone());

        let report = block_on(repo.forget_sender("spammer@shared.test")).unwrap();

        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM messages WHERE sender_email='spammer@shared.test'"
            ),
            0
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM messages WHERE sender_email='friend@shared.test'"
            ),
            1,
            "the same-domain friend must survive"
        );
        // The spammer's derived rows went with the messages; the friend's stayed.
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM classification_feedback WHERE message_id='s1'"
            ),
            0
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM classification_feedback WHERE message_id='f1'"
            ),
            1
        );
        // Only the spammer's profile is gone.
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM sender_profiles WHERE email='spammer@shared.test'"
            ),
            0
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM sender_profiles WHERE email='friend@shared.test'"
            ),
            1
        );
        assert_eq!(report.removed.get("messages"), Some(&2));
        assert_eq!(report.removed.get("sender_profiles"), Some(&1));
    }

    #[test]
    fn reset_learning_drops_learned_state_keeps_builtins_and_messages() {
        let backend = db();
        seed_message(&backend, "m", "a@b.test"); // a message + its classification_feedback
                                                 // Two learned rules (one active, one shadow) + one built-in safety rule that must survive.
        exec(
            &backend,
            "INSERT INTO classification_rules (id, stable_name, scope, band, status, created_by, \
               created_at, updated_at) VALUES \
             ('r_learned','learned.vip','global','learned_active','active','curator', \
               '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z'), \
             ('r_shadow','shadow.candidate','global','agent_shadow','shadow','curator', \
               '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z'), \
             ('r_safety','safety.phishing','global','system_safety','active','system', \
               '2026-06-22T00:00:00Z','2026-06-22T00:00:00Z');
             INSERT INTO classification_rule_versions (id, rule_id, version_number, title, \
               description, condition_json, effect_json, priority, risk_level, created_by, \
               change_reason, created_at) VALUES \
             ('v_learned','r_learned',1,'t','d','{}','{}',1,'low','curator','seed','2026-06-22T00:00:00Z'), \
             ('v_safety','r_safety',1,'t','d','{}','{}',1,'low','system','seed','2026-06-22T00:00:00Z');
             INSERT INTO agent_proposals (id, proposal_type, status, title, rationale, risk_level, \
               recommended_status, proposal_json, source_provider, created_at) VALUES \
             ('prop1','new_rule','pending','t','why','low','shadow','{}','curator','2026-06-22T00:00:00Z');
             INSERT INTO audit_log (id, event_type, actor, payload_json, created_at) VALUES \
             ('aud_sent','mail_sent','user','{\"recipient_domain\":\"acme.test\"}','2026-06-22T00:00:00Z');",
        );
        let repo = SqliteDataRightsRepository::new(backend.clone());

        let report = block_on(repo.reset_learning()).unwrap();

        // Learned + shadow rules (and their versions) gone; the safety rule + its version kept.
        assert_eq!(count(&backend, "SELECT COUNT(*) FROM classification_rules WHERE band IN ('learned_active','agent_shadow')"), 0);
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM classification_rules WHERE band='system_safety'"
            ),
            1
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM classification_rule_versions WHERE rule_id='r_learned'"
            ),
            0
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM classification_rule_versions WHERE rule_id='r_safety'"
            ),
            1
        );
        // The correction corpus and proposals are gone.
        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM classification_feedback"),
            0
        );
        assert_eq!(count(&backend, "SELECT COUNT(*) FROM agent_proposals"), 0);
        // But the message itself is KEPT — reset_learning forgets what was learned, not the mail.
        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM messages WHERE id='m'"),
            1
        );
        // The learn-from-Sent VIP signal (`mail_sent`) is dropped so the same VIP proposals don't
        // re-derive, but the accountability audit trail (`action_applied`) is kept.
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM audit_log WHERE event_type='mail_sent'"
            ),
            0
        );
        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM audit_log WHERE event_type='action_applied'"
            ),
            1
        );
        assert_eq!(report.removed.get("classification_rules"), Some(&2));
        assert_eq!(report.removed.get("classification_feedback"), Some(&1));
        assert_eq!(
            report.removed.get("audit_log"),
            Some(&1),
            "the mail_sent row was tallied"
        );
    }

    #[test]
    fn forget_message_also_erases_its_reminder_and_gcs_an_orphaned_thread() {
        let backend = db();
        // A message on a thread, with a notify-only reminder carrying user-authored note text.
        exec(
            &backend,
            "INSERT INTO threads (id, account_id, subject_root_normalized, participant_domains, \
               first_seen_at, last_seen_at, last_summary, created_at) VALUES \
             ('thr_1','acct','s','[]','2026-06-22T00:00:00Z','2026-06-22T00:00:00Z', \
               'thread summary text','2026-06-22T00:00:00Z');
             INSERT INTO messages (id, account_id, folder_id, thunderbird_message_id, sender_email, \
               sender_domain, subject, received_at, classification_status, thread_id, body_retained, \
               created_at) VALUES \
             ('m1','acct','inbox','tb','a@b.test','b.test','s','2026-06-22T00:00:00Z','classified', \
               'thr_1',0,'2026-06-22T00:00:00Z');
             INSERT INTO reminders (id, message_id, title, note, due_at, status, created_at) VALUES \
             ('rem_1','m1','remind','reply with the quote','2026-06-23T00:00:00Z','pending','2026-06-22T00:00:00Z');",
        );
        let repo = SqliteDataRightsRepository::new(backend.clone());

        let report = block_on(repo.forget_message(&MessageId::from("m1"))).unwrap();

        assert_eq!(
            count(
                &backend,
                "SELECT COUNT(*) FROM reminders WHERE message_id='m1'"
            ),
            0,
            "the reminder (with its note) is erased"
        );
        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM threads WHERE id='thr_1'"),
            0,
            "the now-empty thread is GC'd"
        );
        assert_eq!(report.removed.get("reminders"), Some(&1));
        assert_eq!(report.removed.get("threads"), Some(&1));
    }

    #[test]
    fn forget_sender_keeps_a_thread_that_still_has_another_senders_message() {
        let backend = db();
        // One shared thread holds a message from each of two senders; forgetting one must not GC
        // the thread (the other sender's message still lives there).
        exec(
            &backend,
            "INSERT INTO threads (id, account_id, subject_root_normalized, participant_domains, \
               first_seen_at, last_seen_at, created_at) VALUES \
             ('thr_s','acct','s','[]','2026-06-22T00:00:00Z','2026-06-22T00:00:00Z','2026-06-22T00:00:00Z');
             INSERT INTO messages (id, account_id, folder_id, thunderbird_message_id, sender_email, \
               sender_domain, subject, received_at, classification_status, thread_id, body_retained, \
               created_at) VALUES \
             ('ms','acct','inbox','tb1','spammer@x.test','x.test','s','2026-06-22T00:00:00Z','classified','thr_s',0,'2026-06-22T00:00:00Z'), \
             ('mf','acct','inbox','tb2','friend@x.test','x.test','s','2026-06-22T00:00:00Z','classified','thr_s',0,'2026-06-22T00:00:00Z');",
        );
        let repo = SqliteDataRightsRepository::new(backend.clone());

        block_on(repo.forget_sender("spammer@x.test")).unwrap();

        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM messages WHERE id='ms'"),
            0
        );
        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM messages WHERE id='mf'"),
            1
        );
        assert_eq!(
            count(&backend, "SELECT COUNT(*) FROM threads WHERE id='thr_s'"),
            1,
            "the thread survives — the friend's message still lives there"
        );
    }

    #[test]
    fn export_carries_metadata_and_only_retained_bodies() {
        let backend = db();
        seed_message(&backend, "kept", "a@b.test"); // body_retained=1, body_text='BODY kept'
                                                    // A message whose body was NOT retained but a stale value lingers: must not be exported.
        exec(
            &backend,
            "INSERT INTO messages (id, account_id, folder_id, thunderbird_message_id, sender_email, \
               sender_domain, subject, received_at, classification_status, body_retained, body_text, \
               created_at) VALUES \
             ('nob','acct','inbox','tb','c@d.test','d.test','no body','2026-06-22T01:00:00Z', \
               'classified',0,'LEAKED','2026-06-22T01:00:00Z');
             INSERT INTO classification_rules (id, stable_name, scope, band, status, created_by, \
               created_at, updated_at) VALUES \
             ('rl','learned.vip','global','learned_active','active','curator','2026-06-22T00:00:00Z','2026-06-22T00:00:00Z');",
        );
        let repo = SqliteDataRightsRepository::new(backend);

        let export = block_on(repo.export()).unwrap();

        assert_eq!(export.messages.len(), 2);
        let kept = export
            .messages
            .iter()
            .find(|m| m.id.as_str() == "kept")
            .unwrap();
        assert_eq!(kept.body_text.as_deref(), Some("BODY kept"));
        let nob = export
            .messages
            .iter()
            .find(|m| m.id.as_str() == "nob")
            .unwrap();
        assert!(!nob.body_retained);
        assert_eq!(
            nob.body_text, None,
            "an unretained body must never be exported"
        );
        // The learned rule and the seeded correction are surfaced.
        assert_eq!(export.rules.len(), 1);
        assert_eq!(export.rules[0].stable_name, "learned.vip");
        assert_eq!(export.classification_feedback.len(), 1);
        assert_eq!(export.classification_feedback[0].human_label, "spam");
        // The filing correction corpus is exported too (the other durable learning signal).
        assert_eq!(export.filing_feedback.len(), 1);
        assert_eq!(export.filing_feedback[0].human_chosen_folder, "Archive");
        assert_eq!(export.filing_feedback[0].message_id.as_str(), "kept");
    }
}
