//! Async file recognition example (long audio, <= 12h).
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_APP_ID, TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//!
//! Usage:
//!   cargo run --example file_asr -- <audio.pcm>          # local file
//!   cargo run --example file_asr -- -u <https-url>       # remote URL

use std::env;

use trtc_asr_sdk::asr::FileRecognizer;
use trtc_asr_sdk::common::Credential;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <audio-file> | -u <url>", args[0]);
        std::process::exit(1);
    }

    let credential = Credential::new(
        env::var("TRTC_ASR_APP_ID").expect("TRTC_ASR_APP_ID").parse().expect("TRTC_ASR_APP_ID must be an integer"),
        env::var("TRTC_ASR_SDK_APP_ID").expect("TRTC_ASR_SDK_APP_ID").parse().expect("TRTC_ASR_SDK_APP_ID must be an integer"),
        env::var("TRTC_ASR_SECRET_KEY").expect("TRTC_ASR_SECRET_KEY"),
    );

    let recognizer = FileRecognizer::new(credential);

    let task_id = if args[1] == "-u" {
        let url = args.get(2).expect("missing url after -u");
        recognizer.create_task_from_url(url, "16k_zh_en")
    } else {
        let data = std::fs::read(&args[1]).expect("read audio file");
        recognizer.create_task_from_data(&data, "pcm", "16k_zh_en")
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
