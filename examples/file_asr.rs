//! Async file recognition example (long audio, <= 12h).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_APP_ID, TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//!
//! Usage:
//!   cargo run --example file_asr -- <audio.pcm>          # local file
//!   cargo run --example file_asr -- -u <https-url>       # remote URL

use std::env;

use trtc_asr_sdk::asr::{CreateRecTaskRequest, FileRecognizer, SOURCE_TYPE_URL};
use trtc_asr_sdk::common::Credential;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <audio-file> <engine> [lang] | -u <url> <engine> [lang]",
            args[0]
        );
        std::process::exit(1);
    }
    let url_mode = args[1] == "-u";
    let engine = args
        .get(if url_mode { 3 } else { 2 })
        .map(String::as_str)
        .unwrap_or_else(|| {
            eprintln!("error: engine argument is required (e.g. bigmodel)");
            std::process::exit(2);
        });
    let lang_arg = args
        .get(if url_mode { 4 } else { 3 })
        .map(String::as_str)
        .unwrap_or("");
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

    let recognizer = FileRecognizer::new(credential);

    let mut req = CreateRecTaskRequest {
        engine_model_type: engine.into(),
        channel_num: 1,
        res_text_format: 1,
        language: lang.into(),
        ..Default::default()
    };
    let task_id = if url_mode {
        req.source_type = SOURCE_TYPE_URL;
        req.url = args.get(2).expect("missing url after -u").clone();
        recognizer.create_task(&req)
    } else {
        let data = std::fs::read(&args[1]).expect("read audio file");
        recognizer.create_task_from_data_with_options(&data, &mut req)
    }
    .expect("create task");
    println!("任务已提交: {task_id}");

    match recognizer.wait_for_result(&task_id) {
        Ok(status) => {
            println!("识别结果: {}", status.result);
            println!("音频时长: {:.2} s", status.audio_duration);
            for detail in &status.result_detail {
                let speaker = if !detail.speaker_role_name.is_empty() {
                    detail.speaker_role_name.clone()
                } else if detail.channel_id > 0 {
                    format!("ch{}", detail.channel_id)
                } else {
                    format!("spk{}", detail.speaker_id)
                };
                println!(
                    "  [{speaker}] ({}-{}ms) {}",
                    detail.start_ms, detail.end_ms, detail.final_sentence
                );
            }
        }
        Err(e) => {
            eprintln!("识别失败: {e}");
            std::process::exit(1);
        }
    }
}
