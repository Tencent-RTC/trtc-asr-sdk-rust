//! One-shot sentence recognition client (HTTP).
//!
//! Usage:
//!
//! ```no_run
//! # use trtc_asr_sdk::common::Credential;
//! # use trtc_asr_sdk::asr::SentenceRecognizer;
//! let credential = Credential::new(0, 0, "your-sdk-secret-key");
//! let recognizer = SentenceRecognizer::new(credential);
//! // let result = recognizer.recognize_data(&pcm_bytes, "pcm", "16k_zh_en").unwrap();
//! ```

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::common::errors::{
    invalid_param, AsrError, Result, ERR_CODE_AUTH_FAILED, ERR_CODE_CONNECT_FAILED,
    ERR_CODE_READ_FAILED, ERR_CODE_SERVER_ERROR,
};
use crate::common::{sdkinfo, usersig, Credential};

/// Production HTTPS endpoint for sentence recognition.
pub const SENTENCE_ENDPOINT: &str = "https://asr.cloud-rtc.com";

/// Audio from a URL.
pub const SOURCE_TYPE_URL: i32 = 0;
/// Audio data in the request body (base64 encoded).
pub const SOURCE_TYPE_DATA: i32 = 1;

/// Max audio size for one-shot recognition (before base64 encoding).
pub const MAX_AUDIO_SIZE: usize = 3 * 1024 * 1024;

/// JSON request body for sentence recognition.
///
/// Field names intentionally follow the server-side contract (including the
/// quirky `EngSerViceType` capitalization).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SentenceRecognitionRequest {
    /// Engine model type. Required. E.g. "16k_zh" (Chinese), "16k_zh_en".
    #[serde(rename = "EngSerViceType")]
    pub eng_service_type: String,

    /// Audio source: 0 = URL, 1 = base64 data in body.
    #[serde(rename = "SourceType")]
    pub source_type: i32,

    /// Audio format: "wav", "pcm", "ogg-opus", "mp3", "m4a".
    #[serde(rename = "VoiceFormat")]
    pub voice_format: String,

    /// Audio file URL (required when source_type = 0). ≤ 60s, ≤ 3MB.
    #[serde(rename = "Url", skip_serializing_if = "String::is_empty")]
    pub url: String,

    /// Base64-encoded audio data (required when source_type = 1). ≤ 60s, ≤ 3MB.
    #[serde(rename = "Data", skip_serializing_if = "String::is_empty")]
    pub data: String,

    /// Audio data length in bytes (required when source_type = 1).
    #[serde(rename = "DataLen", skip_serializing_if = "is_zero_i64")]
    pub data_len: i64,

    /// Word-level timing: 0 hide (default), 1 show, 2 show with punctuation.
    #[serde(rename = "WordInfo", skip_serializing_if = "is_zero")]
    pub word_info: i32,

    /// Profanity filter: 0 off (default), 1 filter, 2 replace with *.
    #[serde(rename = "FilterDirty", skip_serializing_if = "is_zero")]
    pub filter_dirty: i32,

    /// Modal particle filter: 0 off (default), 1 partial, 2 strict.
    #[serde(rename = "FilterModal", skip_serializing_if = "is_zero")]
    pub filter_modal: i32,

    /// Punctuation filter: 0 off (default), 2 filter all punctuation.
    #[serde(rename = "FilterPunc", skip_serializing_if = "is_zero")]
    pub filter_punc: i32,

    /// Arabic numeral conversion: 0 off, 1 smart (default).
    #[serde(rename = "ConvertNumMode", skip_serializing_if = "is_zero")]
    pub convert_num_mode: i32,

    /// Hotword vocabulary ID from the console.
    #[serde(rename = "HotwordId", skip_serializing_if = "String::is_empty")]
    pub hotword_id: String,

    /// Temporary inline hotword list: `word1|weight1,word2|weight2`.
    #[serde(rename = "HotwordList", skip_serializing_if = "String::is_empty")]
    pub hotword_list: String,

    /// Custom language model ID.
    #[serde(rename = "CustomizationId", skip_serializing_if = "String::is_empty")]
    pub customization_id: String,

    /// PCM input sample rate override. Only 8000 is supported.
    #[serde(rename = "InputSampleRate", skip_serializing_if = "is_zero")]
    pub input_sample_rate: i32,

    /// Forces the audio language on engines that support it. Empty = auto.
    #[serde(rename = "Language", skip_serializing_if = "String::is_empty")]
    pub language: String,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Word-level timing information for sentence recognition.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SentenceWord {
    #[serde(rename = "Word", default)]
    pub word: String,
    #[serde(rename = "StartTime", default)]
    pub start_time: i64,
    #[serde(rename = "EndTime", default)]
    pub end_time: i64,
}

