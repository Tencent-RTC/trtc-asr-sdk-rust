//! Error definitions for the TRTC-ASR SDK.

use std::fmt;

/// Error codes for TRTC-ASR SDK.
pub const ERR_CODE_INVALID_PARAM: i32 = 1001;
pub const ERR_CODE_CONNECT_FAILED: i32 = 1002;
pub const ERR_CODE_WRITE_FAILED: i32 = 1003;
pub const ERR_CODE_READ_FAILED: i32 = 1004;
pub const ERR_CODE_AUTH_FAILED: i32 = 1005;
pub const ERR_CODE_TIMEOUT: i32 = 1006;
pub const ERR_CODE_SERVER_ERROR: i32 = 1007;
pub const ERR_CODE_ALREADY_STARTED: i32 = 1008;
pub const ERR_CODE_NOT_STARTED: i32 = 1009;
pub const ERR_CODE_ALREADY_STOPPED: i32 = 1010;

/// An error returned by the TRTC-ASR service or the SDK itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrError {
    pub code: i32,
    pub message: String,
}

impl AsrError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        AsrError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AsrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trtc-asr error [{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for AsrError {}

/// Convenience alias used across the SDK.
pub type Result<T> = std::result::Result<T, AsrError>;

pub(crate) fn invalid_param(message: impl Into<String>) -> AsrError {
    AsrError::new(ERR_CODE_INVALID_PARAM, message)
}
