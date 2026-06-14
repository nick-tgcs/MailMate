//! MailMate native messaging host — the stdio edge adapter.
//!
//! Thunderbird talks to the host over [native messaging]: length-prefixed JSON frames
//! on stdin/stdout. This crate provides the production mechanics behind the
//! `Transport` port:
//!
//! - [`native_stdio`] — the frame codec (length-prefixed, native-byte-order) and a
//!   single-writer guard so responses and host-initiated notifications never interleave.
//! - [`dispatch`] — turns an inbound request [`Frame`](mailmate_common::protocol::Frame)
//!   into a response frame, and the `run_loop`/`emit` host driver (including the
//!   oversize-frame guard and malformed-frame handling).
//! - [`manifest`] — the per-OS native-messaging host manifest the installer writes.
//!
//! The envelope types themselves live in `mailmate-common` (so both the host and the
//! core share one definition); this crate owns only the framing and the host loop.
//!
//! [native messaging]: https://developer.mozilla.org/docs/Mozilla/Add-ons/WebExtensions/Native_messaging

pub mod dispatch;
pub mod manifest;
pub mod native_stdio;
