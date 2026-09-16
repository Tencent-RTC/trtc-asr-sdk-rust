//! Sentence (one-shot) speech recognition example over the v3 protocol
//! (POST /v3/transcribe).
//!
//! Recognizes a local audio file (<=60s, <=3MB).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//! (v3 does not need the Tencent Cloud APPID.)
//!
//! Usage: cargo run --example v3_sentence_asr -- <audio.pcm> [engine]

use std::env;

use trtc_asr_sdk::asr::v3::{new_credential, SentenceRecognizer, TranscribeRequest};

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
    let recognizer = SentenceRecognizer::new(credential);

    let data = std::fs::read(&path).expect("read audio file");
    let mut req = TranscribeRequest {
        engine_model_type: engine.clone(),
        voice_format: "pcm".into(),
        language: lang.clone(),
        ..Default::default()
    };
    let resp = recognizer
        .recognize_data_with_options(&data, &mut req)
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
