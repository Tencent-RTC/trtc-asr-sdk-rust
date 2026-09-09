//! TRTC-ASR v3 protocol client.
//!
//! The v3 protocol restructures the wire format around two separated blocks:
//! an "auth" block (sdkappid / usersig / request_id) consumed by the gateway's
//! authentication layer, and a "params" block (engine, VAD, hotwords, filters,
//! ...) consumed by the recognition layer. All field names are snake_case and
//! responses are flat (no Response envelope) with numeric codes.
//!
//! v3 uses SdkAppID as the only customer dimension; the Tencent Cloud AppID
//! is not needed (see [`new_credential`]).
//!
//! Interfaces:
//! - [`SpeechRecognizer`]: WebSocket `/asr/v3` — the URL carries only
//!   voice_id; auth and params travel in a single start frame sent right
//!   after the handshake. `start()` waits synchronously for the server ack.
//! - [`SentenceRecognizer`]: HTTP `POST /v3/transcribe` — one-shot (≤60s).
//! - [`FileRecognizer`]: HTTP `/v3/create_transcription` +
//!   `/v3/describe_transcription` — async task for long audio.
//!
//! The streaming lifecycle machinery (state machine, reader thread, end
//! signal handoff) is reused from the v2 module — the v3 downlink shape is
//! identical to v2, so only the handshake differs.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use base64::engine::general_purpose::STANDARD as B64;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::common::errors::{
    invalid_param, AsrError, Result, ERR_CODE_ALREADY_STARTED, ERR_CODE_AUTH_FAILED,
    ERR_CODE_CONNECT_FAILED, ERR_CODE_NOT_STARTED, ERR_CODE_READ_FAILED, ERR_CODE_SERVER_ERROR,
    ERR_CODE_TIMEOUT, ERR_CODE_WRITE_FAILED,
};
use crate::common::sdkinfo;
use crate::common::{resolve_http_endpoint, resolve_ws_endpoint, usersig, Credential};

use super::params::{validate_enum_option, validate_vad_tuning};
use super::speech_recognizer::{
    connect_ws, new_shared, read_loop, SpeechRecognitionListener, STATE_IDLE, STATE_RUNNING,
    STATE_STOPPED,
};
use tungstenite::Message;

/// Production WebSocket endpoint for the TRTC-ASR service (v3).
pub const ENDPOINT: &str = "wss://asr.cloud-rtc.com";
/// Production HTTPS endpoint for the v3 offline interfaces.
pub const HTTP_ENDPOINT: &str = "https://asr.cloud-rtc.com";

pub const SOURCE_TYPE_URL: i32 = 0;
pub const SOURCE_TYPE_DATA: i32 = 1;

pub const TASK_STATUS_WAITING: i32 = 0;
pub const TASK_STATUS_RUNNING: i32 = 1;
pub const TASK_STATUS_SUCCESS: i32 = 2;
pub const TASK_STATUS_FAILED: i32 = 3;

pub const SPEAKER_DIARIZATION_OFF: i32 = 0;
pub const SPEAKER_DIARIZATION_CLUSTER: i32 = 1;
pub const SPEAKER_DIARIZATION_VOICEPRINT: i32 = 3;

const MAX_AUDIO_SIZE: usize = 3 * 1024 * 1024;
const MAX_FILE_SIZE: usize = 5 * 1024 * 1024;

// Wire size limits, mirroring the server side.
const START_FRAME_MAX_BYTES: usize = 64 * 1024;
const STREAM_FRAME_MAX_BYTES: usize = 256 * 1024;

/// ack timeout: how long `start` waits for the server's start-frame
/// acknowledgement.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

// Server-side accepted ranges (asr-proxy validator_v3.go).
const MAX_VOICE_ID_LEN: usize = 128;
const MIN_MAX_SPEAK_TIME: i32 = 5000;
const MAX_MAX_SPEAK_TIME: i32 = 90000;
const MIN_VAD_SILENCE_TIME_MS: i32 = 240;
const MAX_VAD_SILENCE_TIME_MS: i32 = 2000;
const VALID_VOICE_FORMATS: &[i32] = &[1, 4, 6, 8, 10, 11, 12, 14, 16];

/// Creates a credential for the v3 API: only SdkAppID + SecretKey are
/// needed.
pub fn new_credential(sdk_app_id: i64, secret_key: impl Into<String>) -> Credential {
    Credential::new(0, sdk_app_id, secret_key.into())
}

fn server_error(code: i64, message: &str, request_id: &str) -> AsrError {
    let code = i32::try_from(code).unwrap_or(ERR_CODE_SERVER_ERROR);
    if request_id.is_empty() {
        AsrError::new(code, message.to_string())
    } else {
        AsrError::new(code, format!("{message} (request_id: {request_id})"))
    }
}

/// Temporary voiceprint enrollment entry used with `speaker_diarization=3`.
/// Serialized with the v3 snake_case wire names (`role_name` / `audio_url`),
/// unlike the v2 CamelCase [`crate::common::SpeakerRole`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerRole {
    pub role_name: String,
    pub audio_url: String,
}

/// Recognition context, aligned with Soniox / Volcengine semantics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Context {
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub terms: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub general: Vec<ContextKV>,
}

/// A domain key-value pair of [`Context::general`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextKV {
    pub key: String,
    pub value: String,
}

/// One audio piece of a distributed recording task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioURLItem {
    pub index: i64,
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub label: String,
}

/// A word-level timing entry (transcribe / describe responses).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub start_time: i64,
    pub end_time: i64,
}

