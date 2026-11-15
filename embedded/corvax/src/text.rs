//! z-base-32 (addresses), base64url (wire text) and hex.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

const Z: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

pub fn z32(b: &[u8]) -> String {
    let (mut out, mut acc, mut bits) = (String::new(), 0u32, 0);
    for &x in b {
        acc = acc << 8 | x as u32; bits += 8;
        while bits >= 5 { bits -= 5; out.push(Z[(acc >> bits & 31) as usize] as char) }
    }
    if bits > 0 { out.push(Z[(acc << (5 - bits) & 31) as usize] as char) }
    out
}
pub fn unz32(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in s.bytes() {
        acc = acc << 5 | Z.iter().position(|&z| z == c)? as u32; bits += 5;
        if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8) }
    }
    Some(out)
}
pub fn key32(address: &str) -> Option<[u8; 32]> { unz32(address)?.try_into().ok() }
pub fn b64(b: &[u8]) -> String { URL_SAFE_NO_PAD.encode(b) }
pub fn unb64(s: &str) -> Option<Vec<u8>> { URL_SAFE_NO_PAD.decode(s.trim_end_matches('=').replace('+', "-").replace('/', "_")).ok() }
pub fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
pub fn unhex(s: &str) -> Option<Vec<u8>> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect() }
