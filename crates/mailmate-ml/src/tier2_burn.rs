//! The real on-device Tier-2 classifier: a linear discriminative model trained by gradient
//! descent in **Burn** on the pure-CPU `ndarray` backend, written to a **loadable artifact**
//! the cascade reads and serves predictions from.
//!
//! This is the Phase-8 swap-in behind the [`Tier2Classifier`] port. Where the online
//! [`crate::LogisticRegressionClassifier`] learns one SGD step at a time, this model is the
//! product of a *batch training run*: many epochs of full-batch gradient descent over a
//! redacted, derived corpus, evaluated on a held-out split, and activated only when its
//! held-out precision clears a gate.
//!
//! ## The artifact (a real, loadable Burn record)
//! Training writes a directory containing
//! - `model.mpk` — the Burn `DefaultRecorder` (full-precision named-MessagePack) record of
//!   the `Linear` weights + bias, and
//! - `manifest.json` — the feature **vocabulary** (the column order), the two labels, and the
//!   calibration version,
//!
//! so a fresh process can [`TrainedTier2::load`] the directory and featurize new mail exactly
//! as training did. The cascade loads this and runs `predict` through the loaded Burn model —
//! `artifact_path` is genuinely read and used, not a phantom descriptor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use burn::backend::{Autodiff, NdArray};
use burn::module::{AutodiffModule, Module};
use burn::nn::{Linear, LinearConfig};
use burn::optim::{GradientsParams, Optimizer, SgdConfig};
use burn::record::{DefaultRecorder, Recorder};
use burn::tensor::backend::Backend;
use burn::tensor::{Tensor, TensorData};
use serde::{Deserialize, Serialize};

use mailmate_common::error::MlError;
use mailmate_common::features::{
    CalibratedScores, FeatureVector, LabeledExample, SignalContribution,
};
use mailmate_ports::tier2_classifier::Tier2Classifier;

use crate::featurize::featurize;

/// Pure-CPU inference backend.
type InferBackend = NdArray<f32>;
/// Reverse-mode autodiff over the CPU backend — the training backend.
type TrainBackend = Autodiff<InferBackend>;

/// The calibration tag this Burn-trained model stamps onto its scores.
pub const CALIBRATION_VERSION: &str = "burn-linear-v1";

/// Hyperparameters for a Tier-2 training run.
#[derive(Debug, Clone, Copy)]
pub struct Tier2TrainConfig {
    /// Number of full-batch gradient-descent epochs.
    pub epochs: usize,
    /// Learning rate for the SGD step.
    pub learning_rate: f64,
    /// L2 weight-decay coefficient (keeps a feature seen in few examples from running away).
    pub l2: f64,
    /// RNG seed for the weight initialisation, so a training run is reproducible.
    pub seed: u64,
}

impl Default for Tier2TrainConfig {
    fn default() -> Self {
        Self {
            epochs: 300,
            learning_rate: 0.3,
            l2: 1e-3,
            seed: 1,
        }
    }
}

/// A linear (logistic-regression) model: features → a single logit.
#[derive(Module, Debug)]
pub struct Tier2Model<B: Backend> {
    linear: Linear<B>,
}

impl<B: Backend> Tier2Model<B> {
    fn new(device: &B::Device, n_features: usize) -> Self {
        Self {
            linear: LinearConfig::new(n_features.max(1), 1).init(device),
        }
    }

    fn logits(&self, x: Tensor<B, 2>) -> Tensor<B, 2> {
        self.linear.forward(x)
    }
}

/// The metadata stored beside the Burn record so load-time featurization matches training.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tier2Manifest {
    positive_label: String,
    negative_label: String,
    /// The feature keys, in the column order the model was trained on.
    vocab: Vec<String>,
    calibration_version: String,
}

