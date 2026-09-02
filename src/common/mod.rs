//! Shared types and utilities for the TRTC-ASR SDK.

pub mod credential;
pub mod errors;
pub mod signature;
pub mod usersig;

pub use credential::Credential;
pub use errors::{AsrError, Result};
pub use signature::{
    SpeakerRole, SignatureParams, SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF,
    SPEAKER_DIARIZATION_VOICEPRINT,
};
pub use usersig::{gen_user_sig, DEFAULT_EXPIRE};
