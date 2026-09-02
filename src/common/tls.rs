//! Shared rustls client configuration used by the WebSocket and HTTP
//! transports.
//!
//! Background: SecureTransport (the macOS `native-tls` backend) fails
//! against the production endpoint with "record overflow" on its
//! certificate chain, and ureq's default aws-lc based rustls config fails
//! with "received corrupt message of type InvalidContentType". The ring
//! provider plus the system root CAs works with this server family, so both
//! transports share this config.

use std::sync::Arc;
use std::sync::OnceLock;

/// Returns the shared rustls client config: ring provider, system root CAs,
/// standard certificate + hostname verification.
pub(crate) fn rustls_client_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            let loaded = rustls_native_certs::load_native_certs();
            for cert in loaded.certs {
                let _ = roots.add(cert);
            }
            Arc::new(
                rustls::ClientConfig::builder_with_provider(
                    rustls::crypto::ring::default_provider().into(),
                )
                .with_safe_default_protocol_versions()
                .expect("safe default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
            )
        })
        .clone()
}