/// Held-out evaluation of a trained model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tier2EvalMetrics {
    /// Examples scored (rows with a known label).
    pub n: u32,
    /// Fraction correct overall.
    pub accuracy: f64,
    /// Precision of the positive class — `tp / (tp + fp)`, the Phase-8 gate metric.
    pub precision: f64,
    /// Recall of the positive class — `tp / (tp + fn)`.
    pub recall: f64,
    /// True positives.
    pub true_positives: u32,
    /// False positives.
    pub false_positives: u32,
}

/// A trained, loaded Tier-2 model: the Burn inference module plus everything needed to
/// featurize and explain a prediction.
#[derive(Debug)]
pub struct TrainedTier2 {
    model: Tier2Model<InferBackend>,
    manifest: Tier2Manifest,
    /// `vocab key → column index`, for dense featurization at predict time.
    index: HashMap<String, usize>,
    /// The learned weight per vocab column (for the exact contribution breakdown).
    weights: Vec<f32>,
}

/// Errors from training, saving, or loading a Tier-2 model.
#[derive(Debug)]
pub enum Tier2ModelError {
    /// No example carried one of the two target labels.
    NoExamples,
    /// Filesystem error reading/writing the artifact.
    Io(std::io::Error),
    /// The Burn record could not be read/written.
    Record(String),
    /// The manifest JSON could not be (de)serialized.
    Manifest(serde_json::Error),
}

impl std::fmt::Display for Tier2ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoExamples => write!(f, "no training examples carried a known label"),
            Self::Io(e) => write!(f, "tier2 artifact io: {e}"),
            Self::Record(e) => write!(f, "tier2 burn record: {e}"),
            Self::Manifest(e) => write!(f, "tier2 manifest: {e}"),
        }
    }
}

impl std::error::Error for Tier2ModelError {}

