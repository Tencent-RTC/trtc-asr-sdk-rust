//! One-shot sentence recognition example (audio <= 60s).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_APP_ID, TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//!
//! Usage: cargo run --example sentence_asr -- <audio.pcm> [format] [engine] [lang]

use std::env;

use trtc_asr_sdk::asr::{SentenceRecognitionRequest, SentenceRecognizer};
use trtc_asr_sdk::common::Credential;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <audio-file> [format=pcm] <engine> [lang]",
            args[0]
        );
        std::process::exit(1);
    }
    let path = &args[1];
    let format = args.get(2).map(String::as_str).unwrap_or("pcm");
    let engine = args.get(3).map(String::as_str).unwrap_or_else(|| {
        eprintln!("error: engine argument is required (e.g. bigmodel)");
        std::process::exit(2);
    });
    let lang_arg = args.get(4).map(String::as_str).unwrap_or("");
    // The bigmodel engine is best used with an explicit language; every other
    // engine falls back to server-side detection unless a lang argument is given.
    let lang = if lang_arg.is_empty() && engine == "bigmodel" {
        "zh"
    } else {
        lang_arg
    };

    let credential = Credential::new(
        env::var("TRTC_ASR_APP_ID")
            .expect("TRTC_ASR_APP_ID")
            .parse()
            .expect("TRTC_ASR_APP_ID must be an integer"),
        env::var("TRTC_ASR_SDK_APP_ID")
            .expect("TRTC_ASR_SDK_APP_ID")
            .parse()
            .expect("TRTC_ASR_SDK_APP_ID must be an integer"),
        env::var("TRTC_ASR_SECRET_KEY").expect("TRTC_ASR_SECRET_KEY"),
    );

    let data = std::fs::read(path).expect("read audio file");
    let recognizer = SentenceRecognizer::new(credential);
    let mut req = SentenceRecognitionRequest {
        eng_service_type: engine.to_string(),
        voice_format: format.to_string(),
        language: lang.to_string(),
        ..Default::default()
    };
    match recognizer.recognize_data_with_options(&data, &mut req) {
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