/// A single audio URL validation failure helper matching the server rules
/// (rectask_cluster.go validateDistributedAudioUrlsRequest): audio_urls
/// requires source_type=0 with url/data left empty, unique non-negative
/// indexes and absolute http(s) URLs.
fn validate_audio_urls(
    source_type: i32,
    url: &str,
    data: &str,
    audio_urls: &[AudioURLItem],
) -> Result<()> {
    if source_type != SOURCE_TYPE_URL || !url.is_empty() || !data.is_empty() {
        return Err(invalid_param(
            "AudioURLs cannot be used together with non-zero source_type, url or data",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, item) in audio_urls.iter().enumerate() {
        if item.index < 0 {
            return Err(invalid_param(&format!(
                "AudioURLs[{}].Index must be non-negative",
                i
            )));
        }
        if !seen.insert(item.index) {
            return Err(invalid_param(&format!(
                "AudioURLs index duplicated: {}",
                item.index
            )));
        }
        let raw = item.url.trim();
        if raw.is_empty() {
            return Err(invalid_param(&format!("AudioURLs[{}].URL is required", i)));
        }
        let parsed = Url::parse(raw)
            .map_err(|e| invalid_param(&format!("AudioURLs[{}].URL is not a valid URL: {e}", i)))?;
        if (parsed.scheme() != "http" && parsed.scheme() != "https") || parsed.host_str().is_none()
        {
            return Err(invalid_param(&format!(
                "AudioURLs[{}].URL must be a valid http/https URL",
                i
            )));
        }
    }
    Ok(())
}

/// Builds the v3 auth block (sdkappid / usersig / request_id). The
/// protocol's internal "business" field (gray/routing dimension managed by
/// the service side) is not part of the public SDK surface and is never sent.
fn auth_block(credential: &Credential, request_id: &str) -> Result<serde_json::Value> {
    let user_sig = if credential.user_sig.is_empty() {
        usersig::gen_user_sig(
            credential.sdk_app_id as u64,
            &credential.secret_key,
            request_id,
            86400,
        )
        .map_err(|e| {
            AsrError::new(
                ERR_CODE_AUTH_FAILED,
                format!("generate user sig failed: {e}"),
            )
        })?
    } else {
        credential.user_sig.clone()
    };
    let mut auth = serde_json::json!({
        "sdkappid": credential.sdk_app_id.to_string(),
        "usersig": user_sig,
    });
    if !request_id.is_empty() {
        auth["request_id"] = serde_json::Value::String(request_id.to_string());
    }
    Ok(auth)
}

/// Wraps params into the {"auth": ..., "params": ...} offline body, injecting
/// the SDK telemetry into the params block.
fn offline_body(
    credential: &Credential,
    request_id: &str,
    params: &impl Serialize,
) -> Result<String> {
    let value = serde_json::to_value(params)
        .map_err(|e| AsrError::new(ERR_CODE_READ_FAILED, format!("marshal params failed: {e}")))?;
    offline_body_value(credential, request_id, value)
}

fn offline_body_value(
    credential: &Credential,
    request_id: &str,
    mut params: serde_json::Value,
) -> Result<String> {
    params["sdk_info"] =
        serde_json::to_value(sdkinfo::sdk_report_params()).unwrap_or(serde_json::Value::Null);
    let auth = auth_block(credential, request_id)?;
    Ok(serde_json::json!({ "auth": auth, "params": params }).to_string())
}

/// POSTs a JSON body and returns (status, body). Transport errors map to
/// ERR_CODE_CONNECT_FAILED; non-2xx statuses are returned to the caller (v3
/// error bodies carry the authoritative numeric code).
fn http_post_json(url: &str, body: &str, timeout: Duration) -> Result<(u16, String)> {
    let agent = ureq::AgentBuilder::new()
        // Share the WebSocket transport's rustls config (ring + system
        // roots), mirroring the v2 HTTP clients.
        .tls_config(crate::common::tls::rustls_client_config())
        .timeout(timeout)
        .build();
    let resp = agent
        .post(url)
        .set("Content-Type", "application/json; charset=utf-8")
        .send_string(body);
    super::sentence_recognizer::read_http_response(resp)
}

/// Checks a flat response body. A non-2xx status whose body carries no
/// numeric code is a gateway/LB error page — never mistake it for success
/// (a legitimate v3 endpoint always sends 2xx for code==0).
fn decode_flat_response(body: &str, status: u16) -> Result<serde_json::Value> {
    let data: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        AsrError::new(
            ERR_CODE_SERVER_ERROR,
            format!("invalid response (http {status}): {e}"),
        )
    })?;
    if !data.is_object() {
        return Err(AsrError::new(
            ERR_CODE_SERVER_ERROR,
            format!("invalid response (http {status}): not a JSON object"),
        ));
    }
    let code = data.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    if !(200..300).contains(&status) && code == 0 {
        let mut preview = body.to_string();
        if preview.len() > 256 {
            preview.truncate(256);
            preview.push_str("...");
        }
        return Err(AsrError::new(
            ERR_CODE_SERVER_ERROR,
            format!("http {status} with non-v3 response body: {preview}"),
        ));
    }
    Ok(data)
}

fn response_code(data: &serde_json::Value) -> i64 {
    data.get("code").and_then(|v| v.as_i64()).unwrap_or(0)
}

// =====================================================================
// One-shot sentence recognition (/v3/transcribe)
// =====================================================================

/// The params block of /v3/transcribe (snake_case wire names).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TranscribeRequest {
    pub engine_model_type: String,
    pub source_type: i32,
    pub voice_format: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub data: String,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub data_len: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub word_info: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_dirty: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_modal: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_punc: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub convert_num_mode: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub customization_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_list: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub input_sample_rate: i32,
    /// None leaves the server default; an explicit 0/1 is honored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needvad: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_silence_time: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_diarization: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_number: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
}

/// The flat /v3/transcribe response.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscribeResponse {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub audio_duration: i64,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub language_b47: String,
    #[serde(default)]
    pub word_size: i32,
    #[serde(default)]
    pub word_list: Vec<Word>,
}

/// One-shot sentence recognition client (POST /v3/transcribe).
///
/// A non-zero server code is raised as an [`AsrError`] carrying that code;
/// auth failures arrive with HTTP 200 (body code 4002) — the body code, not
/// the HTTP status, is authoritative.
pub struct SentenceRecognizer {
    credential: Credential,
    endpoint: String,
    timeout: Duration,
}

