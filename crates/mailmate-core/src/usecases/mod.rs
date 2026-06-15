//! The core's use-cases — the product logic, expressed purely over ports.
//!
//! Each use-case is the async orchestration seam for one slice of behaviour. Phase 6 adds
//! the two-pipeline [`planning`] flow (classify → plan → guard); Phase 7 adds the
//! [`correction`] capture flow (record feedback → Tier-2 online update); Phase 8 adds the
//! [`curation`] flow (run the curator → review its proposals); Phase 9 adds the
//! [`training`] flow (derive → train → evaluate → gate an adapter); Phase 10 adds the
//! [`draft`] flow (generate an advisory, review-required reply).

pub mod correction;
pub mod curation;
pub mod draft;
pub mod planning;
pub mod training;

pub use correction::{CorrectionContext, CorrectionService};
pub use curation::{CurationService, ReviewService};
pub use draft::DraftService;
pub use planning::{PlanningOutcome, PlanningService};
pub use training::TrainingService;