impl From<std::io::Error> for Tier2ModelError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Tier2ModelError {
    fn from(e: serde_json::Error) -> Self {
        Self::Manifest(e)
    }
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

/// A feature key must appear in at least this many examples to enter the vocabulary. A
/// one-hot seen in a single example (a unique sender, a one-off subject token) can only
/// memorise that row and never generalises — dropping it both bounds the model dimension and
/// removes pure noise (the batch analogue of the online model's confidence floor).
const MIN_VOCAB_SUPPORT: usize = 2;
/// A hard ceiling on the feature dimension so the dense `[n, d]` training matrix stays bounded
/// even on a high-cardinality corpus (`MAX_ROWS` caps `n`; this caps `d`). Beyond it, only the
/// most-frequent keys are kept.
const MAX_VOCAB_FEATURES: usize = 2048;

/// Build the feature vocabulary from a corpus: keys with enough support, capped to the
/// most-frequent `MAX_VOCAB_FEATURES`, in stable sorted column order. Bounding the vocabulary
/// keeps the dense training matrix from growing without limit on diverse text features.
fn build_vocab(examples: &[LabeledExample]) -> Vec<String> {
    // Count the number of examples each key appears in (keys are unique within one example).
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for ex in examples {
        for (k, _) in featurize(&ex.features) {
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut kept: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(_, c)| *c >= MIN_VOCAB_SUPPORT)
        .collect();
    if kept.len() > MAX_VOCAB_FEATURES {
        // Keep the most-frequent keys; ties broken by key for a deterministic vocabulary.
        kept.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        kept.truncate(MAX_VOCAB_FEATURES);
    }
    let mut keys: Vec<String> = kept.into_iter().map(|(k, _)| k).collect();
    keys.sort();
    keys
}

/// Train a Tier-2 linear classifier by batch gradient descent in Burn. The returned model is
/// on the inference backend, ready to [`TrainedTier2::save`] or serve predictions.
///
/// # Errors
/// [`Tier2ModelError::NoExamples`] if no example carries either label.
pub fn train(
    examples: &[LabeledExample],
    positive_label: &str,
    negative_label: &str,
    config: &Tier2TrainConfig,
) -> Result<TrainedTier2, Tier2ModelError> {
    let vocab = build_vocab(examples);
    let index: HashMap<String, usize> = vocab
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect();
    let d = vocab.len().max(1);

    let mut xs: Vec<f32> = Vec::new();
    let mut ys: Vec<f32> = Vec::new();
    let mut n = 0usize;
    for ex in examples {
        let target = if ex.label == positive_label {
            1.0f32
        } else if ex.label == negative_label {
            0.0f32
        } else {
            continue; // a row labelled neither class is not a training signal for this model
        };
        let mut row = vec![0f32; d];
        for (k, v) in featurize(&ex.features) {
            if let Some(i) = index.get(&k) {
                row[*i] = v as f32;
            }
        }
        xs.extend(row);
        ys.push(target);
        n += 1;
    }
    if n == 0 {
        return Err(Tier2ModelError::NoExamples);
    }

    let device = Default::default();
    <TrainBackend as Backend>::seed(&device, config.seed);
    let mut model = Tier2Model::<TrainBackend>::new(&device, d);
    let x = Tensor::<TrainBackend, 2>::from_data(TensorData::new(xs, [n, d]), &device);
    let y = Tensor::<TrainBackend, 2>::from_data(TensorData::new(ys, [n, 1]), &device);
    let mut optim = SgdConfig::new().init();

    for _ in 0..config.epochs {
        let z = model.logits(x.clone()); // [n, 1]
        // Numerically-stable binary-cross-entropy-with-logits: max(z,0) - z*y + log(1+e^-|z|).
        let bce = z
            .clone()
            .clamp_min(0.0f32)
            .sub(z.clone().mul(y.clone()))
            .add(z.clone().abs().neg().exp().add_scalar(1.0f32).log())
            .mean();
        // L2 weight-decay through the autodiff graph.
        let w = model.linear.weight.val();
        let l2 = w.powf_scalar(2.0f32).sum().mul_scalar(config.l2 as f32);
        let loss = bce.add(l2);

        let grads = loss.backward();
        let grads = GradientsParams::from_grads(grads, &model);
        model = optim.step(config.learning_rate, model, grads);
    }

    let infer = model.valid();
    Ok(assemble(
        infer,
        Tier2Manifest {
            positive_label: positive_label.to_owned(),
            negative_label: negative_label.to_owned(),
            vocab,
            calibration_version: CALIBRATION_VERSION.to_owned(),
        },
    ))
}

/// Extract the dense weight vector + index from a loaded model and its manifest.
fn assemble(model: Tier2Model<InferBackend>, manifest: Tier2Manifest) -> TrainedTier2 {
    let index: HashMap<String, usize> = manifest
        .vocab
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect();
    let weights: Vec<f32> = model
        .linear
        .weight
        .val()
        .into_data()
        .to_vec()
        .unwrap_or_default();
    TrainedTier2 {
        model,
        manifest,
        index,
        weights,
    }
}

impl TrainedTier2 {
    /// The two labels this model discriminates (`positive`, `negative`).
    #[must_use]
    pub fn labels(&self) -> (&str, &str) {
        (&self.manifest.positive_label, &self.manifest.negative_label)
    }

    /// Run the loaded Burn model forward and return `(P(positive), contributions)`.
    fn infer(&self, features: &FeatureVector) -> (f64, Vec<SignalContribution>) {
        let feats = featurize(features);
        let d = self.manifest.vocab.len().max(1);
        let mut row = vec![0f32; d];
        for (k, v) in &feats {
            if let Some(i) = self.index.get(k) {
                row[*i] = *v as f32;
            }
        }
        let device = Default::default();
        let x = Tensor::<InferBackend, 2>::from_data(TensorData::new(row, [1, d]), &device);
        let logit = f64::from(self.model.logits(x).into_scalar());
        let p = sigmoid(logit);
        let top_is_positive = p >= 0.5;
        let contributions = feats
            .iter()
            .filter_map(|(k, v)| {
                let i = *self.index.get(k)?;
                let term = f64::from(self.weights[i]) * v;
                (term.abs() > f64::EPSILON).then(|| SignalContribution {
                    key: k.clone(),
                    value: *v,
                    signed_weight: if top_is_positive { term } else { -term },
                })
            })
            .collect();
        (p, contributions)
    }

    /// Score `held_out` examples and report precision / recall / accuracy. Rows whose label is
    /// neither class are skipped (they are not part of this binary decision).
    #[must_use]
    pub fn evaluate(&self, held_out: &[LabeledExample]) -> Tier2EvalMetrics {
        let (mut tp, mut fp, mut fn_, mut tn) = (0u32, 0u32, 0u32, 0u32);
        for ex in held_out {
            let actual_pos = ex.label == self.manifest.positive_label;
            let actual_neg = ex.label == self.manifest.negative_label;
            if !actual_pos && !actual_neg {
                continue;
            }
            let (p, _) = self.infer(&ex.features);
            match (p >= 0.5, actual_pos) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, true) => fn_ += 1,
                (false, false) => tn += 1,
            }
        }
        let n = tp + fp + fn_ + tn;
        let ratio = |num: u32, den: u32| if den == 0 { 0.0 } else { f64::from(num) / f64::from(den) };
        Tier2EvalMetrics {
            n,
            accuracy: ratio(tp + tn, n),
            precision: ratio(tp, tp + fp),
            recall: ratio(tp, tp + fn_),
            true_positives: tp,
            false_positives: fp,
        }
    }

