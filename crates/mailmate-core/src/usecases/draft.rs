//! The draft-reply use-case: turn a bounded reply context into an advisory, review-required
//! draft.
//!
//! It names only the [`ReplyDrafter`] port, so the model-backed adapter is injected at the
//! edge and the core never sees a concrete provider. The service does the one thing the
//! `never_auto_send_drafts` invariant needs done in code rather than prose: it assigns the
//! draft a fresh id and pins `requires_human_review = true` via [`ReplyDraft::from_drafted`],
//! so nothing the drafter returns can present itself as ready to send.

use std::sync::Arc;

use mailmate_common::error::MailMateError;
use mailmate_common::ids::DraftId;
use mailmate_common::reply::{ReplyDraft, ReplyDraftRequest};
use mailmate_ports::reply_drafter::ReplyDrafter;

use crate::Ports;

/// Produces advisory reply drafts.
#[derive(Clone)]
pub struct DraftService {
    drafter: Arc<dyn ReplyDrafter>,
}

impl DraftService {
    /// Assemble the service from the drafter port.
    #[must_use]
    pub fn new(drafter: Arc<dyn ReplyDrafter>) -> Self {
        Self { drafter }
    }

    /// Assemble the service from the core's [`Ports`] bundle.
    #[must_use]
    pub fn from_ports(ports: &Ports) -> Self {
        Self::new(ports.reply_drafter.clone())
    }

    /// Draft a reply for `request`, returning a review-required [`ReplyDraft`] with a fresh id.
    ///
    /// # Errors
    /// Propagates a drafter failure (mapped from [`mailmate_common::error::AiError`]) as a
    /// [`MailMateError`].
    pub async fn draft_reply(
        &self,
        request: ReplyDraftRequest,
    ) -> Result<ReplyDraft, MailMateError> {
        let drafted = self.drafter.draft(request).await?;
        Ok(ReplyDraft::from_drafted(DraftId::fresh(), drafted))
    }
}
