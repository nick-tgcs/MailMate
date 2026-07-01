//! The per-message **Safety block**: inform-only phishing/malware findings surfaced beside a
//! verdict.
//!
//! These findings never change what MailMate *does* — they are not policy, they take no action
//! and gate nothing. They are a heads-up the panel renders so a human can spot a forged sender,
//! a dangerous attachment, or a deceptive link before acting. The assessment is pure and
//! deterministic (header/attachment metadata + the already-computed feature vector), so it is
//! replayable and testable without a model or the network.

use serde::{Deserialize, Serialize};

use crate::features::{FeatureValue, FeatureVector};
use crate::mail::MessageData;

/// How alarming a safety finding is. Purely advisory — even `Danger` informs, never blocks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetySeverity {
    /// Context worth knowing, not alarming on its own.
    Info,
    /// A cue that warrants a second look.
    Warning,
    /// A strong phishing/malware cue — treat with suspicion.
    Danger,
}

impl SafetySeverity {
    /// The stable snake_case label used on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }
}

/// One inform-only safety finding about a message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SafetyFinding {
    /// Stable code (`auth_failure`, `executable_attachment`, …) for iconography/testing.
    pub id: String,
    /// Short human title ("Sender failed authentication").
    pub title: String,
    /// A one-line explanation of why it matters / what to do.
    pub detail: String,
    /// How alarming it is.
    pub severity: SafetySeverity,
}

impl SafetyFinding {
    fn new(id: &str, title: &str, detail: String, severity: SafetySeverity) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            detail,
            severity,
        }
    }
}

fn is_true(features: &FeatureVector, key: &str) -> bool {
    matches!(features.get(key), Some(FeatureValue::Bool(true)))
}

/// Assess a message for inform-only safety findings, most-severe-first. Derived from the
/// authentication verdict the MTA already wrote, spoofing cues, dangerous-attachment classes,
/// and — only when a body was retained — deceptive/defanged links. Pure: no network, no model.
#[must_use]
pub fn assess_safety(message: &MessageData, features: &FeatureVector) -> Vec<SafetyFinding> {
    let mut findings = Vec::new();

    // Spoofing: a display name that embeds a different address is a classic impersonation tell.
    if is_true(features, "display_name_spoofed") {
        findings.push(SafetyFinding::new(
            "display_name_spoofed",
            "Sender name may be spoofed",
            "The sender's display name embeds a different email address than the one it was \
             sent from — a common impersonation trick."
                .to_owned(),
            SafetySeverity::Danger,
        ));
    }

    // Dangerous attachments.
    if is_true(features, "has_executable") {
        let names = attachment_names(message, is_executable_name);
        findings.push(SafetyFinding::new(
            "executable_attachment",
            "Executable attachment",
            format!(
                "This message carries a runnable program or script ({names}). Opening it could \
                 install malware."
            ),
            SafetySeverity::Danger,
        ));
    }

    // Authentication failure: the receiving server's own SPF/DKIM/DMARC verdict said something
    // failed — the sending domain may be forged.
    if is_true(features, "auth_fail") {
        findings.push(SafetyFinding::new(
            "auth_failure",
            "Sender failed authentication",
            "SPF, DKIM, or DMARC checks failed for this message — the sending address may be \
             forged. Be cautious with links and requests."
                .to_owned(),
            SafetySeverity::Warning,
        ));
    }

    // Reply-To pointing elsewhere — a reply would silently leave the apparent sender's domain.
    if is_true(features, "reply_to_mismatch") {
        findings.push(SafetyFinding::new(
            "reply_to_mismatch",
            "Replies would go elsewhere",
            "The Reply-To address is on a different domain than the sender — a reply would not \
             reach who it appears to."
                .to_owned(),
            SafetySeverity::Warning,
        ));
    }

    // Archive attachments — not dangerous alone, but a common malware-delivery wrapper.
    if is_true(features, "has_archive") {
        let names = attachment_names(message, is_archive_name);
        findings.push(SafetyFinding::new(
            "archive_attachment",
            "Archive attachment",
            format!("This message carries an archive ({names}); archives can conceal executables."),
            SafetySeverity::Info,
        ));
    }

    // Deceptive / defanged links — only when a body was retained to scan. Links are rendered
    // defanged (`example[.]com`) so the finding itself can never be a live clickable link.
    let defanged = defanged_link_domains(message);
    if !defanged.is_empty() {
        findings.push(SafetyFinding::new(
            "links_present",
            "Links in this message",
            format!(
                "Links point to: {}. Hover before clicking.",
                defanged.join(", ")
            ),
            SafetySeverity::Info,
        ));
    }

    findings
}

/// The (comma-joined) filenames of attachments matching `pred`, for a finding's detail.
fn attachment_names(message: &MessageData, pred: fn(&str) -> bool) -> String {
    let names: Vec<&str> = message
        .attachments
        .iter()
        .filter(|a| pred(&a.filename))
        .map(|a| a.filename.as_str())
        .collect();
    if names.is_empty() {
        "attached".to_owned()
    } else {
        names.join(", ")
    }
}

