//! MailMate native messaging host — the stdio edge adapter.
//!
//! Thunderbird talks to the host over [native messaging]: length-prefixed JSON frames
//! on stdin/stdout. This crate provides the production mechanics behind the
//! `Transport` port:
//!
//! - [`native_stdio`] — the frame codec (length-prefixed, native-byte-order) and a
//!   single-writer guard so responses and host-initiated notifications never interleave.
//! - [`dispatch`] — the protocol-level frame builders (`ok_response`/`error_response`/`emit`)
//!   and the Phase-1 ping/`run_loop` driver (oversize-frame guard, malformed-frame handling).
//! - [`manifest`] — the per-OS native-messaging host manifest the installer writes.
//!
//! Phase 10 adds the application layer that turns the host into a real Thunderbird adapter:
//! - [`protocol_dto`] — the inbound wire payloads and their lowering into domain types.
//! - [`convert`] — the outbound projection of domain values onto wire payloads.
//! - [`router`] — [`HostRouter`](router::HostRouter), which routes frames into the core's
//!   use-cases (classify / draft / record) and applies safe actions through the mail client.
//! - [`thunderbird`] — [`ThunderbirdMailClient`](thunderbird::ThunderbirdMailClient), the
//!   `MailClient` adapter over native messaging, and `WriterTransport`, the stdio `Transport`.
//!
//! The router names only ports and core use-cases; the concrete engines/providers/storage are
//! injected by the composition root (deferred to Phase 12 hardening), and the router is tested
//! here against the in-memory fakes. The envelope types live in `mailmate-common` (so the host
//! and the core share one definition); this crate owns the framing, the loop, and the wire
//! shape.
//!
//! [native messaging]: https://developer.mozilla.org/docs/Mozilla/Add-ons/WebExtensions/Native_messaging

pub mod convert;
pub mod dispatch;
pub mod manifest;
pub mod native_stdio;
pub mod protocol_dto;
pub mod router;
pub mod thunderbird;
