//! The MailMate native messaging host binary.
//!
//! Usage:
//! - (no args) — run the native-messaging loop on stdin/stdout (how Thunderbird
//!   launches it via `runtime.connectNative`).
//! - `manifest` — print the host manifest JSON for the installer.
//!
//! All real logic lives in the library modules (`native_stdio`, `dispatch`,
//! `manifest`), which are unit/integration/e2e tested; `main` is thin wiring.

use std::io::{stdin, stdout};
use std::process::ExitCode;

use mailmate_native_host::dispatch::run_loop;
use mailmate_native_host::manifest::NativeHostManifest;
use mailmate_native_host::native_stdio::FrameWriter;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => serve(),
        Some("manifest") => print_manifest(),
        Some(other) => {
            eprintln!("mailmate-native-host: unknown subcommand {other:?} (expected `manifest` or no args)");
            ExitCode::FAILURE
        }
    }
}

/// Run the native-messaging loop until Thunderbird closes the channel.
fn serve() -> ExitCode {
    let writer = FrameWriter::new(stdout().lock());
    let mut reader = stdin().lock();
    match run_loop(&mut reader, &writer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailmate-native-host: loop ended with error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Print the native-messaging host manifest for the current executable path.
fn print_manifest() -> ExitCode {
    let path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "mailmate-native-host".to_owned());
    let manifest = NativeHostManifest::new(path, Vec::new());
    match serde_json::to_string_pretty(&manifest) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("mailmate-native-host: failed to render manifest: {e}");
            ExitCode::FAILURE
        }
    }
}