impl SentenceRecognizer {
    pub fn new(credential: Credential) -> Self {
        SentenceRecognizer {
            credential,
            endpoint: String::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Overrides the default API endpoint (for testing).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Sets the HTTP request timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Sends a sentence recognition request and returns the result.
    pub fn recognize(&self, req: &TranscribeRequest) -> Result<TranscribeResponse> {
        validate_transcribe_request(req)?;
        let request_id = Uuid::new_v4().to_string();
        let body = offline_body(&self.credential, &request_id, req)?;
        let url = format!(
            "{}{}",
            resolve_http_endpoint(&self.endpoint, &self.credential.site)?,
            "/v3/transcribe"
        );
        let (status, resp_body) = http_post_json(&url, &body, self.timeout)?;
        let data = decode_flat_response(&resp_body, status)?;
        let code = response_code(&data);
        if code != 0 {
            return Err(server_error(
                code,
                data.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                data.get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ));
        }
        serde_json::from_value(data).map_err(|e| {
            AsrError::new(
                ERR_CODE_READ_FAILED,
                format!("unmarshal response failed: {e}"),
            )
        })
    }

    /// Recognizes local audio data (auto base64). Max 3MB / 60s.
    pub fn recognize_data(
        &self,
        data: &[u8],
        voice_format: &str,
        engine_model_type: &str,
    ) -> Result<TranscribeResponse> {
        if data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if data.len() > MAX_AUDIO_SIZE {
            return Err(invalid_param("audio data exceeds 3MB limit"));
        }
        use base64::Engine as _;
        self.recognize(&TranscribeRequest {
            engine_model_type: engine_model_type.to_string(),
            source_type: SOURCE_TYPE_DATA,
            voice_format: voice_format.to_string(),
            data: B64.encode(data),
            data_len: data.len() as i64,
            ..Default::default()
        })
    }

    /// Recognizes audio from a URL.
    pub fn recognize_url(
        &self,
        audio_url: &str,
        voice_format: &str,
        engine_model_type: &str,
    ) -> Result<TranscribeResponse> {
        if audio_url.is_empty() {
            return Err(invalid_param("audio URL is empty"));
        }
        self.recognize(&TranscribeRequest {
            engine_model_type: engine_model_type.to_string(),
            source_type: SOURCE_TYPE_URL,
            voice_format: voice_format.to_string(),
            url: audio_url.to_string(),
            ..Default::default()
        })
    }
}

// =====================================================================
// Async file recognition (/v3/create_transcription + describe)
// =====================================================================

/// The params block of /v3/create_transcription (snake_case wire names).
#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateTranscriptionRequest {
    pub engine_model_type: String,
    pub channel_num: i32,
    pub res_text_format: i32,
    pub source_type: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub data: String,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub data_len: i64,
    /// Distributed recording pieces (distributed speaker feature
    /// extraction). When non-empty, source_type/url/data must be left at
    /// their zero values — the server rejects any combination of audio_urls
    /// with a single-audio source.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audio_urls: Vec<AudioURLItem>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub callback_url: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_diarization: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_number: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub voiceprint_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub speaker_roles: Vec<SpeakerRole>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub customization_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_list: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keyword_lib_id_list: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub replace_text_id: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub convert_num_mode: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_dirty: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_punc: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_modal: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub sentence_max_length: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub extra: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub vad_silence_ms: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_level: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_threshold: Option<f64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
}

/// One sentence of a describe_transcription result.
#[derive(Debug, Clone, Deserialize)]
pub struct SentenceDetail {
    #[serde(default)]
    pub final_sentence: String,
    #[serde(default)]
    pub slice_sentence: String,
    #[serde(default)]
    pub written_text: String,
    #[serde(default)]
    pub start_ms: i64,
    #[serde(default)]
    pub end_ms: i64,
    #[serde(default)]
    pub words_num: i32,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub speech_speed: f64,
    #[serde(default)]
    pub speaker_id: i32,
    #[serde(default)]
    pub channel_id: i32,
    #[serde(default)]
    pub speaker_role_name: String,
    #[serde(default)]
    pub silence_time: i32,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub language_b47: String,
}

/// The flat /v3/describe_transcription response.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptionStatus {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub transcription_id: String,
    #[serde(default)]
    pub status: i32,
    #[serde(default)]
    pub status_str: String,
    #[serde(default)]
    pub progress: i32,
    #[serde(default)]
    pub audio_duration: f64,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub result_detail: Vec<SentenceDetail>,
    #[serde(default)]
    pub error_msg: String,
}

/// Async audio file recognition client. The task ID (`transcription_id`) is
/// valid for 24 hours and belongs to the v3 task space: a v1 `RecTaskId`
/// cannot be queried through this client and vice versa.
pub struct FileRecognizer {
    credential: Credential,
    endpoint: String,
    timeout: Duration,
}

