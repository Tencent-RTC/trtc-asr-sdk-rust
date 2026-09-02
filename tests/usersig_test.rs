//! UserSig tests: structure, HMAC correctness, base64url round-trip.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use flate2::read::ZlibDecoder;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::Read;

use trtc_asr_sdk::common::usersig::{
    base64_url_decode, base64_url_encode, gen_user_sig, gen_user_sig_at, DEFAULT_EXPIRE,
};

/// Decodes a UserSig: base64url → zlib inflate → JSON document.
fn decode_sig(sig: &str) -> serde_json::Value {
    let compressed = base64_url_decode(sig).expect("base64url decode");
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    let mut json = String::new();
    decoder
        .read_to_string(&mut json)
        .expect("zlib inflate (server decodes the same way)");
    serde_json::from_str(&json).expect("usersig document is valid JSON")
}

fn expected_hmac(key: &str, user_id: &str, sdk_app_id: u64, now: i64, expire: u64) -> String {
    let content =
        format!("TLS.identifier:{user_id}\nTLS.sdkappid:{sdk_app_id}\nTLS.time:{now}\nTLS.expire:{expire}\n");
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(content.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

#[test]
fn gen_user_sig_structure_and_signature() {
    let sdk_app_id = 1400000000u64;
    let key = "test-secret-key-for-unit-testing";
    let user_id = "test-user-001";
    let expire = 86400u64;
    let now = 1756800000i64; // fixed timestamp for determinism

    let sig = gen_user_sig_at(sdk_app_id, key, user_id, expire, now).expect("gen");
    let doc = decode_sig(&sig);

    assert_eq!(doc["TLS.ver"], "2.0");
    assert_eq!(doc["TLS.identifier"], user_id);
    assert_eq!(doc["TLS.sdkappid"], sdk_app_id);
    assert_eq!(doc["TLS.expire"], expire);
    assert_eq!(doc["TLS.time"], now);
    // No userbuf for plain UserSig.
    assert!(doc.get("TLS.userbuf").is_none());

    // TLS.sig is the standard base64 of the HMAC-SHA256 over the documented
    // content string — this is exactly what the server recomputes.
    let want = expected_hmac(key, user_id, sdk_app_id, now, expire);
    assert_eq!(doc["TLS.sig"], want);
}

#[test]
fn gen_user_sig_is_deterministic_for_fixed_time() {
    let a = gen_user_sig_at(1400000000, "key", "user", 86400, 1756800000).unwrap();
    let b = gen_user_sig_at(1400000000, "key", "user", 86400, 1756800000).unwrap();
    assert_eq!(a, b);
}

#[test]
fn gen_user_sig_default_expire() {
    let sig = gen_user_sig(1400000000, "key", "user", 0).expect("gen with default expire");
    let doc = decode_sig(&sig);
    assert_eq!(doc["TLS.expire"], DEFAULT_EXPIRE);
}

#[test]
fn gen_user_sig_various_inputs() {
    for (app_id, key, user) in [
        (1400000001u64, "key1", "user1"),
        (1400000002, "key2", "user2"),
        (1400000003, "key-with-special-chars!@#$%", "user-with-dashes"),
    ] {
        let sig = gen_user_sig(app_id, key, user, 86400).expect("gen");
        let doc = decode_sig(&sig);
        assert_eq!(doc["TLS.sdkappid"], app_id);
        assert_eq!(doc["TLS.identifier"], user);
    }
}

#[test]
fn gen_user_sig_rejects_empty_key_or_user() {
    assert!(gen_user_sig(1, "", "user", 86400).is_err());
    assert!(gen_user_sig(1, "key", "", 86400).is_err());
}

#[test]
fn base64url_round_trip_and_alphabet() {
    // Craft bytes that produce +, / and = in standard base64.
    let data: Vec<u8> = vec![0xfb, 0xff, 0xff, 0x3e, 0x80];
    let std = B64.encode(&data);
    assert!(std.contains('+') || std.contains('/') || std.contains('='));

    let encoded = base64_url_encode(&data);
    assert!(!encoded.contains('+'));
    assert!(!encoded.contains('/'));
    assert!(!encoded.contains('='));
    // Custom alphabet uses * - and _ padding.
    assert!(encoded.contains('*') || encoded.contains('-') || encoded.contains('_'));

    let decoded = base64_url_decode(&encoded).expect("decode");
    assert_eq!(decoded, data);
}
