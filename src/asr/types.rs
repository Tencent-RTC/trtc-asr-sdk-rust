//! Response types shared by the recognizers.

use serde::{Deserialize, Serialize};

/// A response message from the ASR service (realtime WebSocket protocol).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeechRecognitionResponse {
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default)]
    pub message_id: String,
    /// 1 marks the session-ending frame.
    #[serde(default, rename = "final")]
    pub final_flag: i32,
    #[serde(default)]
    pub result: RecognitionResult,
}

/// Recognition result details carried by a realtime response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecognitionResult {
    /// 0 = sentence begin, 1 = intermediate result, 2 = sentence-final result.
    #[serde(default)]
    pub slice_type: i32,
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub start_time: i64,
    #[serde(default)]
    pub end_time: i64,
    #[serde(default)]
    pub voice_text_str: String,
    #[serde(default)]
    pub word_size: i32,
    #[serde(default)]
    pub word_list: Vec<WordInfo>,
    /// Detected language when the engine reports one.
    #[serde(default)]
    pub language: String,

    /// Speaker attribution of this result, split by speaker turn. This is the
    /// recommended entry point for speaker diarization: one result may contain
    /// several speakers, so a sentence-level speaker is ambiguous by design.
    /// Empty when diarization is disabled. A result is single-speaker when
    /// `speaker_segments.len() == 1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speaker_segments: Vec<SpeakerSegment>,

    /// Legacy sentence-level speaker attribution. `Option` because 0 is a
    /// reserved value and the field is absent on most engines. Prefer
    /// `speaker_segments` / [`WordInfo::speaker_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<i32>,

    /// Trailing silence (ms) that triggered the sentence break; 0 when the
    /// server does not report it.
    #[serde(default)]
    pub finish_silence_ms: i64,

    /// Server-side decoding time (ms) of the last token; 0 when the server
    /// does not report it.
    #[serde(default)]
    pub last_token_runtime_ms: i64,
}

/// A contiguous section of one result attributed to a single speaker.
/// Returned when speaker diarization is enabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeakerSegment {
    /// Speaker number within the current session. Valid IDs start at 1,
    /// -1 means unknown, 0 is reserved.
    #[serde(default)]
    pub speaker_id: i32,

    /// Enrolled role name, returned only with `speaker_diarization=3`. Equals
    /// the requested `SpeakerRole.role_name`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub speaker_name: String,

    #[serde(default)]
    pub start_time: i64,
    #[serde(default)]
    pub end_time: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,

    /// Inclusive indexes into `word_list`, i.e. `word_list[word_start..=word_end]`.
    /// Both are `None` when `word_info=0` (no word list to index into);
    /// 0 is a valid index, hence the `Option`s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_start: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_end: Option<i32>,

    /// Whether this segment is stable: 1 = stable, 0 = not.
    #[serde(default)]
    pub stable_flag: i32,
}

/// Word-level recognition details.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordInfo {
    #[serde(default)]
    pub word: String,
    #[serde(default)]
    pub start_time: i64,
    #[serde(default)]
    pub end_time: i64,
    #[serde(default)]
    pub stable_flag: i32,

    /// Speaker of this word, filled when diarization is enabled together with
    /// `word_info != 0`. Valid IDs start at 1, -1 unknown, 0 absent.
    #[serde(default)]
    pub speaker_id: i32,

    /// Enrolled role name, returned only with `speaker_diarization=3`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub speaker_name: String,
}
