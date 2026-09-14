//! Realtime speech recognition example over the v3 protocol (/asr/v3).
//!
//! Reads a PCM file (16kHz 16bit mono) and streams it in 200ms chunks.
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//! (v3 does not need the Tencent Cloud APPID.)
//!
//! Prerequisite: the server has enabled the EnableV3Route gray switch for
//! your SDKAppID, otherwise start() fails with 4001.
//!
//! Usage: cargo run --example v3_realtime_asr -- <audio.pcm> [engine]

use std::env;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use trtc_asr_sdk::asr::v3::{new_credential, SpeechRecognizer};
use trtc_asr_sdk::asr::{SpeechRecognitionListener, SpeechRecognitionResponse};
use trtc_asr_sdk::common::AsrError;

struct Printer;

impl SpeechRecognitionListener for Printer {
    fn on_recognition_start(&self, resp: &SpeechRecognitionResponse) {
        println!("[start] voice_id={}", resp.voice_id);
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

    // v3 credentials need only SdkAppID + SecretKey (no Tencent Cloud APPID).
    let credential = new_credential(sdk_app_id, secret_key);
    // start/write/stop take &self (the params live in a RefCell), so no `mut`
    // is needed unless a setter such as set_language is called.
    let mut recognizer = SpeechRecognizer::new(credential, engine, Arc::new(Printer));
    recognizer.set_language(lang);

    // start() waits synchronously for the server's ack: authentication (4002)
    // and gray-switch (4001) errors are returned here, not via on_fail.
    recognizer.start().expect("start failed");

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
