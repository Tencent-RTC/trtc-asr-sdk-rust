//! Realtime speech recognition client (WebSocket).
//!
//! Usage:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use trtc_asr_sdk::common::Credential;
//! # use trtc_asr_sdk::asr::{SpeechRecognizer, SpeechRecognitionListener};
//! # struct L; impl SpeechRecognitionListener for L {}
//! let credential = Credential::new(0, 0, "your-sdk-secret-key");
//! let mut recognizer = SpeechRecognizer::new(credential, "16k_zh", Arc::new(L));
//! recognizer.start().unwrap();
//! recognizer.write(b"\0\0").unwrap();
//! recognizer.stop().unwrap();
//! ```
//!
//! Lifecycle and concurrency (mirrors the Go SDK):
//! - A `SpeechRecognizer` is single-use: once it reaches the stopped state
//!   (via [`stop`](SpeechRecognizer::stop) or a terminal error) it cannot be
//!   restarted. Create a new instance to reconnect.
//! - All `set_*` options must be configured before `start` and must not be
//!   called concurrently with `start`.
//! - After `start` returns, `write` and `stop` may be called from threads
//!   other than the one that called `start`. Recognition callbacks are
//!   delivered on an internal reader thread.
//! - `stop` is safe to call from a recognition callback: it detects re-entry
//!   by comparing thread IDs and returns right after sending the end signal.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::thread;
use std::time::Duration;

use tungstenite::{Message, WebSocket};
use url::Url;
use uuid::Uuid;

use crate::common::errors::{
    AsrError, Result, ERR_CODE_ALREADY_STARTED, ERR_CODE_AUTH_FAILED, ERR_CODE_CONNECT_FAILED,
    ERR_CODE_NOT_STARTED, ERR_CODE_READ_FAILED, ERR_CODE_WRITE_FAILED,
};
use crate::common::signature::{SignatureParams, SpeakerRole};
use crate::common::{resolve_ws_endpoint, usersig, Credential};

use super::params::{validate_enum_option, validate_speaker_diarization, validate_vad_tuning};
use super::types::SpeechRecognitionResponse;

/// Production WebSocket endpoint for the TRTC-ASR service.
pub const ENDPOINT: &str = "wss://asr.cloud-rtc.com";

// Recognizer states.
const STATE_IDLE: u8 = 0;
const STATE_STARTING: u8 = 1;
const STATE_RUNNING: u8 = 2;
const STATE_STOPPING: u8 = 3;
const STATE_STOPPED: u8 = 4;

// Write-timeout bounds. A single `write` holds the writer for at most
// `write_timeout` (enforced via the socket write timeout), so `stop`'s
// worst-case wait to acquire the writer for the end signal stays bounded.
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_WRITE_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

// Liveness backstop for the end-signal handoff (see
// `Shared::wait_for_end_signal_until`). Sized so a healthy `stop` never hits
// it under any allowed configuration: reaching the wire costs at most one
// in-flight audio write plus `stop`'s own send, each bounded by the socket
// write timeout, plus a margin for scheduling and TLS flush on a starved CPU.
// It only matters when `stop` dies in between.
const END_SIGNAL_HANDOFF_TIMEOUT: Duration =
    Duration::from_secs(2 * MAX_WRITE_TIMEOUT.as_secs() + 5);

// Stop-timeout bounds: caps how long `stop` waits for the server's final
// response after the end signal before forcing the connection closed.
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_STOP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll interval for the reader thread. The sync socket uses a short read
/// timeout so `close` can always interrupt a blocked read promptly.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(100);

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Callback interface for speech recognition events. All methods have
/// default no-op implementations so tests and simple callers can implement
/// only the events they care about.
pub trait SpeechRecognitionListener: Send + Sync {
    /// Called when the recognition session starts successfully.
    fn on_recognition_start(&self, _response: &SpeechRecognitionResponse) {}
    /// Called when a new sentence begins.
    fn on_sentence_begin(&self, _response: &SpeechRecognitionResponse) {}
    /// Called when intermediate recognition results are available.
    fn on_recognition_result_change(&self, _response: &SpeechRecognitionResponse) {}
    /// Called when a sentence ends with the final result.
    fn on_sentence_end(&self, _response: &SpeechRecognitionResponse) {}
    /// Called when the entire recognition session completes.
    fn on_recognition_complete(&self, _response: &SpeechRecognitionResponse) {}
    /// Called when an error occurs during recognition.
    fn on_fail(&self, _response: Option<&SpeechRecognitionResponse>, _err: &AsrError) {}
}

