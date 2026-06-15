//! The reply-drafter port: generate an advisory reply draft from a bounded context.
//!
//! This is the seam the `draft_reply` task sits behind. The default adapter (in
//! `mailmate-ai`) wraps the `draft_reply` task over the frozen `AiProvider`; a fake drives
//! tests. The port returns only the generated text ([`DraftedReply`]); pinning the
//! review-required flag and assigning an id is the core use-case's job, so no adapter can
//! produce something that bypasses `never_auto_send_drafts`.

use async_trait::async_trait;

use mailmate_common::error::AiError;
use mailmate_common::reply::{DraftedReply, ReplyDraftRequest};

/// Generates advisory reply drafts.
#[async_trait]
pub trait ReplyDrafter: Send + Sync {
    /// Draft a reply for `request`. The result is advisory text only and is always
    /// persisted review-required downstream.
    ///
    /// # Errors
    /// [`AiError`] on a provider/transport failure or a response that fails validation.
    async fn draft(&self, request: ReplyDraftRequest) -> Result<DraftedReply, AiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_object_safe() {
        fn takes(_: &dyn ReplyDrafter) {}
        let _ = takes as fn(&dyn ReplyDrafter);
    }
}
