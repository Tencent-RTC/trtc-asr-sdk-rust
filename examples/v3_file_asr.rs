//! Async audio file recognition example over the v3 protocol
//! (POST /v3/create_transcription + /v3/describe_transcription).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//! (v3 does not need the Tencent Cloud APPID.)
//!
//! Prerequisite: the server has enabled the EnableV3Route gray switch for
//! your SDKAppID, otherwise requests fail with 404/4001.
//!
//! Usage:
//!   cargo run --example v3_file_asr -- local.wav
//!   cargo run --example v3_file_asr -- -u https://example.com/audio.wav

use std::env;

use trtc_asr_sdk::asr::v3::{new_credential, FileRecognizer};

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
    let first = args.next().unwrap_or_default();
    let engine = args.next().unwrap_or_else(|| "16k_zh_en".into());
    if first.is_empty() {
        eprintln!("Usage: v3_file_asr <local.wav> | v3_file_asr -u <url> [engine]");
        std::process::exit(1);
    }

    // v3 credentials need only SdkAppID + SecretKey (no Tencent Cloud APPID).
    let credential = new_credential(sdk_app_id, secret_key);
    let recognizer = FileRecognizer::new(credential);

    let task_id = if first == "-u" {
        let url = args.next().unwrap_or_default();
        recognizer
            .create_task_from_url(&url, &engine)
            .expect("create task failed")
    } else {
        let data = std::fs::read(&first).expect("read audio file");
        recognizer
            .create_task_from_data(&data, &engine)
            .expect("create task failed")
    };
    println!("Task created: {task_id}");

    let status = recognizer.wait_for_result(&task_id).expect("wait failed");
    println!(
        "Status: {}  Duration: {:.2} s",
        status.status_str, status.audio_duration
    );
    println!("Result: {}", status.result);
    for d in &status.result_detail {
        let speaker = if !d.speaker_role_name.is_empty() {
            format!(" [{}]", d.speaker_role_name)
        } else if d.speaker_id > 0 {
            format!(" [spk{}]", d.speaker_id)
        } else {
            String::new()
        };
        println!(
            "  [{:>6} - {:>6}]{} {}",
            d.start_ms, d.end_ms, speaker, d.final_sentence
        );
    }
}
