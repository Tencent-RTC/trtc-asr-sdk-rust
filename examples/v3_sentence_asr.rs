//! Sentence (one-shot) speech recognition example over the v3 protocol
//! (POST /v3/transcribe).
//!
//! Recognizes a local audio file (<=60s, <=3MB).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//! (v3 does not need the Tencent Cloud APPID.)
//!
//! Prerequisite: the server has enabled the EnableV3Route gray switch for
//! your SDKAppID, otherwise requests fail with 404/4001.
//!
//! Usage: cargo run --example v3_sentence_asr -- <audio.pcm> [engine]

use std::env;

use trtc_asr_sdk::asr::v3::{new_credential, SentenceRecognizer};

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
    let engine = args.next().unwrap_or_else(|| "16k_zh_en".into());

    // v3 credentials need only SdkAppID + SecretKey (no Tencent Cloud APPID).
    let credential = new_credential(sdk_app_id, secret_key);
    let recognizer = SentenceRecognizer::new(credential);

    let data = std::fs::read(&path).expect("read audio file");
    let resp = recognizer
        .recognize_data(&data, "pcm", &engine)
        .expect("recognize failed");

    println!("Result: {}", resp.result);
    println!(
        "Duration: {} ms  RequestId: {}",
        resp.audio_duration, resp.request_id
    );
    for w in &resp.word_list {
        println!("  [{:>6} - {:>6}] {}", w.start_time, w.end_time, w.word);
    }
}
