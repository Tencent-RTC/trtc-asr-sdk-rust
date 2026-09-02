//! Parameter validation tests, ported from the Go SDK's params_test.go.

use trtc_asr_sdk::asr::params::{
    validate_enum_option, validate_speaker_diarization, validate_vad_tuning,
};
use trtc_asr_sdk::common::signature::{
    SpeakerRole, SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_OFF,
    SPEAKER_DIARIZATION_VOICEPRINT,
};

fn valid_role() -> SpeakerRole {
    SpeakerRole {
        role_name: "teacher".into(),
        audio_url: "https://example.com/a.wav".into(),
    }
}

#[test]
fn diarization_valid_cases() {
    validate_speaker_diarization(SPEAKER_DIARIZATION_OFF, 0, &[], &[]).unwrap();
    validate_speaker_diarization(SPEAKER_DIARIZATION_CLUSTER, 0, &[], &[]).unwrap();
    validate_speaker_diarization(SPEAKER_DIARIZATION_CLUSTER, 2, &[], &[]).unwrap();
    validate_speaker_diarization(
        SPEAKER_DIARIZATION_VOICEPRINT,
        2,
        &[valid_role()],
        &["vp-1".to_string()],
    )
    .unwrap();
}

#[test]
fn diarization_invalid_cases() {
    let cases: Vec<(i32, i32, Vec<SpeakerRole>, Vec<String>, &str)> = vec![
        (2, 0, vec![], vec![], "SpeakerDiarization must be 0"),
        (
            SPEAKER_DIARIZATION_CLUSTER,
            -1,
            vec![],
            vec![],
            "SpeakerNumber must be >= 0",
        ),
        (
            SPEAKER_DIARIZATION_CLUSTER,
            0,
            vec![valid_role()],
            vec![],
            "require SpeakerDiarization=3",
        ),
        (
            SPEAKER_DIARIZATION_OFF,
            0,
            vec![],
            vec!["vp-1".into()],
            "require SpeakerDiarization=3",
        ),
        (
            SPEAKER_DIARIZATION_VOICEPRINT,
            0,
            vec![SpeakerRole {
                role_name: "".into(),
                audio_url: "https://example.com/a.wav".into(),
            }],
            vec![],
            "RoleName is empty",
        ),
        (
            SPEAKER_DIARIZATION_VOICEPRINT,
            0,
            vec![SpeakerRole {
                role_name: "teacher".into(),
                audio_url: "".into(),
            }],
            vec![],
            "AudioUrl is empty",
        ),
        (
            SPEAKER_DIARIZATION_VOICEPRINT,
            0,
            vec![SpeakerRole {
                role_name: "teacher".into(),
                audio_url: "file:///etc/passwd".into(),
            }],
            vec![],
            "must use http or https",
        ),
        (
            SPEAKER_DIARIZATION_VOICEPRINT,
            0,
            vec![SpeakerRole {
                role_name: "teacher".into(),
                audio_url: "https:///a.wav".into(),
            }],
            vec![],
            "has no host",
        ),
        (
            SPEAKER_DIARIZATION_VOICEPRINT,
            0,
            vec![],
            vec!["".into()],
            "VoiceprintIds[0] is empty",
        ),
    ];

    for (mode, number, roles, ids, want) in cases {
        let err = validate_speaker_diarization(mode, number, &roles, &ids)
            .expect_err(&format!("expected error containing {want:?}"));
        assert!(
            err.message.contains(want),
            "error {:?} should contain {:?}",
            err.message,
            want
        );
    }
}

#[test]
fn diarization_allows_internal_host() {
    // This SDK is customer-facing: internal hosts belong to the caller's own
    // network and stay fetchable for the service, so no SSRF-style blocking.
    validate_speaker_diarization(
        SPEAKER_DIARIZATION_VOICEPRINT,
        0,
        &[SpeakerRole {
            role_name: "teacher".into(),
            audio_url: "http://192.168.1.10/a.wav".into(),
        }],
        &[],
    )
    .unwrap();
}

#[test]
fn vad_tuning_valid_cases() {
    validate_vad_tuning(None, None).unwrap();
    validate_vad_tuning(Some(0), None).unwrap();
    validate_vad_tuning(Some(1), None).unwrap();
    validate_vad_tuning(None, Some(0.0)).unwrap();
    validate_vad_tuning(None, Some(4.0)).unwrap();
}

#[test]
fn vad_tuning_invalid_cases() {
    let err = validate_vad_tuning(Some(2), None).unwrap_err();
    assert!(err.message.contains("VadLevel must be 0"));

    for bad in [-0.5, 4.5, f64::NAN, f64::INFINITY] {
        let err = validate_vad_tuning(None, Some(bad)).unwrap_err();
        assert!(
            err.message.contains("NoiseThreshold must be between"),
            "value {bad}: {}",
            err.message
        );
    }
}

#[test]
fn enum_option_validation() {
    validate_enum_option("InputSampleRate", 0, &[0, 8000]).unwrap();
    validate_enum_option("InputSampleRate", 8000, &[0, 8000]).unwrap();
    let err = validate_enum_option("InputSampleRate", 16000, &[0, 8000]).unwrap_err();
    assert!(err.message.contains("InputSampleRate must be one of"));
}
