-- 0004_curator (sqlite overlay): indexes for the curator reads.
--
-- Conflicts are read by resolution state (the open queue); proposal feedback by the
-- proposal it concerns and aggregated by recency.

CREATE INDEX idx_rule_conflicts_status               ON rule_conflicts(status);
CREATE INDEX idx_rule_proposal_feedback_proposal_id  ON rule_proposal_feedback(proposal_id);
CREATE INDEX idx_rule_proposal_feedback_created_at   ON rule_proposal_feedback(created_at);