    /// Write the loadable artifact (Burn record + manifest) into `dir`.
    ///
    /// # Errors
    /// [`Tier2ModelError`] on a record or filesystem failure.
    pub fn save(&self, dir: &Path) -> Result<(), Tier2ModelError> {
        std::fs::create_dir_all(dir)?;
        DefaultRecorder::new()
            .record(self.model.clone().into_record(), dir.join("model"))
            .map_err(|e| Tier2ModelError::Record(e.to_string()))?;
        let json = serde_json::to_vec_pretty(&self.manifest)?;
        write_private(&dir.join("manifest.json"), &json)?;
        Ok(())
    }

    /// Load a trained model from an artifact directory previously written by [`Self::save`].
    ///
    /// # Errors
    /// [`Tier2ModelError`] if the manifest or Burn record is missing/corrupt.
    pub fn load(dir: &Path) -> Result<Self, Tier2ModelError> {
        let manifest_bytes = std::fs::read(dir.join("manifest.json"))?;
        let manifest: Tier2Manifest = serde_json::from_slice(&manifest_bytes)?;
        let d = manifest.vocab.len().max(1);
        let device = Default::default();
        let model = Tier2Model::<InferBackend>::new(&device, d);
        let record = DefaultRecorder::new()
            .load(dir.join("model"), &device)
            .map_err(|e| Tier2ModelError::Record(e.to_string()))?;
        let model = model.load_record(record);
        Ok(assemble(model, manifest))
    }
}

/// Minimum held-out rows required before the precision gate is trusted (below this a run
/// reports its metrics but never activates — too little signal to promote a model).
pub const MIN_EVAL_ROWS: u32 = 4;
/// Default held-out precision an artifact must clear to be promoted to active.
pub const DEFAULT_PRECISION_GATE: f64 = 0.8;

/// The outcome of a full Tier-2 training run (train → eval the reloaded artifact → gate).
#[derive(Debug, Clone)]
pub struct Tier2TrainingReport {
    /// Examples used for training (after the held-out split).
    pub train_count: usize,
    /// Held-out evaluation of the **reloaded** artifact.
    pub eval: Tier2EvalMetrics,
    /// Whether the artifact cleared the gate and was promoted to active.
    pub activated: bool,
    /// The precision the held-out eval had to clear.
    pub precision_gate: f64,
    /// The active artifact directory, when activated.
    pub artifact_path: Option<String>,
}

