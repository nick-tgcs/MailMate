//! The bounce / NDR detector: a pure heuristic that recognises a Non-Delivery Report (a
//! "your message couldn't be delivered" bounce) from its sender address and subject. When such a
//! report comes back on a tracked follow-up thread, the address is unreachable and chasing a
//! reply is pointless — the host fires an [`ExitEvent::Bounced`] to stop the sequence.
//!
//! [`ExitEvent::Bounced`]: mailmate_common::workflow::ExitEvent::Bounced
//!
//! Dependency-free string matching (no regex, like the privacy scanners). It is a *heuristic*,
//! not an RFC-3464 parser: it recognises the near-universal conventions —
//!
//! - **Sender:** the local-part / address of an automated bounce: `mailer-daemon`, `postmaster`,
//!   or a `bounce`/`no-reply` delivery agent.
//! - **Subject:** the standard NDR phrasings: "undeliverable", "delivery status notification",
//!   "returned mail", "mail delivery failed", "failure notice", "could not be delivered", …
//!
//! Either signal alone is enough — a genuine NDR almost always carries both, and requiring both
//! would miss bounces from non-conventionally-named relays. What is **not** detected: a
//! human-written "your email bounced back to me" (no daemon sender, no NDR subject), a localized
//! subject in a language we don't list, or an inline delivery-status part with an ordinary
//! subject. The host re-confirms before exiting, so a false negative just means the sequence
//! keeps running (the safe direction).

/// Automated-bounce sender markers (matched case-insensitively as substrings of the address).
const BOUNCE_SENDERS: &[&str] = &["mailer-daemon", "postmaster", "mail-daemon"];

/// NDR subject phrasings (matched case-insensitively as substrings).
const BOUNCE_SUBJECTS: &[&str] = &[
    "undeliverable",
    "undelivered mail",
    "delivery status notification",
    "delivery failure",
    "delivery has failed",
    "returned mail",
    "returned to sender",
    "mail delivery failed",
    "failure notice",
    "could not be delivered",
    "message not delivered",
];

/// Whether `sender_email` looks like an automated bounce agent.
#[must_use]
pub fn sender_is_bounce_agent(sender_email: &str) -> bool {
    let lower = sender_email.to_ascii_lowercase();
    BOUNCE_SENDERS.iter().any(|m| lower.contains(m))
}

/// Whether `subject` reads like a Non-Delivery Report.
#[must_use]
pub fn subject_is_ndr(subject: &str) -> bool {
    let lower = subject.to_ascii_lowercase();
    BOUNCE_SUBJECTS.iter().any(|m| lower.contains(m))
}

/// Whether a message (by sender + subject) is a delivery-failure / bounce notification. True when
/// either the sender is a known bounce agent **or** the subject is a standard NDR phrasing — a
/// real bounce almost always satisfies both, but matching on either catches non-conventional
/// relays without missing the common case.
#[must_use]
pub fn is_bounce_notification(sender_email: &str, subject: &str) -> bool {
    sender_is_bounce_agent(sender_email) || subject_is_ndr(subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_a_classic_mailer_daemon_bounce() {
        assert!(is_bounce_notification(
            "MAILER-DAEMON@mx.example.com",
            "Undelivered Mail Returned to Sender"
        ));
        assert!(is_bounce_notification(
            "postmaster@corp.example",
            "Delivery Status Notification (Failure)"
        ));
    }

    #[test]
    fn either_signal_alone_is_enough() {
        // Daemon sender, ordinary subject.
        assert!(is_bounce_notification(
            "mailer-daemon@x.test",
            "Re: your quote"
        ));
        // Ordinary sender, NDR subject (a relay with a non-standard name).
        assert!(is_bounce_notification(
            "relay@bounces.x.test",
            "Mail delivery failed: returning your message"
        ));
    }

    #[test]
    fn an_ordinary_reply_is_not_a_bounce() {
        assert!(!is_bounce_notification(
            "dana@client.test",
            "Re: the proposal — looks good"
        ));
        assert!(!is_bounce_notification(
            "noreply@news.test",
            "Your weekly digest"
        ));
        // A human mentioning a bounce in prose is not an NDR.
        assert!(!is_bounce_notification(
            "dana@client.test",
            "btw your last email bounced back to me"
        ));
    }
}
