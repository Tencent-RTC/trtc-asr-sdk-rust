//! v3 offline interface tests (/v3/transcribe, /v3/create_transcription,
//! /v3/describe_transcription): {auth, params} envelope, snake_case wire,
//! flat responses and error mapping. Mirrors Go asr/v3 sentence/file tests.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{MockHttpResponse, MockHttpServer};

use trtc_asr_sdk::asr::v3::{
    new_credential, AudioURLItem, Context, CreateTranscriptionRequest, FileRecognizer,
    SentenceRecognizer, SpeakerRole, TranscribeRequest, SOURCE_TYPE_DATA, SOURCE_TYPE_URL,
    SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_VOICEPRINT, TASK_STATUS_SUCCESS,
};
use trtc_asr_sdk::common::errors::{ERR_CODE_INVALID_PARAM, ERR_CODE_SERVER_ERROR};

fn test_credential() -> trtc_asr_sdk::common::Credential {
    new_credential(1400000000, "test-secret")
}

// ---------------------------------------------------------------- transcribe

#[test]
fn transcribe_wire_format() {
    let server = MockHttpServer::start(|req| {
        assert_eq!(req.method, "POST");
        assert_eq!(req.target, "/v3/transcribe", "v3 has no query string");
        assert_eq!(
            req.header("Content-Type"),
            Some("application/json; charset=utf-8")
        );
        // v3 carries identity in the body, not headers.
        assert!(req.header("X-TRTC-UserSig").is_none());
        assert!(req.header("X-TRTC-SdkAppId").is_none());

        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        let auth = &body["auth"];
        assert_eq!(auth["sdkappid"], "1400000000");
        assert!(!auth["usersig"].as_str().unwrap_or("").is_empty());
        assert!(!auth["request_id"].as_str().unwrap_or("").is_empty());
        // business is a server-side internal gray dimension; never sent.
        assert!(auth.get("business").is_none());

        let params = &body["params"];
        assert_eq!(params["engine_model_type"], "16k_zh_en");
        assert_eq!(params["source_type"], 1);
        assert_eq!(params["voice_format"], "pcm");
        assert_eq!(params["needvad"], 0, "explicit 0 must be honored");
        assert_eq!(params["language"], "zh");
        assert_eq!(params["word_info"], 1);
        assert_eq!(params["input_sample_rate"], 8000);
        assert_eq!(params["customization_id"], "cust-1");
        assert_eq!(
            params["context"],
            serde_json::json!({"text": "bg", "terms": ["ASR"]})
        );
        assert_eq!(params["sdk_info"]["sdk_lang"], "rust");
        // snake_case everywhere — v1 PascalCase names must not appear.
        for bad in ["EngSerViceType", "SourceType", "VoiceFormat", "Data", "Url"] {
            assert!(params.get(bad).is_none(), "{bad} must not appear");
        }

        MockHttpResponse::json(
            r#"{"code":0,"message":"success","request_id":"req-1","result":"你好世界",
                "audio_duration":1500,"language":"zh","word_size":2,
                "word_list":[{"word":"你好","start_time":0,"end_time":500},
                             {"word":"世界","start_time":500,"end_time":1500}]}"#,
        )
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let resp = r
        .recognize(&TranscribeRequest {
            engine_model_type: "16k_zh_en".into(),
            source_type: SOURCE_TYPE_DATA,
            voice_format: "pcm".into(),
            data: "cG0tZGF0YQ==".into(),
            data_len: 8,
            hotword_list: "腾讯云|10".into(),
            needvad: Some(0), // explicit 0 honored
            language: "zh".into(),
            convert_num_mode: 1,
            word_info: 1,
            filter_punc: 1,
            input_sample_rate: 8000,
            customization_id: "cust-1".into(),
            context: Some(Context {
                text: "bg".into(),
                terms: vec!["ASR".into()],
                general: vec![],
            }),
            ..Default::default()
        })
        .expect("transcribe");

    assert_eq!(resp.code, 0);
    assert_eq!(resp.result, "你好世界");
    assert_eq!(resp.audio_duration, 1500);
    assert_eq!(resp.request_id, "req-1");
    assert_eq!(resp.word_list.len(), 2);
    assert_eq!(resp.word_list[0].word, "你好");
    assert_eq!(
        (resp.word_list[0].start_time, resp.word_list[0].end_time),
        (0, 500)
    );
}

#[test]
fn transcribe_auth_error_is_body_code_http200() {
    // v3 answers auth failures with HTTP 200 and body code 4002.
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(r#"{"code":4002,"message":"auth failed","request_id":"req-x"}"#)
    });
    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let err = r
        .recognize_url("https://example.com/a.wav", "wav", "16k_zh_en")
        .expect_err("must fail with the body code");
    assert_eq!(err.code, 4002);
    assert!(err.message.contains("req-x"));
}

