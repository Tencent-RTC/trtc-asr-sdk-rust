//! Async audio file recognition client (HTTP).
//!
//! Unlike [`SentenceRecognizer`](super::SentenceRecognizer) (one-shot, ≤60s),
//! `FileRecognizer` handles longer audio via an async workflow: submit a task
//! (CreateRecTask), then poll for results (DescribeTaskStatus).
//!
//! Usage:
//!
//! ```no_run
//! # use trtc_asr_sdk::common::Credential;
//! # use trtc_asr_sdk::asr::FileRecognizer;
//! let credential = Credential::new(0, 0, "your-sdk-secret-key");
//! let recognizer = FileRecognizer::new(credential);
//! // let task_id = recognizer.create_task_from_data(&pcm_bytes, "pcm", "16k_zh_en").unwrap();
//! // let status = recognizer.wait_for_result(&task_id).unwrap();
//! ```

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::common::errors::{
    invalid_param, AsrError, Result, ERR_CODE_AUTH_FAILED, ERR_CODE_READ_FAILED,
    ERR_CODE_SERVER_ERROR, ERR_CODE_TIMEOUT,
};
use crate::common::signature::SpeakerRole;
use crate::common::{sdkinfo, usersig, Credential};

use super::params::{validate_speaker_diarization, validate_vad_tuning};
use super::sentence_recognizer::{read_http_response, ApiError, SOURCE_TYPE_DATA, SOURCE_TYPE_URL};

/// Production HTTPS endpoint for audio file recognition.
pub const FILE_ENDPOINT: &str = "https://asr.cloud-rtc.com";

/// Max audio size for data upload (before base64 encoding).
pub const MAX_AUDIO_SIZE: usize = 5 * 1024 * 1024;

/// Task is queued.
pub const TASK_STATUS_WAITING: i32 = 0;
/// Task is being processed.
pub const TASK_STATUS_RUNNING: i32 = 1;
/// Task completed successfully.
pub const TASK_STATUS_SUCCESS: i32 = 2;
/// Task failed.
pub const TASK_STATUS_FAILED: i32 = 3;

/// JSON request body for creating a file recognition task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateRecTaskRequest {
    /// Engine model type. Required. E.g. "16k_zh", "16k_zh_en".
    #[serde(rename = "EngineModelType")]
    pub engine_model_type: String,

    /// Number of audio channels. Required. 1: mono; 2: stereo (8k telephony,
    /// server separates speakers by channel and returns `ChannelId`).
    #[serde(rename = "ChannelNum")]
    pub channel_num: i32,

    /// Result format: 0 basic, 1 word-level timing, 2 word+punctuation timing.
    #[serde(rename = "ResTextFormat")]
    pub res_text_format: i32,

    /// Audio source: 0 = URL, 1 = base64 data in body.
    #[serde(rename = "SourceType")]
    pub source_type: i32,

    /// Audio file URL (required when source_type = 0). ≤ 12h, ≤ 1GB.
    #[serde(rename = "Url", skip_serializing_if = "String::is_empty")]
    pub url: String,

    /// Base64-encoded audio data (required when source_type = 1). ≤ 5MB.
    #[serde(rename = "Data", skip_serializing_if = "String::is_empty")]
    pub data: String,

    /// Audio data length in bytes (required when source_type = 1).
    #[serde(rename = "DataLen", skip_serializing_if = "is_zero_i64")]
    pub data_len: i64,

    /// Callback URL; results are POSTed there when the task completes.
    #[serde(rename = "CallbackUrl", skip_serializing_if = "String::is_empty")]
    pub callback_url: String,

    /// Profanity filter: 0 off (default), 1 filter, 2 replace with *.
    #[serde(rename = "FilterDirty", skip_serializing_if = "is_zero")]
    pub filter_dirty: i32,

    /// Modal particle filter: 0 off (default), 1 partial, 2 strict.
    #[serde(rename = "FilterModal", skip_serializing_if = "is_zero")]
    pub filter_modal: i32,

    /// Punctuation filter: 0 off (default), 1 trailing, 2 all.
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

    /// Replacement word table ID for forced text replacement.
    #[serde(rename = "ReplaceTextId", skip_serializing_if = "String::is_empty")]
    pub replace_text_id: String,

    /// Forces the audio language on engines that support it. Empty = auto.
    #[serde(rename = "Language", skip_serializing_if = "String::is_empty")]
    pub language: String,

    /// Speaker diarization: 0 off (default), 1 anonymous clustering,
    /// 3 voiceprint role authentication. For stereo (channel_num=2) do NOT
    /// enable this: the server fills `ChannelId` per sentence instead.
    #[serde(rename = "SpeakerDiarization", skip_serializing_if = "is_zero")]
    pub speaker_diarization: i32,

    /// Expected speaker count hint. 0 = auto (default).
    #[serde(rename = "SpeakerNumber", skip_serializing_if = "is_zero")]
    pub speaker_number: i32,

    /// Temporary voiceprints (enrollment audio URL + role name).
    /// Only used when speaker_diarization is 3.
    #[serde(rename = "SpeakerRoles", skip_serializing_if = "Vec::is_empty")]
    pub speaker_roles: Vec<SpeakerRole>,

    /// Previously enrolled voiceprint IDs. Only used when
    /// speaker_diarization is 3.
    #[serde(rename = "VoiceprintIds", skip_serializing_if = "Vec::is_empty")]
    pub voiceprint_ids: Vec<String>,

    /// Silence detection threshold in milliseconds.
    #[serde(rename = "VadSilenceMs", skip_serializing_if = "is_zero")]
    pub vad_silence_ms: i32,

    /// VAD profile: 0 = high recall (default), 1 = far-field filtering.
    /// `Option` so an explicit 0 is distinguishable from "not configured".
    #[serde(rename = "VadLevel", skip_serializing_if = "Option::is_none")]
    pub vad_level: Option<i32>,

    /// VAD noise suppression fine-tuning, range [0, 4]. Overrides the profile
    /// selected by `vad_level` when set.
    #[serde(rename = "NoiseThreshold", skip_serializing_if = "Option::is_none")]
    pub noise_threshold: Option<f64>,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Full task status and result returned by DescribeTaskStatus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskStatus {
    #[serde(rename = "RecTaskId", default)]
    pub rec_task_id: String,
    /// 0 waiting, 1 executing, 2 success, 3 failed.
    #[serde(rename = "Status", default)]
    pub status: i32,
    #[serde(rename = "StatusStr", default)]
    pub status_str: String,
    /// Progress 0-100.
    #[serde(rename = "Progress", default)]
    pub progress: i32,
    #[serde(rename = "Result", default)]
    pub result: String,
    #[serde(rename = "ErrorMsg", default)]
    pub error_msg: String,
    #[serde(rename = "ResultDetail", default)]
    pub result_detail: Vec<SentenceDetail>,
    /// Audio duration in seconds.
    #[serde(rename = "AudioDuration", default)]
    pub audio_duration: f64,
}

