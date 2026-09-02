//! URL query parameter construction for the ASR WebSocket request.
//!
//! The `secretid` URL parameter is required by the protocol but internally
//! populated with the APPID — users do not provide a separate SecretID. The
//! `signature` parameter is set to the UserSig value per protocol spec, and
//! the same value is also sent as `usersig` so the gateway can authenticate
//! clients (e.g. browsers) that cannot attach custom WebSocket headers.

use serde::Serialize;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Speaker diarization modes for the `speaker_diarization` parameter.
pub const SPEAKER_DIARIZATION_OFF: i32 = 0;
/// Anonymous clustering: speakers numbered from 1 within the session, -1 unknown.
pub const SPEAKER_DIARIZATION_CLUSTER: i32 = 1;
/// Voiceprint-based role authentication; combine with `SpeakerRole`s and/or
/// voiceprint IDs so recognized speakers carry their role name.
pub const SPEAKER_DIARIZATION_VOICEPRINT: i32 = 3;

/// A temporary voiceprint enrollment entry used with `speaker_diarization=3`.
/// `role_name` is echoed back by the server as `speaker_name` on the matched
/// words / speaker segments.
///
/// JSON field names intentionally match the server-side contract (CamelCase).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SpeakerRole {
    #[serde(rename = "RoleName")]
    pub role_name: String,
    #[serde(rename = "AudioUrl")]
    pub audio_url: String,
}

/// URL query parameters for the ASR WebSocket request.
#[derive(Debug, Clone)]
pub struct SignatureParams {
    pub app_id: i64,
    pub timestamp: i64,
    pub expired: i64,
    pub nonce: u32,
    pub engine_model_type: String,
    pub voice_id: String,
    pub voice_format: i32,
    pub need_vad: i32,

    /// TRTC application ID, sent as the `sdkappid` query parameter.
    /// 0 means not configured.
    pub sdk_app_id: i64,

    // Optional parameters
    pub hotword_id: String,
    /// Temporary inline hotwords: `word|weight,word|weight`.
    pub hotword_list: String,
    pub customization_id: String,
    /// Replacement word table ID.
    pub replace_text_id: String,
    pub filter_dirty: i32,
    pub filter_modal: i32,
    pub filter_punc: i32,
    pub convert_num_mode: i32,
    pub word_info: i32,
    pub vad_silence_time: i32,
    pub max_speak_time: i32,
    /// 8000: feed 8kHz PCM to a 16k engine (upsampled server-side).
    pub input_sample_rate: i32,
    /// Bigmodel engine language hint (e.g. "zh", "en", "auto").
    pub language: String,

    /// 0 = deliver empty results, 1 = skip them (server default). `None`
    /// leaves the parameter out.
    pub filter_empty_result: Option<i32>,

    /// VAD profile: 0 = high recall, 1 = far-field filtering (server default).
    /// `None` leaves the parameter out, so an explicit 0 is distinguishable
    /// from "not configured".
    pub vad_level: Option<i32>,

    /// Fine-tunes VAD noise suppression, range [0, 4]. Overrides the profile
    /// selected by `vad_level` when set. `None` leaves the parameter out
    /// (0 is a valid, meaningful value).
    pub noise_threshold: Option<f64>,

    /// 0 = off (default), 1 = anonymous clustering, 3 = voiceprint role
    /// authentication.
    pub speaker_diarization: i32,

    /// Expected speaker count hint; 0 = auto detection (default). Sent
    /// whenever diarization is enabled.
    pub speaker_number: i32,

    /// Temporary voiceprint enrollment audio, serialized into the
    /// `speaker_roles` JSON array. Only sent when diarization is 3.
    pub speaker_roles: Vec<SpeakerRole>,

    /// Pre-registered voiceprint IDs, serialized into the `voiceprintids`
    /// JSON array. Only sent when diarization is 3.
    pub voiceprint_ids: Vec<String>,
}

impl SignatureParams {
    /// Creates parameters with sensible defaults.
    pub fn new(app_id: i64, engine_model_type: impl Into<String>, voice_id: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        SignatureParams {
            app_id,
            timestamp: now,
            expired: now + 86400,
            nonce: rand::random::<u32>() % 9_999_999 + 1,
            engine_model_type: engine_model_type.into(),
            voice_id: voice_id.into(),
            voice_format: 1, // pcm
            need_vad: 1,
            sdk_app_id: 0,
            hotword_id: String::new(),
            hotword_list: String::new(),
            customization_id: String::new(),
            replace_text_id: String::new(),
            filter_dirty: 0,
            filter_modal: 0,
            filter_punc: 0,
            convert_num_mode: 1,
            word_info: 0,
            vad_silence_time: 0,
            max_speak_time: 0,
            input_sample_rate: 0,
            language: String::new(),
            filter_empty_result: None,
            vad_level: None,
            noise_threshold: None,
            speaker_diarization: 0,
            speaker_number: 0,
            speaker_roles: Vec::new(),
            voiceprint_ids: Vec::new(),
        }
    }

