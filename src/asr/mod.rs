//! ASR speech recognition module.

pub mod file_recognizer;
pub mod params;
pub mod sentence_recognizer;
pub mod speech_recognizer;
pub mod types;

pub use file_recognizer::{
    CreateRecTaskRequest, FileRecognizer, SentenceDetail, SentenceWords, TaskStatus,
    FILE_ENDPOINT, TASK_STATUS_FAILED, TASK_STATUS_RUNNING, TASK_STATUS_SUCCESS,
    TASK_STATUS_WAITING,
};
pub use sentence_recognizer::{
    SentenceRecognitionRequest, SentenceRecognitionResult, SentenceRecognizer, SentenceWord,
    SENTENCE_ENDPOINT, SOURCE_TYPE_DATA, SOURCE_TYPE_URL,
};
pub use speech_recognizer::{SpeechRecognitionListener, SpeechRecognizer, ENDPOINT};
pub use types::{
    RecognitionResult, SpeakerSegment, SpeechRecognitionResponse, WordInfo,
};

pub use crate::common::{
    SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF, SPEAKER_DIARIZATION_VOICEPRINT,
};
