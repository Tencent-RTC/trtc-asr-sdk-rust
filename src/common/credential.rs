//! Credential management.

/// Authentication information for the TRTC-ASR service.
///
/// Three values are needed:
/// - `app_id`: Tencent Cloud account APPID, from <https://console.cloud.tencent.com/cam/capi>
/// - `sdk_app_id`: TRTC application ID, from <https://console.cloud.tencent.com/trtc/app>
/// - `secret_key`: TRTC SDK secret key, from TRTC console > Application Overview > SDK Key
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
        }
    }

    /// Sets a pre-computed UserSig on the credential. When empty the SDK
    /// auto-generates one using `sdk_app_id` and `secret_key`.
    pub fn set_user_sig(&mut self, user_sig: impl Into<String>) {
        self.user_sig = user_sig.into();
    }

    /// Returns the APPID as a string.
    pub fn app_id_str(&self) -> String {
        self.app_id.to_string()
    }
}
