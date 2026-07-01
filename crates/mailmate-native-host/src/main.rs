//! The MailMate native messaging host binary.
//!
//! Usage:
//! - (no args) — run the native-messaging loop on stdin/stdout (how Thunderbird launches it
//!   via `runtime.connectNative`), serving the **fully-wired** router (Phase 12).
//! - `manifest` — print the host manifest JSON for the installer.
//! - `config` — print the effective configuration and the resolved data directory.
//! - `backup <file>` / `restore <file>` — snapshot or restore the embedded database.
//! - `export-rules <file>` / `import-rules <file> [name-prefix]` — portable rule manifests.
//! - `simulate <scenario.json>` — run a what-if scenario through the decision spine.
//! - `bench [iterations]` — micro-benchmark the hot paths.
//!
//! Configuration comes from `MAILMATE_CONFIG` (a TOML file) or the safe defaults; the data
//! directory from `MAILMATE_DATA_DIR` / `$XDG_DATA_HOME` / `$HOME`. `main` is thin wiring over
//! the library modules ([`runtime`](mailmate_native_host::runtime),
//! [`simulation`](mailmate_native_host::simulation),
//! [`benchmark`](mailmate_native_host::benchmark)), which are unit/integration/e2e tested.

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;

use mailmate_native_host::benchmark::run_default_suite;
use mailmate_native_host::manifest::NativeHostManifest;
use mailmate_native_host::runtime::{self, resolve_config, resolve_data_dir};
use mailmate_native_host::simulation::{run_simulation, Scenario};

/// The default per-operation iteration count for `bench`.
const DEFAULT_BENCH_ITERATIONS: u32 = 200;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tail = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        None => serve(),
        Some("manifest") => print_manifest(),
        Some("config") => show_config(),
        Some("backup") => backup(tail),
        Some("restore") => restore(tail),
        Some("export-rules") => export_rules(tail),
        Some("import-rules") => import_rules(tail),
        Some("simulate") => simulate(tail),
        Some("bench") => bench(tail),
        // A browser launching us as a native-messaging host passes the manifest's absolute path
        // (Firefox/Thunderbird, and the xdg-desktop-portal WebExtensions portal) or a `scheme://`
        // extension origin (Chromium) as the first argument — never a subcommand. Serve the
        // stdin/stdout loop, exactly as for the no-args case. Without this the host exits as an
        // "unknown subcommand", which the browser reports as "native host disconnected (no error)".
        Some(arg) if is_browser_launch(arg) => serve(),
        Some(other) => Err(format!(
            "unknown subcommand {other:?} (expected one of: manifest, config, backup, \
             restore, export-rules, import-rules, simulate, bench, or no args to serve)"
        )
        .into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailmate-native-host: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Whether the first CLI argument came from a browser launching us as a native-messaging host
/// rather than a human typing a subcommand. Browsers pass the manifest's absolute path
/// (Firefox/Thunderbird on Linux/macOS, via the WebExtensions portal; on Windows a drive-letter
/// path like `C:\…\com.mailmate.host.json` or a UNC `\\server\…` share) or a `scheme://`
/// extension origin (Chromium). Every subcommand we accept is a bare lowercase word containing no
/// `/`, `\`, `:` or `://`, so none of these forms can be mistaken for one — and a stray subcommand
/// typo still falls through to the helpful error.
fn is_browser_launch(arg: &str) -> bool {
    // Unix absolute path (Linux/macOS) or Chromium's `scheme://` origin.
    arg.starts_with('/')
        || arg.contains("://")
        // Windows UNC path: `\\server\share\…`.
        || arg.starts_with('\\')
        // Windows drive-letter absolute path: a letter, `:`, then a separator (`C:\…` or `C:/…`).
        || is_windows_drive_path(arg)
}

/// Whether `arg` is a Windows drive-letter absolute path (`C:\…` / `C:/…`): an ASCII letter
/// followed by `:` then a path separator. (A bare `c:` with no separator is not treated as one.)
fn is_windows_drive_path(arg: &str) -> bool {
    let mut chars = arg.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('\\' | '/'))
}

/// Resolve config + data dir and run the native-messaging serve loop.
fn serve() -> Result<(), Box<dyn Error>> {
    let config = resolve_config()?;
    let data_dir = resolve_data_dir();
    runtime::serve(config, &data_dir)?;
    Ok(())
}

/// Print the effective configuration as TOML, prefixed with the resolved data directory.
fn show_config() -> Result<(), Box<dyn Error>> {
    let config = resolve_config()?;
    println!("# data directory: {}", resolve_data_dir().display());
    print!("{}", config.to_toml()?);
    Ok(())
}

