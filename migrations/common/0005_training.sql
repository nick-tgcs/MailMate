-- 0005_training (common): the on-device training-layer surface.
--
-- Three durable tables land with the training pipeline (Phase 9). There is deliberately NO
-- `training_examples` table — examples are derived on export from the per-task feedback
-- tables (the single source of truth) and never stored; `training_datasets.example_ids_hash`
-- pins the exact set that produced a dataset. An adapter is registered `candidate` and
-- reaches `active` only through a status transition the evaluation gate drives, so promotion
-- is always an explicit write, never a side effect of insertion.
--
-- (Sequencing note: the architecture sketches an aspirational `0004_followups`, but the
-- curator took 0004, so the training surface is 0005 and Phase-11 follow-ups become 0006.)

CREATE TABLE training_datasets (
    id                TEXT PRIMARY KEY,          -- ds_...
    name              TEXT NOT NULL,
    dataset_type      TEXT NOT NULL,             -- sft / preference / evaluation / safety_counterexample
    base_model_family TEXT,                      -- intended family, if any
    example_ids_hash  TEXT NOT NULL,             -- stable hash of the included source ids
    positive_count    INTEGER NOT NULL,
    negative_count    INTEGER NOT NULL,
    validation_count  INTEGER NOT NULL,
    test_count        INTEGER NOT NULL,
    privacy_level     TEXT NOT NULL,             -- metadata / redacted / full (highest included)
    export_format     TEXT NOT NULL,             -- jsonl_chat / alpaca / preference_jsonl
    artifact_path     TEXT,                      -- where the rendered dataset was written, if anywhere
    created_at        TEXT NOT NULL
);

CREATE TABLE lora_adapters (
    id                  TEXT PRIMARY KEY,        -- lora_...
    name                TEXT NOT NULL,
    adapter_type        TEXT NOT NULL,           -- lora / qlora
    format              TEXT NOT NULL,           -- safetensors / pytorch
    base_model_family   TEXT NOT NULL,
    base_model_name     TEXT NOT NULL,
    base_model_revision TEXT,
    tokenizer_hash      TEXT,
    chat_template_hash  TEXT,
    -- Nullable so an externally-trained adapter imported by metadata (with no local dataset)
    -- can still be registered; a real FK is enforced when present.
    training_dataset_id TEXT REFERENCES training_datasets(id),
    artifact_path       TEXT NOT NULL,
    status              TEXT NOT NULL,           -- candidate / active / retired / failed_eval
    created_at          TEXT NOT NULL
);

CREATE TABLE lora_eval_runs (
    id               TEXT PRIMARY KEY,           -- eval_...
    adapter_id       TEXT NOT NULL REFERENCES lora_adapters(id),
    dataset_id       TEXT NOT NULL REFERENCES training_datasets(id),
    base_provider_id TEXT NOT NULL,
    metrics_json     TEXT NOT NULL,              -- accuracy / win_rate / safety_failures / quality / extra
    safety_failures  INTEGER NOT NULL,
    quality_score    REAL NOT NULL,
    approved_for_use INTEGER NOT NULL,           -- the gate's verdict (0/1)
    created_at       TEXT NOT NULL
);