/// Sentence-level recognition result with word timing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SentenceDetail {
    #[serde(rename = "FinalSentence", default)]
    pub final_sentence: String,
    #[serde(rename = "SliceSentence", default)]
    pub slice_sentence: String,
    #[serde(rename = "WrittenText", default)]
    pub written_text: String,
    #[serde(rename = "StartMs", default)]
    pub start_ms: i64,
    #[serde(rename = "EndMs", default)]
    pub end_ms: i64,
    #[serde(rename = "WordsNum", default)]
    pub words_num: i32,
    #[serde(rename = "Words", default)]
    pub words: Vec<SentenceWords>,
    #[serde(rename = "SpeechSpeed", default)]
    pub speech_speed: f64,
    #[serde(rename = "SilenceTime", default)]
    pub silence_time: i64,

    /// Speaker number of this sentence, when diarization is enabled.
    #[serde(rename = "SpeakerId", default)]
    pub speaker_id: i32,

    /// Enrolled role name, when diarization=3 matched a requested
    /// role/voiceprint. Empty when no enrolled speaker matched.
    #[serde(rename = "SpeakerRoleName", default)]
    pub speaker_role_name: String,

    /// Audio channel for stereo recordings (channel_num=2): 1=left, 2=right.
    #[serde(rename = "ChannelId", default)]
    pub channel_id: i32,

    /// Detected language of this sentence, when the engine reports one.
    #[serde(rename = "Language", default)]
    pub language: String,
}

/// Word-level timing within a sentence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SentenceWords {
    #[serde(rename = "Word", default)]
    pub word: String,
    #[serde(rename = "OffsetStartMs", default)]
    pub offset_start_ms: i64,
    #[serde(rename = "OffsetEndMs", default)]
    pub offset_end_ms: i64,
}

#[derive(Debug, Deserialize)]
struct CreateRecTaskResponse {
    #[serde(rename = "Response")]
    response: Option<CreateRecTaskBody>,
}

