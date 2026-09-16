//! v3 streaming (/asr/v3) tests: start-frame wire format, sync ack error
//! handling, lifecycle and local validation. Mirrors Go asr/v3
//! speech_recognizer_test.go.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{wait_until, MockWsServer, RecordingListener};
use tungstenite::Message;

use trtc_asr_sdk::asr::v3::{
    new_credential, Context, ContextKV, SpeakerRole, SpeechRecognizer,
    SPEAKER_DIARIZATION_VOICEPRINT,
};
use trtc_asr_sdk::common::errors::{ERR_CODE_ALREADY_STARTED, ERR_CODE_INVALID_PARAM};

fn test_credential() -> trtc_asr_sdk::common::Credential {
    new_credential(1400000000, "test-secret")
}

fn ack(voice_id: &str) -> Message {
    Message::text(
        serde_json::json!({"code": 0, "message": "success", "voice_id": voice_id}).to_string(),
    )
}

fn result_frame(slice_type: i32, final_flag: i32, text: &str) -> Message {
    Message::text(
        serde_json::json!({
            "code": 0, "message": "success", "voice_id": "v1", "final": final_flag,
            "result": {
                "slice_type": slice_type, "index": 0,
                "start_time": 0, "end_time": 1000, "voice_text_str": text,
            }
        })
        .to_string(),
    )
}

#[test]
fn start_frame_wire_format() {
    let start_frames: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sf = Arc::clone(&start_frames);

    let server = MockWsServer::start(move |mut ws| {
        // The handler reads the start frame (the first text frame after the
        // handshake) and captures it for wire assertions.
        if let Ok(Message::Text(t)) = ws.read() {
            sf.lock().unwrap().push(t.to_string());
        }
        let _ = ws.send(ack("voice-1"));
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    if t.contains("\"end\"") {
                        break;
                    }
                }
                // Binary audio frames / pings: keep waiting for the end
                // signal.
                Ok(_) => continue,
                Err(_) => return,
            }
        }
        let _ = ws.send(result_frame(2, 1, "done"));
        while ws.read().is_ok() {}
    });

    let listener = Arc::new(RecordingListener::new());
    // Explicit coercion: the v3 client takes the same listener trait as v2
    // (the downlink shape is identical), and the generic Arc::clone call
    // hides the unsized coercion from the compiler.
    let listener_dyn: Arc<dyn trtc_asr_sdk::asr::SpeechRecognitionListener> = listener.clone();
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener_dyn);
    r.set_endpoint(&server.url);
    r.set_voice_id("voice-1");
    r.set_hotword_list("深度学习|10");
    r.set_vad_level(0);
    r.set_noise_threshold(1.5);
    r.set_filter_empty_result(0);
    r.set_convert_num_mode(0); // explicit 0 must be sent on v3
    r.set_speaker_diarization(SPEAKER_DIARIZATION_VOICEPRINT);
    r.set_speaker_roles(vec![SpeakerRole {
        role_name: "teacher".into(),
        audio_url: "https://example.com/t.wav".into(),
    }]);
    r.set_voiceprint_ids(vec!["vp-1".into()]);
    r.set_context(Context {
        text: "bg".into(),
        terms: vec!["ASR".into()],
        general: vec![ContextKV {
            key: "domain".into(),
            value: "Meeting".into(),
        }],
    });
    r.set_write_timeout(Duration::from_millis(500));
    r.set_stop_timeout(Duration::from_secs(2));

    r.start().expect("start should ack synchronously");
    r.write(&[0u8; 1280]).expect("write audio");
    // stop() sends the end signal; the server answers with the terminal
    // frame, which fires on_recognition_complete before stop returns.
    r.stop().expect("stop");
    assert!(wait_until(Duration::from_secs(3), || {
        RecordingListener::count(&listener.complete_count) > 0
    }));

    // Handshake URL: path /asr/v3, query carries only voice_id (auth travels
    // in the start frame, not the URL).
    let target = server
        .request_target
        .lock()
        .unwrap()
        .clone()
        .expect("handshake target captured");
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    assert_eq!(path, "/asr/v3");
    assert_eq!(query, "voice_id=voice-1");
    for banned in ["signature", "usersig", "sdkappid", "secretid", "appid"] {
        assert!(
            !query.contains(banned),
            "v3 must not carry auth in the query, got {query}"
        );
    }

    let frames = start_frames.lock().unwrap();
    assert_eq!(frames.len(), 1, "exactly one start frame");
    let frame: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();

    assert_eq!(frame["type"], "start");
    let auth = &frame["auth"];
    assert_eq!(auth["sdkappid"], "1400000000");
    assert!(!auth["usersig"].as_str().unwrap_or("").is_empty());
    let mut keys: Vec<_> = auth.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["sdkappid", "usersig"]);

    let params = &frame["params"];
    assert_eq!(params["voice_id"], "voice-1");
    assert_eq!(params["engine_model_type"], "16k_zh_en");
    assert_eq!(params["voice_format"], 1);
    assert_eq!(params["needvad"], 1);
    assert_eq!(params["convert_num_mode"], 0); // explicit 0 preserved
    assert_eq!(params["filter_empty_result"], 0); // explicit 0 preserved
    assert_eq!(params["vad_level"], 0); // explicit 0 preserved
    assert_eq!(params["noise_threshold"], 1.5);
    assert_eq!(params["hotword_list"], "深度学习|10");
    assert_eq!(params["speaker_diarization"], 3);
    assert_eq!(params["voiceprint_ids"], serde_json::json!(["vp-1"]));
    // speaker_roles elements are snake_case, not the v2 CamelCase wire.
    assert_eq!(
        params["speaker_roles"],
        serde_json::json!([{"role_name": "teacher", "audio_url": "https://example.com/t.wav"}])
    );
    assert_eq!(
        params["context"],
        serde_json::json!({
            "text": "bg", "terms": ["ASR"],
            "general": [{"key": "domain", "value": "Meeting"}]
        })
    );
    assert_eq!(params["sdk_info"]["sdk_lang"], "rust");
}

