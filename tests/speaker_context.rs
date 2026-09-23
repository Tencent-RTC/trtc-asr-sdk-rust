//! v2 断点续传（speaker context）：query 参数下发、本地校验与首响应
//! `speaker_continue` 的捕获。

mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use common::MockWsServer;
use tungstenite::Message;

use trtc_asr_sdk::asr::speech_recognizer::SpeechRecognizer;
use trtc_asr_sdk::asr::types::SpeakerContinue;
use trtc_asr_sdk::asr::SpeechRecognitionListener;
use trtc_asr_sdk::common::{
    Credential, SignatureParams, SPEAKER_DIARIZATION_CLUSTER, SPEAKER_DIARIZATION_VOICEPRINT,
    SPEAKER_CONTEXT_ASYNC, SPEAKER_CONTEXT_SYNC,
};

struct Nop;

impl SpeechRecognitionListener for Nop {}

fn credential() -> Credential {
    Credential::new(1300000000, 1400000000, "test-secret")
}

fn signature_params(mode: i32, ctx_id: &str) -> SignatureParams {
    let mut p = SignatureParams::new(1400000000, "bigmodel", "v1");
    p.sdk_app_id = 1400000000;
    p.speaker_diarization = SPEAKER_DIARIZATION_CLUSTER;
    p.enable_speaker_context = mode;
    p.speaker_context_id = ctx_id.to_string();
    p
}

#[test]
fn query_emits_context_params() {
    let query = signature_params(SPEAKER_CONTEXT_SYNC, "abc123").build_query_string();
    assert!(query.contains("enable_speaker_context=1"), "{query}");
    assert!(query.contains("speaker_context_id=abc123"), "{query}");

    // 异步模式值为 2
    let query = signature_params(SPEAKER_CONTEXT_ASYNC, "abc123").build_query_string();
    assert!(query.contains("enable_speaker_context=2"), "{query}");
}

#[test]
fn query_omits_context_params_when_off() {
    let query = signature_params(0, "abc123").build_query_string();
    assert!(!query.contains("speaker_context"), "{query}");
}

#[test]
fn validation_requires_diarization() {
    let mut r = SpeechRecognizer::new(credential(), "bigmodel", Arc::new(Nop));
    r.set_enable_speaker_context(SPEAKER_CONTEXT_SYNC);
    let err = r.start().expect_err("必须本地报错");
    assert!(err.to_string().contains("set_speaker_diarization"), "{err}");
}

#[test]
fn validation_rejects_unknown_mode() {
    let mut r = SpeechRecognizer::new(credential(), "bigmodel", Arc::new(Nop));
    r.set_speaker_diarization(SPEAKER_DIARIZATION_VOICEPRINT);
    r.set_enable_speaker_context(3);
    let err = r.start().expect_err("必须本地报错");
    assert!(err.to_string().contains("EnableSpeakerContext"), "{err}");
}

/// 走真实 Start 路径：握手 query 带上两个参数，首响应 speaker_continue 被捕获。
#[test]
fn start_query_and_first_response_capture() {
    let closed = Arc::new(AtomicBool::new(false));
    let closed2 = Arc::clone(&closed);

    let server = MockWsServer::start(move |mut ws| {
        let _ = ws.send(Message::text(
            r#"{"code":0,"message":"success","voice_id":"v1","speaker_continue":{"continue_status":"resumed","speaker_context_id":"abc"}}"#,
        ));
        let _ = ws.send(Message::text(
            r#"{"code":0,"message":"success","voice_id":"v1","final":1,"result":{"slice_type":2,"index":0}}"#,
        ));
        loop {
            if ws.read().is_err() {
                closed2.store(true, Ordering::SeqCst);
                return;
            }
        }
    });

    let mut r = SpeechRecognizer::new(credential(), "bigmodel", Arc::new(Nop));
    r.set_endpoint(&server.url);
    r.set_speaker_diarization(SPEAKER_DIARIZATION_CLUSTER);
    r.set_enable_speaker_context(SPEAKER_CONTEXT_SYNC);
    r.set_speaker_context_id("abc");
    r.start().expect("start should succeed");

    // 握手 query：升级 URI 应携带两个参数（签名校验由 signature_test 覆盖）。
    let target = server
        .request_target
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    assert!(
        target.contains("enable_speaker_context=1"),
        "handshake query missing enable_speaker_context: {target}"
    );
    assert!(
        target.contains("speaker_context_id=abc"),
        "handshake query missing speaker_context_id: {target}"
    );

    // 首响应的 speaker_continue 经 reader 捕获，getter 可读取。
    let mut captured = None;
    for _ in 0..100 {
        captured = r.speaker_continue();
        if captured.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let sc = captured.expect("speaker_continue 应被 reader 捕获");
    assert_eq!(sc.continue_status, "resumed");
    assert_eq!(sc.speaker_context_id, "abc");

    // 会话可能在 final 帧后已自行结束（state=Stopped），此时 stop() 返回
    // NotStarted 属预期；这里只需保证不 panic。
    let _ = r.stop();
}
