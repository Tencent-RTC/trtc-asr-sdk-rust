//! FileRecognizer tests, ported from the Go SDK's file_recognizer_test.go
//! and file_recognizer_speaker_test.go.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{MockHttpResponse, MockHttpServer};

use trtc_asr_sdk::asr::{
    CreateRecTaskRequest, FileRecognizer, TASK_STATUS_FAILED, TASK_STATUS_RUNNING,
    TASK_STATUS_SUCCESS, TASK_STATUS_WAITING,
};
use trtc_asr_sdk::common::errors::{ERR_CODE_INVALID_PARAM, ERR_CODE_SERVER_ERROR, ERR_CODE_TIMEOUT};
use trtc_asr_sdk::common::{Credential, SpeakerRole, SPEAKER_DIARIZATION_VOICEPRINT};

fn test_credential() -> Credential {
    Credential::new(1300000000, 1400000000, "test-secret")
}

fn create_success_server() -> MockHttpServer {
    MockHttpServer::start(|req| {
        assert_eq!(req.method, "POST");
        assert!(req.target.starts_with("/v1/CreateRecTask?"), "{}", req.target);
        assert_eq!(
            req.header("Content-Type"),
            Some("application/json; charset=utf-8")
        );
        assert!(!req.header("X-TRTC-SdkAppId").unwrap_or("").is_empty());
        assert!(!req.header("X-TRTC-UserSig").unwrap_or("").is_empty());
        for key in ["AppId", "Secretid", "RequestId", "Timestamp"] {
            assert!(!req.query(key).unwrap_or_default().is_empty(), "missing {key}");
        }

        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["EngineModelType"], "16k_zh_en");
        assert_eq!(body["SourceType"], 1);

        MockHttpResponse::json(
            r#"{"Response":{"Data":{"RecTaskId":"test-task-id-12345"},"RequestId":"test-request-id"}}"#,
        )
    })
}

