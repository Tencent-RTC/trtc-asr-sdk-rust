//! SpeechRecognizer lifecycle & concurrency tests, ported from the Go SDK's
//! speech_recognizer_test.go. All tests run against a local mock WebSocket
//! server.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{wait_until, MockWsServer, RecordingListener};
use tungstenite::Message;

use trtc_asr_sdk::asr::{SpeechRecognitionListener, SpeechRecognizer};
use trtc_asr_sdk::common::errors::{
    AsrError, ERR_CODE_ALREADY_STARTED, ERR_CODE_INVALID_PARAM, ERR_CODE_NOT_STARTED,
};
use trtc_asr_sdk::common::Credential;

fn test_credential() -> Credential {
    Credential::new(1300000000, 1400000000, "test-secret")
}

fn recognizer(listener: Arc<RecordingListener>, server: &MockWsServer) -> SpeechRecognizer {
    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.set_write_timeout(Duration::from_millis(500));
    // Keep stop() fast: tests that don't specifically exercise the stop
    // timeout should not wait the 10s default.
    r.set_stop_timeout(Duration::from_secs(2));
    r
}

#[test]
fn write_before_start_returns_not_started() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|_ws| {});
    let r = recognizer(listener, &server);
    let err = r.write(b"abc").unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);
}

#[test]
fn stop_before_start_returns_not_started() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|_ws| {});
    let r = recognizer(listener, &server);
    let err = r.stop().unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);
}

#[test]
fn start_twice_returns_already_started() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|mut ws| {
        // Hold the session open until the client goes away.
        while ws.read().is_ok() {}
    });
    let r = recognizer(listener, &server);
    r.start().expect("first start");
    let err = r.start().unwrap_err();
    assert_eq!(err.code, ERR_CODE_ALREADY_STARTED);
    r.stop().expect("stop");
    server.join();
}

#[test]
fn start_rejects_invalid_options_and_stays_reusable() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|mut ws| {
        while ws.read().is_ok() {}
    });
    let mut r = recognizer(listener, &server);

    r.set_speaker_diarization(2); // unsupported
    let err = r.start().unwrap_err();
    assert_eq!(err.code, ERR_CODE_INVALID_PARAM);
    assert!(err.message.contains("SpeakerDiarization must be 0"));

    // A rejected start leaves the recognizer reusable after fixing options.
    r.set_speaker_diarization(0);
    r.start().expect("start after fixing options");
    r.stop().expect("stop");
    server.join();
}

#[test]
fn start_rejects_out_of_range_noise_threshold() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|_ws| {});
    let mut r = recognizer(listener, &server);
    r.set_noise_threshold(5.0);
    let err = r.start().unwrap_err();
    assert!(err.message.contains("NoiseThreshold must be between"));
}

#[test]
fn start_rejects_roles_without_voiceprint_mode() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|_ws| {});
    let mut r = recognizer(listener, &server);
    r.set_speaker_diarization(1);
    r.set_speaker_roles(vec![trtc_asr_sdk::common::SpeakerRole {
        role_name: "teacher".into(),
        audio_url: "https://example.com/a.wav".into(),
    }]);
    let err = r.start().unwrap_err();
    assert!(err.message.contains("require SpeakerDiarization=3"));
}

#[test]
fn handshake_sends_auth_query_params() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|mut ws| {
        // Answer the end signal with a final frame so stop() returns promptly.
        loop {
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
        }
    });
    let mut r = recognizer(listener, &server);
    r.set_voice_id("voice-handshake");
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        server.request_target.lock().unwrap().is_some()
    }));
    let target = server.request_target.lock().unwrap().clone().unwrap();
    assert!(target.starts_with("/asr/v2/1300000000?"), "{target}");
    for key in [
        "sdkappid=1400000000",
        "usersig=",
        "signature=",
        "secretid=1300000000",
        "voice_id=voice-handshake",
        "engine_model_type=16k_zh_en",
        "timestamp=",
        "expired=",
        "nonce=",
    ] {
        assert!(target.contains(key), "missing {key} in {target}");
    }
    // SecretKey must never reach the wire.
    assert!(!target.contains("test-secret"));

    r.stop().expect("stop");
    server.join();
}

