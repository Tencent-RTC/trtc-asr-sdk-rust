//! SentenceRecognizer tests, ported from the Go SDK's sentence_recognizer_test.go.

mod common;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use common::{MockHttpResponse, MockHttpServer};

use trtc_asr_sdk::asr::{
    SentenceRecognitionRequest, SentenceRecognizer, SOURCE_TYPE_DATA, SOURCE_TYPE_URL,
};
use trtc_asr_sdk::common::errors::{
    ERR_CODE_INVALID_PARAM, ERR_CODE_SERVER_ERROR,
};
use trtc_asr_sdk::common::Credential;

fn test_credential() -> Credential {
    Credential::new(1300000000, 1400000000, "test-secret")
}

#[test]
fn recognize_rejects_invalid_requests() {
    let r = SentenceRecognizer::new(test_credential());

    let err = r.recognize(&SentenceRecognitionRequest::default()).unwrap_err();
    assert_eq!(err.code, ERR_CODE_INVALID_PARAM);
    assert!(err.message.contains("EngServiceType is required"));

    let err = r
        .recognize(&SentenceRecognitionRequest {
            eng_service_type: "16k_zh".into(),
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("VoiceFormat is required"));

    let err = r
        .recognize(&SentenceRecognitionRequest {
            eng_service_type: "16k_zh".into(),
            voice_format: "pcm".into(),
            source_type: SOURCE_TYPE_URL,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("Url is required when SourceType=0"));

    let err = r
        .recognize(&SentenceRecognitionRequest {
            eng_service_type: "16k_zh".into(),
            voice_format: "pcm".into(),
            source_type: SOURCE_TYPE_DATA,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("Data is required when SourceType=1"));
}

#[test]
fn recognize_data_rejects_empty_and_oversized() {
    let r = SentenceRecognizer::new(test_credential());

    let err = r.recognize_data(&[], "pcm", "16k_zh").unwrap_err();
    assert!(err.message.contains("audio data is empty"));

    let big = vec![0u8; 3 * 1024 * 1024 + 1];
    let err = r.recognize_data(&big, "pcm", "16k_zh").unwrap_err();
    assert!(err.message.contains("3MB"));
}

#[test]
fn recognize_url_rejects_empty_url() {
    let r = SentenceRecognizer::new(test_credential());
    let err = r.recognize_url("", "wav", "16k_zh").unwrap_err();
    assert!(err.message.contains("audio URL is empty"));
}

#[test]
fn recognize_data_success() {
    let server = MockHttpServer::start(|req| {
        assert_eq!(req.method, "POST");
        assert!(req.target.starts_with("/v1/SentenceRecognition?"));
        assert_eq!(
            req.header("Content-Type"),
            Some("application/json; charset=utf-8")
        );
        assert!(!req.header("X-TRTC-SdkAppId").unwrap_or("").is_empty());
        assert!(!req.header("X-TRTC-UserSig").unwrap_or("").is_empty());

        // Query parameters per protocol.
        assert!(!req.query("AppId").unwrap_or_default().is_empty());
        assert!(!req.query("Secretid").unwrap_or_default().is_empty());
        assert!(!req.query("RequestId").unwrap_or_default().is_empty());
        assert!(!req.query("Timestamp").unwrap_or_default().is_empty());

        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["EngSerViceType"], "16k_zh_en");
        assert_eq!(body["SourceType"], SOURCE_TYPE_DATA);
        assert_eq!(body["VoiceFormat"], "pcm");
        // Data round-trips through base64.
        let raw = B64.decode(body["Data"].as_str().unwrap()).unwrap();
        assert_eq!(raw, b"fake-pcm-audio");
        assert_eq!(body["DataLen"], 14);

        MockHttpResponse::json(
            r#"{"Response":{"Result":"今天天气不错。","AudioDuration":2380,"WordSize":1,"WordList":[{"Word":"今天","StartTime":200,"EndTime":500}],"RequestId":"req-1"}}"#,
        )
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let result = r
        .recognize_data(b"fake-pcm-audio", "pcm", "16k_zh_en")
        .expect("recognize");
    assert_eq!(result.result, "今天天气不错。");
    assert_eq!(result.audio_duration, 2380);
    assert_eq!(result.word_size, 1);
    assert_eq!(result.word_list.len(), 1);
    assert_eq!(result.word_list[0].word, "今天");
    assert_eq!(result.request_id, "req-1");
}

#[test]
fn recognize_url_success() {
    let server = MockHttpServer::start(|req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["SourceType"], SOURCE_TYPE_URL);
        assert_eq!(body["Url"], "https://example.com/test.wav");
        MockHttpResponse::json(r#"{"Response":{"Result":"hello","AudioDuration":1000,"RequestId":"req-2"}}"#)
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let result = r
        .recognize_url("https://example.com/test.wav", "wav", "16k_zh_en")
        .expect("recognize url");
    assert_eq!(result.result, "hello");
}

#[test]
fn recognize_server_error() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Error":{"Code":"4002","Message":"鉴权失败"},"RequestId":"req-err"}}"#,
        )
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r.recognize_data(b"fake-audio", "pcm", "16k_zh_en").unwrap_err();
    assert_eq!(err.code, ERR_CODE_SERVER_ERROR);
    assert!(err.message.contains("4002"), "{}", err.message);
    assert!(err.message.contains("req-err"), "{}", err.message);
}

#[test]
fn recognize_http_error() {
    let server = MockHttpServer::start(|_req| MockHttpResponse {
        status: 500,
        body: "internal server error".into(),
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r.recognize_data(b"fake-audio", "pcm", "16k_zh_en").unwrap_err();
    assert!(err.message.contains("500"), "{}", err.message);
}

#[test]
fn recognize_data_with_options() {
    let server = MockHttpServer::start(|req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["FilterDirty"], 1);
        assert_eq!(body["WordInfo"], 2);
        assert_eq!(body["HotwordId"], "hw-123");
        MockHttpResponse::json(r#"{"Response":{"Result":"ok","AudioDuration":10,"RequestId":"r"}}"#)
    });

    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let mut req = SentenceRecognitionRequest {
        eng_service_type: "16k_zh_en".into(),
        voice_format: "pcm".into(),
        filter_dirty: 1,
        word_info: 2,
        hotword_id: "hw-123".into(),
        ..Default::default()
    };
    let result = r
        .recognize_data_with_options(b"fake-audio", &mut req)
        .expect("recognize with options");
    assert_eq!(result.result, "ok");
}

#[test]
fn preset_usersig_is_sent_verbatim() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(r#"{"Response":{"Result":"ok","RequestId":"r"}}"#)
    });

    let mut cred = test_credential();
    cred.set_user_sig("preset-user-sig-value");
    let mut r = SentenceRecognizer::new(cred);
    r.set_endpoint(&server.url);

    r.recognize_data(b"fake-audio", "pcm", "16k_zh").unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].header("X-TRTC-UserSig"), Some("preset-user-sig-value"));
}

#[test]
fn request_serialization_omits_empty_fields() {
    let req = SentenceRecognitionRequest {
        eng_service_type: "16k_zh_en".into(),
        source_type: SOURCE_TYPE_URL,
        voice_format: "wav".into(),
        url: "https://example.com/a.wav".into(),
        ..Default::default()
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("EngSerViceType"));
    assert!(!json.contains("HotwordId"));
    assert!(!json.contains("FilterDirty"));
    assert!(!json.contains("DataLen"));
    assert!(!json.contains("Language"));
}