impl FileRecognizer {
    pub fn new(credential: Credential) -> Self {
        FileRecognizer {
            credential,
            endpoint: String::new(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Overrides the default API endpoint (for testing).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Sets the HTTP request timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Submits a file recognition task and returns the transcription ID.
    pub fn create_task(&self, req: &CreateTranscriptionRequest) -> Result<String> {
        validate_create_request(req)?;
        let request_id = Uuid::new_v4().to_string();
        let body = offline_body(&self.credential, &request_id, req)?;
        let url = format!(
            "{}{}",
            resolve_http_endpoint(&self.endpoint, &self.credential.site)?,
            "/v3/create_transcription"
        );
        let (status, resp_body) = http_post_json(&url, &body, self.timeout)?;
        let data = decode_flat_response(&resp_body, status)?;
        let code = response_code(&data);
        if code != 0 {
            return Err(server_error(
                code,
                data.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                data.get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ));
        }
        let id = data
            .get("transcription_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                "empty transcription_id in response",
            ));
        }
        Ok(id)
    }

    /// Submits local audio data (auto base64). Max 5MB.
    pub fn create_task_from_data(&self, data: &[u8], engine_model_type: &str) -> Result<String> {
        if data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if data.len() > MAX_FILE_SIZE {
            return Err(invalid_param("audio data exceeds 5MB limit"));
        }
        use base64::Engine as _;
        self.create_task(&CreateTranscriptionRequest {
            engine_model_type: engine_model_type.to_string(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_DATA,
            data: B64.encode(data),
            data_len: data.len() as i64,
            ..Default::default()
        })
    }

    /// Submits an audio URL. Audio ≤12h, ≤1GB.
    pub fn create_task_from_url(&self, audio_url: &str, engine_model_type: &str) -> Result<String> {
        if audio_url.is_empty() {
            return Err(invalid_param("audio URL is empty"));
        }
        self.create_task(&CreateTranscriptionRequest {
            engine_model_type: engine_model_type.to_string(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_URL,
            url: audio_url.to_string(),
            ..Default::default()
        })
    }

    /// Queries the status of a file recognition task.
    pub fn describe_task(&self, transcription_id: &str) -> Result<TranscriptionStatus> {
        if transcription_id.is_empty() {
            return Err(invalid_param("transcription_id is empty"));
        }
        let request_id = Uuid::new_v4().to_string();
        let params = serde_json::json!({
            "transcription_id": transcription_id,
            "sdk_info": sdkinfo::sdk_report_params(),
        });
        let body = offline_body_value(&self.credential, &request_id, params)?;
        let url = format!(
            "{}{}",
            resolve_http_endpoint(&self.endpoint, &self.credential.site)?,
            "/v3/describe_transcription"
        );
        let (status, resp_body) = http_post_json(&url, &body, self.timeout)?;
        let data = decode_flat_response(&resp_body, status)?;
        let code = response_code(&data);
        if code != 0 {
            return Err(server_error(
                code,
                data.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                data.get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ));
        }
        serde_json::from_value(data).map_err(|e| {
            AsrError::new(
                ERR_CODE_READ_FAILED,
                format!("unmarshal response failed: {e}"),
            )
        })
    }

    /// Polls for results with the default interval (1s) and timeout (10min).
    pub fn wait_for_result(&self, transcription_id: &str) -> Result<TranscriptionStatus> {
        self.wait_for_result_with_interval(
            transcription_id,
            Duration::from_secs(1),
            Duration::from_secs(600),
        )
    }

    /// Polls for results with a custom interval and timeout.
    pub fn wait_for_result_with_interval(
        &self,
        transcription_id: &str,
        interval: Duration,
        timeout: Duration,
    ) -> Result<TranscriptionStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.describe_task(transcription_id)?;
            if status.status == TASK_STATUS_SUCCESS {
                return Ok(status);
            }
            if status.status == TASK_STATUS_FAILED {
                return Err(AsrError::new(
                    ERR_CODE_SERVER_ERROR,
                    format!(
                        "task failed: {} (transcription_id: {})",
                        status.error_msg, status.transcription_id
                    ),
                ));
            }
            if Instant::now() > deadline {
                return Err(AsrError::new(
                    ERR_CODE_TIMEOUT,
                    format!(
                        "task not completed within {:?} (transcription_id: {}, Status: {})",
                        timeout, transcription_id, status.status_str
                    ),
                ));
            }
            std::thread::sleep(interval);
        }
    }
}

// =====================================================================
// Realtime speech recognition (/asr/v3)
// =====================================================================

/// The params block of the /asr/v3 start frame (snake_case wire names).
#[derive(Debug, Clone, Default, Serialize)]
pub struct OnlineParams {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice_id: String,
    pub engine_model_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    pub voice_format: i32,
    pub needvad: i32,
    pub convert_num_mode: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_silence_time: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_level: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_threshold: Option<f64>,
    #[serde(skip_serializing_if = "is_zero")]
    pub max_speak_time: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_dirty: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_modal: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub filter_punc: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_empty_result: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_sample_rate: Option<i32>,
    #[serde(skip_serializing_if = "is_zero")]
    pub word_info: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub word_with_space: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hotword_list: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_diarization: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub speaker_number: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub voiceprint_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub speaker_roles: Vec<SpeakerRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
    /// SDK telemetry: the gateway replays the start frame byte-for-byte to
    /// the worker, which ignores unknown keys, so it survives in server-side
    /// dumps without disturbing the protocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_info: Option<serde_json::Value>,
}

/// The main client for realtime speech recognition over the v3 protocol.
///
/// The lifecycle machinery (state machine, reader thread, end-signal handoff)
/// is shared with the v2 client; only the handshake differs: the URL carries
/// only voice_id, and auth/params travel in a single start frame.
///
/// `start()` waits synchronously for the server's start-frame ack, so
/// authentication (4002), gray-switch (4001) and scheduling (5000) errors are
/// raised directly from `start()` instead of surfacing later via `on_fail`.
pub struct SpeechRecognizer {
    credential: Credential,
    listener: Arc<dyn SpeechRecognitionListener>,
    endpoint: String,
    engine_model_type: String,
    params: std::cell::RefCell<OnlineParams>,
    voice_id: String,
    write_timeout: Duration,
    stop_timeout: Duration,
    shared: Arc<super::speech_recognizer::Shared>,
}

impl Drop for SpeechRecognizer {
    fn drop(&mut self) {
        let state = self.shared.state();
        if state == STATE_RUNNING || state == super::speech_recognizer::STATE_STOPPING {
            self.shared.finish_pub();
        }
    }
}

impl SpeechRecognizer {
    /// Creates a new v3 recognizer.
    pub fn new(
        credential: Credential,
        engine_model_type: impl Into<String>,
        listener: Arc<dyn SpeechRecognitionListener>,
    ) -> Self {
        SpeechRecognizer {
            credential,
            listener,
            endpoint: String::new(),
            engine_model_type: engine_model_type.into(),
            params: std::cell::RefCell::new(OnlineParams {
                voice_format: 1,
                needvad: 1,
                convert_num_mode: 1,
                ..Default::default()
            }),
            voice_id: String::new(),
            write_timeout: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(10),
            shared: new_shared(),
        }
    }

