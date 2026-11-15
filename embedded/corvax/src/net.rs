//! KRPC over HTTP (SPEC §3): POST <anchor>/krpc with a bencoded query, text/plain, one reply.
//! The HTTP client is pluggable: `TcpNet` (plain HTTP over std TCP) here; ESP-IDF's HTTPS client on the ESP32.
use crate::bencode::{decode, dict, encode, Value};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub trait Net: Send + Sync {
    fn post(&self, url: &str, body: &[u8], timeout: Duration) -> Result<Vec<u8>>;
}

/// One query; returns the reply's `r` dictionary, or the remote error as an Err.
pub fn rpc(net: &dyn Net, url: &str, q: &str, a: Value, timeout: Duration) -> Result<Value> {
    let body = encode(&dict([("t", Value::from(&crate::crypto::random::<4>()[..])), ("y", "q".into()), ("q", q.into()), ("a", a), ("ro", Value::Int(1))]));
    let m = decode(&net.post(&format!("{}/krpc", url.trim_end_matches('/')), &body, timeout)?).ok_or_else(|| Error::new(ErrorKind::InvalidData, "bad reply"))?;
    match m.get("y").and_then(Value::str) {
        Some("r") => m.get("r").cloned().ok_or_else(|| Error::new(ErrorKind::InvalidData, "no r")),
        _ => Err(Error::other(format!("{q}: {:?}", m.get("e").and_then(Value::list).and_then(|e| e.get(1)).and_then(Value::str)))),
    }
}

/// Plain HTTP/1.1 over TCP (local anchors, anchors without TLS). Connection: close; Content-Length or chunked.
pub struct TcpNet;
impl Net for TcpNet {
    fn post(&self, url: &str, body: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let rest = url.strip_prefix("http://").ok_or_else(|| Error::new(ErrorKind::Unsupported, "TcpNet speaks http:// only"))?;
        let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
        let addr = hostport.to_socket_addrs()?.next().ok_or_else(|| Error::new(ErrorKind::NotFound, "no address"))?;
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        s.set_read_timeout(Some(timeout))?;
        s.write_all(format!("POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes())?;
        s.write_all(body)?;
        let mut raw = Vec::new();
        s.take(1 << 20).read_to_end(&mut raw)?;
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| Error::new(ErrorKind::InvalidData, "no header"))?;
        let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
        if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") { return Err(Error::other(head.lines().next().unwrap_or("").to_string())) }
        let mut b = raw[split + 4..].to_vec();
        if head.contains("transfer-encoding: chunked") {
            let (mut out, mut i) = (Vec::new(), 0);
            while let Some(e) = b[i..].windows(2).position(|w| w == b"\r\n") {
                let n = usize::from_str_radix(String::from_utf8_lossy(&b[i..i + e]).trim(), 16).unwrap_or(0);
                if n == 0 { break }
                out.extend(b.get(i + e + 2..i + e + 2 + n).unwrap_or_default());
                i += e + 4 + n;
            }
            b = out;
        }
        Ok(b)
    }
}
