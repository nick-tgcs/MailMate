-- 0006_followups (sqlite overlay): indexes for the follow-up scheduler and pipeline reads.
--
-- The load-bearing one is `workflow_instances(status, next_due_at)` — the scheduler drain's
-- exact predicate (the `messages(classification_status)` analogue). The rest serve the
-- reply-exit lookup, the pipeline list, the per-instance/-item feedback reads, the conflict
-- open-queue, and the shadow promotion report.

CREATE INDEX idx_workflow_instances_status_next_due ON workflow_instances(status, next_due_at);
CREATE INDEX idx_workflow_instances_thread_status   ON workflow_instances(thread_id, status);
CREATE INDEX idx_workflow_instances_pipeline_item   ON workflow_instances(pipeline_item_id);

CREATE INDEX idx_pipeline_items_thread_id           ON pipeline_items(thread_id);
CREATE INDEX idx_pipeline_items_account_stage       ON pipeline_items(account_id, stage);
CREATE INDEX idx_pipeline_items_counterparty_domain ON pipeline_items(counterparty_domain);
CREATE INDEX idx_pipeline_items_last_activity_at    ON pipeline_items(last_activity_at);

CREATE INDEX idx_workflow_definitions_status        ON workflow_definitions(status);
CREATE INDEX idx_workflow_definitions_scope         ON workflow_definitions(scope);
CREATE INDEX idx_workflow_definition_versions_wf    ON workflow_definition_versions(workflow_id);

CREATE INDEX idx_followup_feedback_instance         ON followup_feedback(workflow_instance_id);
CREATE INDEX idx_followup_feedback_pipeline_item    ON followup_feedback(pipeline_item_id);
CREATE INDEX idx_followup_feedback_created_at       ON followup_feedback(created_at);

CREATE INDEX idx_workflow_shadow_outcomes_workflow  ON workflow_shadow_outcomes(workflow_id);
CREATE INDEX idx_workflow_conflicts_status          ON workflow_conflicts(status);
