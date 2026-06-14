-- 0003_audit_and_feedback (sqlite overlay): indexes for the learning-loop reads.
--
-- Per-task feedback is read by message and aggregated by recency; audit/shadow by recency;
-- evidence by the proposal/rule it links; proposals by review status.

CREATE INDEX idx_classification_feedback_message_id ON classification_feedback(message_id);
CREATE INDEX idx_classification_feedback_created_at  ON classification_feedback(created_at);
CREATE INDEX idx_filing_feedback_message_id          ON filing_feedback(message_id);
CREATE INDEX idx_filing_feedback_created_at          ON filing_feedback(created_at);
CREATE INDEX idx_filing_feedback_matched_rule_id     ON filing_feedback(matched_rule_id);
CREATE INDEX idx_audit_log_created_at                ON audit_log(created_at);
CREATE INDEX idx_audit_log_message_id                ON audit_log(message_id);
CREATE INDEX idx_shadow_outcomes_created_at          ON shadow_outcomes(created_at);
CREATE INDEX idx_shadow_outcomes_rule_id             ON shadow_outcomes(rule_id);
CREATE INDEX idx_rule_evidence_proposal_id           ON rule_evidence(proposal_id);
CREATE INDEX idx_rule_evidence_rule_id               ON rule_evidence(rule_id);
CREATE INDEX idx_agent_proposals_status              ON agent_proposals(status);