/// Duplex byte stream used by the WebSocket: plain TCP for `ws://` and
/// TLS-wrapped TCP for `wss://`.
enum Stream {
    Plain(TcpStream),
    Tls(rustls::StreamOwned<rustls::ClientConnection, TcpStream>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Runtime state shared between the recognizer (API threads) and the reader
/// thread.
struct Shared {
    state: AtomicU8,
    /// The WebSocket connection. Reads hold the lock for at most
    /// [`READ_POLL_INTERVAL`] (socket read timeout), so writers and `close`
    /// never block long.
    ws: Mutex<Option<WebSocket<Stream>>>,
    /// Force-close handle: a cloned fd of the underlying socket. Shutting it
    /// down unblocks any in-flight read/write with an I/O error.
    shutdown: Mutex<Option<TcpStream>>,
    /// Signalled (set + notify_all) when the reader thread has fully exited,
    /// including delivery of terminal callbacks.
    done: (Mutex<bool>, Condvar),
    /// Set once a terminal response (Final=1 or code!=0) has been received.
    /// External `stop` callers then wait for `done` without a timeout so the
    /// terminal callbacks can finish.
    terminal_received: AtomicBool,
    /// Set after `stop` writes its end signal. Once stopping begins, the
    /// reader waits for this handoff instead of racing `stop` to re-acquire
    /// the WebSocket mutex after each polling read.
    end_signal_sent: AtomicBool,
    /// Sticky flag raised when a handoff hit its liveness deadline. From then
    /// on the reader stops yielding the socket: nobody is going to publish
    /// the end frame any more, and re-arming the deadline every round would
    /// leave the connection unattended for most of the session.
    handoff_expired: AtomicBool,
    /// Test-only acknowledgement that a reader has checked the handoff
    /// predicate while holding done.0 and is about to wait on its Condvar.
    /// This makes the lost-wakeup regression tests deterministic.
    #[cfg(test)]
    handoff_waiting: AtomicBool,
    /// Thread ID of the reader thread, used to detect re-entrant `stop` calls
    /// from listener callbacks (cleaner than Go's stack walking).
    reader_tid: Mutex<Option<thread::ThreadId>>,
    stop_timeout: Mutex<Duration>,
    finish_once: Once,
    /// The resolved voice ID for the running session (generated when the user
    /// did not set one).
    voice_id: Mutex<String>,
}

impl Shared {
    fn set_voice_id(&self, id: &str) {
        *self.voice_id.lock().unwrap() = id.to_string();
    }
    fn voice_id(&self) -> String {
        self.voice_id.lock().unwrap().clone()
    }
}

impl Shared {
    /// Publishes a terminal state while holding the mutex associated with the
    /// lifecycle condition variable. Every predicate consumed by a condvar
    /// wait must be changed under that same mutex; otherwise a reader can
    /// observe the old predicate and miss the notification.
    fn mark_stopped_and_wake(&self) {
        {
            let _guard = self.done.0.lock().unwrap();
            self.state.store(STATE_STOPPED, Ordering::SeqCst);
        }
        self.done.1.notify_all();
    }

    /// Advances the recognizer to the terminal stopped state exactly once and
    /// closes the connection. Invoked before terminal callbacks (so a
    /// `stop`/`write` from inside a callback returns immediately) and again
    /// from the reader thread's exit path as a catch-all.
    fn finish(&self) {
        self.finish_once.call_once(|| {
            self.mark_stopped_and_wake();
            // Do not hold done.0 while closing: close() takes the socket
            // mutex, whereas the reader releases done.0 before taking it.
            self.close();
        });
    }

    fn close(&self) {
        if let Ok(mut guard) = self.shutdown.lock() {
            if let Some(sock) = guard.take() {
                // Force-unblock any in-flight read/write. Errors are expected
                // when the peer already went away; ignore them.
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Ok(mut guard) = self.ws.lock() {
            if let Some(mut ws) = guard.take() {
                let _ = ws.close(None);
            }
        }
    }

    fn set_done(&self) {
        if let Ok(mut guard) = self.done.0.lock() {
            *guard = true;
        }
        self.done.1.notify_all();
    }

    /// Records terminal arrival under the done mutex and wakes any stop()
    /// in its timed wait, so it can switch to the unbounded wait for the
    /// terminal callbacks without a lost-wakeup window.
    fn mark_terminal_received(&self) {
        {
            let _guard = self.done.0.lock().unwrap();
            self.terminal_received.store(true, Ordering::SeqCst);
        }
        self.done.1.notify_all();
    }

    /// Publishes that stop() has placed the end frame on the wire. The done
    /// mutex makes the flag update and reader's condition wait atomic with
    /// respect to the notification.
    fn mark_end_signal_sent(&self) {
        {
            let _guard = self.done.0.lock().unwrap();
            self.end_signal_sent.store(true, Ordering::SeqCst);
        }
        self.done.1.notify_all();
    }

    /// Once a caller transitions RUNNING -> STOPPING, give that caller the
    /// WebSocket mutex until it writes the end frame. Without this handoff the
    /// polling reader can repeatedly re-acquire the mutex after a timeout and
    /// starve stop() long enough to lose the server's final response.
    ///
    /// Returns false when the session became terminal before an end frame was
    /// sent, in which case the reader must exit rather than acquire the socket.
    fn wait_for_end_signal_if_stopping(&self) -> bool {
        self.wait_for_end_signal_until(std::time::Instant::now() + END_SIGNAL_HANDOFF_TIMEOUT)
    }

    /// Handoff body with an injectable deadline (tests use a short one).
    ///
    /// The deadline is a liveness backstop, not part of the protocol: if
    /// stop() dies between the state transition and the end frame, nobody
    /// will ever publish a predicate change, and an unbounded wait would park
    /// the reader forever with the socket unattended. Expiry is sticky, so on
    /// the next round the reader is back to plain polling rather than
    /// stalling once per deadline.
    fn wait_for_end_signal_until(&self, deadline: std::time::Instant) -> bool {
        let (done_lock, cvar) = &self.done;
        let mut guard = done_lock.lock().unwrap();
        while self.state() == STATE_STOPPING
            && !self.end_signal_sent.load(Ordering::SeqCst)
            && !self.handoff_expired.load(Ordering::SeqCst)
        {
            // Absolute deadline: spurious wakes must not extend the handoff.
            let now = std::time::Instant::now();
            if now >= deadline {
                self.handoff_expired.store(true, Ordering::SeqCst);
                break;
            }
            // The flag is set while done.0 is held. A test that observes it
            // and then calls mark_end_signal_sent()/finish() cannot notify
            // before Condvar::wait atomically releases done.0 and parks.
            #[cfg(test)]
            self.handoff_waiting.store(true, Ordering::SeqCst);
            let (g, _) = cvar.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
            #[cfg(test)]
            self.handoff_waiting.store(false, Ordering::SeqCst);
        }
        #[cfg(test)]
        self.handoff_waiting.store(false, Ordering::SeqCst);
        self.state() != STATE_STOPPED
    }

    fn state(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }
}

/// The main client for realtime speech recognition.
pub struct SpeechRecognizer {
    credential: Credential,
    listener: Arc<dyn SpeechRecognitionListener>,

    // Configuration. Set via `set_*` before `start`.
    endpoint: String,
    engine_model_type: String,
    voice_format: i32,
    need_vad: i32,
    convert_num_mode: i32,
    hotword_id: String,
    hotword_list: String,
    customization_id: String,
    replace_text_id: String,
    filter_dirty: i32,
    filter_modal: i32,
    filter_punc: i32,
    filter_empty_result: Option<i32>,
    word_info: i32,
    vad_silence_time: i32,
    vad_level: Option<i32>,
    noise_threshold: Option<f64>,
    max_speak_time: i32,
    input_sample_rate: i32,
    speaker_diarization: i32,
    speaker_number: i32,
    speaker_roles: Vec<SpeakerRole>,
    voiceprint_ids: Vec<String>,
    voice_id: String,
    language: String,

    write_timeout: Duration,
    stop_timeout: Duration,

    shared: Arc<Shared>,
}

/// RAII backstop: dropping a still-running recognizer finishes the session
/// (marks it stopped and closes the connection) so the reader thread exits
/// instead of leaking until the server closes the socket. The graceful
/// protocol shutdown (end signal + final response) still requires an
/// explicit [`SpeechRecognizer::stop`].
impl Drop for SpeechRecognizer {
    fn drop(&mut self) {
        if self.shared.state() == STATE_RUNNING || self.shared.state() == STATE_STOPPING {
            self.shared.finish();
        }
    }
}

impl SpeechRecognizer {
    /// Creates a new recognizer.
    ///
    /// - `credential`: TRTC authentication credential
    /// - `engine_model_type`: recognition engine model (e.g. "16k_zh",
    ///   "8k_zh", "16k_zh_en")
    /// - `listener`: callback listener for recognition events
    pub fn new(
        credential: Credential,
        engine_model_type: impl Into<String>,
        listener: Arc<dyn SpeechRecognitionListener>,
    ) -> Self {
        SpeechRecognizer {
            credential,
            listener,
            endpoint: String::new(),
            engine_model_type: engine_model_type.into(),
            voice_format: 1, // PCM
            need_vad: 1,
            convert_num_mode: 1,
            hotword_id: String::new(),
            hotword_list: String::new(),
            customization_id: String::new(),
            replace_text_id: String::new(),
            filter_dirty: 0,
            filter_modal: 0,
            filter_punc: 0,
            filter_empty_result: None,
            word_info: 0,
            vad_silence_time: 0,
            vad_level: None,
            noise_threshold: None,
            max_speak_time: 0,
            input_sample_rate: 0,
            speaker_diarization: 0,
            speaker_number: 0,
            speaker_roles: Vec::new(),
            voiceprint_ids: Vec::new(),
            voice_id: String::new(),
            language: String::new(),
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            stop_timeout: DEFAULT_STOP_TIMEOUT,
            shared: Arc::new(Shared {
                state: AtomicU8::new(STATE_IDLE),
                ws: Mutex::new(None),
                shutdown: Mutex::new(None),
                done: (Mutex::new(false), Condvar::new()),
                terminal_received: AtomicBool::new(false),
                end_signal_sent: AtomicBool::new(false),
                handoff_expired: AtomicBool::new(false),
                #[cfg(test)]
                handoff_waiting: AtomicBool::new(false),
                reader_tid: Mutex::new(None),
                stop_timeout: Mutex::new(DEFAULT_STOP_TIMEOUT),
                finish_once: Once::new(),
                voice_id: Mutex::new(String::new()),
            }),
        }
    }

    /// Overrides the WebSocket endpoint (for testing against a mock server).
    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    /// Sets the audio encoding format. 1: PCM (default).
    pub fn set_voice_format(&mut self, format: i32) {
        self.voice_format = format;
    }

    /// Sets whether to enable VAD. 0: disable, 1: enable (default).
    pub fn set_need_vad(&mut self, need_vad: i32) {
        self.need_vad = need_vad;
    }

    /// Sets the number conversion mode. 0: none, 1: smart (default), 3: math.
    pub fn set_convert_num_mode(&mut self, mode: i32) {
        self.convert_num_mode = mode;
    }

    /// Sets the hotword list ID for biasing recognition.
    pub fn set_hotword_id(&mut self, id: impl Into<String>) {
        self.hotword_id = id.into();
    }

    /// Sets a temporary inline hotword list, which does not require creating
    /// a hotword table on the console.
    ///
    /// Format: `word1|weight1,word2|weight2`. Each word is at most 30 bytes
    /// and the weight must be 1-11 (11 = super hotword) or 100 (homophone
    /// replacement).
    pub fn set_hotword_list(&mut self, list: impl Into<String>) {
        self.hotword_list = list.into();
    }

    /// Sets the custom language model ID.
    pub fn set_customization_id(&mut self, id: impl Into<String>) {
        self.customization_id = id.into();
    }

    /// Sets the replacement word table ID used for forced text replacement on
    /// the recognized result.
    pub fn set_replace_text_id(&mut self, id: impl Into<String>) {
        self.replace_text_id = id.into();
    }

    /// Sets the profanity filter mode. 0: off (default), 1: filter,
    /// 2: replace with *.
    pub fn set_filter_dirty(&mut self, mode: i32) {
        self.filter_dirty = mode;
    }

    /// Sets the modal particle filter mode. 0: off (default), 1: partial,
    /// 2: strict.
    pub fn set_filter_modal(&mut self, mode: i32) {
        self.filter_modal = mode;
    }

    /// Sets the sentence-ending punctuation filter mode. 0: off (default),
    /// 1: filter.
    pub fn set_filter_punc(&mut self, mode: i32) {
        self.filter_punc = mode;
    }

    /// Sets whether empty recognition results are delivered. 0: deliver,
    /// 1: skip (server default).
    ///
    /// Calling this makes the choice explicit on the wire, so passing 0 is
    /// honored instead of falling back to the server default.
    pub fn set_filter_empty_result(&mut self, mode: i32) {
        self.filter_empty_result = Some(mode);
    }

    /// Sets whether to show word-level timing information. 0: no (default),
    /// 1: yes, 2: include punctuation timing.
    ///
    /// Word-level speaker attribution (`WordInfo::speaker_id`) requires a
    /// non-zero value together with [`set_speaker_diarization`](Self::set_speaker_diarization).
    pub fn set_word_info(&mut self, mode: i32) {
        self.word_info = mode;
    }

    /// Sets the silence detection threshold in milliseconds.
    /// Range: 240-2000, default: server-side (currently 800).
    pub fn set_vad_silence_time(&mut self, ms: i32) {
        self.vad_silence_time = ms;
    }

    /// Selects the VAD profile: 0 = high recall, 1 = far-field noise
    /// filtering (server default).
    ///
    /// Calling this makes the choice explicit on the wire, so passing 0 is
    /// honored instead of falling back to the server default.
    pub fn set_vad_level(&mut self, level: i32) {
        self.vad_level = Some(level);
    }

    /// Fine-tunes VAD noise suppression. Valid range: [0, 4]; larger values
    /// suppress more noise at the cost of recall. When set, it overrides the
    /// profile selected by [`set_vad_level`](Self::set_vad_level).
    pub fn set_noise_threshold(&mut self, threshold: f64) {
        self.noise_threshold = Some(threshold);
    }

    /// Sets the maximum speech time in milliseconds.
    /// Range: 5000-90000, default: 60000.
    pub fn set_max_speak_time(&mut self, ms: i32) {
        self.max_speak_time = ms;
    }

    /// Declares the sample rate of the incoming PCM audio. Only 8000 is
    /// supported, which lets an 8kHz stream be fed to a 16k engine.
    pub fn set_input_sample_rate(&mut self, rate: i32) {
        self.input_sample_rate = rate;
    }

    /// Enables realtime speaker diarization:
    /// - [`SPEAKER_DIARIZATION_OFF`](crate::common::SPEAKER_DIARIZATION_OFF) (0): disabled (default)
    /// - [`SPEAKER_DIARIZATION_CLUSTER`](crate::common::SPEAKER_DIARIZATION_CLUSTER) (1): anonymous clustering
    /// - [`SPEAKER_DIARIZATION_VOICEPRINT`](crate::common::SPEAKER_DIARIZATION_VOICEPRINT) (3): voiceprint role authentication
    pub fn set_speaker_diarization(&mut self, mode: i32) {
        self.speaker_diarization = mode;
    }

    /// Hints the expected number of speakers. 0 = auto detection (default).
    /// Applies to both diarization modes.
    pub fn set_speaker_number(&mut self, n: i32) {
        self.speaker_number = n;
    }

    /// Registers temporary voiceprints for this session. Only used with
    /// voiceprint diarization mode.
    pub fn set_speaker_roles(&mut self, roles: Vec<SpeakerRole>) {
        self.speaker_roles = roles;
    }

    /// Registers previously enrolled voiceprints by ID. Only used with
    /// voiceprint diarization mode.
    pub fn set_voiceprint_ids(&mut self, ids: Vec<String>) {
        self.voiceprint_ids = ids;
    }

    /// Sets a custom voice ID. A UUID is generated when left empty.
    pub fn set_voice_id(&mut self, id: impl Into<String>) {
        self.voice_id = id.into();
    }

    /// Sets the language hint for the bigmodel engine (e.g. "zh", "en",
    /// "auto").
    pub fn set_language(&mut self, lang: impl Into<String>) {
        self.language = lang.into();
    }

    /// Sets the timeout for a single audio write.
    ///
    /// The value is clamped to [50ms, 30s]; a non-positive value resets it to
    /// the default (5s). Because `stop` must acquire the writer to send the
    /// end signal, an unbounded write timeout would let an in-flight write
    /// delay `stop` indefinitely — clamping keeps `stop`'s worst-case exit
    /// time predictable.
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = clamp_timeout(
            timeout,
            MIN_WRITE_TIMEOUT,
            MAX_WRITE_TIMEOUT,
            DEFAULT_WRITE_TIMEOUT,
        );
    }

    /// Sets how long `stop` waits for the server's final response after
    /// sending the end signal before forcing the connection closed.
    ///
    /// The value is clamped to [1s, 60s]; a non-positive value resets it to
    /// the default (10s).
    pub fn set_stop_timeout(&mut self, timeout: Duration) {
        self.stop_timeout = clamp_timeout(
            timeout,
            MIN_STOP_TIMEOUT,
            MAX_STOP_TIMEOUT,
            DEFAULT_STOP_TIMEOUT,
        );
    }

    /// Initiates the WebSocket connection and begins the recognition session.
    ///
    /// Takes `&self` (not `&mut self`) so a recognizer shared via `Arc` can be
    /// started after being captured by its own listener — required for
    /// callbacks that call [`stop`](Self::stop).
    pub fn start(&self) -> Result<()> {
        let cas = self.shared.state.compare_exchange(
            STATE_IDLE,
            STATE_STARTING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        if cas.is_err() {
            return Err(AsrError::new(
                ERR_CODE_ALREADY_STARTED,
                "recognizer already started",
            ));
        }

        // Validate before dialing so an invalid option fails locally instead
        // of costing a connection and coming back as a server-side 4001.
        if let Err(e) = self.validate_options() {
            self.shared.state.store(STATE_IDLE, Ordering::SeqCst);
            return Err(e);
        }

        if let Err(e) = self.connect() {
            self.shared.state.store(STATE_IDLE, Ordering::SeqCst);
            return Err(e);
        }

        self.shared.state.store(STATE_RUNNING, Ordering::SeqCst);

        // Start reading responses on the reader thread.
        *self.shared.stop_timeout.lock().unwrap() = self.stop_timeout;
        let shared = Arc::clone(&self.shared);
        let listener = Arc::clone(&self.listener);
        // connect() resolved the final voice_id (generated when unset).
        let voice_id = self.shared.voice_id();
        thread::spawn(move || read_loop(shared, listener, voice_id));

        Ok(())
    }

    /// Checks the options that have a documented server-side range.
    fn validate_options(&self) -> Result<()> {
        validate_speaker_diarization(
            self.speaker_diarization,
            self.speaker_number,
            &self.speaker_roles,
            &self.voiceprint_ids,
        )?;
        validate_vad_tuning(self.vad_level, self.noise_threshold)?;
        if let Some(v) = self.filter_empty_result {
            validate_enum_option("FilterEmptyResult", v, &[0, 1])?;
        }
        // 8000 is the only supported override; 0 means "use the engine rate".
        validate_enum_option("InputSampleRate", self.input_sample_rate, &[0, 8000])
    }

    /// Sends audio data to the ASR service for recognition.
    /// The data should be in the format specified by
    /// [`set_voice_format`](Self::set_voice_format) (default: PCM).
    pub fn write(&self, data: &[u8]) -> Result<()> {
        if self.shared.state() != STATE_RUNNING {
            return Err(AsrError::new(ERR_CODE_NOT_STARTED, "recognizer not running"));
        }

        // Serialize all writes through the connection mutex. The socket write
        // timeout bounds how long a single write can hold it, so `stop` can
        // always acquire the writer promptly to send the end signal.
        let mut guard = self.shared.ws.lock().unwrap();
        let ws = match guard.as_mut() {
            Some(ws) => ws,
            None => {
                return Err(AsrError::new(
                    ERR_CODE_NOT_STARTED,
                    "connection not established",
                ))
            }
        };

        // Re-check the state under the lock. Between the entry check above
        // and acquiring the lock, `stop` may have transitioned the state and
        // sent the end signal. Writing audio after end would violate the
        // protocol, so bail out instead.
        if self.shared.state() != STATE_RUNNING {
            return Err(AsrError::new(ERR_CODE_NOT_STARTED, "recognizer not running"));
        }

        ws.send(Message::Binary(data.to_vec().into())).map_err(|e| {
            AsrError::new(
                ERR_CODE_WRITE_FAILED,
                format!("write audio data failed: {e}"),
            )
        })
    }

    /// Gracefully stops the recognition session.
    ///
    /// Sends the end signal and waits for the server's final response (up to
    /// `stop_timeout`) before forcing the connection closed. Worst-case
    /// duration is bounded by `write_timeout` (to acquire the writer) plus
    /// `stop_timeout`.
    ///
    /// Safe to call from a recognition callback: re-entry is detected via the
    /// reader thread ID. For terminal callbacks the recognizer has already
    /// advanced to stopped, so `stop` returns immediately with not-running;
    /// for non-terminal callbacks it sends the end signal and returns without
    /// waiting (waiting would self-block: callbacks run on the reader thread).
    pub fn stop(&self) -> Result<()> {
        // State changes that participate in the reader's end-signal Condvar
        // handoff are made under done.0, the mutex associated with that
        // condition variable. This prevents a reader from missing STOPPING.
        let cas = {
            let _guard = self.shared.done.0.lock().unwrap();
            self.shared.state.compare_exchange(
                STATE_RUNNING,
                STATE_STOPPING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
        };
        if cas.is_err() {
            return Err(AsrError::new(ERR_CODE_NOT_STARTED, "recognizer not running"));
        }
        self.shared.done.1.notify_all();

        // Keep connection-missing distinct from a natural terminal finish:
        // both appear as no send result, but only the former is an API error.
        let (send_result, connection_missing) = {
            let mut guard = self.shared.ws.lock().unwrap();
            match guard.as_mut() {
                None if self.shared.state() == STATE_STOPPED => (None, false),
                None => (None, true),
                Some(_) if self.shared.state() == STATE_STOPPED => (None, false),
                Some(ws) => (Some(ws.send(Message::Text(r#"{"type":"end"}"#.into()))), false),
            }
        };

        if connection_missing {
            self.shared.finish();
            return Err(AsrError::new(
                ERR_CODE_NOT_STARTED,
                "connection not established",
            ));
        }

        let end_frame_written = matches!(&send_result, Some(Ok(())));
        if let Some(Err(e)) = send_result {
            if self.shared.state() == STATE_STOPPED {
                self.wait_for_read_loop_or_close();
                return Ok(());
            }
            self.shared.finish();
            return Err(AsrError::new(
                ERR_CODE_WRITE_FAILED,
                format!("send end signal failed: {e}"),
            ));
        }
        if end_frame_written {
            self.shared.mark_end_signal_sent();
        } else {
            // The reader reached a terminal state while stop() was waiting
            // for the WebSocket writer. Preserve the callback completion
            // guarantee before reporting success.
            self.wait_for_read_loop_or_close();
            return Ok(());
        }

        // If stop is called from within a listener callback (which runs on
        // the reader thread), waiting on done here would self-block until
        // timeout. Return after sending end; the watchdog preserves the
        // timeout semantics if the server never sends a terminal response.
        if self.called_from_listener_callback() {
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || shared.wait_for_read_loop_or_close_impl());
            return Ok(());
        }

        self.wait_for_read_loop_or_close();
        self.shared.finish();
        Ok(())
    }

    fn wait_for_read_loop_or_close(&self) {
        self.shared.wait_for_read_loop_or_close_impl();
    }

    fn called_from_listener_callback(&self) -> bool {
        let tid = thread::current().id();
        self.shared
            .reader_tid
            .lock()
            .map(|guard| *guard == Some(tid))
            .unwrap_or(false)
    }

    fn connect(&self) -> Result<()> {
        let voice_id = if self.voice_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            self.voice_id.clone()
        };

        // Resolve UserSig locally without mutating the shared credential.
        let user_sig = if self.credential.user_sig.is_empty() {
            usersig::gen_user_sig(
                self.credential.sdk_app_id as u64,
                &self.credential.secret_key,
                &voice_id,
                86400,
            )
            .map_err(|e| {
                AsrError::new(
                    ERR_CODE_AUTH_FAILED,
                    format!("generate user sig failed: {e}"),
                )
            })?
        } else {
            self.credential.user_sig.clone()
        };

        // Build request parameters (AppID is used for the URL secretid
        // parameter). Authentication identity (sdkappid + usersig) travels in
        // the query string instead of headers, so browser WebSocket clients
        // work without header support.
        let mut p = SignatureParams::new(
            self.credential.app_id,
            self.engine_model_type.clone(),
            voice_id.clone(),
        );
        p.sdk_app_id = self.credential.sdk_app_id;
        p.voice_format = self.voice_format;
        p.need_vad = self.need_vad;
        p.convert_num_mode = self.convert_num_mode;
        p.hotword_id = self.hotword_id.clone();
        p.hotword_list = self.hotword_list.clone();
        p.customization_id = self.customization_id.clone();
        p.replace_text_id = self.replace_text_id.clone();
        p.filter_dirty = self.filter_dirty;
        p.filter_modal = self.filter_modal;
        p.filter_punc = self.filter_punc;
        p.filter_empty_result = self.filter_empty_result;
        p.word_info = self.word_info;
        p.vad_silence_time = self.vad_silence_time;
        p.vad_level = self.vad_level;
        p.noise_threshold = self.noise_threshold;
        p.max_speak_time = self.max_speak_time;
        p.input_sample_rate = self.input_sample_rate;
        p.speaker_diarization = self.speaker_diarization;
        p.speaker_number = self.speaker_number;
        p.speaker_roles = self.speaker_roles.clone();
        p.voiceprint_ids = self.voiceprint_ids.clone();
        p.language = self.language.clone();

        let query = p.build_query_string_with_signature(&user_sig);
        // URL path uses the Tencent Cloud AppID (not SdkAppID).
        let base = resolve_ws_endpoint(&self.endpoint, &self.credential.site)?;
        let ws_url = format!("{}/asr/v2/{}?{}", base, self.credential.app_id, query);

        let (ws, shutdown) = connect_ws(
            &ws_url,
            HANDSHAKE_TIMEOUT,
            READ_POLL_INTERVAL,
            self.write_timeout,
        )?;

        *self.shared.ws.lock().unwrap() = Some(ws);
        *self.shared.shutdown.lock().unwrap() = Some(shutdown);
        self.shared.set_voice_id(&voice_id);
        Ok(())
    }
}

impl Shared {
    fn wait_for_read_loop_or_close_impl(&self) {
        let (done_lock, cvar) = &self.done;
        let mut guard = done_lock.lock().unwrap();
        let timeout = *self.stop_timeout.lock().unwrap();
        // Absolute deadline: spurious wakes must NOT reset the budget,
        // otherwise a stream of them could keep stop() blocked far beyond
        // stop_timeout (Java already uses the deadline pattern).
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if *guard {
                return;
            }
            if self.terminal_received.load(Ordering::SeqCst) {
                // The terminal response has arrived (possibly after this
                // waiter entered the timed wait); wait without a timeout so
                // the terminal callbacks can finish — mirrors the Go SDK's
                // waitForCallbacksOrAbort.
                while !*guard {
                    guard = cvar.wait(guard).unwrap();
                }
                return;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                // Timed out waiting for the server's final response: force
                // the connection closed so the reader thread exits.
                drop(guard);
                self.close();
                // Give the reader thread a brief chance to observe the closed
                // connection and run its exit path; callers must not wait
                // indefinitely here (the reader thread will exit on its own).
                let guard = done_lock.lock().unwrap();
                let _ = cvar.wait_timeout(guard, READ_POLL_INTERVAL * 3).unwrap();
                return;
            }
            // Wakes (from set_done / mark_terminal_received / spurious) loop
            // back and re-check both flags with the remaining budget.
            let remaining = deadline.saturating_duration_since(now);
            let (g, _) = cvar.wait_timeout(guard, remaining).unwrap();
            guard = g;
        }
    }
}

/// The reader thread body. Delivers events from the server until the session
/// ends, then marks the lifecycle done.
fn read_loop(
    shared: Arc<Shared>,
    listener: Arc<dyn SpeechRecognitionListener>,
    voice_id: String,
) {
    *shared.reader_tid.lock().unwrap() = Some(thread::current().id());

    let result = catch_unwind(AssertUnwindSafe(|| {
        read_loop_inner(&shared, &listener, &voice_id);
    }));

    if let Err(payload) = result {
        // A panic from a user-supplied listener callback must never crash the
        // host process. Finish the lifecycle first (so a re-entrant stop from
        // on_fail observes the stopped state), then surface the panic.
        shared.finish();
        let msg = panic_message(&payload);
        let backtrace = std::backtrace::Backtrace::capture();
        safe_on_fail(
            &listener,
            None,
            &AsrError::new(
                ERR_CODE_READ_FAILED,
                format!("recovered from panic in read_loop: {msg}\n{backtrace}"),
            ),
        );
    }

    // finish() is idempotent; this is the catch-all for exit paths that did
    // not finish explicitly (e.g. a caller-initiated close).
    shared.finish();
    shared.set_done();
}

fn read_loop_inner(
    shared: &Arc<Shared>,
    listener: &Arc<dyn SpeechRecognitionListener>,
    voice_id: &str,
) {
    fire_callback(listener, &SpeechRecognitionResponse {
        code: 0,
        message: "success".to_string(),
        voice_id: voice_id.to_string(),
        ..Default::default()
    }, &|l, r| l.on_recognition_start(r));

    loop {
        // Give ordinary audio writers a chance after a polling read. For a
        // pending Stop, the explicit end-signal handoff below is stronger:
        // this reader waits outside ws until stop() has written the end frame.
        thread::yield_now();
        if !shared.wait_for_end_signal_if_stopping() {
            return;
        }
        let read_result = {
            let mut guard = shared.ws.lock().unwrap();
            match guard.as_mut() {
                None => return,
                Some(ws) => ws.read(),
            }
        };

        let message = match read_result {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Socket read poll timeout: exit if the lifecycle has already
                // ended, otherwise keep polling.
                if shared.state() == STATE_STOPPED {
                    return;
                }
                continue;
            }
            Err(e) => {
                if shared.state() >= STATE_STOPPING {
                    return;
                }
                // Terminal: finish the lifecycle before notifying, so a
                // stop/write from inside on_fail sees the stopped state and
                // returns immediately instead of waiting on done (which only
                // this thread closes).
                shared.finish();
                safe_on_fail(
                    listener,
                    None,
                    &AsrError::new(
                        ERR_CODE_READ_FAILED,
                        format!("read message failed: {e}"),
                    ),
                );
                return;
            }
        };

        let text = match message {
            Message::Text(t) => t,
            Message::Binary(_) => {
                // The server protocol only uses text frames; treat binary as
                // an unmarshal failure (non-terminal), like the Go SDK does.
                safe_on_fail(
                    listener,
                    None,
                    &AsrError::new(
                        ERR_CODE_READ_FAILED,
                        "unmarshal response failed: unexpected binary frame",
                    ),
                );
                continue;
            }
            Message::Close(_) => {
                if shared.state() >= STATE_STOPPING {
                    return;
                }
                shared.finish();
                safe_on_fail(
                    listener,
                    None,
                    &AsrError::new(ERR_CODE_READ_FAILED, "connection closed by server"),
                );
                return;
            }
            // Ping/Pong/Frame: tungstenite answers pings internally.
            _ => continue,
        };

        // Parse the payload once for probing and once typed.
        let probe: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                // Non-terminal: the session continues.
                safe_on_fail(
                    listener,
                    None,
                    &AsrError::new(
                        ERR_CODE_READ_FAILED,
                        format!("unmarshal response failed: {e}"),
                    ),
                );
                continue;
            }
        };

        let resp: SpeechRecognitionResponse = match serde_json::from_value(probe.clone()) {
            Ok(r) => r,
            Err(e) => {
                safe_on_fail(
                    listener,
                    None,
                    &AsrError::new(
                        ERR_CODE_READ_FAILED,
                        format!("unmarshal response failed: {e}"),
                    ),
                );
                continue;
            }
        };

        if resp.code != 0 {
            shared.finish();
            // Wake a stop() that is still inside its timed wait so it can
            // switch to the unbounded wait for the terminal callbacks.
            shared.mark_terminal_received();
            safe_on_fail(
                listener,
                Some(&resp),
                &AsrError::new(resp.code, resp.message.clone()),
            );
            return;
        }

        // Check completion before dispatching the terminal response. A
        // Final=1 response can still carry slice_type=2, which dispatches
        // on_sentence_end; finish first so stop/write from that callback
        // observes the stopped state.
        if resp.final_flag == 1 {
            shared.finish();
            // Same wake-up contract as the error path above.
            shared.mark_terminal_received();
            dispatch_event(listener, &resp);
            safe_complete(listener, &resp);
            return;
        }

        // Skip the connection acknowledgement frame: after connect, the
        // server sends an ack that carries no "result" object
        // (e.g. {"code":0,"message":"success","voice_id":"v1"}). Decoding such
        // a frame yields a zero-valued result whose slice_type=0 would
        // otherwise be misread as "sentence begin". The session start is
        // already signaled via on_recognition_start at reader entry.
        if probe.get("result").is_none() || probe.get("result") == Some(&serde_json::Value::Null) {
            continue;
        }

        dispatch_event(listener, &resp);
    }
}

fn dispatch_event(
    listener: &Arc<dyn SpeechRecognitionListener>,
    resp: &SpeechRecognitionResponse,
) {
    if resp.final_flag == 1 && resp.result.slice_type != 2 {
        return;
    }
    match resp.result.slice_type {
        0 => fire_callback(listener, resp, &|l, r| l.on_sentence_begin(r)),
        1 => fire_callback(listener, resp, &|l, r| l.on_recognition_result_change(r)),
        2 => fire_callback(listener, resp, &|l, r| l.on_sentence_end(r)),
        _ => {}
    }
}

/// Invokes a listener callback. Panics propagate to the reader thread's
/// catch_unwind, matching the Go SDK where a panicking non-terminal callback
/// is recovered by readLoop's deferred recover.
fn fire_callback(
    listener: &Arc<dyn SpeechRecognitionListener>,
    resp: &SpeechRecognitionResponse,
    f: &dyn Fn(&Arc<dyn SpeechRecognitionListener>, &SpeechRecognitionResponse),
) {
    f(listener, resp)
}

/// Delivers an on_fail callback while shielding the reader thread from a
/// panic inside the user-supplied listener.
fn safe_on_fail(
    listener: &Arc<dyn SpeechRecognitionListener>,
    resp: Option<&SpeechRecognitionResponse>,
    err: &AsrError,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        listener.on_fail(resp, err);
    }));
}