    /// Overrides the WebSocket endpoint (for testing against a mock server).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Sets the audio encoding format. 1: PCM (default).
    pub fn set_voice_format(&mut self, format: i32) {
        self.params.borrow_mut().voice_format = format;
    }

    /// Sets whether to enable VAD. Unlike the v2 transport, v3 honors an
    /// explicit 0 (it is sent on the wire, not dropped).
    pub fn set_need_vad(&mut self, need_vad: i32) {
        self.params.borrow_mut().needvad = need_vad;
    }

    /// Sets the number conversion mode. Unlike the v2 transport, v3 honors
    /// an explicit 0.
    pub fn set_convert_num_mode(&mut self, mode: i32) {
        self.params.borrow_mut().convert_num_mode = mode;
    }

    /// Sets the hotword list ID (sdkappid-scoped v3 vocabulary).
    pub fn set_hotword_id(&mut self, id: impl Into<String>) {
        self.params.borrow_mut().hotword_id = id.into();
    }

    /// Sets a temporary inline hotword list (`word1|weight1,word2|weight2`).
    pub fn set_hotword_list(&mut self, list: impl Into<String>) {
        self.params.borrow_mut().hotword_list = list.into();
    }

    pub fn set_filter_dirty(&mut self, mode: i32) {
        self.params.borrow_mut().filter_dirty = mode;
    }
    pub fn set_filter_modal(&mut self, mode: i32) {
        self.params.borrow_mut().filter_modal = mode;
    }
    pub fn set_filter_punc(&mut self, mode: i32) {
        self.params.borrow_mut().filter_punc = mode;
    }
    pub fn set_filter_empty_result(&mut self, mode: i32) {
        self.params.borrow_mut().filter_empty_result = Some(mode);
    }
    /// 0: off (default), 1: on, 2: with punctuation, 100: caption.
    pub fn set_word_info(&mut self, mode: i32) {
        self.params.borrow_mut().word_info = mode;
    }
    /// Whether English words are joined with spaces: 0=no (default), 1=yes.
    pub fn set_word_with_space(&mut self, mode: i32) {
        self.params.borrow_mut().word_with_space = mode;
    }
    pub fn set_vad_silence_time(&mut self, ms: i32) {
        self.params.borrow_mut().vad_silence_time = Some(ms);
    }
    pub fn set_vad_level(&mut self, level: i32) {
        self.params.borrow_mut().vad_level = Some(level);
    }
    pub fn set_noise_threshold(&mut self, threshold: f64) {
        self.params.borrow_mut().noise_threshold = Some(threshold);
    }
    pub fn set_max_speak_time(&mut self, ms: i32) {
        self.params.borrow_mut().max_speak_time = ms;
    }
    pub fn set_input_sample_rate(&mut self, rate: i32) {
        self.params.borrow_mut().input_sample_rate = Some(rate);
    }
    pub fn set_speaker_diarization(&mut self, mode: i32) {
        self.params.borrow_mut().speaker_diarization = mode;
    }
    pub fn set_speaker_number(&mut self, n: i32) {
        self.params.borrow_mut().speaker_number = n;
    }
    /// Registers temporary voiceprints (v3 SpeakerRole, snake_case wire).
    pub fn set_speaker_roles(&mut self, roles: Vec<SpeakerRole>) {
        self.params.borrow_mut().speaker_roles = roles;
    }
    pub fn set_voiceprint_ids(&mut self, ids: Vec<String>) {
        self.params.borrow_mut().voiceprint_ids = ids;
    }
    pub fn set_voice_id(&mut self, id: impl Into<String>) {
        self.voice_id = id.into();
    }
    pub fn set_language(&mut self, lang: impl Into<String>) {
        self.params.borrow_mut().language = lang.into();
    }
    pub fn set_context(&mut self, context: Context) {
        self.params.borrow_mut().context = Some(context);
    }

    /// Sets the timeout for a single audio write (clamped to [50ms, 30s]).
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = super::speech_recognizer::clamp_timeout_pub(
            timeout,
            Duration::from_millis(50),
            Duration::from_secs(30),
            Duration::from_secs(5),
        );
    }

    /// Sets how long `stop` waits for the final response (clamped to
    /// [1s, 60s]).
    pub fn set_stop_timeout(&mut self, timeout: Duration) {
        self.stop_timeout = super::speech_recognizer::clamp_timeout_pub(
            timeout,
            Duration::from_secs(1),
            Duration::from_secs(60),
            Duration::from_secs(10),
        );
    }