#[test]
fn transcribe_non_v3_json_body_is_not_success() {
    // A gateway/LB JSON error page (no numeric code) on a non-2xx status
    // must not decode to code==0 success.
    let server = MockHttpServer::start(|_req| MockHttpResponse {
        status: 502,
        body: r#"{"error":"upstream unavailable"}"#.into(),
    });
    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let err = r
        .recognize_url("https://example.com/a.wav", "wav", "16k_zh_en")
        .expect_err("non-v3 body must fail");
    assert_eq!(err.code, ERR_CODE_SERVER_ERROR);
    assert!(err.message.contains("502"));
}

#[test]
fn transcribe_local_validation() {
    let mut r = SentenceRecognizer::new(test_credential());
    assert!(r.recognize_data(b"", "pcm", "16k_zh_en").is_err());
    assert!(r
        .recognize(&TranscribeRequest {
            source_type: SOURCE_TYPE_DATA,
            voice_format: "pcm".into(),
            ..Default::default()
        })
        .is_err());
    // needvad=2 fails locally.
    assert!(r
        .recognize(&TranscribeRequest {
            engine_model_type: "16k_zh_en".into(),
            source_type: SOURCE_TYPE_URL,
            voice_format: "wav".into(),
            url: "https://example.com/a.wav".into(),
            needvad: Some(2),
            ..Default::default()
        })
        .is_err());
    // input_sample_rate=16000 fails locally (8000 is the only override).
    assert!(r
        .recognize(&TranscribeRequest {
            engine_model_type: "16k_zh_en".into(),
            source_type: SOURCE_TYPE_URL,
            voice_format: "wav".into(),
            url: "https://example.com/a.wav".into(),
            input_sample_rate: 16000,
            ..Default::default()
        })
        .is_err());
}

// ---------------------------------------------------------------- create

#[test]
fn create_task_wire_format() {
    let server = MockHttpServer::start(|req| {
        assert_eq!(req.target, "/v3/create_transcription");
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["auth"]["sdkappid"], "1400000000");
        let params = &body["params"];
        for key in [
            "engine_model_type",
            "channel_num",
            "res_text_format",
            "source_type",
            "url",
            "callback_url",
            "hotword_id",
            "vad_silence_ms",
            "vad_level",
            "language",
            "speaker_diarization",
            "speaker_roles",
            "voiceprint_ids",
            "sdk_info",
        ] {
            assert!(params.get(key).is_some(), "{key} must be present");
        }
        for bad in ["EngineModelType", "ChannelNum", "ResTextFormat", "Url"] {
            assert!(params.get(bad).is_none(), "{bad} must not appear");
        }
        assert_eq!(
            params["speaker_roles"],
            serde_json::json!([{"role_name": "teacher", "audio_url": "https://example.com/t.wav"}])
        );
        MockHttpResponse::json(
            r#"{"code":0,"message":"success","request_id":"r","transcription_id":"tid-abc"}"#,
        )
    });

    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let id = r
        .create_task(&CreateTranscriptionRequest {
            engine_model_type: "16k_zh_en".into(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_URL,
            url: "https://example.com/a.wav".into(),
            callback_url: "https://example.com/cb".into(),
            hotword_id: "hw-1".into(),
            vad_silence_ms: 600,
            vad_level: Some(1),
            language: "zh".into(),
            speaker_diarization: SPEAKER_DIARIZATION_VOICEPRINT,
            speaker_roles: vec![SpeakerRole {
                role_name: "teacher".into(),
                audio_url: "https://example.com/t.wav".into(),
            }],
            voiceprint_ids: vec!["vp-1".into()],
            ..Default::default()
        })
        .expect("create task");
    assert_eq!(id, "tid-abc");
}

#[test]
fn create_task_audio_urls_distributed_path() {
    // Distributed recording: audio_urls requires source_type=0 with url/data
    // empty — the SDK must let it through and serialize audio_urls.
    let server = MockHttpServer::start(|req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        let params = &body["params"];
        assert_eq!(
            params["audio_urls"],
            serde_json::json!([
                {"index": 0, "url": "https://example.com/a.wav", "label": "a"},
                {"index": 1, "url": "https://example.com/b.wav"}
            ])
        );
        assert!(params.get("url").is_none());
        assert!(params.get("data").is_none());
        MockHttpResponse::json(
            r#"{"code":0,"message":"success","request_id":"r","transcription_id":"tid-d"}"#,
        )
    });

    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let id = r
        .create_task(&CreateTranscriptionRequest {
            engine_model_type: "16k_zh_en".into(),
            channel_num: 1,
            res_text_format: 1,
            source_type: SOURCE_TYPE_URL, // zero value; url/data stay empty
            speaker_diarization: SPEAKER_DIARIZATION_CLUSTER,
            audio_urls: vec![
                AudioURLItem {
                    index: 0,
                    url: "https://example.com/a.wav".into(),
                    label: "a".into(),
                },
                AudioURLItem {
                    index: 1,
                    url: "https://example.com/b.wav".into(),
                    label: String::new(),
                },
            ],
            ..Default::default()
        })
        .expect("distributed task must pass");
    assert_eq!(id, "tid-d");
}