/// Successful sentence recognition result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SentenceRecognitionResult {
    /// Recognition text.
    #[serde(rename = "Result", default)]
    pub result: String,

    /// Audio duration in milliseconds.
    #[serde(rename = "AudioDuration", default)]
    pub audio_duration: i64,

    /// Word count (0 when word info is not enabled).
    #[serde(rename = "WordSize", default)]
    pub word_size: i32,

    /// Word-level timing details (empty when word info is not enabled).
    #[serde(rename = "WordList", default)]
    pub word_list: Vec<SentenceWord>,

    /// Unique request identifier.
    #[serde(rename = "RequestId", default)]
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
struct SentenceApiResponse {
    #[serde(rename = "Response")]
    response: Option<SentenceResponseBody>,
}

#[derive(Debug, Deserialize)]
struct SentenceResponseBody {
    #[serde(rename = "Error")]
    error: Option<ApiError>,
    #[serde(rename = "RequestId", default)]
    request_id: String,
    #[serde(rename = "Result", default)]
    result: String,
    #[serde(rename = "AudioDuration", default)]
    audio_duration: i64,
    #[serde(rename = "WordSize", default)]
    word_size: i32,
    #[serde(rename = "WordList", default)]
    word_list: Vec<SentenceWord>,
}

impl SentenceResponseBody {
    fn into_result(self) -> SentenceRecognitionResult {
        SentenceRecognitionResult {
            result: self.result,
            audio_duration: self.audio_duration,
            word_size: self.word_size,
            word_list: self.word_list,
            request_id: self.request_id,
        }
    }
}

/// Server-side API error object shared by the HTTP recognizers.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiError {
    #[serde(rename = "Code")]
    pub code: String,
    #[serde(rename = "Message")]
    pub message: String,
}

/// Client for one-shot sentence recognition.
pub struct SentenceRecognizer {
    credential: Credential,
    endpoint: String,
    agent: ureq::Agent,
}

impl SentenceRecognizer {
    pub fn new(credential: Credential) -> Self {
        SentenceRecognizer {
            credential,
            endpoint: SENTENCE_ENDPOINT.to_string(),
            agent: ureq::AgentBuilder::new()
                // Share the WebSocket transport's rustls config (ring +
                // system roots): ureq's default aws-lc config is rejected by
                // this server family with "received corrupt message of type
                // InvalidContentType".
                .tls_config(crate::common::tls::rustls_client_config())
                .timeout(Duration::from_secs(30))
                .build(),
        }
    }