    /// Checks the options that have a documented server-side range,
    /// mirroring the proxy's v3 online validator so an invalid value fails
    /// locally instead of coming back as a remote 4001.
    fn validate_options(&self) -> Result<()> {
        let p = self.params.borrow();
        if self.voice_id.len() > MAX_VOICE_ID_LEN {
            return Err(invalid_param(&format!(
                "VoiceID length must not exceed {}",
                MAX_VOICE_ID_LEN
            )));
        }
        validate_v3_speaker_diarization(
            p.speaker_diarization,
            p.speaker_number,
            &p.speaker_roles,
            &p.voiceprint_ids,
        )?;
        validate_vad_tuning(p.vad_level, p.noise_threshold)?;
        if p.max_speak_time != 0
            && (p.max_speak_time < MIN_MAX_SPEAK_TIME || p.max_speak_time > MAX_MAX_SPEAK_TIME)
        {
            return Err(invalid_param(&format!(
                "MaxSpeakTime must be between {} and {} ms, got {}",
                MIN_MAX_SPEAK_TIME, MAX_MAX_SPEAK_TIME, p.max_speak_time
            )));
        }
        // vad_silence_time==None means "not set"; the range check only
        // applies to an explicitly set value with VAD enabled.
        if let Some(vst) = p.vad_silence_time {
            if vst != 0
                && p.needvad == 1
                && (vst < MIN_VAD_SILENCE_TIME_MS || vst > MAX_VAD_SILENCE_TIME_MS)
            {
                return Err(invalid_param(&format!(
                    "VadSilenceTime must be between {} and {} ms (needvad=1), got {}",
                    MIN_VAD_SILENCE_TIME_MS, MAX_VAD_SILENCE_TIME_MS, vst
                )));
            }
        }
        validate_enum_option("NeedVad", p.needvad, &[0, 1])?;
        validate_enum_option("ConvertNumMode", p.convert_num_mode, &[0, 1, 3])?;
        validate_enum_option("FilterDirty", p.filter_dirty, &[0, 1, 2])?;
        validate_enum_option("FilterModal", p.filter_modal, &[0, 1, 2])?;
        validate_enum_option("FilterPunc", p.filter_punc, &[0, 1])?;
        validate_enum_option("WordInfo", p.word_info, &[0, 1, 2, 100])?;
        validate_enum_option("WordWithSpace", p.word_with_space, &[0, 1])?;
        validate_enum_option("VoiceFormat", p.voice_format, VALID_VOICE_FORMATS)?;
        if let Some(v) = p.filter_empty_result {
            validate_enum_option("FilterEmptyResult", v, &[0, 1])?;
        }
        // 8000 is the only supported override; unset means "engine rate".
        if let Some(v) = p.input_sample_rate {
            validate_enum_option("InputSampleRate", v, &[0, 8000])?;
        }
        Ok(())
    }

    /// Assembles the start frame as a JSON string — it must be sent as a
    /// TEXT frame (a binary first frame is rejected by the server with
    /// 4010).
    fn build_start_frame(&self, user_sig: &str, voice_id: &str) -> Result<String> {
        let mut online = self.params.borrow().clone();
        online.voice_id = voice_id.to_string();
        online.engine_model_type = self.engine_model_type.clone();
        let mut params = serde_json::to_value(&online).map_err(|e| {
            AsrError::new(ERR_CODE_READ_FAILED, format!("marshal params failed: {e}"))
        })?;
        params["sdk_info"] =
            serde_json::to_value(sdkinfo::sdk_report_params()).unwrap_or(serde_json::Value::Null);
        let frame = serde_json::json!({
            "type": "start",
            "auth": {
                "sdkappid": self.credential.sdk_app_id.to_string(),
                "usersig": user_sig,
            },
            "params": params,
        });
        let data = serde_json::to_string(&frame).map_err(|e| {
            AsrError::new(
                ERR_CODE_READ_FAILED,
                format!("marshal start frame failed: {e}"),
            )
        })?;
        if data.len() > START_FRAME_MAX_BYTES {
            return Err(invalid_param(&format!(
                "start frame exceeds {} bytes ({})",
                START_FRAME_MAX_BYTES,
                data.len()
            )));
        }
        Ok(data)
    }

    /// Connects to /asr/v3, sends the start frame and waits for the server
    /// ack. Auth (4002), gray-switch (4001) and scheduling (5000) errors are
    /// returned here synchronously.
    fn connect_and_ack(&self) -> Result<String> {
        let voice_id = if self.voice_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            self.voice_id.clone()
        };

        // Resolve UserSig locally without mutating the shared credential.
        // The v3 signature identifier is the voice_id.
        let user_sig = if self.credential.user_sig.is_empty() {
            usersig::gen_user_sig(
                self.credential.sdk_app_id as u64,
                &self.credential.secret_key,
                &voice_id,
                86400,
            )
            .map_err(|e| {
                AsrError::new(
                    ERR_CODE_AUTH_FAILED,
                    format!("generate user sig failed: {e}"),
                )
            })?
        } else {
            self.credential.user_sig.clone()
        };

        let frame = self.build_start_frame(&user_sig, &voice_id)?;

        let base = resolve_ws_endpoint(&self.endpoint, &self.credential.site)?;
        // The URL carries only voice_id; auth and params travel in the start
        // frame, so the handshake stays header-free.
        let mut ws_url = Url::parse(&format!("{}/asr/v3", base)).map_err(|e| {
            AsrError::new(
                ERR_CODE_CONNECT_FAILED,
                format!("invalid endpoint url: {e}"),
            )
        })?;
        ws_url.query_pairs_mut().append_pair("voice_id", &voice_id);

        let (ws, shutdown) = connect_ws(
            ws_url.as_str(),
            super::speech_recognizer::HANDSHAKE_TIMEOUT,
            super::speech_recognizer::READ_POLL_INTERVAL,
            self.write_timeout,
        )?;
        *self.shared.ws.lock().unwrap() = Some(ws);
        *self.shared.shutdown.lock().unwrap() = Some(shutdown);
        self.shared.set_voice_id(&voice_id);

        // Send the start frame immediately (the server enforces a 3s
        // deadline). It must be a TEXT frame.
        {
            let mut guard = self.shared.ws.lock().unwrap();
            let send_result = match guard.as_mut() {
                Some(ws) => Some(ws.send(Message::Text(frame.into()))),
                None => None,
            };
            if let Some(Err(e)) = send_result {
                drop(guard);
                self.shared.close_pub();
                return Err(AsrError::new(
                    ERR_CODE_WRITE_FAILED,
                    format!("send start frame failed: {e}"),
                ));
            }
        }