#[test]
fn create_task_audio_urls_rejects_bad_combinations() {
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint("http://127.0.0.1:0"); // unreachable; validation must fail first

    let base = || CreateTranscriptionRequest {
        engine_model_type: "16k_zh_en".into(),
        channel_num: 1,
        res_text_format: 1,
        audio_urls: vec![AudioURLItem {
            index: 0,
            url: "https://example.com/a.wav".into(),
            label: String::new(),
        }],
        ..Default::default()
    };

    let mut req = base();
    req.url = "https://example.com/x.wav".into();
    assert!(r.create_task(&req).is_err(), "with url must fail");

    let mut req = base();
    req.data = "AAAA".into();
    req.data_len = 3;
    assert!(r.create_task(&req).is_err(), "with data must fail");

    let mut req = base();
    req.source_type = SOURCE_TYPE_DATA;
    assert!(
        r.create_task(&req).is_err(),
        "non-zero source_type must fail"
    );

    let mut req = base();
    req.audio_urls[0].index = -1;
    assert!(r.create_task(&req).is_err(), "negative index must fail");

    let mut req = base();
    req.audio_urls.push(AudioURLItem {
        index: 0,
        url: "https://example.com/b.wav".into(),
        label: String::new(),
    });
    assert!(r.create_task(&req).is_err(), "duplicate index must fail");

    let mut req = base();
    req.audio_urls[0].url = " ".into();
    assert!(r.create_task(&req).is_err(), "empty url must fail");

    let mut req = base();
    req.audio_urls[0].url = "ftp://x/a.wav".into();
    assert!(r.create_task(&req).is_err(), "non-http url must fail");
}

// ---------------------------------------------------------------- describe

#[test]
fn describe_task_poll_and_mapping() {
    let server = MockHttpServer::start(|req| {
        assert_eq!(req.target, "/v3/describe_transcription");
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["params"]["transcription_id"], "t1");
        // First call: running; second: success.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            MockHttpResponse::json(
                r#"{"code":0,"message":"success","request_id":"r1","transcription_id":"t1",
                    "status":1,"status_str":"executing","progress":40}"#,
            )
        } else {
            MockHttpResponse::json(
                r#"{"code":0,"message":"success","request_id":"r2","transcription_id":"t1",
                    "status":2,"status_str":"success","progress":100,"audio_duration":6.312,
                    "result":"全文",
                    "result_detail":[{"final_sentence":"第一句话。","start_ms":0,"end_ms":1200,
                        "words_num":1,"words":[{"word":"第一句","start_time":0,"end_time":900}],
                        "speaker_id":1,"speaker_role_name":"teacher","channel_id":0,
                        "speech_speed":3.2,"language":"zh","language_b47":"zh-CN"}]}"#,
            )
        }
    });

    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let status = r
        .wait_for_result_with_interval("t1", Duration::from_millis(10), Duration::from_secs(5))
        .expect("poll to success");

    assert_eq!(status.status, TASK_STATUS_SUCCESS);
    assert_eq!(status.result, "全文");
    assert!((status.audio_duration - 6.312).abs() < 1e-9);
    let d = &status.result_detail[0];
    assert_eq!(d.final_sentence, "第一句话。");
    assert_eq!((d.speaker_id, d.speaker_role_name.as_str()), (1, "teacher"));
    assert_eq!(d.words[0].word, "第一句");
    assert_eq!((d.words[0].start_time, d.words[0].end_time), (0, 900));
    assert_eq!(d.language_b47, "zh-CN");
}

#[test]
fn describe_task_ownership_error() {
    // Cross-account query: 403 + body code 4002 — judge by the body code.
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse {
        status: 403,
        body: r#"{"code":4002,"message":"transcription_id does not belong to this sdkappid","request_id":"r"}"#
            .into(),
    }
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let err = r.describe_task("tid-foreign").expect_err("must fail");
    assert_eq!(err.code, 4002);
}

#[test]
fn create_task_missing_transcription_id() {
    let server =
        MockHttpServer::start(|_req| MockHttpResponse::json(r#"{"code":0,"message":"success"}"#));
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(server.url.clone());
    let err = r
        .create_task_from_url("https://example.com/a.wav", "16k_zh_en")
        .expect_err("code=0 without transcription_id is a protocol violation");
    assert_eq!(err.code, ERR_CODE_SERVER_ERROR);
}