#[test]
fn full_session_lifecycle() {
    let listener = Arc::new(RecordingListener::new());
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        // Handshake ack: no result object — must not trigger sentence begin.
        ws.send(Message::Text(
            r#"{"code":0,"message":"success","voice_id":"v1"}"#.into(),
        ))
        .unwrap();
        // Sentence begin.
        ws.send(Message::Text(
            r#"{"code":0,"message":"success","voice_id":"v1","message_id":"m1","result":{"slice_type":0,"index":0,"voice_text_str":"今天。"}}"#.into(),
        ))
        .unwrap();
        // Intermediate result.
        ws.send(Message::Text(
            r#"{"code":0,"message":"success","voice_id":"v1","message_id":"m2","result":{"slice_type":1,"index":0,"voice_text_str":"今天天气"}}"#.into(),
        ))
        .unwrap();
        // Read audio frames until the end signal, then reply final.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"success","voice_id":"v1","message_id":"m3","final":1,"result":{"slice_type":2,"index":0,"voice_text_str":"今天天气不错。"}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut r = recognizer(Arc::clone(&listener), &server);
    r.set_voice_id("v1");
    r.start().expect("start");
    r.write(b"\x00\x01\x02\x03").expect("write audio");
    r.stop().expect("stop");

    assert_eq!(RecordingListener::count(&l.start_count), 1);
    // The handshake ack must NOT be dispatched as sentence begin.
    assert_eq!(RecordingListener::count(&l.sentence_begin_count), 1);
    assert_eq!(RecordingListener::count(&l.change_count), 1);
    assert_eq!(RecordingListener::count(&l.sentence_end_count), 1);
    assert_eq!(RecordingListener::count(&l.complete_count), 1);
    assert_eq!(RecordingListener::count(&l.fail_count), 0);

    // A late write must report not-running.
    let err = r.write(b"late").unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);

    server.join();
}

#[test]
fn write_and_end_signal_reach_server() {
    let listener = Arc::new(RecordingListener::new());
    let got_audio = Arc::new(AtomicBool::new(false));
    let got_end = Arc::new(AtomicBool::new(false));
    let (a, e) = (Arc::clone(&got_audio), Arc::clone(&got_end));
    let server = MockWsServer::start(move |mut ws| {
        loop {
            match ws.read() {
                Ok(Message::Binary(b)) if *b == *b"\x01\x02\x03" => a.store(true, Ordering::SeqCst),
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    e.store(true, Ordering::SeqCst);
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let r = recognizer(listener, &server);
    r.start().expect("start");
    r.write(b"\x01\x02\x03").expect("write");
    r.stop().expect("stop");

    assert!(got_audio.load(Ordering::SeqCst), "server should receive the audio frame");
    assert!(got_end.load(Ordering::SeqCst), "server should receive the end signal");
    server.join();
}

#[test]
fn server_error_triggers_on_fail_and_stops() {
    let listener = Arc::new(RecordingListener::new());
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        ws.send(Message::Text(
            r#"{"code":4006,"message":"quota exceeded","voice_id":"v1","result":{}}"#.into(),
        ))
        .unwrap();
        while ws.read().is_ok() {}
    });

    let r = recognizer(listener, &server);
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        RecordingListener::count(&l.fail_count) == 1
    }));
    let events = l.events.lock().unwrap().clone();
    assert!(events.contains(&"fail:4006".to_string()), "{events:?}");

    // After a terminal error the recognizer is stopped: late writes fail.
    let err = r.write(b"late").unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);
    // stop on an already-terminated recognizer reports not-running.
    let err = r.stop().unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);
    server.join();
}

