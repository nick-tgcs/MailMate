//! The core's use-cases — the product logic, expressed purely over ports.
//!
//! Each use-case is the async orchestration seam for one slice of behaviour. Phase 6 adds
//! the two-pipeline [`planning`] flow (classify → plan → guard).

pub mod planning;

pub use planning::{PlanningOutcome, PlanningService};