/// `backup <dest>` — snapshot the configured database.
fn backup(args: &[String]) -> Result<(), Box<dyn Error>> {
    let dest = args.first().ok_or("usage: backup <dest-file>")?;
    runtime::backup(&resolve_config()?, &resolve_data_dir(), Path::new(dest))?;
    println!("backed up the database to {dest}");
    Ok(())
}

/// `restore <src>` — replace the configured database from a snapshot.
fn restore(args: &[String]) -> Result<(), Box<dyn Error>> {
    let src = args.first().ok_or("usage: restore <backup-file>")?;
    runtime::restore(&resolve_config()?, &resolve_data_dir(), Path::new(src))?;
    println!("restored the database from {src}");
    Ok(())
}

/// `export-rules <dest>` — write the operative rule set as a JSON manifest.
fn export_rules(args: &[String]) -> Result<(), Box<dyn Error>> {
    let dest = args.first().ok_or("usage: export-rules <dest-file>")?;
    let count = runtime::export_rules(&resolve_config()?, &resolve_data_dir(), Path::new(dest))?;
    println!("exported {count} rule(s) to {dest}");
    Ok(())
}

/// `import-rules <src> [name-prefix]` — create draft rules from a JSON manifest.
fn import_rules(args: &[String]) -> Result<(), Box<dyn Error>> {
    let src = args
        .first()
        .ok_or("usage: import-rules <src-file> [name-prefix]")?;
    let prefix = args.get(1).map_or("imported", String::as_str);
    let (imported, skipped) = runtime::import_rules(
        &resolve_config()?,
        &resolve_data_dir(),
        Path::new(src),
        prefix,
    )?;
    println!("imported {imported} rule(s) as drafts, skipped {skipped}");
    Ok(())
}

/// `simulate <scenario.json>` — run a what-if scenario and print the JSON report.
fn simulate(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = args.first().ok_or("usage: simulate <scenario-file.json>")?;
    let text = std::fs::read_to_string(path)?;
    let scenario: Scenario = serde_json::from_str(&text)?;
    let report = run_simulation(scenario);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// `bench [iterations]` — micro-benchmark the hot paths and print a one-line-per-op report.
fn bench(args: &[String]) -> Result<(), Box<dyn Error>> {
    let iterations = args
        .first()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_BENCH_ITERATIONS);
    for result in run_default_suite(iterations) {
        println!("{}", result.summary());
    }
    Ok(())
}

/// Print the native-messaging host manifest for the current executable path.
fn print_manifest() -> Result<(), Box<dyn Error>> {
    let path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "mailmate-native-host".to_owned());
    let manifest = NativeHostManifest::new(path, Vec::new());
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_browser_launch;

    #[test]
    fn recognizes_a_browser_native_messaging_launch() {
        // Firefox/Thunderbird (and the WebExtensions portal): the manifest's absolute path.
        assert!(is_browser_launch(
            "/home/n/.mozilla/native-messaging-hosts/com.mailmate.host.json"
        ));
        // Chromium: a `scheme://…` extension origin.
        assert!(is_browser_launch("chrome-extension://abcdefghijklmnop/"));
        // Windows: a drive-letter manifest path (backslash or forward-slash separated)…
        assert!(is_browser_launch(
            r"C:\Users\n\AppData\Roaming\Mozilla\NativeMessagingHosts\com.mailmate.host.json"
        ));
        assert!(is_browser_launch("D:/Mozilla/com.mailmate.host.json"));
        // …and a UNC share path.
        assert!(is_browser_launch(
            r"\\fileserver\hosts\com.mailmate.host.json"
        ));
    }

    #[test]
    fn a_bare_drive_letter_without_a_separator_is_not_a_launch() {
        // Defensive: `c:` alone (no separator) must not be mistaken for a Windows path — though no
        // real subcommand looks like this, the drive-letter rule stays tight.
        assert!(!is_browser_launch("c:"));
        assert!(!is_browser_launch("c:thing"));
    }

    #[test]
    fn leaves_bare_subcommand_words_to_the_subcommand_matcher() {
        // None of these may be mistaken for a browser launch, so real subcommands still dispatch
        // and a typo still reaches the "unknown subcommand" error.
        for word in [
            "manifest",
            "config",
            "backup",
            "restore",
            "export-rules",
            "import-rules",
            "simulate",
            "bench",
            "frobnicate",
            "",
        ] {
            assert!(
                !is_browser_launch(word),
                "{word:?} must stay a subcommand token, not a browser launch"
            );
        }
    }
}