#[test]
fn final_with_slice_zero_only_completes() {
    // A Final=1 frame whose result is not slice_type=2 must only fire
    // on_recognition_complete (no spurious sentence begin).
    let listener = Arc::new(RecordingListener::new());
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        ws.send(Message::Text(
            r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":0,"index":0}}"#.into(),
        ))
        .unwrap();
        while ws.read().is_ok() {}
    });

    let r = recognizer(listener, &server);
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        RecordingListener::count(&l.complete_count) == 1
    }));
    assert_eq!(RecordingListener::count(&l.sentence_begin_count), 0);
    assert_eq!(RecordingListener::count(&l.sentence_end_count), 0);
    server.join();
}

#[test]
fn malformed_frame_is_non_terminal() {
    let listener = Arc::new(RecordingListener::new());
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        ws.send(Message::Text("not-json".into())).unwrap();
        ws.send(Message::Text(
            r#"{"code":0,"message":"ok","voice_id":"v1","result":{"slice_type":1,"index":0,"voice_text_str":"hi"}}"#.into(),
        ))
        .unwrap();
        loop {
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
        }
    });

    let r = recognizer(listener, &server);
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        RecordingListener::count(&l.change_count) == 1
    }));
    // One non-terminal OnFail for the malformed frame, session continues.
    assert_eq!(RecordingListener::count(&l.fail_count), 1);
    let events = l.events.lock().unwrap().clone();
    assert!(events.contains(&"fail:1004".to_string()), "{events:?}");

    r.stop().expect("stop");
    server.join();
}

/// A listener that calls `stop` from a non-terminal callback. Re-entry must
/// return promptly (the callback runs on the reader thread, so waiting for
/// the terminal response there would self-deadlock).
struct StopFromChangeListener {
    inner: RecordingListener,
    recognizer: Mutex<Option<Arc<SpeechRecognizer>>>,
    stop_duration: Mutex<Option<Duration>>,
    stop_result: Mutex<Option<std::result::Result<(), AsrError>>>,
}

impl SpeechRecognitionListener for StopFromChangeListener {
    fn on_recognition_start(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        self.inner.on_recognition_start(r);
    }
    fn on_sentence_begin(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        self.inner.on_sentence_begin(r);
    }
    fn on_recognition_result_change(&self, resp: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        self.inner.on_recognition_result_change(resp);
        let r = self.recognizer.lock().unwrap().as_ref().unwrap().clone();
        let start = Instant::now();
        let res = r.stop();
        *self.stop_duration.lock().unwrap() = Some(start.elapsed());
        *self.stop_result.lock().unwrap() = Some(res);
    }
    fn on_sentence_end(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        self.inner.on_sentence_end(r);
    }
    fn on_recognition_complete(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        self.inner.on_recognition_complete(r);
    }
    fn on_fail(&self, r: Option<&trtc_asr_sdk::asr::SpeechRecognitionResponse>, e: &AsrError) {
        self.inner.on_fail(r, e);
    }
}

#[test]
fn stop_from_non_terminal_callback_returns_promptly() {
    let listener = Arc::new(StopFromChangeListener {
        inner: RecordingListener::new(),
        recognizer: Mutex::new(None),
        stop_duration: Mutex::new(None),
        stop_result: Mutex::new(None),
    });
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        ws.send(Message::Text(
            r#"{"code":0,"message":"ok","voice_id":"v1","result":{"slice_type":1,"index":0,"voice_text_str":"partial"}}"#.into(),
        ))
        .unwrap();
        // Read until end signal, then finish the session.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", l.clone());
    r.set_endpoint(&server.url);
    let r = Arc::new(r);
    *l.recognizer.lock().unwrap() = Some(Arc::clone(&r));
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(10), || {
        l.stop_result.lock().unwrap().is_some()
    }), "stop() inside on_recognition_result_change did not return promptly (self-deadlock?)");

    let dur = l.stop_duration.lock().unwrap().unwrap();
    assert!(dur < Duration::from_secs(5), "stop took {dur:?}, want prompt return");
    l.stop_result.lock().unwrap().take().unwrap().expect("stop result ok");

    // The watchdog completes the session after the server replies final.
    assert!(wait_until(Duration::from_secs(10), || {
        RecordingListener::count(&l.inner.complete_count) == 1
    }));
    server.join();
}

