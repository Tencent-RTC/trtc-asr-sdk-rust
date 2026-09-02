//! SDK self-identification reporting tests, ported from the Go SDK's
//! sdkinfo_test.go. Asserts all three transports (WebSocket handshake,
//! sentence HTTP, file HTTP) carry the same identification parameters.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    percent_decode, wait_until, MockHttpResponse, MockHttpServer, MockWsServer, RecordingListener,
};
use tungstenite::Message;

use trtc_asr_sdk::asr::{
    CreateRecTaskRequest, FileRecognizer, SentenceRecognitionRequest, SentenceRecognizer,
    SpeechRecognizer, SOURCE_TYPE_URL,
};
use trtc_asr_sdk::common::{sdk_platform, Credential, SDK_LANGUAGE, SDK_TYPE, SDK_VERSION};

fn test_credential() -> Credential {
    Credential::new(1300000000, 1400000000, "test-secret")
}

/// Parses a request target (`/path?a=1&b=2`) into decoded key/value pairs.
fn query_of(target: &str) -> Vec<(String, String)> {
    let query = match target.split_once('?') {
        Some((_, q)) => q,
        None => return Vec::new(),
    };
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let mut kv = pair.splitn(2, '=');
            (
                percent_decode(kv.next().unwrap_or("")),
                percent_decode(kv.next().unwrap_or("")),
            )
        })
        .collect()
}

fn query_get(target: &str, key: &str) -> Option<String> {
    query_of(target)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// Checks that a captured request target carries the SDK identification the
/// service relies on for diagnostics.
fn assert_sdk_report_params(target: &str) {
    assert_eq!(
        query_get(target, "sdk_lang").as_deref(),
        Some(SDK_LANGUAGE),
        "sdk_lang in {target}"
    );
    assert_eq!(
        query_get(target, "sdk_type").as_deref(),
        Some(SDK_TYPE),
        "sdk_type in {target}"
    );
    assert_eq!(
        query_get(target, "version").as_deref(),
        Some(SDK_VERSION),
        "version in {target}"
    );
    assert_eq!(
        query_get(target, "platform").as_deref(),
        Some(sdk_platform()),
        "platform in {target}"
    );
}

#[test]
fn sdk_version_matches_crate_version() {
    assert_eq!(SDK_VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(SDK_LANGUAGE, "rust");
    assert_eq!(SDK_TYPE, "server");
}

#[test]
fn sdk_platform_is_normalized() {
    // Any target we build on must map into the service vocabulary; unknown
    // platforms are reported verbatim rather than mislabeled.
    let expected = match std::env::consts::OS {
        "macos" => "mac",
        other => other,
    };
    assert_eq!(sdk_platform(), expected);
}

#[test]
fn speech_recognizer_handshake_reports_sdk_identity() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|mut ws| loop {
        match ws.read() {
            Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                let _ = ws.send(Message::Text(
                    r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                ));
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
    });

    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.set_voice_id("voice-sdkinfo");
    r.set_stop_timeout(Duration::from_secs(2));
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        server.request_target.lock().unwrap().is_some()
    }));
    let target = server.request_target.lock().unwrap().clone().unwrap();
    let _ = r.stop();
    server.join();

    assert_sdk_report_params(&target);
    // The pre-existing protocol parameters must survive the addition.
    assert_eq!(
        query_get(&target, "voice_id").as_deref(),
        Some("voice-sdkinfo")
    );
    assert_eq!(
        query_get(&target, "engine_model_type").as_deref(),
        Some("16k_zh_en")
    );
    assert_eq!(
        query_get(&target, "secretid").as_deref(),
        Some("1300000000")
    );
    for key in ["signature", "usersig", "timestamp", "expired", "nonce"] {
        assert!(
            !query_get(&target, key).unwrap_or_default().is_empty(),
            "missing {key} in {target}"
        );
    }
}

#[test]
fn sentence_recognizer_reports_sdk_identity() {
    let server = MockHttpServer::start(|_req| {
        MockHttpResponse::json(r#"{"Response":{"RequestId":"req-1","Result":"hello"}}"#)
    });
    let mut r = SentenceRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    r.recognize(&SentenceRecognitionRequest {
        eng_service_type: "16k_zh".into(),
        voice_format: "wav".into(),
        source_type: SOURCE_TYPE_URL,
        url: "https://example.com/test.wav".into(),
        ..Default::default()
    })
    .expect("recognize");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let target = &requests[0].target;
    assert_sdk_report_params(target);
    assert!(target.starts_with("/v1/SentenceRecognition?"), "{target}");
    for key in ["AppId", "Secretid", "RequestId", "Timestamp"] {
        assert!(
            !query_get(target, key).unwrap_or_default().is_empty(),
            "missing {key} in {target}"
        );
    }
}

#[test]
fn file_recognizer_reports_sdk_identity_on_both_endpoints() {
    let server = MockHttpServer::start(|req| {
        if req.target.starts_with("/v1/CreateRecTask") {
            MockHttpResponse::json(
                r#"{"Response":{"Data":{"RecTaskId":"task-42"},"RequestId":"req-1"}}"#,
            )
        } else {
            MockHttpResponse::json(
                r#"{"Response":{"Data":{"TaskId":"task-42","Status":2,"StatusStr":"success","Result":"hello"},"RequestId":"req-2"}}"#,
            )
        }
    });
    let mut r = FileRecognizer::new(test_credential());
    r.set_endpoint(&server.url);

    let task_id = r
        .create_task(&CreateRecTaskRequest {
            engine_model_type: "16k_zh".into(),
            channel_num: 1,
            source_type: SOURCE_TYPE_URL,
            url: "https://example.com/test.wav".into(),
            ..Default::default()
        })
        .expect("create task");
    r.describe_task_status(&task_id).expect("describe task");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    // Both CreateRecTask and DescribeTaskStatus go through the shared request
    // path, so both must report.
    assert!(requests[0].target.starts_with("/v1/CreateRecTask?"));
    assert!(requests[1].target.starts_with("/v1/DescribeTaskStatus?"));
    for req in &requests {
        assert_sdk_report_params(&req.target);
        for key in ["AppId", "Secretid", "RequestId", "Timestamp"] {
            assert!(
                !query_get(&req.target, key).unwrap_or_default().is_empty(),
                "missing {key} in {}",
                req.target
            );
        }
    }
}
