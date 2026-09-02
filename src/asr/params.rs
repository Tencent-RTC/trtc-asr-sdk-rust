//! Shared parameter validation for the recognizers.
//!
//! The service validates every parameter as well, but rejecting an obviously
//! invalid value locally turns a remote 4001 ("参数不合法") into an immediate,
//! descriptive error and avoids burning a connection or a task quota.

use url::Url;

use crate::common::errors::{invalid_param, Result};
use crate::common::signature::{
    SpeakerRole, SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF,
    SPEAKER_DIARIZATION_VOICEPRINT,
};

/// Server-side accepted noise threshold range.
pub const MIN_NOISE_THRESHOLD: f64 = 0.0;
pub const MAX_NOISE_THRESHOLD: f64 = 4.0;

/// Checks the diarization mode and its enrollment input. `roles` /
/// `voiceprint_ids` are only meaningful with mode 3, but supplying them for
/// another mode is a caller mistake worth surfacing.
pub fn validate_speaker_diarization(
    mode: i32,
    speaker_number: i32,
    roles: &[SpeakerRole],
    voiceprint_ids: &[String],
) -> Result<()> {
    match mode {
        SPEAKER_DIARIZATION_OFF | SPEAKER_DIARIZATION_CLUSTER | SPEAKER_DIARIZATION_VOICEPRINT => {}
        _ => {
            return Err(invalid_param(format!(
                "SpeakerDiarization must be 0 (off), 1 (cluster) or 3 (voiceprint), got {mode}"
            )))
        }
    }

    if speaker_number < 0 {
        return Err(invalid_param(format!(
            "SpeakerNumber must be >= 0 (0 = auto detection), got {speaker_number}"
        )));
    }

    if mode != SPEAKER_DIARIZATION_VOICEPRINT && (!roles.is_empty() || !voiceprint_ids.is_empty()) {
        return Err(invalid_param(
            "SpeakerRoles/VoiceprintIds require SpeakerDiarization=3",
        ));
    }

    for (i, role) in roles.iter().enumerate() {
        if role.role_name.is_empty() {
            return Err(invalid_param(format!("SpeakerRoles[{i}].RoleName is empty")));
        }
        validate_enrollment_url(i, &role.audio_url)?;
    }

    for (i, id) in voiceprint_ids.iter().enumerate() {
        if id.is_empty() {
            return Err(invalid_param(format!("VoiceprintIds[{i}] is empty")));
        }
    }

    Ok(())
}

/// Requires an absolute http(s) URL for enrollment audio.
///
/// The URL is fetched by the ASR service, not by the SDK: this is a
/// customer-facing client library, so it only rejects inputs that can never
/// work (bad syntax, non-http scheme, missing host). Reachability and network
/// policies belong to the service-side allow list.
fn validate_enrollment_url(index: usize, raw_url: &str) -> Result<()> {
    if raw_url.trim().is_empty() {
        return Err(invalid_param(format!("SpeakerRoles[{index}].AudioUrl is empty")));
    }

    // Split scheme://authority explicitly: WHATWG URL parsing (the url crate)
    // normalizes "https:///a.wav" into host "a.wav", while Go's
    // url.ParseRequestURI — the reference behavior — reports an empty host.
    // Checking the authority substring preserves the Go semantics.
    let (scheme, rest) = match raw_url.split_once("://") {
        Some(parts) => parts,
        None => {
            return Err(invalid_param(format!(
                "SpeakerRoles[{index}].AudioUrl is not a valid URL: missing scheme"
            )))
        }
    };
    if scheme != "http" && scheme != "https" {
        return Err(invalid_param(format!(
            "SpeakerRoles[{index}].AudioUrl must use http or https, got {scheme:?}"
        )));
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.is_empty() {
        return Err(invalid_param(format!(
            "SpeakerRoles[{index}].AudioUrl has no host"
        )));
    }
    // General syntax sanity check on top of the manual splits.
    Url::parse(raw_url).map_err(|e| {
        invalid_param(format!("SpeakerRoles[{index}].AudioUrl is not a valid URL: {e}"))
    })?;
    Ok(())
}

/// Checks the VAD profile and noise threshold.
pub fn validate_vad_tuning(vad_level: Option<i32>, noise_threshold: Option<f64>) -> Result<()> {
    if let Some(level) = vad_level {
        if level != 0 && level != 1 {
            return Err(invalid_param(format!(
                "VadLevel must be 0 (high recall) or 1 (far-field filtering), got {level}"
            )));
        }
    }
    if let Some(v) = noise_threshold {
        // NaN fails every comparison, so test the valid range positively.
        if !(v >= MIN_NOISE_THRESHOLD && v <= MAX_NOISE_THRESHOLD) {
            return Err(invalid_param(format!(
                "NoiseThreshold must be between {MIN_NOISE_THRESHOLD:.1} and {MAX_NOISE_THRESHOLD:.1}, got {v}"
            )));
        }
    }
    Ok(())
}

/// Checks a small enumerated option such as `input_sample_rate`.
pub fn validate_enum_option(name: &str, value: i32, allowed: &[i32]) -> Result<()> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(invalid_param(format!(
        "{name} must be one of {allowed:?}, got {value}"
    )))
}
