//! Realtime speech recognition example over the v3 protocol (/asr/v3).
//!
//! Reads a PCM file (16kHz 16bit mono) and streams it in 200ms chunks.
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//! (v3 does not need the Tencent Cloud APPID.)
//!
//! Usage: cargo run --example v3_realtime_asr -- <audio.pcm> [engine] [lang] [speaker-context] [speaker-context-id]
//!
//! Speaker diarization can be made resumable across connections
//! ("断点续传"): keep the printed speaker_context_id and pass it back on the
//! next connection, e.g. `... -- test.pcm bigmodel zh 1` then
//! `... -- test.pcm bigmodel zh 1 <speaker_context_id>`.

use std::env;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use trtc_asr_sdk::asr::v3::{
    new_credential, SpeechRecognizer, SPEAKER_DIARIZATION_CLUSTER,
};
use trtc_asr_sdk::asr::{SpeechRecognitionListener, SpeechRecognitionResponse};
use trtc_asr_sdk::common::AsrError;

struct Printer;

impl SpeechRecognitionListener for Printer {
    fn on_recognition_start(&self, resp: &SpeechRecognitionResponse) {
        println!("[start] voice_id={}", resp.voice_id);
        if let Some(sc) = resp.speaker_continue.as_ref() {
            println!(
                "speaker context: status=\"{}\" id={}",
                sc.continue_status, sc.speaker_context_id
            );
        }
    }
    fn on_sentence_begin(&self, resp: &SpeechRecognitionResponse) {
        println!("[begin] index={}", resp.result.index);
    }
    fn on_recognition_result_change(&self, resp: &SpeechRecognitionResponse) {
        println!("[change] {}", resp.result.voice_text_str);
    }
    fn on_sentence_end(&self, resp: &SpeechRecognitionResponse) {
        println!(
            "[end] index={} text={} ({}-{}ms)",
            resp.result.index,
            resp.result.voice_text_str,
            resp.result.start_time,
            resp.result.end_time
        );
        for seg in &resp.result.speaker_segments {
            let label = if seg.speaker_name.is_empty() {
                format!("spk{}", seg.speaker_id)
            } else {
                seg.speaker_name.clone()
            };
            println!("  [{label}] {} ({}-{} ms)", seg.text, seg.start_time, seg.end_time);
        }
    }
    fn on_recognition_complete(&self, _resp: &SpeechRecognitionResponse) {
        println!("[complete]");
    }
    fn on_fail(&self, _resp: Option<&SpeechRecognitionResponse>, err: &AsrError) {
        eprintln!("[fail] {err}");
    }
}

fn main() {
    let sdk_app_id: i64 = env::var("TRTC_ASR_SDK_APP_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let secret_key = env::var("TRTC_ASR_SECRET_KEY").unwrap_or_default();
    if sdk_app_id == 0 || secret_key.is_empty() {
        eprintln!("Set TRTC_ASR_SDK_APP_ID and TRTC_ASR_SECRET_KEY first.");
        std::process::exit(1);
    }

    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "examples/test.pcm".into());
    let engine = args.next().unwrap_or_else(|| {
        eprintln!("error: engine argument is required (e.g. bigmodel)");
        std::process::exit(2);
    });
    let lang_arg = args.next().unwrap_or_default();
    // The bigmodel engine is best used with an explicit language; every other
    // engine falls back to server-side detection unless a lang argument is given.
    let lang = if lang_arg.is_empty() && engine == "bigmodel" {
        "zh".to_string()
    } else {
        lang_arg
    };

    // arg[3]: resumable speaker diarization (0 off / 1 sync / 2 async);
    // arg[4]: the speaker_context_id printed by an earlier run.
    let speaker_context: i32 = args.next().unwrap_or_default().parse().unwrap_or(0);
    let speaker_context_id = args.next().unwrap_or_default();

    // v3 credentials need only SdkAppID + SecretKey (no Tencent Cloud APPID).
    let credential = new_credential(sdk_app_id, secret_key);
    // start/write/stop take &self (the params live in a RefCell), so no `mut`
    // is needed unless a setter such as set_language is called.
    let mut recognizer = SpeechRecognizer::new(credential, engine, Arc::new(Printer));
    recognizer.set_language(lang);
    // 调试/抓包：TRTC_ASR_WS_ENDPOINT 可把连接指到本地代理（如 ws://127.0.0.1:8899）。
    if let Ok(endpoint) = env::var("TRTC_ASR_WS_ENDPOINT") {
        if !endpoint.is_empty() {
            recognizer.set_endpoint(endpoint);
        }
    }
    // Speaker-context persistence requires speaker diarization; the SDK
    // rejects the combination locally otherwise.
    if speaker_context != 0 {
        recognizer.set_speaker_diarization(SPEAKER_DIARIZATION_CLUSTER);
        recognizer.set_enable_speaker_context(speaker_context);
    }
    if !speaker_context_id.is_empty() {
        recognizer.set_speaker_context_id(speaker_context_id);
    }

    // start() waits synchronously for the server's ack: authentication (4002)
    // and gray-switch (4001) errors are returned here, not via on_fail. While
    // resuming a speaker context the ack arrives once the server has applied
    // the stored snapshot.
    recognizer.start().expect("start failed");

    // The handshake result is also available without a callback. Persist the
    // id: passing it back keeps speaker ids stable across connections.
    if let Some(sc) = recognizer.speaker_continue() {
        println!(
            "speaker context id: {} (status: \"{}\") — reuse it as the 5th argument",
            sc.speaker_context_id, sc.continue_status
        );
    }

    let mut file = File::open(&path).expect("open audio file");
    // 200ms of 16kHz 16bit mono PCM. The server rate-limits to at most 3s of
    // audio per 1s wall-clock (error 4000): keep the pacing when enlarging
    // the buffer.
    let mut buf = [0u8; 6400];
    loop {
        let n = file.read(&mut buf).expect("read audio");
        if n == 0 {
            break;
        }
        recognizer.write(&buf[..n]).expect("write audio");
        std::thread::sleep(Duration::from_millis(200));
    }

    recognizer.stop().expect("stop failed");
}
