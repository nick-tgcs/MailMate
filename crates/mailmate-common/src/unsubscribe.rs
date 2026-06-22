//! Parsing the `List-Unsubscribe` header into a one-click unsubscribe affordance.
//!
//! RFC 2369 puts one or more `<URI>` targets in `List-Unsubscribe` (a `mailto:` and/or an
//! `http(s):` URL). RFC 8058 adds `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, marking
//! the HTTPS target safe to unsubscribe from with a single background POST — no confirmation
//! page. This pure parser turns those headers into a typed affordance the panel renders; the
//! extension performs the mailto-compose or the one-click POST. No network here.

use serde::{Deserialize, Serialize};

/// A parsed `mailto:` unsubscribe target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MailtoUnsubscribe {
    /// The address to send the unsubscribe request to.
    pub to: String,
    /// The `subject` the mailto requested, if any (often `unsubscribe`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// The one-click unsubscribe options parsed from a message's headers.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnsubscribeOptions {
    /// A `mailto:` target, preferred for a no-tracking unsubscribe (open a pre-addressed compose).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailto: Option<MailtoUnsubscribe>,
    /// An `http(s):` target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_url: Option<String>,
    /// Whether the HTTPS target supports an RFC 8058 one-click POST (no confirmation page).
    pub one_click: bool,
}

/// Parse `List-Unsubscribe` (+ optional `List-Unsubscribe-Post`) into a typed affordance, or
/// `None` when there is nothing actionable. Tolerant of whitespace and missing angle brackets.
#[must_use]
pub fn parse_unsubscribe(
    list_unsubscribe: Option<&str>,
    list_unsubscribe_post: Option<&str>,
) -> Option<UnsubscribeOptions> {
    let raw = list_unsubscribe?.trim();
    if raw.is_empty() {
        return None;
    }

    let mut options = UnsubscribeOptions::default();
    for target in split_targets(raw) {
        let target = target.trim();
        if let Some(rest) = strip_scheme(target, "mailto:") {
            if options.mailto.is_none() {
                options.mailto = Some(parse_mailto(rest));
            }
        } else if options.http_url.is_none()
            && (starts_with_ci(target, "https://") || starts_with_ci(target, "http://"))
        {
            options.http_url = Some(target.to_owned());
        }
    }

    if options.mailto.is_none() && options.http_url.is_none() {
        return None;
    }

    // RFC 8058 one-click is only meaningful with an HTTPS target plus the marker header.
    let post_marker = list_unsubscribe_post
        .map(str::trim)
        .is_some_and(|p| p.to_ascii_lowercase().contains("one-click"));
    options.one_click = post_marker
        && options
            .http_url
            .as_deref()
            .is_some_and(|u| starts_with_ci(u, "https://"));

    Some(options)
}

/// Split a `List-Unsubscribe` value into its `<...>` targets. Targets are comma-separated and
/// usually angle-bracketed; this tolerates either by stripping a leading `<` / trailing `>`.
fn split_targets(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|t| t.trim().trim_start_matches('<').trim_end_matches('>').trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Case-insensitively strip a URI scheme prefix, returning the remainder.
fn strip_scheme<'a>(target: &'a str, scheme: &str) -> Option<&'a str> {
    starts_with_ci(target, scheme).then(|| &target[scheme.len()..])
}

fn starts_with_ci(haystack: &str, prefix: &str) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Parse the `addr?subject=…` part of a `mailto:` URI into a target + optional subject.
fn parse_mailto(rest: &str) -> MailtoUnsubscribe {
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    let subject = query
        .split('&')
        .find_map(|kv| kv.split_once('='))
        .filter(|(k, _)| k.eq_ignore_ascii_case("subject"))
        .map(|(_, v)| percent_decode_minimal(v));
    MailtoUnsubscribe {
        to: addr.trim().to_owned(),
        subject,
    }
}

/// A minimal percent/`+` decode for the common `subject=` cases (`%20`, `+`). Not a full URL
/// decoder — just enough to render a readable subject; unknown escapes pass through.
fn percent_decode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_header_means_no_affordance() {
        assert!(parse_unsubscribe(None, None).is_none());
        assert!(parse_unsubscribe(Some("   "), None).is_none());
        // A header with neither a mailto nor an http target yields nothing actionable.
        assert!(parse_unsubscribe(Some("<ftp://x.test/u>"), None).is_none());
    }

    #[test]
    fn a_mailto_target_is_parsed_with_its_subject() {
        let opts = parse_unsubscribe(
            Some("<mailto:unsub@list.test?subject=unsubscribe>"),
            None,
        )
        .unwrap();
        let mailto = opts.mailto.unwrap();
        assert_eq!(mailto.to, "unsub@list.test");
        assert_eq!(mailto.subject.as_deref(), Some("unsubscribe"));
        assert!(!opts.one_click, "no http + post marker ⇒ not one-click");
    }

    #[test]
    fn both_targets_are_kept_and_one_click_needs_the_post_marker_and_https() {
        let header = "<mailto:unsub@list.test>, <https://list.test/u?id=42>";
        // Without the marker header, an https target is present but not one-click.
        let plain = parse_unsubscribe(Some(header), None).unwrap();
        assert_eq!(plain.http_url.as_deref(), Some("https://list.test/u?id=42"));
        assert!(plain.mailto.is_some());
        assert!(!plain.one_click);

        // With the RFC 8058 marker on an https target, it is one-click.
        let oneclick =
            parse_unsubscribe(Some(header), Some("List-Unsubscribe=One-Click")).unwrap();
        assert!(oneclick.one_click);
    }

    #[test]
    fn one_click_requires_https_not_plain_http() {
        let opts = parse_unsubscribe(
            Some("<http://list.test/u>"),
            Some("List-Unsubscribe=One-Click"),
        )
        .unwrap();
        // The marker is present, but the target is plain http — never a silent one-click.
        assert_eq!(opts.http_url.as_deref(), Some("http://list.test/u"));
        assert!(!opts.one_click);
    }

    #[test]
    fn subject_percent_and_plus_escapes_decode() {
        let opts = parse_unsubscribe(
            Some("<mailto:u@l.test?subject=please%20remove+me>"),
            None,
        )
        .unwrap();
        assert_eq!(opts.mailto.unwrap().subject.as_deref(), Some("please remove me"));
    }

    #[test]
    fn it_round_trips_on_the_wire() {
        let opts = parse_unsubscribe(
            Some("<mailto:u@l.test>, <https://l.test/u>"),
            Some("List-Unsubscribe=One-Click"),
        )
        .unwrap();
        let json = serde_json::to_string(&opts).unwrap();
        let back: UnsubscribeOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, opts);
    }
}
