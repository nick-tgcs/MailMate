-- 0005_training (sqlite overlay): indexes for the training-layer reads.
--
-- Datasets are listed by recency; adapters by recency and filtered by lifecycle status (the
-- active adapter is read on a hot path); eval runs are read per adapter, newest first.

CREATE INDEX idx_training_datasets_created_at ON training_datasets(created_at);
CREATE INDEX idx_lora_adapters_status         ON lora_adapters(status);
CREATE INDEX idx_lora_adapters_created_at     ON lora_adapters(created_at);
CREATE INDEX idx_lora_eval_runs_adapter_id    ON lora_eval_runs(adapter_id);