#[test]
fn start_auth_error_is_synchronous() {
    let server = MockWsServer::start(|mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            let _ = ws.send(Message::text(
                serde_json::json!({"code": 4002, "message": "auth failed", "voice_id": "v1"})
                    .to_string(),
            ));
        }
        while ws.read().is_ok() {}
    });

    let listener = Arc::new(RecordingListener::new());
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    let err = r.start().expect_err("start must fail synchronously");
    assert_eq!(err.code, 4002);
    assert!(err.message.contains("auth failed"));
    // The failed start leaves no usable session.
    let err = r.write(b"abc").unwrap_err();
    assert_eq!(err.code, trtc_asr_sdk::common::errors::ERR_CODE_NOT_STARTED);
}

#[test]
fn start_4001_error_is_synchronous() {
    let server = MockWsServer::start(|mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            let _ = ws.send(Message::text(
                serde_json::json!({"code": 4001, "message": "v3 interface not enabled", "voice_id": "v1"})
                    .to_string(),
            ));
        }
        while ws.read().is_ok() {}
    });

    let listener = Arc::new(RecordingListener::new());
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    let err = r.start().expect_err("start must fail synchronously");
    assert_eq!(err.code, 4001);
}

#[test]
fn normal_flow_dispatches_events() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(move |mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            let _ = ws.send(ack("v1"));
        }
        let _ = ws.send(result_frame(0, 0, ""));
        let _ = ws.send(result_frame(1, 0, "你好"));
        let _ = ws.send(result_frame(2, 0, "你好。"));
        // The terminal frame goes out only after the client's end signal.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    if t.contains("\"end\"") {
                        break;
                    }
                }
                // Binary audio frames / pings: keep waiting for the end
                // signal.
                Ok(_) => continue,
                Err(_) => return,
            }
        }
        let _ = ws.send(result_frame(2, 1, "你好。"));
        while ws.read().is_ok() {}
    });

    let listener_dyn: Arc<dyn trtc_asr_sdk::asr::SpeechRecognitionListener> = listener.clone();
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener_dyn);
    r.set_endpoint(&server.url);
    r.set_write_timeout(Duration::from_millis(500));
    r.set_stop_timeout(Duration::from_secs(2));
    r.start().expect("start");
    r.write(&[0u8; 1280]).expect("write");
    assert!(wait_until(Duration::from_secs(3), || {
        RecordingListener::count(&listener.sentence_begin_count) >= 1
            && RecordingListener::count(&listener.change_count) >= 1
            && RecordingListener::count(&listener.sentence_end_count) >= 1
    }));
    r.stop().expect("stop");
    // The standalone sentence-end (slice_type=2, final=0) and the terminal
    // final=1&slice_type=2 frame each dispatch on_sentence_end — mirroring Go.
    assert!(RecordingListener::count(&listener.sentence_begin_count) >= 1);
    assert!(RecordingListener::count(&listener.change_count) >= 1);
    assert!(RecordingListener::count(&listener.sentence_end_count) >= 2);
    assert!(RecordingListener::count(&listener.complete_count) >= 1);
    assert!(RecordingListener::count(&listener.fail_count) == 0);
}