/// Deterministic train/held-out split: every 5th example (by position) is held out (≈20%).
/// Stable so a run is reproducible and the held-out set never overlaps training.
fn split_holdout(examples: &[LabeledExample]) -> (Vec<LabeledExample>, Vec<LabeledExample>) {
    let mut train = Vec::new();
    let mut held = Vec::new();
    for (i, ex) in examples.iter().enumerate() {
        if i % 5 == 0 {
            held.push(ex.clone());
        } else {
            train.push(ex.clone());
        }
    }
    (train, held)
}

/// Promote a candidate artifact directory to the active path, preserving the prior active model
/// if anything goes wrong. The prior active is first moved aside to a `.bak` sibling (an atomic
/// rename); only then is the candidate renamed into place; the backup is removed on success and
/// **restored on failure** — so a failed promotion leaves the prior active model intact, never a
/// hole. (Both renames are within the same parent directory, so neither is cross-filesystem.)
fn promote(candidate_dir: &Path, active_dir: &Path) -> Result<(), Tier2ModelError> {
    if let Some(parent) = active_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !active_dir.exists() {
        // No prior active: a single rename is already all-or-nothing.
        return std::fs::rename(candidate_dir, active_dir).map_err(Tier2ModelError::Io);
    }
    let backup = active_dir.with_extension("bak");
    let _ = std::fs::remove_dir_all(&backup); // clear any stale backup from a prior crash
    std::fs::rename(active_dir, &backup)?; // move the prior active aside (atomic)
    match std::fs::rename(candidate_dir, active_dir) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(e) => {
            // Restore the prior active; the candidate stays on disk for inspection.
            let _ = std::fs::rename(&backup, active_dir);
            Err(Tier2ModelError::Io(e))
        }
    }
}