    /// Builds the URL query string with all parameters (without signature).
    pub fn build_query_string(&self) -> String {
        encode_params(&self.to_map())
    }

    /// Builds the URL query string with `signature` and `usersig` set to the
    /// given UserSig value (per protocol both carry the UserSig).
    pub fn build_query_string_with_signature(&self, user_sig: &str) -> String {
        let mut params = self.to_map();
        params.insert("signature".to_string(), user_sig.to_string());
        params.insert("usersig".to_string(), user_sig.to_string());
        encode_params(&params)
    }

    fn to_map(&self) -> BTreeMap<String, String> {
        // "secretid" is required by protocol; internally use AppID as its value.
        let mut m = BTreeMap::new();
        m.insert("secretid".to_string(), self.app_id.to_string());
        m.insert("timestamp".to_string(), self.timestamp.to_string());
        m.insert("expired".to_string(), self.expired.to_string());
        m.insert("nonce".to_string(), self.nonce.to_string());
        m.insert("engine_model_type".to_string(), self.engine_model_type.clone());
        m.insert("voice_id".to_string(), self.voice_id.clone());
        m.insert("voice_format".to_string(), self.voice_format.to_string());
        m.insert("needvad".to_string(), self.need_vad.to_string());

        if self.sdk_app_id > 0 {
            m.insert("sdkappid".to_string(), self.sdk_app_id.to_string());
        }
        if !self.hotword_id.is_empty() {
            m.insert("hotword_id".to_string(), self.hotword_id.clone());
        }
        if !self.hotword_list.is_empty() {
            m.insert("hotword_list".to_string(), self.hotword_list.clone());
        }
        if !self.customization_id.is_empty() {
            m.insert("customization_id".to_string(), self.customization_id.clone());
        }
        if !self.replace_text_id.is_empty() {
            m.insert("replace_text_id".to_string(), self.replace_text_id.clone());
        }
        if self.filter_dirty != 0 {
            m.insert("filter_dirty".to_string(), self.filter_dirty.to_string());
        }
        if self.filter_modal != 0 {
            m.insert("filter_modal".to_string(), self.filter_modal.to_string());
        }
        if self.filter_punc != 0 {
            m.insert("filter_punc".to_string(), self.filter_punc.to_string());
        }
        if let Some(v) = self.filter_empty_result {
            m.insert("filter_empty_result".to_string(), v.to_string());
        }
        if self.convert_num_mode != 0 {
            m.insert("convert_num_mode".to_string(), self.convert_num_mode.to_string());
        }
        if self.word_info != 0 {
            m.insert("word_info".to_string(), self.word_info.to_string());
        }
        if self.vad_silence_time != 0 {
            m.insert("vad_silence_time".to_string(), self.vad_silence_time.to_string());
        }
        if self.max_speak_time != 0 {
            m.insert("max_speak_time".to_string(), self.max_speak_time.to_string());
        }
        if self.input_sample_rate != 0 {
            m.insert("input_sample_rate".to_string(), self.input_sample_rate.to_string());
        }
        // vad_level / noise_threshold are tri-state: an explicit 0 differs
        // from "not configured" (the server defaults vad_level to 1), so they
        // are only emitted when the caller set them.
        if let Some(v) = self.vad_level {
            m.insert("vad_level".to_string(), v.to_string());
        }
        if let Some(v) = self.noise_threshold {
            // Matches Go strconv.FormatFloat(v, 'f', 3, 64): "0.000", "1.500".
            m.insert("noise_threshold".to_string(), format!("{v:.3}"));
        }
        if self.speaker_diarization != 0 {
            m.insert(
                "speaker_diarization".to_string(),
                self.speaker_diarization.to_string(),
            );
            if self.speaker_number != 0 {
                m.insert("speaker_number".to_string(), self.speaker_number.to_string());
            }
        }
        // speaker_roles / voiceprintids only apply to voiceprint mode.
        if self.speaker_diarization == SPEAKER_DIARIZATION_VOICEPRINT {
            if !self.speaker_roles.is_empty() {
                if let Ok(json) = serde_json::to_string(&self.speaker_roles) {
                    m.insert("speaker_roles".to_string(), json);
                }
            }
            if !self.voiceprint_ids.is_empty() {
                if let Ok(json) = serde_json::to_string(&self.voiceprint_ids) {
                    m.insert("voiceprintids".to_string(), json);
                }
            }
        }
        if !self.language.is_empty() {
            m.insert("language".to_string(), self.language.clone());
        }
        m
    }
}

fn encode_params(params: &BTreeMap<String, String>) -> String {
    // BTreeMap iterates keys in sorted order, matching Go's sort.Strings.
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encodes a query value with Go `url.QueryEscape` semantics:
/// unreserved `[A-Za-z0-9-_.~]` stay as-is, space becomes `+`, everything
/// else becomes `%XX` (uppercase hex, per UTF-8 byte).
pub fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
