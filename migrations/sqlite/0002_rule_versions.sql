-- 0002_rule_versions (sqlite overlay): indexes for the rule snapshot reads.
--
-- The engine loads its snapshot by (status, scope) — the index that backs
-- `get_active_rules`/`get_shadow_rules` — and walks a rule's versions by rule_id.

CREATE INDEX idx_classification_rules_status ON classification_rules(status);
CREATE INDEX idx_classification_rules_scope  ON classification_rules(scope);
CREATE INDEX idx_action_rules_status         ON action_rules(status);
CREATE INDEX idx_action_rules_scope          ON action_rules(scope);
CREATE INDEX idx_classification_rule_versions_rule_id ON classification_rule_versions(rule_id);
CREATE INDEX idx_action_rule_versions_rule_id        ON action_rule_versions(rule_id);