#[test]
fn oversized_audio_frame_fails_locally() {
    let server = MockWsServer::start(|mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            let _ = ws.send(ack("v1"));
        }
        while ws.read().is_ok() {}
    });
    let listener = Arc::new(RecordingListener::new());
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.start().expect("start");
    let err = r.write(&vec![0u8; 256 * 1024 + 1]).unwrap_err();
    assert_eq!(err.code, ERR_CODE_INVALID_PARAM);
}

#[test]
fn local_validation_rejects_out_of_range_options() {
    let long_voice_id = "x".repeat(129);
    let cases: Vec<(&str, Box<dyn Fn(&mut SpeechRecognizer)>)> = vec![
        (
            "voice_id too long",
            Box::new(|r: &mut SpeechRecognizer| r.set_voice_id(long_voice_id.clone())),
        ),
        (
            "max_speak_time too small",
            Box::new(|r| r.set_max_speak_time(1000)),
        ),
        (
            "max_speak_time too large",
            Box::new(|r| r.set_max_speak_time(90001)),
        ),
        (
            "vad_silence_time too small",
            Box::new(|r| r.set_vad_silence_time(100)),
        ),
        (
            "vad_silence_time too large",
            Box::new(|r| r.set_vad_silence_time(2001)),
        ),
        ("needvad invalid", Box::new(|r| r.set_need_vad(2))),
        (
            "convert_num_mode invalid",
            Box::new(|r| r.set_convert_num_mode(2)),
        ),
        ("filter_dirty invalid", Box::new(|r| r.set_filter_dirty(3))),
        ("filter_modal invalid", Box::new(|r| r.set_filter_modal(3))),
        ("filter_punc invalid", Box::new(|r| r.set_filter_punc(2))),
        ("word_info invalid", Box::new(|r| r.set_word_info(3))),
        (
            "word_with_space invalid",
            Box::new(|r| r.set_word_with_space(2)),
        ),
        ("voice_format invalid", Box::new(|r| r.set_voice_format(2))),
        (
            "input_sample_rate invalid",
            Box::new(|r| r.set_input_sample_rate(16000)),
        ),
        (
            "filter_empty_result invalid",
            Box::new(|r| r.set_filter_empty_result(2)),
        ),
        ("vad_level invalid", Box::new(|r| r.set_vad_level(2))),
        (
            "noise_threshold out of range",
            Box::new(|r| r.set_noise_threshold(4.1)),
        ),
        (
            "diarization invalid",
            Box::new(|r| r.set_speaker_diarization(2)),
        ),
    ];
    for (name, apply) in cases {
        let server = MockWsServer::start(|_ws| {});
        let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", {
            Arc::new(RecordingListener::new())
        });
        r.set_endpoint(&server.url);
        apply(&mut r);
        let err = r.start().expect_err(name);
        assert_eq!(err.code, ERR_CODE_INVALID_PARAM, "{}", name);
    }

    // Boundary values must pass local validation (they then fail at dial,
    // which is expected with a dead endpoint).
    let server = MockWsServer::start(|_ws| {});
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", {
        Arc::new(RecordingListener::new())
    });
    r.set_endpoint(&server.url);
    r.set_max_speak_time(90000);
    r.set_vad_silence_time(240);
    r.set_voice_format(12);
    r.set_word_info(100);
    // start() fails with a transport error (mock accepts, ack never comes) —
    // but it must NOT be INVALID_PARAM.
    if let Err(err) = r.start() {
        assert_ne!(err.code, ERR_CODE_INVALID_PARAM);
    }
}

#[test]
fn double_start_returns_already_started() {
    let server = MockWsServer::start(move |mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            let _ = ws.send(ack("v1"));
        }
        // Terminal frame only after the client's end signal.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    if t.contains("\"end\"") {
                        break;
                    }
                }
                // Binary audio frames / pings: keep waiting for the end
                // signal.
                Ok(_) => continue,
                Err(_) => return,
            }
        }
        let _ = ws.send(result_frame(2, 1, "done"));
        while ws.read().is_ok() {}
    });
    let listener = Arc::new(RecordingListener::new());
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.start().expect("first start");
    let err = r.start().unwrap_err();
    assert_eq!(err.code, ERR_CODE_ALREADY_STARTED);
    r.stop().expect("stop");
}

#[test]
fn ack_timeout_fails_start() {
    // A server that never acks: start must fail with READ_FAILED after the
    // 5s ack timeout rather than hang forever.
    let server = MockWsServer::start(|mut ws| {
        if let Ok(Message::Text(_)) = ws.read() {
            // no ack
        }
        loop {
            if ws.read().is_err() {
                break;
            }
        }
    });
    let listener = Arc::new(RecordingListener::new());
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    let err = r.start().expect_err("ack timeout");
    assert_eq!(err.code, trtc_asr_sdk::common::errors::ERR_CODE_READ_FAILED);
    assert!(err.message.contains("ack timeout"));
}