/// A listener that panics inside a callback; the SDK must recover and surface
/// the panic via on_fail instead of crashing the host process.
struct PanicListener {
    inner: RecordingListener,
}

impl SpeechRecognitionListener for PanicListener {
    fn on_recognition_result_change(&self, _resp: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        panic!("listener boom");
    }
    fn on_fail(&self, r: Option<&trtc_asr_sdk::asr::SpeechRecognitionResponse>, e: &AsrError) {
        self.inner.on_fail(r, e);
    }
}

#[test]
fn listener_panic_is_recovered_and_reported() {
    let listener = Arc::new(PanicListener {
        inner: RecordingListener::new(),
    });
    let l = Arc::clone(&listener);
    let server = MockWsServer::start(|mut ws| {
        ws.send(Message::Text(
            r#"{"code":0,"message":"ok","voice_id":"v1","result":{"slice_type":1,"index":0,"voice_text_str":"hi"}}"#.into(),
        ))
        .unwrap();
        while ws.read().is_ok() {}
    });

    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.start().expect("start");

    assert!(wait_until(Duration::from_secs(2), || {
        RecordingListener::count(&l.inner.fail_count) == 1
    }));
    let events = l.inner.events.lock().unwrap().clone();
    assert!(events.contains(&"fail:1004".to_string()), "{events:?}");

    // The recognizer is stopped after the panic; late writes fail.
    let err = r.write(b"late").unwrap_err();
    assert_eq!(err.code, ERR_CODE_NOT_STARTED);
    server.join();
}

