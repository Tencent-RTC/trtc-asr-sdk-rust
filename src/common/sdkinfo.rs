//! SDK self-identification carried on every request.
//!
//! Every request (WebSocket handshake and HTTP API calls) reports which SDK
//! language, version and OS platform produced it. Without this, a customer
//! issue can only be traced to an AppID — not to the concrete client build
//! that triggered it, which is what makes cross-version regressions
//! diagnosable.
//!
//! The values travel as URL query parameters rather than headers because a
//! browser-originated WebSocket handshake cannot set custom headers, and the
//! three transports must report identically.

use std::collections::BTreeMap;

use crate::common::signature::query_escape;

/// Released version of this SDK. Sourced from Cargo.toml so the crate
/// manifest stays the single place a release has to be bumped.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Identifies the SDK implementation language.
pub const SDK_LANGUAGE: &str = "rust";

/// Distinguishes this family of SDKs from the client-side ones. All six
/// language bindings here run server-side, so the value is constant; it exists
/// so server-side telemetry can bucket traffic the same way it does for the
/// mobile/desktop client SDKs.
pub const SDK_TYPE: &str = "server";

/// Reports the OS platform the SDK is running on, normalized to the
/// vocabulary the service expects: windows, linux, mac, android, ios. Any
/// other target OS is reported verbatim so a new platform shows up in
/// telemetry instead of being silently misattributed.
pub fn sdk_platform() -> &'static str {
    // std::env::consts::OS over cfg!: one expression covers every target,
    // and the constant is resolved at compile time anyway.
    match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "windows",
        "linux" => "linux",
        "android" => "android",
        "ios" => "ios",
        other => other,
    }
}

/// Returns the SDK identification parameters shared by every transport.
pub fn sdk_report_params() -> BTreeMap<&'static str, &'static str> {
    let mut m = BTreeMap::new();
    m.insert("platform", sdk_platform());
    m.insert("sdk_lang", SDK_LANGUAGE);
    m.insert("sdk_type", SDK_TYPE);
    m.insert("version", SDK_VERSION);
    m
}

/// Returns the SDK identification parameters as an encoded query fragment
/// (no leading `&`), for the transports that build their URL by string
/// concatenation.
pub fn sdk_report_query() -> String {
    // BTreeMap iterates keys in sorted order, matching the Go SDK's output.
    sdk_report_params()
        .iter()
        .map(|(k, v)| format!("{k}={}", query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}
