//! The pluggable trainer backends that ship with this crate.
//!
//! The in-process **Burn** default lives in `mailmate-ml` (feature-gated, behind this
//! crate's [`TrainerBackend`](crate::trainer::TrainerBackend) trait). The two backends here
//! need no GPU and no external tool, so every test of the pipeline runs on them:
//!
//! - [`mock`] — a deterministic in-memory trainer. It advertises full capabilities
//!   (including `lora`), so the LoRA orchestration path can be exercised end-to-end without
//!   a real adapter.
//! - [`external`] — a subprocess-shaped backend that drives an external toolchain (the
//!   proven LoRA fallback) through an injectable [`external::CommandRunner`], so the
//!   serialization + result handling is tested without spawning a process.

pub mod external;
pub mod mock;
