//! TRTC UserSig generation (TLS sig API v2 compatible).
//!
//! Layout of the ticket (identical to the official Go/Java implementations):
//!
//! 1. Build a JSON document
//!    `{"TLS.ver":"2.0","TLS.identifier":..,"TLS.sdkappid":..,"TLS.expire":..,"TLS.time":..,"TLS.sig":..}`
//!    where `TLS.sig` is the standard base64 of the HMAC-SHA256 of the string
//!    `"TLS.identifier:<id>\nTLS.sdkappid:<appid>\nTLS.time:<now>\nTLS.expire:<expire>\n"`
//!    keyed by the SDK secret key.
//! 2. zlib-compress the JSON document.
//! 3. Encode with the Tencent variant of base64url:
//!    alphabet `A-Za-z0-9*-`, padding `_` (i.e. `+`→`*`, `/`→`-`, `=`→`_`).

use base64::{engine::general_purpose::STANDARD as B64_STD, Engine as _};
use flate2::{write::ZlibEncoder, Compression};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use super::errors::{invalid_param, Result};

/// Default UserSig validity: 180 days in seconds (matches the Go SDK).
pub const DEFAULT_EXPIRE: u64 = 86400 * 180;

/// Generates a TRTC UserSig.
///
/// - `sdk_app_id`: TRTC application ID
/// - `key`: TRTC SDK secret key
/// - `user_id`: unique user identifier (maps to `voice_id` in ASR)
/// - `expire`: signature validity in seconds; `0` uses [`DEFAULT_EXPIRE`]
pub fn gen_user_sig(sdk_app_id: u64, key: &str, user_id: &str, expire: u64) -> Result<String> {
    let expire = if expire == 0 { DEFAULT_EXPIRE } else { expire };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid_param(format!("system clock before epoch: {e}")))?
        .as_secs() as i64;
    gen_user_sig_at(sdk_app_id, key, user_id, expire, now)
}

/// Deterministic core of [`gen_user_sig`] with an explicit timestamp, exposed
/// for tests and for callers that need reproducible signatures.
pub fn gen_user_sig_at(
    sdk_app_id: u64,
    key: &str,
    user_id: &str,
    expire: u64,
    now: i64,
) -> Result<String> {
    if key.is_empty() {
        return Err(invalid_param("secret key is empty"));
    }
    if user_id.is_empty() {
        return Err(invalid_param("user id is empty"));
    }

    let content = format!(
        "TLS.identifier:{user_id}\nTLS.sdkappid:{sdk_app_id}\nTLS.time:{now}\nTLS.expire:{expire}\n"
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes())
        .map_err(|e| invalid_param(format!("hmac key error: {e}")))?;
    mac.update(content.as_bytes());
    let sig = mac.finalize().into_bytes();

    let doc = serde_json::json!({
        "TLS.ver": "2.0",
        "TLS.identifier": user_id,
        "TLS.sdkappid": sdk_app_id,
        "TLS.expire": expire,
        "TLS.time": now,
        "TLS.sig": B64_STD.encode(sig),
    });
    let mut payload = serde_json::to_vec(&doc)
        .map_err(|e| invalid_param(format!("marshal usersig doc failed: {e}")))?;
    // Go's json.Encoder.Encode appends a newline; harmless for the server but
    // kept for byte-level parity.
    payload.push(b'\n');

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&payload)
        .map_err(|e| invalid_param(format!("zlib compress failed: {e}")))?;
    let compressed = enc
        .finish()
        .map_err(|e| invalid_param(format!("zlib finish failed: {e}")))?;

    Ok(base64_url_encode(&compressed))
}

/// Encodes bytes with the Tencent base64url variant (`*`/`-` alphabet, `_`
/// padding) used by UserSig.
pub fn base64_url_encode(data: &[u8]) -> String {
    B64_STD
        .encode(data)
        .replace('+', "*")
        .replace('/', "-")
        .replace('=', "_")
}

/// Decodes the Tencent base64url variant. Provided for tooling/tests.
pub fn base64_url_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.replace('_', "=").replace('-', "/").replace('*', "+");
    B64_STD
        .decode(s)
        .map_err(|e| invalid_param(format!("base64url decode failed: {e}")))
}
