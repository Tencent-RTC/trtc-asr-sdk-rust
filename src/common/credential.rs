//! Credential management.

use super::errors::{invalid_param, Result};

/// China (domestic) ASR cluster. Empty site is treated the same.
pub const SITE_CN: &str = "cn";
/// International ASR cluster (`asr-intl.cloud-rtc.com`).
pub const SITE_INTL: &str = "intl";

pub const HOST_CN: &str = "asr.cloud-rtc.com";
pub const HOST_INTL: &str = "asr-intl.cloud-rtc.com";

/// Authentication information for the TRTC-ASR service.
///
/// Three values are needed:
/// - `app_id`: Tencent Cloud account APPID, from <https://console.cloud.tencent.com/cam/capi>
/// - `sdk_app_id`: TRTC application ID, from <https://console.cloud.tencent.com/trtc/app>
/// - `secret_key`: TRTC SDK secret key, from TRTC console > Application Overview > SDK Key
///
/// Call [`Credential::set_site`] with [`SITE_INTL`] to use the international
/// cluster. The default is the China site. Because the credential is moved
/// into the recognizer, set the site before constructing the recognizer.
#[derive(Clone, Default)]
pub struct Credential {
    /// Tencent Cloud account APPID. Used in the WebSocket URL path:
    /// `wss://asr.cloud-rtc.com/asr/v2/<appid>`.
    pub app_id: i64,

    /// TRTC application ID.
    pub sdk_app_id: i64,

    /// TRTC SDK secret key. Used to generate UserSig. Never transmitted over
    /// the network.
    pub secret_key: String,

    /// Pre-computed TRTC authentication signature. Auto-generated from
    /// `sdk_app_id` + `secret_key` when left empty.
    pub user_sig: String,

    /// ASR cluster. Empty or [`SITE_CN`] is China ([`HOST_CN`]);
    /// [`SITE_INTL`] is international ([`HOST_INTL`]).
    pub site: String,
}

// Custom Debug redacts the secret material so accidental {:?} logging cannot
// leak credentials into logs.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("app_id", &self.app_id)
            .field("sdk_app_id", &self.sdk_app_id)
            .field("secret_key", &"<redacted>")
            .field(
                "user_sig",
                &if self.user_sig.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("site", &self.site)
            .finish()
    }
}

impl Credential {
    pub fn new(app_id: i64, sdk_app_id: i64, secret_key: impl Into<String>) -> Self {
        Credential {
            app_id,
            sdk_app_id,
            secret_key: secret_key.into(),
            user_sig: String::new(),
            site: String::new(),
        }
    }

    /// Sets a pre-computed UserSig on the credential. When empty the SDK
    /// auto-generates one using `sdk_app_id` and `secret_key`.
    pub fn set_user_sig(&mut self, user_sig: impl Into<String>) {
        self.user_sig = user_sig.into();
    }

    /// Selects the ASR cluster: [`SITE_CN`] (default) or [`SITE_INTL`].
    /// Must be called before the credential is moved into a recognizer.
    pub fn set_site(&mut self, site: impl Into<String>) {
        self.site = site.into();
    }

    /// Returns the APPID as a string.
    pub fn app_id_str(&self) -> String {
        self.app_id.to_string()
    }
}

/// Returns the ASR hostname for site. Empty / cn is domestic; intl is international.
pub fn host_for_site(site: &str) -> Result<String> {
    match site.trim().to_ascii_lowercase().as_str() {
        "" | SITE_CN => Ok(HOST_CN.to_string()),
        SITE_INTL => Ok(HOST_INTL.to_string()),
        _ => Err(invalid_param(format!(
            "unsupported site {site:?}, want {SITE_CN:?} or {SITE_INTL:?}"
        ))),
    }
}

pub fn ws_endpoint_for_site(site: &str) -> Result<String> {
    Ok(format!("wss://{}", host_for_site(site)?))
}

pub fn http_endpoint_for_site(site: &str) -> Result<String> {
    Ok(format!("https://{}", host_for_site(site)?))
}

/// Returns override when non-empty, otherwise the site-derived realtime origin.
pub fn resolve_ws_endpoint(override_ep: &str, site: &str) -> Result<String> {
    if !override_ep.is_empty() {
        return Ok(override_ep.to_string());
    }
    ws_endpoint_for_site(site)
}

/// Returns override when non-empty, otherwise the site-derived HTTPS origin.
pub fn resolve_http_endpoint(override_ep: &str, site: &str) -> Result<String> {
    if !override_ep.is_empty() {
        return Ok(override_ep.to_string());
    }
    http_endpoint_for_site(site)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::errors::ERR_CODE_INVALID_PARAM;

    #[test]
    fn host_for_site_maps_known_values() {
        assert_eq!(host_for_site("").unwrap(), HOST_CN);
        assert_eq!(host_for_site(SITE_CN).unwrap(), HOST_CN);
        assert_eq!(host_for_site("CN").unwrap(), HOST_CN);
        assert_eq!(host_for_site(" cn ").unwrap(), HOST_CN);
        assert_eq!(host_for_site(SITE_INTL).unwrap(), HOST_INTL);
        assert_eq!(host_for_site("INTL").unwrap(), HOST_INTL);
        let err = host_for_site("mars").unwrap_err();
        assert_eq!(err.code, ERR_CODE_INVALID_PARAM);
    }

    #[test]
    fn resolve_helpers_honor_override_and_site() {
        assert_eq!(
            resolve_ws_endpoint("", SITE_INTL).unwrap(),
            format!("wss://{HOST_INTL}")
        );
        assert_eq!(
            resolve_http_endpoint("", "").unwrap(),
            format!("https://{HOST_CN}")
        );
        assert_eq!(
            resolve_ws_endpoint("wss://mock.local", SITE_INTL).unwrap(),
            "wss://mock.local"
        );
    }

    #[test]
    fn set_site_stores_cluster() {
        let mut c = Credential::new(1, 2, "k");
        assert!(c.site.is_empty());
        c.set_site(SITE_INTL);
        assert_eq!(c.site, SITE_INTL);
    }
}