        // Wait for the ack. A failure arrives as a structured error frame
        // ({code,message,voice_id}) followed by a normal close.
        let deadline = Instant::now() + ACK_TIMEOUT;
        loop {
            let read_result = {
                let mut guard = self.shared.ws.lock().unwrap();
                match guard.as_mut() {
                    Some(ws) => Some(ws.read()),
                    None => None,
                }
            };
            match read_result {
                None => {
                    return Err(AsrError::new(
                        ERR_CODE_CONNECT_FAILED,
                        "connection lost during start ack",
                    ));
                }
                Some(Ok(Message::Text(t))) => {
                    let ack: serde_json::Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(e) => {
                            self.shared.close_pub();
                            return Err(AsrError::new(
                                ERR_CODE_SERVER_ERROR,
                                format!("invalid start ack: {e}"),
                            ));
                        }
                    };
                    let code = ack.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                    if code != 0 {
                        let message = ack
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.shared.close_pub();
                        return Err(AsrError::new(
                            i32::try_from(code).unwrap_or(ERR_CODE_SERVER_ERROR),
                            message,
                        ));
                    }
                    // The ack frame never carries a result per the protocol
                    // (the worker acks before recognition starts); a
                    // defensive ack-with-result is consumed here.
                    return Ok(t.to_string());
                }
                Some(Ok(Message::Close(_))) => {
                    self.shared.close_pub();
                    return Err(AsrError::new(
                        ERR_CODE_READ_FAILED,
                        "read start ack failed: connection closed by server",
                    ));
                }
                // Ping/Pong/Frame: tungstenite answers pings internally.
                Some(Ok(_)) => continue,
                Some(Err(tungstenite::Error::Io(e)))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if Instant::now() >= deadline {
                        self.shared.close_pub();
                        return Err(AsrError::new(
                            ERR_CODE_READ_FAILED,
                            "read start ack failed: ack timeout",
                        ));
                    }
                    continue;
                }
                Some(Err(e)) => {
                    self.shared.close_pub();
                    return Err(AsrError::new(
                        ERR_CODE_READ_FAILED,
                        format!("read start ack failed: {e}"),
                    ));
                }
            }
        }
    }

    /// Initiates the WebSocket connection, sends the start frame and waits
    /// for the server ack before spawning the reader thread.
    pub fn start(&self) -> Result<()> {
        let cas = self.shared.compare_exchange_starting();
        if cas.is_err() {
            return Err(AsrError::new(
                ERR_CODE_ALREADY_STARTED,
                "recognizer already started",
            ));
        }

        // Validate before dialing so an invalid option fails locally instead
        // of costing a connection and coming back as a server-side 4001.
        if let Err(e) = self.validate_options() {
            self.shared.state.store(STATE_IDLE, Ordering::SeqCst);
            return Err(e);
        }

        if let Err(e) = self.connect_and_ack() {
            self.shared.state.store(STATE_IDLE, Ordering::SeqCst);
            return Err(e);
        }

        self.shared.state.store(STATE_RUNNING, Ordering::SeqCst);

        // Start reading responses on the reader thread.
        self.shared.set_stop_timeout_pub(self.stop_timeout);
        let shared = Arc::clone(&self.shared);
        let listener = Arc::clone(&self.listener);
        let voice_id = self.shared.voice_id();
        thread::spawn(move || read_loop(shared, listener, voice_id));

        Ok(())
    }

    /// Sends one audio frame (binary, ≤256KB) to the ASR service.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        if self.shared.state() != STATE_RUNNING {
            return Err(AsrError::new(
                ERR_CODE_NOT_STARTED,
                "recognizer not running",
            ));
        }
        if data.len() > STREAM_FRAME_MAX_BYTES {
            return Err(invalid_param(&format!(
                "audio frame exceeds {} bytes",
                STREAM_FRAME_MAX_BYTES
            )));
        }

        let mut guard = self.shared.ws.lock().unwrap();
        let ws = match guard.as_mut() {
            Some(ws) => ws,
            None => {
                return Err(AsrError::new(
                    ERR_CODE_NOT_STARTED,
                    "connection not established",
                ))
            }
        };

        // Re-check the state under the lock: stop may have sent the end
        // signal in between, and writing audio after end would violate the
        // protocol.
        if self.shared.state() != STATE_RUNNING {
            return Err(AsrError::new(
                ERR_CODE_NOT_STARTED,
                "recognizer not running",
            ));
        }

        ws.send(Message::Binary(data.to_vec().into())).map_err(|e| {
            AsrError::new(
                ERR_CODE_WRITE_FAILED,
                format!("write audio data failed: {e}"),
            )
        })
    }

    /// Gracefully stops the recognition session: sends the end signal and
    /// waits for the server's final response (up to `stop_timeout`).
    pub fn stop(&self) -> Result<()> {
        let cas = {
            let _guard = self.shared.done.0.lock().unwrap();
            self.shared.state.compare_exchange(
                STATE_RUNNING,
                super::speech_recognizer::STATE_STOPPING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
        };
        if cas.is_err() {
            return Err(AsrError::new(
                ERR_CODE_NOT_STARTED,
                "recognizer not running",
            ));
        }
        self.shared.done.1.notify_all();

        let (send_result, connection_missing) = {
            let mut guard = self.shared.ws.lock().unwrap();
            match guard.as_mut() {
                None if self.shared.state() == STATE_STOPPED => (None, false),
                None => (None, true),
                Some(_) if self.shared.state() == STATE_STOPPED => (None, false),
                Some(ws) => (
                    Some(ws.send(Message::Text(r#"{"type":"end"}"#.into()))),
                    false,
                ),
            }
        };

        if connection_missing {
            self.shared.finish_pub();
            return Err(AsrError::new(
                ERR_CODE_NOT_STARTED,
                "connection not established",
            ));
        }

        let end_frame_written = matches!(&send_result, Some(Ok(())));
        if let Some(Err(e)) = send_result {
            if self.shared.state() == STATE_STOPPED {
                self.shared.wait_for_read_loop_or_close_pub();
                return Ok(());
            }
            self.shared.finish_pub();
            return Err(AsrError::new(
                ERR_CODE_WRITE_FAILED,
                format!("send end signal failed: {e}"),
            ));
        }
        if end_frame_written {
            self.shared.mark_end_signal_sent_pub();
        } else {
            self.shared.wait_for_read_loop_or_close_pub();
            return Ok(());
        }

        // Re-entrant stop from a listener callback must not self-block on
        // the reader thread; a watchdog preserves the timeout semantics.
        if self.called_from_listener_callback() {
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || shared.wait_for_read_loop_or_close_pub());
            return Ok(());
        }

        self.shared.wait_for_read_loop_or_close_pub();
        self.shared.finish_pub();
        Ok(())
    }

    fn called_from_listener_callback(&self) -> bool {
        let tid = thread::current().id();
        self.shared
            .reader_tid
            .lock()
            .map(|guard| *guard == Some(tid))
            .unwrap_or(false)
    }
}

/// v3-speaker-diarization validation (roles use the snake_case wire fields
/// role_name / audio_url, unlike the v2 CamelCase SpeakerRole).
fn validate_v3_speaker_diarization(
    mode: i32,
    speaker_number: i32,
    roles: &[SpeakerRole],
    voiceprint_ids: &[String],
) -> Result<()> {
    if ![SPEAKER_DIARIZATION_OFF, 1, SPEAKER_DIARIZATION_VOICEPRINT].contains(&mode) {
        return Err(invalid_param(&format!(
            "SpeakerDiarization must be 0 (off), 1 (cluster) or 3 (voiceprint), got {}",
            mode
        )));
    }
    if speaker_number < 0 {
        return Err(invalid_param(&format!(
            "SpeakerNumber must be >= 0 (0 = auto detection), got {}",
            speaker_number
        )));
    }
    if mode != SPEAKER_DIARIZATION_VOICEPRINT && (!roles.is_empty() || !voiceprint_ids.is_empty()) {
        return Err(invalid_param(
            "SpeakerRoles/VoiceprintIds require SpeakerDiarization=3",
        ));
    }
    for (i, role) in roles.iter().enumerate() {
        if role.role_name.is_empty() {
            return Err(invalid_param(&format!(
                "SpeakerRoles[{}].RoleName is empty",
                i
            )));
        }
        if role.audio_url.trim().is_empty() {
            return Err(invalid_param(&format!(
                "SpeakerRoles[{}].AudioURL is empty",
                i
            )));
        }
        let parsed = Url::parse(&role.audio_url).map_err(|e| {
            invalid_param(&format!(
                "SpeakerRoles[{}].AudioURL is not a valid URL: {e}",
                i
            ))
        })?;
        if (parsed.scheme() != "http" && parsed.scheme() != "https") || parsed.host_str().is_none()
        {
            return Err(invalid_param(&format!(
                "SpeakerRoles[{}].AudioURL must use http or https",
                i
            )));
        }
    }
    for (i, id) in voiceprint_ids.iter().enumerate() {
        if id.is_empty() {
            return Err(invalid_param(&format!("VoiceprintIds[{}] is empty", i)));
        }
    }
    Ok(())
}

fn validate_transcribe_request(req: &TranscribeRequest) -> Result<()> {
    if req.engine_model_type.is_empty() {
        return Err(invalid_param("engine_model_type is required"));
    }
    if req.voice_format.is_empty() {
        return Err(invalid_param("voice_format is required"));
    }
    if req.source_type == SOURCE_TYPE_URL && req.url.is_empty() {
        return Err(invalid_param("url is required when source_type=0"));
    }
    if req.source_type == SOURCE_TYPE_DATA && req.data.is_empty() {
        return Err(invalid_param("data is required when source_type=1"));
    }
    if req.speaker_diarization != 0 || req.speaker_number != 0 {
        validate_v3_speaker_diarization(req.speaker_diarization, req.speaker_number, &[], &[])?;
    }
    if let Some(v) = req.needvad {
        validate_enum_option("needvad", v, &[0, 1])?;
    }
    // 8000 is the only supported override; 0 means "use the engine rate".
    validate_enum_option("input_sample_rate", req.input_sample_rate, &[0, 8000])
}

fn validate_create_request(req: &CreateTranscriptionRequest) -> Result<()> {
    if req.engine_model_type.is_empty() {
        return Err(invalid_param("engine_model_type is required"));
    }
    if req.channel_num != 1 && req.channel_num != 2 {
        return Err(invalid_param("channel_num must be 1 or 2"));
    }
    if ![0, 1, 2, 3].contains(&req.res_text_format) {
        return Err(invalid_param(&format!(
            "res_text_format must be one of [0, 1, 2, 3], got {}",
            req.res_text_format
        )));
    }
    if req.channel_num == 2 && req.speaker_diarization != 0 {
        return Err(invalid_param(
            "speaker_diarization is not supported for stereo (channel_num=2); sentences carry channel_id instead",
        ));
    }
    if !req.audio_urls.is_empty() {
        // Distributed path: the server requires source_type=0 with url/data
        // empty (rectask_cluster.go validateDistributedAudioUrlsRequest).
        validate_audio_urls(req.source_type, &req.url, &req.data, &req.audio_urls)?;
    } else {
        if req.source_type == SOURCE_TYPE_URL && req.url.is_empty() {
            return Err(invalid_param("url is required when source_type=0"));
        }
        if req.source_type == SOURCE_TYPE_DATA && req.data.is_empty() {
            return Err(invalid_param("data is required when source_type=1"));
        }
    }
    validate_v3_speaker_diarization(
        req.speaker_diarization,
        req.speaker_number,
        &req.speaker_roles,
        &req.voiceprint_ids,
    )?;
    if let Some(v) = req.noise_threshold {
        validate_vad_tuning(None, Some(v))?;
    }
    if let Some(v) = req.vad_level {
        if v != 0 && v != 1 {
            return Err(invalid_param(&format!(
                "VadLevel must be 0 (high recall) or 1 (far-field filtering), got {}",
                v
            )));
        }
    }
    Ok(())
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}