/// Terminal frame arrives during stop()'s timed wait, but the terminal
/// callback runs LONGER than the stop timeout. stop() must not return early:
/// once the terminal response is in, it waits for the callbacks to finish
/// (mirrors the Go SDK's waitForCallbacksOrAbort terminal branch).
#[test]
fn stop_waits_for_slow_terminal_callback_beyond_timeout() {
    struct SlowCompleteListener {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }
    impl SpeechRecognitionListener for SlowCompleteListener {
        fn on_recognition_complete(
            &self,
            _r: &trtc_asr_sdk::asr::SpeechRecognitionResponse,
        ) {
            self.entered.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let listener = Arc::new(SlowCompleteListener {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let server = MockWsServer::start(|mut ws| {
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.set_stop_timeout(Duration::from_secs(1)); // terminal callback will outlive this
    r.start().unwrap();

    let stop_returned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop_returned);
    let stop_thread = std::thread::spawn(move || {
        r.stop().unwrap();
        flag.store(true, Ordering::SeqCst);
    });

    // Wait until the terminal callback is running, then let it exceed the
    // stop timeout.
    assert!(wait_until(Duration::from_secs(3), || entered.load(Ordering::SeqCst)));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !stop_returned.load(Ordering::SeqCst),
        "stop returned while the terminal callback was still running past stop timeout"
    );

    release.store(true, Ordering::SeqCst);
    stop_thread.join().unwrap();
    assert!(stop_returned.load(Ordering::SeqCst));
    server.join();
}

#[test]
fn stop_times_out_and_force_closes_when_server_never_finishes() {
    let listener = Arc::new(RecordingListener::new());
    let l = Arc::clone(&listener);
    let server_got_end = Arc::new(AtomicBool::new(false));
    let ge = Arc::clone(&server_got_end);
    let server = MockWsServer::start(move |mut ws| {
        // Read the end signal but never reply with a final frame; exit when
        // the client force-closes.
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => ge.store(true, Ordering::SeqCst),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut r = recognizer(listener, &server);
    r.set_stop_timeout(Duration::from_secs(1));
    r.start().expect("start");

    let start = Instant::now();
    r.stop().expect("stop returns even without a final frame");
    let elapsed = start.elapsed();
    assert!(server_got_end.load(Ordering::SeqCst), "server should have read the end signal");
    assert!(
        elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(5),
        "stop should wait ~stop_timeout, took {elapsed:?}"
    );
    assert_eq!(RecordingListener::count(&l.complete_count), 0);
    server.join();
}

#[test]
fn external_stop_waits_for_terminal_callback() {
    struct BlockingCompleteListener {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }
    impl SpeechRecognitionListener for BlockingCompleteListener {
        fn on_recognition_complete(&self, _r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
            self.entered.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let listener = Arc::new(BlockingCompleteListener {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });

    let server = MockWsServer::start(|mut ws| {
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut r = SpeechRecognizer::new(test_credential(), "16k_zh_en", listener);
    r.set_endpoint(&server.url);
    r.start().expect("start");
    let r = Arc::new(r);

    let r2 = Arc::clone(&r);
    let stop_handle = std::thread::spawn(move || r2.stop());

    // Wait until the terminal callback is running.
    assert!(wait_until(Duration::from_secs(10), || entered.load(Ordering::SeqCst)));
    std::thread::sleep(Duration::from_millis(100));
    // stop must not return while the terminal callback is still running.
    assert!(!stop_handle.is_finished(), "stop returned before terminal callback finished");

    release.store(true, Ordering::SeqCst);
    stop_handle
        .join()
        .expect("join stop thread")
        .expect("stop result ok");
    server.join();
}

#[test]
fn timeout_clamps() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|_ws| {});
    let mut r = recognizer(listener, &server);

    // Write timeout clamps to [50ms, 30s]; non-positive resets to 5s.
    r.set_write_timeout(Duration::from_secs(3600));
    r.set_write_timeout(Duration::from_nanos(1));
    r.set_write_timeout(Duration::ZERO);
    r.set_write_timeout(Duration::from_secs(2));

    // Stop timeout clamps to [1s, 60s]; non-positive resets to 10s.
    r.set_stop_timeout(Duration::from_secs(3600));
    r.set_stop_timeout(Duration::from_millis(1));
    r.set_stop_timeout(Duration::ZERO);
    r.set_stop_timeout(Duration::from_secs(5));
}

#[test]
fn reconnect_requires_new_instance() {
    let listener = Arc::new(RecordingListener::new());
    let server = MockWsServer::start(|mut ws| {
        while ws.read().is_ok() {}
    });
    let r = recognizer(listener, &server);
    r.start().expect("start");
    r.stop().expect("stop");
    let err = r.start().unwrap_err();
    assert_eq!(err.code, ERR_CODE_ALREADY_STARTED, "single-use recognizer");
    server.join();
}

#[test]
fn concurrent_writes_and_stop_do_not_deadlock() {
    let listener = Arc::new(RecordingListener::new());
    let sent = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&sent);
    let server = MockWsServer::start(move |mut ws| {
        loop {
            match ws.read() {
                Ok(Message::Binary(_)) => {
                    s.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Message::Text(t)) if t == r#"{"type":"end"}"# => {
                    ws.send(Message::Text(
                        r#"{"code":0,"message":"ok","voice_id":"v1","final":1,"result":{"slice_type":2}}"#.into(),
                    ))
                    .unwrap();
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let r = recognizer(listener, &server);
    r.start().expect("start");
    let r = Arc::new(r);

    let mut writers = Vec::new();
    for _ in 0..4 {
        let r = Arc::clone(&r);
        writers.push(std::thread::spawn(move || {
            for _ in 0..25 {
                let _ = r.write(b"\x00\x01");
            }
        }));
    }
    for w in writers {
        w.join().expect("writer thread");
    }
    r.stop().expect("stop");
    assert!(sent.load(Ordering::SeqCst) > 0);
    server.join();
}
