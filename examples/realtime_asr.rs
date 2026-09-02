//! Realtime speech recognition example.
//!
//! Reads a PCM file (16kHz 16bit mono) and streams it in 200ms chunks.
//!
//! Credentials come from environment variables:
//!   TRTC_ASR_APP_ID, TRTC_ASR_SDK_APP_ID, TRTC_ASR_SECRET_KEY
//!
//! Usage: cargo run --example realtime_asr -- <audio.pcm> [engine]

use std::env;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use trtc_asr_sdk::asr::{SpeechRecognitionListener, SpeechRecognitionResponse, SpeechRecognizer};
use trtc_asr_sdk::common::{AsrError, Credential};

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
            resp.result.index, resp.result.voice_text_str, resp.result.start_time, resp.result.end_time
        );
        for seg in &resp.result.speaker_segments {
            let name = if seg.speaker_name.is_empty() {
                format!("spk{}", seg.speaker_id)
            } else {
                seg.speaker_name.clone()
            };
            println!("       [{name}] {} ({}-{}ms)", seg.text, seg.start_time, seg.end_time);
        }
    }
    fn on_recognition_complete(&self, resp: &SpeechRecognitionResponse) {
        println!("[complete] voice_id={}", resp.voice_id);
    }
    fn on_fail(&self, _resp: Option<&SpeechRecognitionResponse>, err: &AsrError) {
        eprintln!("[fail] {err}");
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <audio.pcm> [engine_model_type]", args[0]);
        std::process::exit(1);
    }
    let path = &args[1];
    let engine = args.get(2).map(String::as_str).unwrap_or("16k_zh_en");

    let credential = Credential::new(
        env::var("TRTC_ASR_APP_ID").expect("TRTC_ASR_APP_ID").parse().expect("TRTC_ASR_APP_ID must be an integer"),
        env::var("TRTC_ASR_SDK_APP_ID").expect("TRTC_ASR_SDK_APP_ID").parse().expect("TRTC_ASR_SDK_APP_ID must be an integer"),
        env::var("TRTC_ASR_SECRET_KEY").expect("TRTC_ASR_SECRET_KEY"),
    );

    let recognizer = SpeechRecognizer::new(credential, engine, Arc::new(Printer));
    // recognizer.set_speaker_diarization(trtc_asr_sdk::asr::SPEAKER_DIARIZATION_CLUSTER);
    // recognizer.set_word_info(1);
    recognizer.start().expect("start");

    let mut file = File::open(path).expect("open audio file");
    let mut buf = vec![0u8; 6400]; // 200ms of 16kHz 16bit mono PCM
    loop {
        let n = file.read(&mut buf).expect("read audio");
        if n == 0 {
            break;
        }
        if let Err(e) = recognizer.write(&buf[..n]) {
            eprintln!("write failed: {e}");
            break;
        }
        std::thread::sleep(Duration::from_millis(200)); // simulate realtime
    }

    recognizer.stop().expect("stop");
}
