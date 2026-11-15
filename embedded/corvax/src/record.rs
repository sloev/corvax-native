//! Beacons (SPEC §2): pkarr-compatible BEP 44 records holding a DNS packet of TXT records named "_cx", "key=value".
//! Anchor lines (SPEC §5): corvax-anchor:<z32 key>@<host>:<port>[;web=<origin>][;q=<0-100>].
use crate::bencode::{encode, Value};
use crate::crypto::{verify, Identity};
use crate::text::{b64, unz32, z32};

#[derive(Clone, Debug)]
pub struct Beacon { pub key: [u8; 32], pub seq: u64, pub sig: [u8; 64], pub v: Vec<u8> }

pub fn signable(seq: u64, v: &[u8]) -> Vec<u8> {
    [b"3:seq".as_slice(), &encode(&Value::Int(seq as i64)), b"1:v", &encode(&Value::from(v))].concat()
}
impl Beacon {
    /// fields: (key, value) TXT pairs; x (our X25519 key) is added last, as the JS core does.
    pub fn make(id: &Identity, fields: &[(&str, String)], seq: u64) -> Option<Beacon> {
        let mut txt: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
        txt.push(format!("x={}", b64(&id.x_pub)));
        let v = dns_encode(&txt)?;
        if v.len() > 1000 { return None }
        Some(Beacon { key: id.ed_pub, seq, sig: id.sign(&signable(seq, &v)), v })
    }
    pub fn valid(&self) -> bool { self.v.len() <= 1000 && verify(&self.key, &self.sig, &signable(self.seq, &self.v)) }
    pub fn address(&self) -> String { z32(&self.key) }
    pub fn pack(&self) -> Vec<u8> { [self.sig.as_slice(), &self.seq.to_be_bytes(), &self.v].concat() }
    pub fn unpack(key: [u8; 32], b: &[u8]) -> Option<Beacon> {
        (b.len() >= 72).then(|| Beacon { key, sig: b[..64].try_into().unwrap(), seq: u64::from_be_bytes(b[64..72].try_into().unwrap()), v: b[72..].to_vec() })
    }
    /// All values of one field, e.g. "c" (inbox anchor lines), "x", "n", "v".
    pub fn field(&self, k: &str) -> Vec<String> {
        dns_decode(&self.v).unwrap_or_default().into_iter().filter_map(|t| t.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.to_string())).collect()
    }
    pub fn x_pub(&self) -> Option<[u8; 32]> { crate::text::unb64(self.field("x").first()?)?.try_into().ok() }
    pub fn version(&self) -> u64 { self.field("v").first().and_then(|v| v.parse().ok()).unwrap_or(1) }
}

const NAME: &[u8] = &[3, b'_', b'c', b'x', 0];
fn dns_encode(txts: &[String]) -> Option<Vec<u8>> {
    let mut out = vec![0, 0, 0x84, 0, 0, 0, (txts.len() >> 8) as u8, txts.len() as u8, 0, 0, 0, 0];
    for t in txts {
        let s = t.as_bytes();
        if s.len() > 255 { return None }
        out.extend(NAME);
        out.extend([0, 16, 0, 1, 0, 0, 1, 44, ((s.len() + 1) >> 8) as u8, (s.len() + 1) as u8, s.len() as u8]);
        out.extend(s);
    }
    Some(out)
}
fn dns_decode(b: &[u8]) -> Option<Vec<String>> {
    let u16at = |o: usize| Some((*b.get(o)? as usize) << 8 | *b.get(o + 1)? as usize);
    let skip = |mut o: usize| -> Option<usize> { loop { let n = *b.get(o)?; if n == 0 { return Some(o + 1) } if n >= 0xc0 { return Some(o + 2) } o += n as usize + 1 } };
    let (mut o, mut out) = (12, Vec::new());
    for _ in 0..u16at(4)? { o = skip(o)? + 4 }
    for _ in 0..u16at(6)? {
        o = skip(o)?;
        let (ty, len) = (u16at(o)?, u16at(o + 8)?);
        o += 10;
        if ty == 16 { let mut p = o; while p < o + len { let n = *b.get(p)? as usize; out.push(String::from_utf8_lossy(b.get(p + 1..p + 1 + n)?).into_owned()); p += n + 1 } }
        o += len;
    }
    Some(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Anchor { pub key: String, pub host: String, pub port: u16, pub web: Option<String> }
impl Anchor {
    pub fn parse(line: &str) -> Option<Anchor> {
        let rest = line.trim().strip_prefix("corvax-anchor:")?;
        let (key, rest) = rest.split_once('@')?;
        if key.len() != 52 || unz32(key)?.len() != 32 { return None }
        let mut parts = rest.split(';');
        let hp = parts.next()?;
        let (host, port) = if let Some(h) = hp.strip_prefix('[') { let (h, p) = h.split_once("]:")?; (h, p) } else { hp.rsplit_once(':')? };
        let mut a = Anchor { key: key.to_lowercase(), host: host.to_lowercase(), port: port.parse().ok().filter(|p| *p > 0)?, web: None };
        for kv in parts { if let Some(w) = kv.strip_prefix("web=").filter(|w| w.starts_with("http://") || w.starts_with("https://")) { a.web = Some(w.to_string()) } }
        Some(a)
    }
    pub fn line(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("corvax-anchor:{}@{}:{}{}", self.key, host, self.port, self.web.as_ref().map(|w| format!(";web={w}")).unwrap_or_default())
    }
    /// Where to reach it over HTTP.
    pub fn url(&self) -> String {
        self.web.clone().unwrap_or_else(|| if self.host.contains(':') { format!("http://[{}]:{}", self.host, self.port) } else { format!("http://{}:{}", self.host, self.port) })
    }
}