#[test]
fn validate_create_request() {
    let r = FileRecognizer::new(test_credential());

    let err = r.create_task(&CreateRecTaskRequest::default()).unwrap_err();
    assert_eq!(err.code, ERR_CODE_INVALID_PARAM);
    assert!(err.message.contains("EngineModelType is required"));

    let err = r
        .create_task(&CreateRecTaskRequest {
            engine_model_type: "16k_zh".into(),
            channel_num: 0,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("ChannelNum must be positive"));

    let err = r
        .create_task(&CreateRecTaskRequest {
            engine_model_type: "16k_zh".into(),
            channel_num: 1,
            source_type: 0, // URL
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("Url is required"));

    let err = r
        .create_task(&CreateRecTaskRequest {
            engine_model_type: "16k_zh".into(),
            channel_num: 1,
            source_type: 1, // Data
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.message.contains("Data is required"));
}

#[test]
fn create_task_from_data_rejects_empty_and_oversized() {
    let r = FileRecognizer::new(test_credential());

    let err = r.create_task_from_data(&[], "pcm", "16k_zh").unwrap_err();
    assert!(err.message.contains("audio data is empty"));

    let big = vec![0u8; 6 * 1024 * 1024]; // 6MB > 5MB limit
    let err = r.create_task_from_data(&big, "pcm", "16k_zh").unwrap_err();
    assert!(err.message.contains("5MB"));
}

#[test]
fn create_task_from_url_rejects_empty() {
    let r = FileRecognizer::new(test_credential());
    let err = r.create_task_from_url("", "16k_zh").unwrap_err();
    assert!(err.message.contains("audio URL is empty"));
}

#[test]
fn create_task_success() {
    let server = create_success_server();
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let task_id = r
        .create_task_from_data(b"fake-pcm-audio", "pcm", "16k_zh_en")
        .expect("create task");
    assert_eq!(task_id, "test-task-id-12345");
}

#[test]
fn create_task_server_error() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Error":{"Code":"4002","Message":"鉴权失败"},"RequestId":"test-request-id"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r
        .create_task_from_data(b"fake-audio", "pcm", "16k_zh_en")
        .unwrap_err();
    assert_eq!(err.code, ERR_CODE_SERVER_ERROR);
    assert!(err.message.contains("4002"), "{}", err.message);
}

#[test]
fn create_task_http_error() {
    let server = MockHttpServer::start(|_req| MockHttpResponse {
        status: 500,
        body: "internal server error".into(),
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r
        .create_task_from_data(b"fake-audio", "pcm", "16k_zh_en")
        .unwrap_err();
    assert!(err.message.contains("500"), "{}", err.message);
}

#[test]
fn create_task_empty_task_id_is_error() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(r#"{"Response":{"Data":{"RecTaskId":""},"RequestId":"r"}}"#)
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r
        .create_task_from_data(b"fake-audio", "pcm", "16k_zh_en")
        .unwrap_err();
    assert!(err.message.contains("empty RecTaskId"));
}

#[test]
fn create_task_with_diarization_and_vad_options() {
    let server = MockHttpServer::start(|req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["SpeakerDiarization"], 3);
        assert_eq!(body["SpeakerNumber"], 2);
        assert_eq!(
            body["SpeakerRoles"],
            serde_json::json!([{"RoleName":"teacher","AudioUrl":"https://example.com/a.wav"}])
        );
        assert_eq!(body["VoiceprintIds"], serde_json::json!(["vp-1"]));
        // VadLevel=0 must be serialized (Option distinguishes "unset").
        assert_eq!(body["VadLevel"], 0);
        assert_eq!(body["NoiseThreshold"], 1.5);
        MockHttpResponse::json(r#"{"Response":{"Data":{"RecTaskId":"task-diar"},"RequestId":"r"}}"#)
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let req = CreateRecTaskRequest {
        engine_model_type: "16k_zh_en".into(),
        channel_num: 1,
        res_text_format: 1,
        source_type: 0,
        url: "https://example.com/call.wav".into(),
        speaker_diarization: SPEAKER_DIARIZATION_VOICEPRINT,
        speaker_number: 2,
        speaker_roles: vec![SpeakerRole {
            role_name: "teacher".into(),
            audio_url: "https://example.com/a.wav".into(),
        }],
        voiceprint_ids: vec!["vp-1".into()],
        vad_level: Some(0),
        noise_threshold: Some(1.5),
        ..Default::default()
    };
    let task_id = r.create_task(&req).expect("create task");
    assert_eq!(task_id, "task-diar");
}

#[test]
fn create_task_rejects_invalid_diarization() {
    let r = FileRecognizer::new(test_credential());
    let req = CreateRecTaskRequest {
        engine_model_type: "16k_zh_en".into(),
        channel_num: 1,
        source_type: 0,
        url: "https://example.com/call.wav".into(),
        speaker_diarization: 2, // unsupported
        ..Default::default()
    };
    let err = r.create_task(&req).unwrap_err();
    assert!(err.message.contains("SpeakerDiarization must be 0"));
}

#[test]
fn create_task_from_data_with_options() {
    let server = MockHttpServer::start(|req| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["FilterDirty"], 1);
        assert_eq!(body["ResTextFormat"], 2);
        assert_eq!(body["HotwordId"], "hw-123");
        MockHttpResponse::json(r#"{"Response":{"Data":{"RecTaskId":"task-opts"},"RequestId":"r"}}"#)
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let mut req = CreateRecTaskRequest {
        engine_model_type: "16k_zh_en".into(),
        channel_num: 1,
        res_text_format: 2,
        filter_dirty: 1,
        hotword_id: "hw-123".into(),
        ..Default::default()
    };
    let task_id = r
        .create_task_from_data_with_options(b"fake-audio", &mut req)
        .expect("create with options");
    assert_eq!(task_id, "task-opts");
}

#[test]
fn describe_task_status_empty_id() {
    let r = FileRecognizer::new(test_credential());
    let err = r.describe_task_status("").unwrap_err();
    assert!(err.message.contains("RecTaskId is empty"));
}

#[test]
fn describe_task_status_success_with_details() {
    let server = MockHttpServer::start(|req| {
        assert!(req.target.starts_with("/v1/DescribeTaskStatus?"));
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["RecTaskId"], "task-123");

        MockHttpResponse::json(
            r#"{"Response":{"Data":{
                "RecTaskId":"task-123","Status":2,"StatusStr":"success","Progress":100,
                "Result":"今天天气不错。","AudioDuration":2.38,
                "ResultDetail":[{
                    "FinalSentence":"今天天气不错。","SliceSentence":"今天 天气 不错",
                    "StartMs":200,"EndMs":1380,"WordsNum":1,"SpeechSpeed":2.0,
                    "SpeakerId":1,"SpeakerRoleName":"teacher","ChannelId":0,"Language":"zh",
                    "Words":[{"Word":"今天","OffsetStartMs":200,"OffsetEndMs":500}]
                }]
            },"RequestId":"req-123"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let status = r.describe_task_status("task-123").expect("describe");
    assert_eq!(status.status, TASK_STATUS_SUCCESS);
    assert_eq!(status.result, "今天天气不错。");
    assert!((status.audio_duration - 2.38).abs() < 1e-9);
    assert_eq!(status.result_detail.len(), 1);
    let detail = &status.result_detail[0];
    assert_eq!(detail.final_sentence, "今天天气不错。");
    assert_eq!(detail.speaker_id, 1);
    assert_eq!(detail.speaker_role_name, "teacher");
    assert_eq!(detail.language, "zh");
    assert_eq!(detail.words[0].word, "今天");
}

#[test]
fn describe_task_status_task_failed_fields() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Data":{"RecTaskId":"task-fail","Status":3,"StatusStr":"failed","ErrorMsg":"Failed to download audio file"},"RequestId":"req-456"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let status = r.describe_task_status("task-fail").expect("describe");
    assert_eq!(status.status, TASK_STATUS_FAILED);
    assert_eq!(status.error_msg, "Failed to download audio file");
}

#[test]
fn wait_for_result_immediate_success() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Data":{"RecTaskId":"task-ok","Status":2,"StatusStr":"success","Result":"识别结果","AudioDuration":1.5},"RequestId":"req-ok"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let status = r.wait_for_result("task-ok").expect("wait");
    assert_eq!(status.result, "识别结果");
}

#[test]
fn wait_for_result_polling_then_success() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let server = MockHttpServer::start(move |_req| {
        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
        let body = if n < 3 {
            r#"{"Response":{"Data":{"RecTaskId":"task-poll","Status":1,"StatusStr":"doing"},"RequestId":"req-poll"}}"#
        } else {
            r#"{"Response":{"Data":{"RecTaskId":"task-poll","Status":2,"StatusStr":"success","Result":"轮询成功","AudioDuration":3.0},"RequestId":"req-poll"}}"#
        };
        MockHttpResponse::json(body)
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let status = r
        .wait_for_result_with_interval("task-poll", Duration::from_millis(50), Duration::from_secs(5))
        .expect("wait");
    assert_eq!(status.result, "轮询成功");
    assert!(calls.load(Ordering::SeqCst) >= 3);
    let _ = TASK_STATUS_RUNNING; // silence unused-const style warnings
}

#[test]
fn wait_for_result_task_failed() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Data":{"RecTaskId":"task-err","Status":3,"StatusStr":"failed","ErrorMsg":"转码失败"},"RequestId":"req-err"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r.wait_for_result("task-err").unwrap_err();
    assert_eq!(err.code, ERR_CODE_SERVER_ERROR);
    assert!(err.message.contains("转码失败"), "{}", err.message);
}

#[test]
fn wait_for_result_timeout() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(
            r#"{"Response":{"Data":{"RecTaskId":"task-slow","Status":0,"StatusStr":"waiting"},"RequestId":"req-slow"}}"#,
        )
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let err = r
        .wait_for_result_with_interval("task-slow", Duration::from_millis(20), Duration::from_millis(100))
        .unwrap_err();
    assert_eq!(err.code, ERR_CODE_TIMEOUT);
    assert!(err.message.contains("not completed"), "{}", err.message);
    let _ = TASK_STATUS_WAITING;
}

#[test]
fn request_serialization_skips_none_and_empty() {
    let req = CreateRecTaskRequest {
        engine_model_type: "16k_zh".into(),
        channel_num: 1,
        res_text_format: 1,
        source_type: 0,
        url: "https://example.com/a.wav".into(),
        ..Default::default()
    };
    let json = serde_json::to_string(&req).unwrap();
    for key in [
        "VadLevel",
        "NoiseThreshold",
        "SpeakerRoles",
        "VoiceprintIds",
        "SpeakerDiarization",
        "Data",
        "CallbackUrl",
        "Language",
    ] {
        assert!(!json.contains(key), "{key} should be omitted: {json}");
    }
}