fn has_extension(filename: &str, exts: &[&str]) -> bool {
    let lower = filename.to_ascii_lowercase();
    exts.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

fn is_executable_name(filename: &str) -> bool {
    has_extension(
        filename,
        &[
            "exe", "scr", "bat", "cmd", "com", "js", "vbs", "jar", "msi", "ps1", "lnk",
        ],
    )
}

fn is_archive_name(filename: &str) -> bool {
    has_extension(filename, &["zip", "rar", "7z", "tar", "gz", "tgz"])
}

/// The distinct, **defanged** domains of `http(s)` links in a retained body (`example.com` →
/// `example[.]com`), capped so the finding stays readable. Empty when no body was retained.
fn defanged_link_domains(message: &MessageData) -> Vec<String> {
    const MAX: usize = 5;
    let Some(body) = &message.body_text else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for token in body.split(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"') {
        let Some(rest) = token
            .strip_prefix("https://")
            .or_else(|| token.strip_prefix("http://"))
        else {
            continue;
        };
        // The host is everything up to the first '/', '?', or ':'.
        let host: String = rest
            .chars()
            .take_while(|c| *c != '/' && *c != '?' && *c != ':')
            .collect();
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        let defanged = host.replace('.', "[.]");
        if !out.contains(&defanged) {
            out.push(defanged);
            if out.len() >= MAX {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AccountId, FolderId};
    use crate::mail::{Attachment, MessageHeaders};

    fn message(attachments: Vec<Attachment>, body: Option<&str>) -> MessageData {
        MessageData {
            id: None,
            client_message_id: "1".to_owned(),
            account_id: AccountId::from("acct"),
            folder_id: FolderId::from("inbox"),
            thread_id: None,
            headers: MessageHeaders::default(),
            body_text: body.map(str::to_owned),
            attachments,
            remote_content_loaded: false,
            sender_seen_count: None,
            sender_in_address_book: None,
        }
    }

    fn attachment(name: &str) -> Attachment {
        Attachment {
            filename: name.to_owned(),
            content_type: "application/octet-stream".to_owned(),
            size_bytes: 10,
        }
    }

    fn features(pairs: &[(&str, bool)]) -> FeatureVector {
        let mut fv = FeatureVector::new();
        for (k, v) in pairs {
            fv.insert(*k, FeatureValue::Bool(*v));
        }
        fv
    }

    #[test]
    fn a_clean_message_has_no_safety_findings() {
        let findings = assess_safety(&message(vec![], None), &FeatureVector::new());
        assert!(findings.is_empty());
    }

    #[test]
    fn auth_failure_and_spoof_become_findings_most_severe_first() {
        let msg = message(vec![], None);
        let fv = features(&[
            ("auth_fail", true),
            ("display_name_spoofed", true),
            ("reply_to_mismatch", true),
        ]);
        let findings = assess_safety(&msg, &fv);
        let ids: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"auth_failure"));
        assert!(ids.contains(&"display_name_spoofed"));
        assert!(ids.contains(&"reply_to_mismatch"));
        // The spoof (Danger) is surfaced before the auth/reply warnings.
        assert_eq!(findings[0].id, "display_name_spoofed");
        assert_eq!(findings[0].severity, SafetySeverity::Danger);
    }

    #[test]
    fn an_executable_attachment_is_a_danger_finding_naming_the_file() {
        let msg = message(vec![attachment("invoice.pdf.exe")], None);
        let fv = features(&[("has_executable", true)]);
        let findings = assess_safety(&msg, &fv);
        let exe = findings
            .iter()
            .find(|f| f.id == "executable_attachment")
            .unwrap();
        assert_eq!(exe.severity, SafetySeverity::Danger);
        assert!(
            exe.detail.contains("invoice.pdf.exe"),
            "names the file: {}",
            exe.detail
        );
    }

    #[test]
    fn links_in_a_retained_body_are_listed_defanged() {
        let msg = message(
            vec![],
            Some("Click https://paypa1.com/login or http://safe.test/x now"),
        );
        let findings = assess_safety(&msg, &FeatureVector::new());
        let links = findings.iter().find(|f| f.id == "links_present").unwrap();
        // Domains are defanged so the finding can never render a live link.
        assert!(
            links.detail.contains("paypa1[.]com"),
            "got {}",
            links.detail
        );
        assert!(links.detail.contains("safe[.]test"));
        assert!(!links.detail.contains("paypa1.com"));
    }

    #[test]
    fn no_body_means_no_link_finding() {
        let msg = message(vec![], None);
        let fv = features(&[("auth_fail", true)]);
        let findings = assess_safety(&msg, &fv);
        assert!(findings.iter().all(|f| f.id != "links_present"));
    }

    #[test]
    fn severity_labels_are_stable() {
        assert_eq!(SafetySeverity::Info.as_str(), "info");
        assert_eq!(SafetySeverity::Warning.as_str(), "warning");
        assert_eq!(SafetySeverity::Danger.as_str(), "danger");
    }
}
