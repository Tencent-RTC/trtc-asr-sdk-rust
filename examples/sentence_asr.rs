//! One-shot sentence recognition example (audio <= 60s).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_APP_ID, TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//!
//! Usage: cargo run --example sentence_asr -- <audio.pcm> [format] [engine]

use std::env;

use trtc_asr_sdk::asr::SentenceRecognizer;
use trtc_asr_sdk::common::Credential;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <audio-file> [format=pcm] [engine=16k_zh_en]", args[0]);
        std::process::exit(1);
    }
    let path = &args[1];
    let format = args.get(2).map(String::as_str).unwrap_or("pcm");
    let engine = args.get(3).map(String::as_str).unwrap_or("16k_zh_en");

    let credential = Credential::new(
        env::var("TRTC_ASR_APP_ID").expect("TRTC_ASR_APP_ID").parse().expect("TRTC_ASR_APP_ID must be an integer"),
        env::var("TRTC_ASR_SDK_APP_ID").expect("TRTC_ASR_SDK_APP_ID").parse().expect("TRTC_ASR_SDK_APP_ID must be an integer"),
        env::var("TRTC_ASR_SECRET_KEY").expect("TRTC_ASR_SECRET_KEY"),
    );

    let data = std::fs::read(path).expect("read audio file");
    let recognizer = SentenceRecognizer::new(credential);
    match recognizer.recognize_data(&data, format, engine) {
        Ok(result) => {
            println!("识别结果: {}", result.result);
            println!("音频时长: {} ms", result.audio_duration);
        }
        Err(e) => {
            eprintln!("识别失败: {e}");
            std::process::exit(1);
        }
    }
}
