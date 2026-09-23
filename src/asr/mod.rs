//! ASR speech recognition module.

pub mod file_recognizer;
pub mod params;
pub mod sentence_recognizer;
pub mod speech_recognizer;
pub mod types;
pub mod v3;

pub use file_recognizer::{
    CreateRecTaskRequest, FileRecognizer, SentenceDetail, SentenceWords, TaskStatus, FILE_ENDPOINT,
    TASK_STATUS_FAILED, TASK_STATUS_RUNNING, TASK_STATUS_SUCCESS, TASK_STATUS_WAITING,
};
pub use sentence_recognizer::{
    SentenceRecognitionRequest, SentenceRecognitionResult, SentenceRecognizer, SentenceWord,
    SENTENCE_ENDPOINT, SOURCE_TYPE_DATA, SOURCE_TYPE_URL,
};
pub use speech_recognizer::{SpeechRecognitionListener, SpeechRecognizer, ENDPOINT};
pub use types::{
    RecognitionResult, SpeakerContinue, SpeakerSegment, SpeechRecognitionResponse, WordInfo,
};
pub use v3::{
    CONTINUE_STATUS_DEGRADED, CONTINUE_STATUS_DISABLED, CONTINUE_STATUS_FRESH,
    CONTINUE_STATUS_RESUMED, SPEAKER_CONTEXT_ASYNC, SPEAKER_CONTEXT_OFF, SPEAKER_CONTEXT_SYNC,
};

pub use crate::common::{
    SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF, SPEAKER_DIARIZATION_VOICEPRINT,
};
