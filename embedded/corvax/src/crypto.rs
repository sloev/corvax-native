//! Identity, signatures and sealed messages (SPEC §2), byte-compatible with the WebCrypto-based JS core.
//!   one 32-byte seed → Ed25519 key = HKDF(seed, "cx/ed25519"), X25519 key = HKDF(seed, "cx/x25519")
//!   message wire = eph_x 32 | iv 12 | AES-256-GCM_K(inner | Ed25519 sig(inner)),  K = HKDF(X25519(eph, to_x), "cx/msg" | eph_x | to_x)
use crate::bencode::{decode, encode, Value};
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use x25519_dalek::{PublicKey, StaticSecret};

pub const MAX_MESSAGE: usize = 16384;
pub const MAX_AGE: u64 = 600_000;
pub const KEEP_MAX: u64 = 7 * 86_400_000;

pub fn random<const N: usize>() -> [u8; N] { let mut b = [0u8; N]; getrandom::getrandom(&mut b).expect("random"); b }
pub fn hkdf(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, ikm).expand(info, &mut out).expect("32 bytes");
    out
}
pub fn sha256(b: &[u8]) -> [u8; 32] { Sha256::digest(b).into() }
pub fn sha1(b: &[u8]) -> [u8; 20] { sha1::Sha1::digest(b).into() }

pub struct Identity { pub seed: [u8; 32], signing: SigningKey, xsecret: StaticSecret, pub ed_pub: [u8; 32], pub x_pub: [u8; 32] }
impl Identity {
    pub fn new(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&hkdf(&seed, b"cx/ed25519"));
        let xsecret = StaticSecret::from(hkdf(&seed, b"cx/x25519"));
        let (ed_pub, x_pub) = (signing.verifying_key().to_bytes(), PublicKey::from(&xsecret).to_bytes());
        Identity { seed, signing, xsecret, ed_pub, x_pub }
    }
    pub fn generate() -> Self { Self::new(random()) }
    pub fn address(&self) -> String { crate::text::z32(&self.ed_pub) }
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] { self.signing.sign(msg).to_bytes() }
}
pub fn verify(public: &[u8], sig: &[u8], msg: &[u8]) -> bool {
    let (Ok(k), Ok(s)) = (<[u8; 32]>::try_from(public), <[u8; 64]>::try_from(sig)) else { return false };
    VerifyingKey::from_bytes(&k).map(|k| k.verify(msg, &Signature::from_bytes(&s)).is_ok()).unwrap_or(false)
}

fn ecdh(secret: &StaticSecret, public: &[u8; 32]) -> Option<[u8; 32]> {
    let s = secret.diffie_hellman(&PublicKey::from(*public)).to_bytes();
    (s != [0; 32]).then_some(s)
}
fn cipher(shared: &[u8; 32], eph_x: &[u8], to_x: &[u8]) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(&hkdf(shared, &[b"cx/msg".as_slice(), eph_x, to_x].concat())).expect("key")
}

/// Seal `body` (JSON) to a recipient. compress: deflate the body when smaller (only for Beacons with v ≥ 2).
/// exp: absolute ms for a kept message (store-carry-forward, SPEC §14).
pub fn seal(me: &Identity, to: &[u8; 32], to_x: &[u8; 32], body: &serde_json::Value, compress: bool, now: u64, exp: Option<u64>) -> Option<Vec<u8>> {
    let eph = StaticSecret::from(random::<32>());
    let eph_x = PublicKey::from(&eph).to_bytes();
    let json = serde_json::to_vec(body).ok()?;
    let mut inner: BTreeMap<Vec<u8>, Value> = [("from", Value::from(&me.ed_pub[..])), ("to", Value::from(&to[..])), ("id", Value::from(&random::<16>()[..])), ("ts", Value::Int(now as i64))]
        .into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect();
    let z = if compress && json.len() > 200 { Some(miniz_oxide::deflate::compress_to_vec(&json, 6)) } else { None };
    match z { Some(z) if z.len() < json.len() => inner.insert(b"z".to_vec(), z.into()), _ => inner.insert(b"body".to_vec(), json.into()) };
    if let Some(e) = exp { inner.insert(b"exp".to_vec(), Value::Int(e as i64)); }
    let inner = encode(&Value::Dict(inner));
    let iv = random::<12>();
    let sig = me.sign(&inner);
    let ct = cipher(&ecdh(&eph, to_x)?, &eph_x, to_x).encrypt(Nonce::from_slice(&iv), [inner.as_slice(), &sig].concat().as_slice()).ok()?;
    let out = [eph_x.as_slice(), &iv, &ct].concat();
    (out.len() <= MAX_MESSAGE).then_some(out)
}

#[derive(Debug, Clone)]
pub struct Message { pub from: [u8; 32], pub id: String, pub ts: u64, pub exp: u64, pub body: serde_json::Value }

/// Open a message for `me`, valid at `now` (ms). Replays (same id) are the caller's to drop.
pub fn open(me: &Identity, wire: &[u8], now: u64) -> Option<Message> {
    if wire.len() > MAX_MESSAGE || wire.len() < 32 + 12 + 16 + 64 { return None }
    let eph_x: [u8; 32] = wire[..32].try_into().ok()?;
    let pt = cipher(&ecdh(&me.xsecret, &eph_x)?, &eph_x, &me.x_pub).decrypt(Nonce::from_slice(&wire[32..44]), &wire[44..]).ok()?;
    let (inner, sig) = pt.split_at(pt.len().checked_sub(64)?);
    let m = decode(inner)?;
    let from: [u8; 32] = m.get("from")?.bytes()?.try_into().ok()?;
    if m.get("to")?.bytes()? != me.ed_pub || !verify(&from, sig, inner) { return None }
    let ts = m.get("ts")?.int()? as u64;
    let exp = m.get("exp").and_then(Value::int).map(|e| e as u64);
    let fresh = match exp { Some(e) => now <= e && e.saturating_sub(ts) <= KEEP_MAX, None => now.saturating_sub(ts) <= MAX_AGE };
    if !fresh || ts > now + MAX_AGE { return None }
    let json = match m.get("z") {
        Some(z) => miniz_oxide::inflate::decompress_to_vec_with_limit(z.bytes()?, 4 * MAX_MESSAGE).ok()?,   // ≤ 64 KB
        None => m.get("body")?.bytes()?.to_vec(),
    };
    Some(Message { from, id: crate::text::hex(m.get("id")?.bytes()?), ts, exp: exp.unwrap_or(0), body: serde_json::from_slice(&json).ok()? })
}
