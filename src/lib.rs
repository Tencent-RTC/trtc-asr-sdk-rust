//! TRTC-ASR SDK for Rust.
//!
//! Mirrors the Go SDK (trtc-asr-sdk-go): realtime speech recognition over
//! WebSocket ([`asr::SpeechRecognizer`]), one-shot sentence recognition over
//! HTTP ([`asr::SentenceRecognizer`]) and async file recognition
//! ([`asr::FileRecognizer`]).
//!
//! ```no_run
//! use std::sync::Arc;
//! use trtc_asr_sdk::common::Credential;
//! use trtc_asr_sdk::asr::{SpeechRecognizer, SpeechRecognitionListener};
//!
//! struct L;
//! impl SpeechRecognitionListener for L {}
//!
//! let credential = Credential::new(0, 0, "your-sdk-secret-key");
//! let mut recognizer = SpeechRecognizer::new(credential, "16k_zh", Arc::new(L));
//! // recognizer.start().unwrap();
//! // recognizer.write(b"...pcm...").unwrap();
//! // recognizer.stop().unwrap();
//! ```

pub mod asr;
pub mod common;