#[derive(Debug, Deserialize)]
struct CreateRecTaskBody {
    #[serde(rename = "Data")]
    data: Option<CreateRecTaskData>,
    #[serde(rename = "RequestId", default)]
    request_id: String,
    #[serde(rename = "Error")]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct CreateRecTaskData {
    #[serde(rename = "RecTaskId", default)]
    rec_task_id: String,
}

#[derive(Debug, Deserialize)]
struct DescribeTaskStatusResponse {
    #[serde(rename = "Response")]
    response: Option<DescribeTaskStatusBody>,
}

#[derive(Debug, Deserialize)]
struct DescribeTaskStatusBody {
    #[serde(rename = "Data")]
    data: Option<TaskStatus>,
    #[serde(rename = "RequestId", default)]
    request_id: String,
    #[serde(rename = "Error")]
    error: Option<ApiError>,
}

/// Client for async audio file recognition.
pub struct FileRecognizer {
    credential: Credential,
    endpoint: String,
    agent: ureq::Agent,
}

impl FileRecognizer {
    pub fn new(credential: Credential) -> Self {
        FileRecognizer {
            credential,
            endpoint: FILE_ENDPOINT.to_string(),
            agent: ureq::AgentBuilder::new()
                // Same shared rustls config as the other transports; see
                // common::tls for why the ureq default is rejected.
                .tls_config(crate::common::tls::rustls_client_config())
                .timeout(Duration::from_secs(60))
                .build(),
        }
    }

