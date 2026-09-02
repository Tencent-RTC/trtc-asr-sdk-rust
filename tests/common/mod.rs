//! Shared test helpers: a minimal HTTP/1.1 mock server and a WebSocket mock
//! server, so recognizer tests run hermetically on localhost.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// An HTTP request captured by [`MockHttpServer`].
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub method: String,
    /// Path plus query string, e.g. `/v1/CreateRecTask?AppId=1&...`.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl CapturedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Returns the decoded value of a query parameter.
    pub fn query(&self, key: &str) -> Option<String> {
        let query = self.target.splitn(2, '?').nth(1)?;
        for pair in query.split('&') {
            let mut kv = pair.splitn(2, '=');
            let k = percent_decode(kv.next().unwrap_or(""));
            if k == key {
                return Some(percent_decode(kv.next().unwrap_or("")));
            }
        }
        None
    }
}

pub struct MockHttpResponse {
    pub status: u16,
    pub body: String,
}

impl MockHttpResponse {
    pub fn json(body: impl Into<String>) -> Self {
        MockHttpResponse {
            status: 200,
            body: body.into(),
        }
    }
}

/// A minimal HTTP/1.1 server: one request per connection, `Connection:
/// close` semantics.
pub struct MockHttpServer {
    pub url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl MockHttpServer {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&CapturedRequest) -> MockHttpResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock http server");
        let addr = listener.local_addr().unwrap();
        let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let reqs = Arc::clone(&requests);
        let handler = Arc::new(handler);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let handler = Arc::clone(&handler);
                let reqs = Arc::clone(&reqs);
                thread::spawn(move || {
                    if let Some(req) = read_request(&mut stream) {
                        reqs.lock().unwrap().push(req.clone());
                        let resp = handler(&req);
                        write_response(&mut stream, resp.status, &resp.body);
                    }
                });
            }
        });
        MockHttpServer {
            url: format!("http://{addr}"),
            requests,
        }
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<CapturedRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.trim().split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(idx) = line.find(':') {
            let name = line[..idx].trim().to_string();
            let value = line[idx + 1..].trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    Some(CapturedRequest {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Percent-decodes a query value (`+` → space, `%XX` → byte).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A mock WebSocket server. Accepts one connection and runs `handler` on a
/// background thread. The handshake request target (path + query) is captured
/// for assertions.
pub struct MockWsServer {
    pub url: String,
    pub request_target: Arc<Mutex<Option<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockWsServer {
    pub fn start<F>(handler: F) -> Self
    where
        F: FnOnce(tungstenite::WebSocket<TcpStream>) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ws server");
        let addr = listener.local_addr().unwrap();
        let target: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let t2 = Arc::clone(&target);
        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let captured = Arc::clone(&t2);
                let cb = move |req: &tungstenite::handshake::server::Request,
                               resp: tungstenite::handshake::server::Response| {
                    *captured.lock().unwrap() = Some(req.uri().to_string());
                    Ok(resp)
                };
                if let Ok(ws) = tungstenite::accept_hdr(stream, cb) {
                    handler(ws);
                }
            }
        });
        MockWsServer {
            url: format!("ws://{addr}"),
            request_target: target,
            handle: Some(handle),
        }
    }

    /// Waits (with a bound) for the server handler thread to finish.
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A listener that records callback invocations for assertions.
#[derive(Default)]
pub struct RecordingListener {
    pub start_count: Mutex<usize>,
    pub sentence_begin_count: Mutex<usize>,
    pub change_count: Mutex<usize>,
    pub sentence_end_count: Mutex<usize>,
    pub complete_count: Mutex<usize>,
    pub fail_count: Mutex<usize>,
    pub events: Mutex<Vec<String>>,
}

impl RecordingListener {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(m: &Mutex<usize>) -> usize {
        *m.lock().unwrap()
    }
}

impl trtc_asr_sdk::asr::SpeechRecognitionListener for RecordingListener {
    fn on_recognition_start(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        *self.start_count.lock().unwrap() += 1;
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", r.voice_id));
    }
    fn on_sentence_begin(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        *self.sentence_begin_count.lock().unwrap() += 1;
        self.events
            .lock()
            .unwrap()
            .push(format!("begin:{}", r.result.index));
    }
    fn on_recognition_result_change(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        *self.change_count.lock().unwrap() += 1;
        self.events
            .lock()
            .unwrap()
            .push(format!("change:{}", r.result.voice_text_str));
    }
    fn on_sentence_end(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        *self.sentence_end_count.lock().unwrap() += 1;
        self.events
            .lock()
            .unwrap()
            .push(format!("end:{}", r.result.voice_text_str));
    }
    fn on_recognition_complete(&self, r: &trtc_asr_sdk::asr::SpeechRecognitionResponse) {
        *self.complete_count.lock().unwrap() += 1;
        self.events
            .lock()
            .unwrap()
            .push(format!("complete:{}", r.final_flag));
    }
    fn on_fail(
        &self,
        _r: Option<&trtc_asr_sdk::asr::SpeechRecognitionResponse>,
        e: &trtc_asr_sdk::common::AsrError,
    ) {
        *self.fail_count.lock().unwrap() += 1;
        self.events.lock().unwrap().push(format!("fail:{}", e.code));
    }
}

/// Waits until `cond` holds or the timeout elapses. Returns the outcome.
pub fn wait_until<F: Fn() -> bool>(timeout: std::time::Duration, cond: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    cond()
}