/// Delivers the on_recognition_complete callback with the same
/// panic-shielding guarantee as [`safe_on_fail`].
fn safe_complete(
    listener: &Arc<dyn SpeechRecognitionListener>,
    resp: &SpeechRecognitionResponse,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        listener.on_recognition_complete(resp);
    }));
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn clamp_timeout(v: Duration, min: Duration, max: Duration, default: Duration) -> Duration {
    if v.is_zero() {
        default
    } else if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// Opens a WebSocket connection to `url`, returning the WebSocket plus a
/// force-close handle for the underlying socket.
fn connect_ws(
    url: &str,
    handshake_timeout: Duration,
    read_poll: Duration,
    write_timeout: Duration,
) -> Result<(WebSocket<Stream>, TcpStream)> {
    let uri = Url::parse(url).map_err(|e| {
        AsrError::new(ERR_CODE_CONNECT_FAILED, format!("invalid endpoint url: {e}"))
    })?;
    let host = uri.host_str().ok_or_else(|| {
        AsrError::new(ERR_CODE_CONNECT_FAILED, "endpoint url has no host")
    })?;
    let secure = match uri.scheme() {
        "wss" => true,
        "ws" => false,
        scheme => {
            return Err(AsrError::new(
                ERR_CODE_CONNECT_FAILED,
                format!("unsupported scheme {scheme:?}, want ws or wss"),
            ))
        }
    };
    let port = uri.port_or_known_default().ok_or_else(|| {
        AsrError::new(ERR_CODE_CONNECT_FAILED, "endpoint url has no port")
    })?;

    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("dns resolve failed: {e}")))?
        .next()
        .ok_or_else(|| AsrError::new(ERR_CODE_CONNECT_FAILED, "dns resolve returned no address"))?;

    let tcp = TcpStream::connect_timeout(&addr, handshake_timeout).map_err(|e| {
        AsrError::new(ERR_CODE_CONNECT_FAILED, format!("tcp connect failed: {e}"))
    })?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(handshake_timeout))
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("set read timeout: {e}")))?;
    tcp.set_write_timeout(Some(handshake_timeout))
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("set write timeout: {e}")))?;

    // Clone the fd before wrapping it in TLS/WebSocket so `close` can force a
    // shutdown while the reader thread is blocked on read.
    let shutdown = tcp
        .try_clone()
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("clone socket failed: {e}")))?;

    let stream = if secure {
        let config = crate::common::tls::rustls_client_config();
        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("invalid tls server name: {e}")))?;
        let conn = rustls::ClientConnection::new(config, server_name).map_err(|e| {
            AsrError::new(ERR_CODE_CONNECT_FAILED, format!("tls connection init failed: {e}"))
        })?;
        Stream::Tls(rustls::StreamOwned::new(conn, tcp))
    } else {
        Stream::Plain(tcp)
    };

    let (ws, _response) = tungstenite::client::client(url, stream).map_err(|e| {
        AsrError::new(ERR_CODE_CONNECT_FAILED, format!("websocket handshake failed: {e}"))
    })?;

    // After the handshake switch to the steady-state timeouts: a short read
    // poll so close() can interrupt the reader, and the configured write
    // timeout for audio writes / the end signal.
    shutdown
        .set_read_timeout(Some(read_poll))
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("set read timeout: {e}")))?;
    shutdown
        .set_write_timeout(Some(write_timeout))
        .map_err(|e| AsrError::new(ERR_CODE_CONNECT_FAILED, format!("set write timeout: {e}")))?;

    Ok((ws, shutdown))
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn stopping_shared() -> Arc<Shared> {
        Arc::new(Shared {
            state: AtomicU8::new(STATE_STOPPING),
            ws: Mutex::new(None),
            shutdown: Mutex::new(None),
            done: (Mutex::new(false), Condvar::new()),
            terminal_received: AtomicBool::new(false),
            end_signal_sent: AtomicBool::new(false),
            handoff_expired: AtomicBool::new(false),
            handoff_waiting: AtomicBool::new(false),
            reader_tid: Mutex::new(None),
            stop_timeout: Mutex::new(DEFAULT_STOP_TIMEOUT),
            finish_once: Once::new(),
            voice_id: Mutex::new(String::new()),
        })
    }

    fn wait_until_reader_is_parked(shared: &Shared) {
        // Sleep rather than spin: this helper exists to reproduce CPU-starved
        // CI, where a busy-waiting probe would compete with the very reader
        // it is waiting for.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.handoff_waiting.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "reader never reached the handoff wait");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn spawn_handoff_waiter(shared: &Arc<Shared>) -> (thread::JoinHandle<()>, mpsc::Receiver<bool>) {
        let reader = Arc::clone(shared);
        let (result_tx, result_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_tx
                .send(reader.wait_for_end_signal_if_stopping())
                .unwrap();
        });
        (handle, result_rx)
    }

    #[test]
    fn reader_waits_for_stop_to_send_end_signal() {
        let shared = stopping_shared();
        let (handle, result_rx) = spawn_handoff_waiter(&shared);
        wait_until_reader_is_parked(&shared);

        // handoff_waiting is published while done.0 is held, so this update
        // cannot run until Condvar::wait has atomically released that mutex.
        shared.mark_end_signal_sent();
        assert!(
            result_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "reader should resume after the end signal is written"
        );
        handle.join().unwrap();
    }

    #[test]
    fn finish_wakes_reader_waiting_for_end_signal() {
        let shared = stopping_shared();
        let (handle, result_rx) = spawn_handoff_waiter(&shared);
        wait_until_reader_is_parked(&shared);

        // This exercises the send-failure / Drop terminal path that used to
        // publish STATE_STOPPED without holding done.0 and could lose wakeups.
        shared.finish();
        assert!(
            !result_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "reader must exit when the session becomes terminal before end"
        );
        handle.join().unwrap();
    }

    #[test]
    fn reader_exits_when_session_stops_before_end_signal() {
        let shared = stopping_shared();
        shared.finish();
        assert!(!shared.wait_for_end_signal_if_stopping());
    }

    #[test]
    fn reader_resumes_polling_when_the_handoff_expires() {
        // stop() can die between the RUNNING -> STOPPING transition and the
        // end frame (a poisoned ws mutex panics the writer), leaving nobody
        // to publish either flag. The reader must then fall back to the
        // pre-handoff behaviour instead of parking forever, otherwise the
        // session leaks a thread that no longer reads the socket.
        let shared = stopping_shared();
        let reader = Arc::clone(&shared);
        let (result_tx, result_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(50);
            result_tx
                .send(reader.wait_for_end_signal_until(deadline))
                .unwrap();
        });

        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("reader parked past the handoff deadline"),
            "reader should resume polling while the session is still stopping"
        );
        handle.join().unwrap();
    }

    #[test]
    fn expired_handoff_does_not_stall_every_later_read() {
        // Expiry must be sticky. Re-arming the deadline each round would turn
        // an abandoned stop() into "one socket read per handoff timeout",
        // which still leaves the connection unattended for pings, close
        // frames and any final the server does send.
        let shared = stopping_shared();
        assert!(shared.wait_for_end_signal_until(Instant::now() + Duration::from_millis(50)));

        // Still STOPPING with no end frame, yet the next round must not wait.
        let start = Instant::now();
        assert!(shared.wait_for_end_signal_until(Instant::now() + Duration::from_secs(60)));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "expired handoff must not re-arm its deadline (waited {:?})",
            start.elapsed()
        );
    }
}
