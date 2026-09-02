//! SignatureParams query-building tests, ported from the Go SDK's
//! signature_test.go / signature_speaker_test.go.

use trtc_asr_sdk::common::signature::{
    query_escape, SpeakerRole, SignatureParams, SPEAKER_DIARIZATION_CLUSTER,
    SPEAKER_DIARIZATION_VOICEPRINT,
};

fn query_get(qs: &str, key: &str) -> Option<String> {
    for pair in qs.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            return kv.next().map(decode);
        }
    }
    None
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'%'));
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn new_signature_params_defaults() {
    let p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    assert_eq!(p.app_id, 1300403317);
    assert_eq!(p.engine_model_type, "16k_zh");
    assert_eq!(p.voice_id, "voice-001");
    assert_eq!(p.voice_format, 1);
    assert_eq!(p.need_vad, 1);
    assert_ne!(p.timestamp, 0);
    assert!(p.expired > p.timestamp);
    assert!(p.nonce >= 1 && p.nonce <= 9_999_999);
}

#[test]
fn build_query_string_contains_required_keys() {
    let p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    let qs = p.build_query_string();
    assert!(!qs.is_empty());
    for key in [
        "secretid=",
        "timestamp=",
        "expired=",
        "nonce=",
        "engine_model_type=",
        "voice_id=",
    ] {
        assert!(qs.contains(key), "missing {key} in {qs}");
    }
    assert!(qs.contains("secretid=1300403317"));
    assert!(!qs.contains("signature="), "must not contain signature: {qs}");
}

#[test]
fn build_query_string_with_signature() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.sdk_app_id = 1400000000;
    let user_sig = "eJwtzDEOgCAQRdG9UBMH-test-user-sig";
    let qs = p.build_query_string_with_signature(user_sig);

    assert_eq!(query_get(&qs, "signature").as_deref(), Some(user_sig));
    assert_eq!(query_get(&qs, "usersig").as_deref(), Some(user_sig));
    assert_eq!(query_get(&qs, "sdkappid").as_deref(), Some("1400000000"));
    for key in ["secretid", "timestamp", "expired", "nonce"] {
        assert!(query_get(&qs, key).is_some(), "missing {key}");
    }
}

#[test]
fn secret_key_never_in_query_string() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.sdk_app_id = 1400000000;
    let qs = p.build_query_string_with_signature("some-user-sig");
    assert!(!qs.contains("secret_key"));
    assert!(!qs.contains("secretkey"));
}

#[test]
fn omits_unset_optional_params() {
    let p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    let qs = p.build_query_string();
    for key in [
        "speaker_diarization",
        "speaker_number",
        "speaker_roles",
        "voiceprintids",
        "noise_threshold",
        "vad_level",
        "filter_empty_result",
        "hotword_list",
        "replace_text_id",
        "input_sample_rate",
        "sdkappid",
        "language",
    ] {
        assert!(!qs.contains(&format!("{key}=")), "{key} should be omitted: {qs}");
    }
}

#[test]
fn speaker_diarization_cluster_query() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.speaker_diarization = SPEAKER_DIARIZATION_CLUSTER;
    p.speaker_number = 2;
    // Enrollment input only applies to mode 3 and must not leak into mode 1.
    p.speaker_roles = vec![SpeakerRole {
        role_name: "teacher".into(),
        audio_url: "https://example.com/a.wav".into(),
    }];
    p.voiceprint_ids = vec!["vp-1".into()];

    let qs = p.build_query_string();
    assert_eq!(query_get(&qs, "speaker_diarization").as_deref(), Some("1"));
    assert_eq!(query_get(&qs, "speaker_number").as_deref(), Some("2"));
    assert!(query_get(&qs, "speaker_roles").is_none());
    assert!(query_get(&qs, "voiceprintids").is_none());
}

#[test]
fn speaker_diarization_voiceprint_query() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.speaker_diarization = SPEAKER_DIARIZATION_VOICEPRINT;
    p.speaker_roles = vec![
        SpeakerRole {
            role_name: "teacher".into(),
            audio_url: "https://example.com/a.wav".into(),
        },
        SpeakerRole {
            role_name: "student".into(),
            audio_url: "https://example.com/b.wav".into(),
        },
    ];
    p.voiceprint_ids = vec!["vp-1".into(), "vp-2".into()];
    p.speaker_number = 0; // auto detection: parameter is omitted

    let qs = p.build_query_string();
    assert_eq!(query_get(&qs, "speaker_diarization").as_deref(), Some("3"));
    assert!(query_get(&qs, "speaker_number").is_none());

    let roles_json = query_get(&qs, "speaker_roles").expect("speaker_roles present");
    let roles: Vec<SpeakerRole> = serde_json::from_str(&roles_json).expect("valid JSON");
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0].role_name, "teacher");
    assert_eq!(roles[1].audio_url, "https://example.com/b.wav");

    let ids_json = query_get(&qs, "voiceprintids").expect("voiceprintids present");
    let ids: Vec<String> = serde_json::from_str(&ids_json).expect("valid JSON");
    assert_eq!(ids, vec!["vp-1", "vp-2"]);
}

#[test]
fn tri_state_vad_tuning() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.vad_level = Some(0);
    p.noise_threshold = Some(0.0);
    p.filter_empty_result = Some(0);

    // An explicit 0 differs from "unset": the server defaults vad_level to 1
    // and filter_empty_result to 1, so both must reach the wire.
    let qs = p.build_query_string();
    assert_eq!(query_get(&qs, "vad_level").as_deref(), Some("0"));
    assert_eq!(query_get(&qs, "filter_empty_result").as_deref(), Some("0"));
    // Go strconv.FormatFloat('f', 3): "0.000".
    assert_eq!(query_get(&qs, "noise_threshold").as_deref(), Some("0.000"));

    p.noise_threshold = Some(1.5);
    let qs = p.build_query_string();
    assert_eq!(query_get(&qs, "noise_threshold").as_deref(), Some("1.500"));
}

#[test]
fn advanced_optional_params() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.hotword_list = "腾讯云|5,ASR|11".into();
    p.replace_text_id = "replace-1".into();
    p.input_sample_rate = 8000;
    p.language = "zh".into();

    let qs = p.build_query_string();
    assert_eq!(query_get(&qs, "hotword_list").as_deref(), Some("腾讯云|5,ASR|11"));
    assert_eq!(query_get(&qs, "replace_text_id").as_deref(), Some("replace-1"));
    assert_eq!(query_get(&qs, "input_sample_rate").as_deref(), Some("8000"));
    assert_eq!(query_get(&qs, "language").as_deref(), Some("zh"));
}

#[test]
fn query_string_keys_are_sorted() {
    let mut p = SignatureParams::new(1300403317, "16k_zh", "voice-001");
    p.hotword_id = "hw".into();
    let qs = p.build_query_string_with_signature("sig");
    let keys: Vec<&str> = qs.split('&').map(|kv| kv.splitn(2, '=').next().unwrap()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "query keys must be sorted like Go's sort.Strings");
}

#[test]
fn query_escape_matches_go_semantics() {
    assert_eq!(query_escape("abcXYZ019-_.~"), "abcXYZ019-_.~");
    assert_eq!(query_escape("a b"), "a+b");
    assert_eq!(query_escape("a+b"), "a%2Bb");
    assert_eq!(query_escape("词|5,"), "%E8%AF%8D%7C5%2C");
    assert_eq!(query_escape("100%"), "100%25");
}
