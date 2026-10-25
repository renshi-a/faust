//! Devices that may answer Faust's confirmations from afar, such as a phone paired with the
//! app. The app relays the questions and the answers, but the app can be rewritten, so an
//! answer counts only when it is signed by a device whose key the user accepted in Faust's
//! own dialog. A device signs, with its Ed25519 key,
//!
//! ```text
//! faust-confirm|v2|<id>|<kind>|<sha256 of title, "\n" and text, as hex>|<choice>
//! ```
//!
//! The id is new for every confirmation, so an answer can't be replayed, and the question is
//! part of what is signed, so an app can't show the device one question and answer another.
//!
//! The keys live in `faust/devices.json`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Device {
    name: String,
    /// The Ed25519 public key, base64url.
    public_key: String,
    added: i64,
}

fn list(file: &Path) -> Vec<Device> {
    std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(file: &Path, devices: &[Device]) {
    if let Ok(text) = serde_json::to_string_pretty(devices) {
        let _ = std::fs::write(file, text + "\n");
    }
}

/// Trusts answers signed with `key`.
pub(crate) fn add(file: &Path, name: &str, key: &[u8; 32]) {
    let encoded = base64url(key);
    let mut devices = list(file);
    devices.retain(|d| d.public_key != encoded);
    devices.push(Device { name: name.to_owned(), public_key: encoded, added: super::state::now() });
    save(file, &devices);
}

pub(crate) fn remove(file: &Path, key: &[u8; 32]) {
    let encoded = base64url(key);
    let mut devices = list(file);
    let before = devices.len();
    devices.retain(|d| d.public_key != encoded);
    if devices.len() != before {
        save(file, &devices);
    }
}

/// The questions sent to the app for devices, by confirmation id: (kind, digest).
static ASKED: Mutex<BTreeMap<String, (String, String)>> = Mutex::new(BTreeMap::new());

/// Remembers a confirmation sent to the app, so an answer to it can be checked.
pub(crate) fn asked(id: &str, kind: &str, title: &str, text: &str) {
    let digest = crate::proc::hex(&Sha256::digest(format!("{title}\n{text}").as_bytes()));
    ASKED.lock().expect("asked poisoned").insert(id.to_owned(), (kind.to_owned(), digest));
}

/// The confirmation was answered: no answer to it counts any more.
pub(crate) fn closed(id: &str) {
    ASKED.lock().expect("asked poisoned").remove(id);
}

/// What a device signs to answer confirmation `id` with `choice`.
fn payload(id: &str, kind: &str, digest: &str, choice: &str) -> String {
    format!("faust-confirm|v2|{id}|{kind}|{digest}|{choice}")
}

/// Is this a signed answer, from a trusted device, to an open confirmation?
pub(crate) fn verify(file: &Path, id: &str, choice: &str, public_key: &str, signature: &str) -> bool {
    let (Some(key), Some(sig)) = (decode_key(public_key), base64url_decode(signature)) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(sig) else { return false };
    let Some((kind, digest)) = ASKED.lock().expect("asked poisoned").get(id).cloned() else { return false };
    let encoded = base64url(&key);
    if !list(file).iter().any(|d| d.public_key == encoded) {
        return false;
    }
    let Ok(vk) = VerifyingKey::from_bytes(&key) else { return false };
    vk.verify_strict(payload(id, &kind, &digest, choice).as_bytes(), &Signature::from_bytes(&sig)).is_ok()
}

/// A short fingerprint to compare with the device's screen: `AB12 CD34 EF56`.
pub(crate) fn fingerprint(key: &[u8; 32]) -> String {
    let h: String = Sha256::digest(key).iter().take(6).map(|b| format!("{b:02X}")).collect();
    format!("{} {} {}", &h[0..4], &h[4..8], &h[8..12])
}

pub(crate) fn decode_key(s: &str) -> Option<[u8; 32]> {
    base64url_decode(s)?.try_into().ok()
}

fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let v = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=n {
            out.push(char::from(ALPHABET[(v >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

/// Base64 in either alphabet, padding optional.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn only_trusted_signed_answers_to_open_questions_count() {
        let dir = std::env::temp_dir().join(format!("faust-devices-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("devices.json");
        let phone = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let key = phone.verifying_key().to_bytes();
        let pk = base64url(&key);
        let digest = |title: &str, text: &str| crate::proc::hex(&Sha256::digest(format!("{title}\n{text}").as_bytes()));
        let sign = |k: &SigningKey, id: &str, text: &str, choice: &str| {
            base64url(&k.sign(payload(id, "switch", &digest("Switch?", text), choice).as_bytes()).to_bytes())
        };
        asked("c1", "switch", "Switch?", "New version");

        assert!(!verify(&file, "c1", "switch", &pk, &sign(&phone, "c1", "New version", "switch")), "not trusted yet");
        add(&file, "Pixel", &key);
        assert!(verify(&file, "c1", "switch", &pk, &sign(&phone, "c1", "New version", "switch")));
        assert!(
            !verify(&file, "c1", "switch", &pk, &sign(&phone, "c1", "Something else", "switch")),
            "another question"
        );
        assert!(!verify(&file, "c1", "cancel", &pk, &sign(&phone, "c1", "New version", "switch")), "another choice");
        assert!(!verify(&file, "c1", "switch", &pk, &sign(&other, "c1", "New version", "switch")), "another key");
        assert!(!verify(&file, "c2", "switch", &pk, &sign(&phone, "c2", "New version", "switch")), "never asked");
        assert!(!verify(&file, "c1", "switch", &pk, "not base64!"));
        assert_eq!(decode_key(&pk), Some(key));
        assert_eq!(fingerprint(&key).len(), 14);
        closed("c1");
        assert!(!verify(&file, "c1", "switch", &pk, &sign(&phone, "c1", "New version", "switch")), "already answered");
        remove(&file, &key);
        let _ = std::fs::remove_dir_all(dir);
    }
}
