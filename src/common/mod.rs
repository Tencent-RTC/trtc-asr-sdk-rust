//! Shared types and utilities for the TRTC-ASR SDK.

pub mod credential;
pub mod errors;
pub mod sdkinfo;
pub mod signature;
pub(crate) mod tls;
pub mod usersig;

pub use credential::{
    host_for_site, http_endpoint_for_site, resolve_http_endpoint, resolve_ws_endpoint,
    ws_endpoint_for_site, Credential, HOST_CN, HOST_INTL, SITE_CN, SITE_INTL,
};
pub use errors::{AsrError, Result};
pub use sdkinfo::{
    sdk_platform, sdk_report_params, sdk_report_query, SDK_LANGUAGE, SDK_TYPE, SDK_VERSION,
};
pub use signature::{
    SpeakerRole, SignatureParams, SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF,
    SPEAKER_DIARIZATION_VOICEPRINT,
};
pub use usersig::{gen_user_sig, DEFAULT_EXPIRE};
