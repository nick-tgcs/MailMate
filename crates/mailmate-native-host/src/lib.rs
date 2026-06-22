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
//! Phase 12 lands the **composition root** the earlier phases deferred: the host now wires the
//! real engines/providers/storage into a live [`HostRouter`] and serves it.
//! - [`config`] — the `[storage]`/`[retention]`/`[followups]`/`[ai]` TOML configuration.
//! - [`clock`] / [`secret_store`] — the production `Clock` and `SecretStore` leaf adapters.
//! - [`runtime`] — `build_app` (assemble the full `Ports` + follow-up suite over SQLite) and
//!   `serve` (catch-up drain → the stdio loop), plus the maintenance operations
//!   (backup/restore, rule import/export).
//! - [`simulation`] — the deterministic what-if runner over the rule/cascade/policy spine.
//! - [`benchmark`] — a dependency-free timing harness for the hot paths.
//!
//! The envelope types live in `mailmate-common` (so the host and the core share one
//! definition); this crate owns the framing, the loop, the wire shape, and the wiring.
//!
//! [native messaging]: https://developer.mozilla.org/docs/Mozilla/Add-ons/WebExtensions/Native_messaging

pub mod benchmark;
pub mod bootstrap;
pub mod clock;
pub mod config;
pub mod convert;
pub mod dispatch;
pub mod http_client;
pub mod logging;
pub mod manifest;
pub mod native_stdio;
pub mod protocol_dto;
pub mod provider;
pub mod router;
pub mod runtime;
pub mod secret_store;
pub mod simulation;
pub mod thunderbird;
pub mod tier2_training;