    /// Overrides the default API endpoint (for testing).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Overrides the HTTP timeout (default 30s).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.agent = ureq::AgentBuilder::new()
            .tls_config(crate::common::tls::rustls_client_config())
            .timeout(timeout)
            .build();
    }

    /// Sends a sentence recognition request and returns the result.
    pub fn recognize(
        &self,
        req: &SentenceRecognitionRequest,
    ) -> Result<SentenceRecognitionResult> {
        validate_request(req)?;

        let request_id = Uuid::new_v4().to_string();

        // Generate UserSig using RequestId as the userID per protocol spec.
        let user_sig = if self.credential.user_sig.is_empty() {
            usersig::gen_user_sig(
                self.credential.sdk_app_id as u64,
                &self.credential.secret_key,
                &request_id,
                86400,
            )
            .map_err(|e| {
                AsrError::new(ERR_CODE_AUTH_FAILED, format!("generate user sig failed: {e}"))
            })?
        } else {
            self.credential.user_sig.clone()
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let req_url = format!(
            "{}/v1/SentenceRecognition?AppId={}&Secretid={}&RequestId={}&Timestamp={}&{}",
            self.endpoint,
            self.credential.app_id,
            self.credential.app_id,
            request_id,
            now,
            sdkinfo::sdk_report_query()
        );

        let body = serde_json::to_string(req)
            .map_err(|e| invalid_param(format!("marshal request failed: {e}")))?;

        let http_resp = self
            .agent
            .post(&req_url)
            .set("Content-Type", "application/json; charset=utf-8")
            .set("X-TRTC-SdkAppId", &self.credential.sdk_app_id.to_string())
            .set("X-TRTC-UserSig", &user_sig)
            .send_string(&body);

        let (status, resp_body) = read_http_response(http_resp)?;
        if status != 200 {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                format!("http status {status}: {resp_body}"),
            ));
        }

        // Check for API-level errors first.
        let api: SentenceApiResponse = serde_json::from_str(&resp_body).map_err(|e| {
            AsrError::new(ERR_CODE_READ_FAILED, format!("unmarshal response failed: {e}"))
        })?;
        let response = api
            .response
            .ok_or_else(|| AsrError::new(ERR_CODE_SERVER_ERROR, "empty response from server"))?;
        if let Some(err) = &response.error {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                format!(
                    "server error [{}]: {} (RequestId: {})",
                    err.code, err.message, response.request_id
                ),
            ));
        }

        Ok(response.into_result())
    }

    /// Convenience method that sends local audio data for recognition,
    /// handling base64 encoding automatically.
    pub fn recognize_data(
        &self,
        data: &[u8],
        voice_format: &str,
        engine_model_type: &str,
    ) -> Result<SentenceRecognitionResult> {
        if data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if data.len() > MAX_AUDIO_SIZE {
            return Err(invalid_param("audio data exceeds 3MB limit"));
        }
        let req = SentenceRecognitionRequest {
            eng_service_type: engine_model_type.to_string(),
            source_type: SOURCE_TYPE_DATA,
            voice_format: voice_format.to_string(),
            data: B64.encode(data),
            data_len: data.len() as i64,
            ..Default::default()
        };
        self.recognize(&req)
    }

    /// Convenience method that sends an audio URL for recognition.
    pub fn recognize_url(
        &self,
        audio_url: &str,
        voice_format: &str,
        engine_model_type: &str,
    ) -> Result<SentenceRecognitionResult> {
        if audio_url.is_empty() {
            return Err(invalid_param("audio URL is empty"));
        }
        let req = SentenceRecognitionRequest {
            eng_service_type: engine_model_type.to_string(),
            source_type: SOURCE_TYPE_URL,
            voice_format: voice_format.to_string(),
            url: audio_url.to_string(),
            ..Default::default()
        };
        self.recognize(&req)
    }

    /// Sends local audio data with a pre-configured request, handling base64
    /// encoding automatically. `data`/`data_len` are set from `raw_data`.
    pub fn recognize_data_with_options(
        &self,
        raw_data: &[u8],
        req: &mut SentenceRecognitionRequest,
    ) -> Result<SentenceRecognitionResult> {
        if raw_data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if raw_data.len() > MAX_AUDIO_SIZE {
            return Err(invalid_param("audio data exceeds 3MB limit"));
        }
        req.source_type = SOURCE_TYPE_DATA;
        req.data = B64.encode(raw_data);
        req.data_len = raw_data.len() as i64;
        self.recognize(req)
    }
}

fn validate_request(req: &SentenceRecognitionRequest) -> Result<()> {
    if req.eng_service_type.is_empty() {
        return Err(invalid_param("EngServiceType is required"));
    }
    if req.voice_format.is_empty() {
        return Err(invalid_param("VoiceFormat is required"));
    }
    if req.source_type == SOURCE_TYPE_URL && req.url.is_empty() {
        return Err(invalid_param("Url is required when SourceType=0"));
    }
    if req.source_type == SOURCE_TYPE_DATA && req.data.is_empty() {
        return Err(invalid_param("Data is required when SourceType=1"));
    }
    Ok(())
}

/// Normalizes ureq's result into (status, body), mapping transport errors to
/// SDK errors.
pub(crate) fn read_http_response(
    http_resp: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<(u16, String)> {
    match http_resp {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().map_err(|e| {
                AsrError::new(ERR_CODE_READ_FAILED, format!("read response body failed: {e}"))
            })?;
            Ok((status, body))
        }
        // ureq surfaces non-2xx statuses as Error::Status; the caller still
        // needs the status and body.
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Ok((status, body))
        }
        Err(e) => Err(AsrError::new(
            ERR_CODE_CONNECT_FAILED,
            format!("http request failed: {e}"),
        )),
    }
}