/// Run a full Tier-2 training pass: split off a held-out set, train a Burn model on the rest,
/// save it to `candidate_dir`, **reload it from disk**, score the *reloaded* artifact on the
/// held-out set, and promote it to `active_dir` only if held-out precision clears
/// `precision_gate` (and there were enough held-out rows to trust). The gate therefore
/// measures exactly the bytes that would be deployed — not the in-memory model, and never the
/// base/fallback model. A failing run leaves the prior active model untouched.
///
/// # Errors
/// [`Tier2ModelError::NoExamples`] if the training split carries no known label; filesystem /
/// record errors on save or reload.
pub fn train_eval_gate(
    examples: &[LabeledExample],
    positive_label: &str,
    negative_label: &str,
    config: &Tier2TrainConfig,
    candidate_dir: &Path,
    active_dir: &Path,
    precision_gate: f64,
) -> Result<Tier2TrainingReport, Tier2ModelError> {
    let (train_set, held) = split_holdout(examples);
    let trained = train(&train_set, positive_label, negative_label, config)?;
    let train_count = train_set
        .iter()
        .filter(|e| e.label == positive_label || e.label == negative_label)
        .count();

    let _ = std::fs::remove_dir_all(candidate_dir);
    trained.save(candidate_dir)?;

    // Evaluate the RELOADED artifact (the deployable bytes), not the in-memory model.
    let reloaded = TrainedTier2::load(candidate_dir)?;
    let eval = reloaded.evaluate(&held);

    let activated = eval.n >= MIN_EVAL_ROWS && eval.precision >= precision_gate;
    let artifact_path = if activated {
        promote(candidate_dir, active_dir)?;
        Some(active_dir.to_string_lossy().into_owned())
    } else {
        let _ = std::fs::remove_dir_all(candidate_dir);
        None
    };

    Ok(Tier2TrainingReport {
        train_count,
        eval,
        activated,
        precision_gate,
        artifact_path,
    })
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// A [`Tier2Classifier`] backed by a trained, loaded Burn artifact. `predict` runs the loaded
/// model; `update` is a no-op — this is a *batch* model whose corrections accumulate in the
/// feedback table (the source of truth) and fold in at the next training run, never silently
/// mutating a gated, activated model between retrains.
pub struct BurnTier2Classifier {
    inner: TrainedTier2,
}

impl BurnTier2Classifier {
    /// Wrap a trained model as the cascade's Tier-2 adapter.
    #[must_use]
    pub fn new(inner: TrainedTier2) -> Self {
        Self { inner }
    }

    /// Load an artifact directory and wrap it as a Tier-2 adapter.
    ///
    /// # Errors
    /// [`Tier2ModelError`] if the artifact is missing/corrupt.
    pub fn load(dir: &Path) -> Result<Self, Tier2ModelError> {
        Ok(Self::new(TrainedTier2::load(dir)?))
    }
}

#[async_trait]
impl Tier2Classifier for BurnTier2Classifier {
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError> {
        let (p, contributions) = self.inner.infer(&features);
        let (pos, neg) = self.inner.labels();
        let mut scores = std::collections::BTreeMap::new();
        scores.insert(pos.to_owned(), p);
        scores.insert(neg.to_owned(), 1.0 - p);
        Ok(CalibratedScores {
            scores,
            calibration_version: self.inner.manifest.calibration_version.clone(),
            contributions,
        })
    }

    async fn update(&self, _labeled: LabeledExample) -> Result<(), MlError> {
        // Batch model: a gated, activated artifact is immutable between training runs. The
        // correction is persisted in the feedback table by the correction path and folds into
        // the next run — so learning is not lost, only deferred to the next vetted artifact.
        Ok(())
    }
}

/// A [`Tier2Classifier`] whose backing model can be hot-swapped in place behind its `Arc`. The
/// cascade holds one of these as its Tier-2; activating a freshly-trained, gated artifact swaps
/// it in without a host restart — the same hot-reload shape the rule engines use. Reads take a
/// snapshot `Arc` and release the lock before awaiting, so a swap never blocks an in-flight
/// prediction.
pub struct SwappableTier2 {
    current: RwLock<Arc<dyn Tier2Classifier>>,
}

impl SwappableTier2 {
    /// Wrap an initial backing model (e.g. the online logistic regression, or a loaded Burn
    /// artifact at startup).
    #[must_use]
    pub fn new(initial: Arc<dyn Tier2Classifier>) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    /// Swap in a new backing model; subsequent predictions use it.
    pub fn swap(&self, next: Arc<dyn Tier2Classifier>) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = next;
    }

    fn snapshot(&self) -> Arc<dyn Tier2Classifier> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Tier2Classifier for SwappableTier2 {
    async fn predict(&self, features: FeatureVector) -> Result<CalibratedScores, MlError> {
        self.snapshot().predict(features).await
    }

    async fn update(&self, labeled: LabeledExample) -> Result<(), MlError> {
        self.snapshot().update(labeled).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use mailmate_common::features::FeatureValue;

    fn ex(label: &str, pairs: &[(&str, FeatureValue)]) -> LabeledExample {
        let mut fv = FeatureVector::new();
        for (k, v) in pairs {
            fv.insert(*k, v.clone());
        }
        LabeledExample {
            features: fv,
            label: label.to_owned(),
        }
    }

    /// A linearly-separable spam/ham corpus: spam has a link, ham does not.
    fn corpus() -> Vec<LabeledExample> {
        let mut v = Vec::new();
        for i in 0..12 {
            v.push(ex(
                "spam",
                &[
                    ("has_link", FeatureValue::Bool(true)),
                    ("sender", FeatureValue::Text(format!("bad{i}@x.example"))),
                ],
            ));
            v.push(ex(
                "ham",
                &[
                    ("has_link", FeatureValue::Bool(false)),
                    ("sender", FeatureValue::Text(format!("friend{i}@home.example"))),
                ],
            ));
        }
        v
    }

    #[test]
    fn training_separates_the_two_classes() {
        let trained = train(&corpus(), "spam", "ham", &Tier2TrainConfig::default()).unwrap();
        let spammy = ex("spam", &[("has_link", FeatureValue::Bool(true))]);
        let hammy = ex("ham", &[("has_link", FeatureValue::Bool(false))]);
        let (ps, _) = trained.infer(&spammy.features);
        let (ph, _) = trained.infer(&hammy.features);
        assert!(ps > 0.5, "spam should score > 0.5: {ps}");
        assert!(ph < 0.5, "ham should score < 0.5: {ph}");
        assert!(ps > ph + 0.3, "the classes should separate: {ps} vs {ph}");
    }

    #[test]
    fn no_known_label_is_an_error() {
        let only_other = vec![ex("phishing", &[("x", FeatureValue::Bool(true))])];
        let err = train(&only_other, "spam", "ham", &Tier2TrainConfig::default()).unwrap_err();
        assert!(matches!(err, Tier2ModelError::NoExamples));
    }

    #[test]
    fn the_artifact_round_trips_and_the_cascade_path_uses_it() {
        let dir = std::env::temp_dir().join(format!("mm_tier2_artifact_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trained = train(&corpus(), "spam", "ham", &Tier2TrainConfig::default()).unwrap();
        let spammy = ex("spam", &[("has_link", FeatureValue::Bool(true))]).features;
        let before = trained.infer(&spammy).0;
        trained.save(&dir).unwrap();
        assert!(dir.join("model.mpk").exists(), "the Burn record must be written");
        assert!(dir.join("manifest.json").exists(), "the manifest must be written");

        // A fresh load (a "restart") reproduces the trained prediction — the artifact is faithful.
        let clf = BurnTier2Classifier::load(&dir).unwrap();
        let scored = block_on(clf.predict(spammy)).unwrap();
        let after = scored.scores["spam"];
        assert!(
            (before - after).abs() < 1e-5,
            "reloaded artifact reproduces the score: {before} vs {after}"
        );
        assert_eq!(scored.calibration_version, CALIBRATION_VERSION);
        assert!(!scored.contributions.is_empty(), "contributions are exposed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evaluate_reports_precision_on_held_out_data() {
        // Train on one half, evaluate on a disjoint held-out half of the same separable corpus.
        let all = corpus();
        let (train_set, held) = all.split_at(all.len() / 2);
        let trained = train(train_set, "spam", "ham", &Tier2TrainConfig::default()).unwrap();
        let metrics = trained.evaluate(held);
        assert!(metrics.n > 0, "held-out rows were scored");
        assert!(
            metrics.precision >= 0.9,
            "a separable corpus should classify cleanly: {metrics:?}"
        );
        assert!(metrics.accuracy >= 0.9, "{metrics:?}");
    }

    fn artifact_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("mm_tier2_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("candidate"), base.join("active"))
    }

    #[test]
    fn a_separable_corpus_trains_evals_the_reloaded_artifact_and_activates() {
        let (cand, active) = artifact_dirs("gate_pass");
        let report = train_eval_gate(
            &corpus(),
            "spam",
            "ham",
            &Tier2TrainConfig::default(),
            &cand,
            &active,
            DEFAULT_PRECISION_GATE,
        )
        .unwrap();
        assert!(report.activated, "separable corpus should activate: {report:?}");
        assert!(report.eval.n >= MIN_EVAL_ROWS, "{report:?}");
        assert!(report.eval.precision >= DEFAULT_PRECISION_GATE, "{report:?}");
        assert!(active.join("model.mpk").exists(), "active artifact written");
        assert!(!cand.exists(), "candidate was promoted (renamed) to active");
        // The cascade can load the active artifact and use it.
        let clf = BurnTier2Classifier::load(&active).unwrap();
        let spammy = ex("spam", &[("has_link", FeatureValue::Bool(true))]).features;
        assert!(block_on(clf.predict(spammy)).unwrap().scores["spam"] > 0.5);
        let _ = std::fs::remove_dir_all(active.parent().unwrap());
    }

    #[test]
    fn an_unlearnable_corpus_does_not_activate_and_leaves_active_untouched() {
        // Identical features, labels independent of them → the model cannot separate, so
        // held-out precision falls below a strict gate and nothing is promoted.
        let mut noisy = Vec::new();
        for i in 0..20 {
            let label = if i % 2 == 0 { "spam" } else { "ham" };
            noisy.push(ex(label, &[("x", FeatureValue::Bool(true))]));
        }
        let (cand, active) = artifact_dirs("gate_fail");
        let report = train_eval_gate(
            &noisy,
            "spam",
            "ham",
            &Tier2TrainConfig::default(),
            &cand,
            &active,
            0.95,
        )
        .unwrap();
        assert!(!report.activated, "noise must not clear a 0.95 gate: {report:?}");
        assert!(report.artifact_path.is_none());
        assert!(!active.exists(), "no model was promoted to active");
        assert!(!cand.exists(), "the rejected candidate was cleaned up");
    }

    #[test]
    fn the_swappable_tier2_hot_swaps_the_backing_model() {
        use mailmate_common::error::MlError;
        use mailmate_ports::tier2_classifier::Tier2Classifier;

        // A trivial stand-in that always returns a fixed positive score, to prove the swap took.
        struct Fixed(f64);
        #[async_trait]
        impl Tier2Classifier for Fixed {
            async fn predict(
                &self,
                _f: FeatureVector,
            ) -> Result<CalibratedScores, MlError> {
                let mut scores = std::collections::BTreeMap::new();
                scores.insert("spam".to_owned(), self.0);
                scores.insert("ham".to_owned(), 1.0 - self.0);
                Ok(CalibratedScores {
                    scores,
                    calibration_version: "fixed".to_owned(),
                    contributions: vec![],
                })
            }
            async fn update(&self, _l: LabeledExample) -> Result<(), MlError> {
                Ok(())
            }
        }

        let swap = SwappableTier2::new(Arc::new(Fixed(0.1)));
        let fv = FeatureVector::new();
        assert!((block_on(swap.predict(fv.clone())).unwrap().scores["spam"] - 0.1).abs() < 1e-9);
        swap.swap(Arc::new(Fixed(0.9)));
        assert!((block_on(swap.predict(fv)).unwrap().scores["spam"] - 0.9).abs() < 1e-9);
    }

    #[test]
    fn build_vocab_drops_ungeneralizable_singletons_and_bounds_the_dimension() {
        // 30 examples, each a UNIQUE sender one-hot plus a shared `has_link` key.
        let mut examples = Vec::new();
        for i in 0..30 {
            examples.push(ex(
                "spam",
                &[
                    ("has_link", FeatureValue::Bool(true)),
                    ("sender", FeatureValue::Text(format!("u{i}@x"))),
                ],
            ));
        }
        let vocab = build_vocab(&examples);
        // The 30 singleton sender one-hots are dropped; only the shared key (support 30) remains —
        // so the dense matrix dimension stays bounded on a high-cardinality corpus.
        assert_eq!(vocab, vec!["has_link".to_owned()], "singletons dropped: {vocab:?}");
    }

    #[test]
    fn a_second_activation_replaces_the_active_artifact_and_leaves_no_backup() {
        let (cand, active) = artifact_dirs("replace");
        let cfg = Tier2TrainConfig::default();
        let r1 = train_eval_gate(&corpus(), "spam", "ham", &cfg, &cand, &active, DEFAULT_PRECISION_GATE).unwrap();
        assert!(r1.activated && active.join("model.mpk").exists());
        // Re-train: the backup→rename→cleanup path replaces the prior active in place.
        let r2 = train_eval_gate(&corpus(), "spam", "ham", &cfg, &cand, &active, DEFAULT_PRECISION_GATE).unwrap();
        assert!(r2.activated, "second activation should succeed: {r2:?}");
        assert!(active.join("model.mpk").exists(), "active still present after replace");
        assert!(!active.with_extension("bak").exists(), "no stale backup left behind");
        assert!(!cand.exists(), "candidate was consumed by promotion");
        let _ = std::fs::remove_dir_all(active.parent().unwrap());
    }
}