    /// Overrides the default API endpoint (for testing).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Overrides the HTTP timeout (default 60s).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.agent = ureq::AgentBuilder::new()
            .tls_config(crate::common::tls::rustls_client_config())
            .timeout(timeout)
            .build();
    }

    /// Submits an audio file recognition task and returns the task ID.
    pub fn create_task(&self, req: &CreateRecTaskRequest) -> Result<String> {
        validate_create_request(req)?;

        let body = serde_json::to_value(req)
            .map_err(|e| invalid_param(format!("marshal request failed: {e}")))?;
        let resp_body = self.do_request("/v1/CreateRecTask", &body)?;

        let resp: CreateRecTaskResponse = serde_json::from_str(&resp_body).map_err(|e| {
            AsrError::new(ERR_CODE_READ_FAILED, format!("unmarshal response failed: {e}"))
        })?;
        let response = resp
            .response
            .ok_or_else(|| AsrError::new(ERR_CODE_SERVER_ERROR, "empty response from server"))?;
        if let Some(err) = response.error {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                format!(
                    "server error [{}]: {} (RequestId: {})",
                    err.code, err.message, response.request_id
                ),
            ));
        }
        let task_id = response
            .data
            .map(|d| d.rec_task_id)
            .unwrap_or_default();
        if task_id.is_empty() {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                "empty RecTaskId in response",
            ));
        }
        Ok(task_id)
    }

    /// Convenience method that submits local audio data (≤ 5MB), handling
    /// base64 encoding automatically.
    pub fn create_task_from_data(
        &self,
        data: &[u8],
        voice_format: &str,
        engine_model_type: &str,
    ) -> Result<String> {
        let _ = voice_format; // kept for API parity with the Go SDK
        if data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if data.len() > MAX_AUDIO_SIZE {
            return Err(invalid_param("audio data exceeds 5MB limit"));
        }
        let req = CreateRecTaskRequest {
            engine_model_type: engine_model_type.to_string(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_DATA,
            data: B64.encode(data),
            data_len: data.len() as i64,
            ..Default::default()
        };
        self.create_task(&req)
    }

    /// Convenience method that submits an audio URL (≤ 12h / ≤ 1GB).
    pub fn create_task_from_url(&self, audio_url: &str, engine_model_type: &str) -> Result<String> {
        if audio_url.is_empty() {
            return Err(invalid_param("audio URL is empty"));
        }
        let req = CreateRecTaskRequest {
            engine_model_type: engine_model_type.to_string(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_URL,
            url: audio_url.to_string(),
            ..Default::default()
        };
        self.create_task(&req)
    }

    /// Submits local audio data with a pre-configured request, handling
    /// base64 encoding automatically. `data`, `data_len` and `source_type`
    /// are set from `raw_data`.
    pub fn create_task_from_data_with_options(
        &self,
        raw_data: &[u8],
        req: &mut CreateRecTaskRequest,
    ) -> Result<String> {
        if raw_data.is_empty() {
            return Err(invalid_param("audio data is empty"));
        }
        if raw_data.len() > MAX_AUDIO_SIZE {
            return Err(invalid_param("audio data exceeds 5MB limit"));
        }
        req.source_type = SOURCE_TYPE_DATA;
        req.data = B64.encode(raw_data);
        req.data_len = raw_data.len() as i64;
        self.create_task(req)
    }

    /// Queries the status of a file recognition task.
    pub fn describe_task_status(&self, rec_task_id: &str) -> Result<TaskStatus> {
        if rec_task_id.is_empty() {
            return Err(invalid_param("RecTaskId is empty"));
        }

        let body = serde_json::json!({ "RecTaskId": rec_task_id });
        let resp_body = self.do_request("/v1/DescribeTaskStatus", &body)?;

        let resp: DescribeTaskStatusResponse = serde_json::from_str(&resp_body).map_err(|e| {
            AsrError::new(ERR_CODE_READ_FAILED, format!("unmarshal response failed: {e}"))
        })?;
        let response = resp
            .response
            .ok_or_else(|| AsrError::new(ERR_CODE_SERVER_ERROR, "empty response from server"))?;
        if let Some(err) = response.error {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                format!(
                    "server error [{}]: {} (RequestId: {})",
                    err.code, err.message, response.request_id
                ),
            ));
        }
        response
            .data
            .ok_or_else(|| AsrError::new(ERR_CODE_SERVER_ERROR, "empty response from server"))
    }

    /// Polls for the result until the task completes or fails.
    /// Default poll interval is 1s, max wait is 10 minutes.
    pub fn wait_for_result(&self, rec_task_id: &str) -> Result<TaskStatus> {
        self.wait_for_result_with_interval(rec_task_id, Duration::from_secs(1), Duration::from_secs(600))
    }

    /// Polls for the result with a custom interval and timeout.
    pub fn wait_for_result_with_interval(
        &self,
        rec_task_id: &str,
        interval: Duration,
        timeout: Duration,
    ) -> Result<TaskStatus> {
        let deadline = Instant::now() + timeout;

        loop {
            let status = self.describe_task_status(rec_task_id)?;

            match status.status {
                TASK_STATUS_SUCCESS => return Ok(status),
                TASK_STATUS_FAILED => {
                    return Err(AsrError::new(
                        ERR_CODE_SERVER_ERROR,
                        format!(
                            "task failed: {} (RecTaskId: {})",
                            status.error_msg, status.rec_task_id
                        ),
                    ))
                }
                _ => {}
            }

            if Instant::now() > deadline {
                return Err(AsrError::new(
                    ERR_CODE_TIMEOUT,
                    format!(
                        "task not completed within {:?} (RecTaskId: {}, Status: {})",
                        timeout, rec_task_id, status.status_str
                    ),
                ));
            }

            std::thread::sleep(interval);
        }
    }

    /// Sends an HTTP POST to the given API path with a JSON body and returns
    /// the response body.
    fn do_request(&self, path: &str, body: &serde_json::Value) -> Result<String> {
        let request_id = Uuid::new_v4().to_string();

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
        // Applies to both CreateRecTask and DescribeTaskStatus, which share
        // this request path.
        let req_url = format!(
            "{}{}?AppId={}&Secretid={}&RequestId={}&Timestamp={}&{}",
            self.endpoint,
            path,
            self.credential.app_id,
            self.credential.app_id,
            request_id,
            now,
            sdkinfo::sdk_report_query()
        );

        let http_resp = self
            .agent
            .post(&req_url)
            .set("Content-Type", "application/json; charset=utf-8")
            .set("X-TRTC-SdkAppId", &self.credential.sdk_app_id.to_string())
            .set("X-TRTC-UserSig", &user_sig)
            .send_string(&body.to_string());

        let (status, resp_body) = read_http_response(http_resp)?;
        if status != 200 {
            return Err(AsrError::new(
                ERR_CODE_SERVER_ERROR,
                format!("http status {status}: {resp_body}"),
            ));
        }
        Ok(resp_body)
    }
}

fn validate_create_request(req: &CreateRecTaskRequest) -> Result<()> {
    if req.engine_model_type.is_empty() {
        return Err(invalid_param("EngineModelType is required"));
    }
    if req.channel_num <= 0 {
        return Err(invalid_param("ChannelNum must be positive"));
    }
    if req.source_type == SOURCE_TYPE_URL && req.url.is_empty() {
        return Err(invalid_param("Url is required when SourceType=0"));
    }
    if req.source_type == SOURCE_TYPE_DATA && req.data.is_empty() {
        return Err(invalid_param("Data is required when SourceType=1"));
    }
    validate_speaker_diarization(
        req.speaker_diarization,
        req.speaker_number,
        &req.speaker_roles,
        &req.voiceprint_ids,
    )?;
    validate_vad_tuning(req.vad_level, req.noise_threshold)
}
