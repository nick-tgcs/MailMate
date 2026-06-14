//! The native-messaging host manifest.
//!
//! Thunderbird (Gecko) discovers the host through a small JSON manifest registered
//! per-OS (a file under a well-known directory on Linux/macOS, a registry key on
//! Windows). The manifest names the host, the executable path, the `stdio` transport,
//! and the extension ids permitted to connect. The installer (`install` subcommand,
//! later phase) writes this; here we own the type and its serialization.

use serde::{Deserialize, Serialize};

/// The reverse-DNS native-messaging host name.
///
/// The extension passes this exact string to `runtime.connectNative(...)`, and the
/// per-OS manifest is registered under this name.
pub const HOST_NAME: &str = "com.mailmate.host";

/// A Gecko native-messaging host manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeHostManifest {
    /// Must equal [`HOST_NAME`].
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Absolute path to the host executable.
    pub path: String,
    /// Transport — always `"stdio"` for MailMate. Serialized as `type`.
    #[serde(rename = "type")]
    pub transport: String,
    /// Extension ids permitted to connect (Gecko uses `allowed_extensions`).
    pub allowed_extensions: Vec<String>,
}

impl NativeHostManifest {
    /// Build a manifest for the host at `path`, permitting `allowed_extensions`.
    #[must_use]
    pub fn new(path: impl Into<String>, allowed_extensions: Vec<String>) -> Self {
        Self {
            name: HOST_NAME.to_owned(),
            description: "MailMate native messaging host".to_owned(),
            path: path.into(),
            transport: "stdio".to_owned(),
            allowed_extensions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_stdio_type_and_host_name() {
        let manifest = NativeHostManifest::new(
            "/opt/mailmate/mailmate-native-host",
            vec!["mailmate@example.com".to_owned()],
        );
        let value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(value["name"], HOST_NAME);
        assert_eq!(value["type"], "stdio");
        assert_eq!(value["path"], "/opt/mailmate/mailmate-native-host");
        assert_eq!(value["allowed_extensions"][0], "mailmate@example.com");
        // `transport` is renamed to `type` on the wire, not duplicated.
        assert!(value.get("transport").is_none());
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = NativeHostManifest::new("/usr/bin/mailmate-native-host", Vec::new());
        let json = serde_json::to_string(&manifest).unwrap();
        let back: NativeHostManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest);
    }
}
