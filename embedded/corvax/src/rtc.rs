//! WebRTC DataChannels (SPEC §7) with str0m (sans-IO, pure-Rust DTLS/SRTP crypto: no OpenSSL, runs on ESP-IDF).
//! Same signalling as the JS core: {cx:"offer"|"answer"|"cand"|"busy", sid, ...} sealed messages; two negotiated
//! channels, id 0 "app" (yours) and id 1 "cx" (control: mesh queries as "K" ‖ base64url(bencode)).
//! Our candidates ride inside the SDP; theirs may trickle (c, or batched cs).
use crate::bencode::{dict, encode, Value};
use crate::peer::{Event, Peer};
use crate::text::{b64, unb64};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, IceConnectionState, Input, Output, Rtc};

pub(crate) enum Cmd { Cand(String), Answer(String), Write(u8, bool, Vec<u8>), Close }

/// An open connection to a peer: write to its "app" channel.
#[derive(Clone, Debug)]
pub struct Conn { pub address: String, tx: mpsc::Sender<Cmd> }
impl Conn {
    pub fn send_text(&self, s: &str) { let _ = self.tx.send(Cmd::Write(0, false, s.as_bytes().to_vec())); }
    pub fn send_bytes(&self, b: &[u8]) { let _ = self.tx.send(Cmd::Write(0, true, b.to_vec())); }
    pub fn close(&self) { let _ = self.tx.send(Cmd::Close); }
}

#[derive(Default)]
pub struct Rtcs {
    sessions: Mutex<HashMap<String, (String, mpsc::Sender<Cmd>)>>,   // sid -> (address, commands)
    conns: Mutex<HashMap<String, mpsc::Sender<Cmd>>>,                // address -> open connection
    pub ips: Mutex<Vec<IpAddr>>,                                     // our addresses (host candidates); empty: found by routing
    pub accept: std::sync::atomic::AtomicBool,                       // answer offers for "app" connections
}
impl Rtcs {
    /// A fire-and-forget KRPC query on the control channel of an open connection to `address`.
    pub(crate) fn query(&self, address: &str, q: &str, a: Value) -> bool {
        let Some(tx) = self.conns.lock().unwrap().get(address).cloned() else { return false };
        let body = encode(&dict([("t", Value::from(&crate::crypto::random::<4>()[..])), ("y", "q".into()), ("q", q.into()), ("a", a)]));
        tx.send(Cmd::Write(1, false, format!("K{}", b64(&body)).into_bytes())).is_ok()
    }
    pub(crate) fn close_all(&self) { for (_, tx) in self.sessions.lock().unwrap().values() { let _ = tx.send(Cmd::Close); } }
}

/// Our IP addresses for host candidates: the configured ones, else the route to the internet (no packet is sent), plus loopback.
fn ips(p: &Peer) -> Vec<IpAddr> {
    let set = p.0.rtc.ips.lock().unwrap().clone();
    if !set.is_empty() { return set }
    let mut out = vec![];
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") { if s.connect("8.8.8.8:80").is_ok() { if let Ok(a) = s.local_addr() { if !a.ip().is_unspecified() { out.push(a.ip()) } } } }
    out.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    out
}
fn channel(label: &str, id: u16) -> ChannelConfig { ChannelConfig { label: label.into(), negotiated: Some(id), ..Default::default() } }

fn new_rtc(p: &Peer) -> std::io::Result<(Rtc, UdpSocket, Vec<IpAddr>)> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    let port = sock.local_addr()?.port();
    let mut rtc = Rtc::builder().build(Instant::now());
    let ips = ips(p);
    for ip in &ips { if let Ok(c) = Candidate::host(SocketAddr::new(*ip, port), "udp") { rtc.add_local_candidate(c); } }
    Ok((rtc, sock, ips))
}

/// Connect to a peer (it must accept). Returns once the channels are open.
pub fn connect(p: &Peer, address: &str, timeout: Duration) -> std::io::Result<Conn> {
    let (mut rtc, sock, ips) = new_rtc(p)?;
    let mut api = rtc.sdp_api();
    let app = api.add_channel_with_config(channel("app", 0));
    let ctl = api.add_channel_with_config(channel("cx", 1));
    let (offer, pending) = api.apply().ok_or_else(|| std::io::Error::other("no offer"))?;
    let sid = b64(&crate::crypto::random::<9>());
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    p.0.rtc.sessions.lock().unwrap().insert(sid.clone(), (address.into(), tx.clone()));
    p.signal(address, json!({ "cx": "offer", "sid": sid, "sdp": offer.to_sdp_string(), "p": "app", "v": 2 }), None)?;
    let (me, addr, sid2) = (p.clone(), address.to_string(), sid.clone());
    spawn(move || run(me, addr, sid2, rtc, sock, ips, app, ctl, rx, Some(pending), Some(ready_tx)));
    match ready_rx.recv_timeout(timeout) {
        Ok(()) => Ok(Conn { address: address.into(), tx }),
        Err(_) => { let _ = tx.send(Cmd::Close); Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "ice-failed")) }
    }
}

/// Signalling messages for us (from Peer::on_wire).
pub(crate) fn on_signal(p: &Peer, from: &str, body: &J) {
    let sid = body.get("sid").and_then(J::as_str).unwrap_or_default().to_string();
    if sid.is_empty() || sid.len() > 24 { return }
    let mine = p.0.rtc.sessions.lock().unwrap().get(&sid).filter(|(a, _)| a == from).map(|(_, tx)| tx.clone());
    match body.get("cx").and_then(J::as_str) {
        Some("answer") => if let (Some(tx), Some(s)) = (mine, body.get("sdp").and_then(J::as_str)) { let _ = tx.send(Cmd::Answer(s.into())); },
        Some("cand") => if let Some(tx) = mine {
            let list: Vec<String> = match body.get("cs").and_then(J::as_array) {
                Some(cs) => cs.iter().take(32).filter_map(|c| c.get(0).and_then(J::as_str).map(String::from)).collect(),
                None => body.get("c").and_then(J::as_str).map(|c| vec![c.to_string()]).unwrap_or_default(),
            };
            for c in list { let _ = tx.send(Cmd::Cand(c)); }
        },
        Some("busy") => if let Some(tx) = mine { let _ = tx.send(Cmd::Close); },
        Some("offer") => if mine.is_none() {
            let (p, from, body) = (p.clone(), from.to_string(), body.clone());          // DTLS setup needs a big stack: its own thread
            spawn(move || if let Err(e) = answer(&p, &from, &sid, &body) { log::warn!("answer to {from}: {e}") });
        },
        _ => {}
    }
}

fn answer(p: &Peer, from: &str, sid: &str, body: &J) -> std::io::Result<()> {
    let purpose = body.get("p").and_then(J::as_str).unwrap_or("app");
    if purpose == "topic" || (purpose == "app" && !p.0.rtc.accept.load(std::sync::atomic::Ordering::Relaxed)) {
        return p.signal(from, json!({ "cx": "busy", "sid": sid }), None);      // topic links and unwanted calls: politely no
    }
    let offer = SdpOffer::from_sdp_string(body.get("sdp").and_then(J::as_str).unwrap_or_default()).map_err(|e| std::io::Error::other(e.to_string()))?;
    let (mut rtc, sock, ips) = new_rtc(p)?;
    let ans: SdpAnswer = rtc.sdp_api().accept_offer(offer).map_err(|e| std::io::Error::other(e.to_string()))?;
    let app = rtc.direct_api().create_data_channel(channel("app", 0));
    let ctl = rtc.direct_api().create_data_channel(channel("cx", 1));
    let (tx, rx) = mpsc::channel();
    p.0.rtc.sessions.lock().unwrap().insert(sid.into(), (from.into(), tx));
    p.signal(from, json!({ "cx": "answer", "sid": sid, "sdp": ans.to_sdp_string() }), None)?;
    let (me, addr, sid) = (p.clone(), from.to_string(), sid.to_string());
    spawn(move || run(me, addr, sid, rtc, sock, ips, app, ctl, rx, None, None));
    Ok(())
}

fn spawn(f: impl FnOnce() + Send + 'static) { std::thread::Builder::new().name("corvax-rtc".into()).stack_size(256 * 1024).spawn(f).expect("thread"); }

/// One connection: drive str0m with the UDP socket and our commands until it closes.
#[allow(clippy::too_many_arguments)]
fn run(p: Peer, address: String, sid: String, mut rtc: Rtc, sock: UdpSocket, ips: Vec<IpAddr>, app: ChannelId, ctl: ChannelId,
       rx: mpsc::Receiver<Cmd>, mut pending: Option<SdpPendingOffer>, ready: Option<mpsc::Sender<()>>) {
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let (mut opened, mut announced, started) = ([false; 2], false, Instant::now());
    let mut buf = vec![0u8; 2048];
    'outer: loop {
        while let Ok(c) = rx.try_recv() {
            match c {
                Cmd::Cand(s) => { if let Ok(c) = Candidate::from_sdp_string(s.trim_start_matches("a=")) { rtc.add_remote_candidate(c) } }
                Cmd::Answer(s) => if let (Some(pend), Ok(a)) = (pending.take(), SdpAnswer::from_sdp_string(&s)) { if rtc.sdp_api().accept_answer(pend, a).is_err() { break 'outer } },
                Cmd::Write(ch, bin, data) => { if let Some(mut c) = rtc.channel(if ch == 0 { app } else { ctl }) { let _ = c.write(bin, &data); } }
                Cmd::Close => break 'outer,
            }
        }
        let deadline = loop {
            match rtc.poll_output() {
                Ok(Output::Transmit(t)) => { let _ = sock.send_to(&t.contents, t.destination); }
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Event(e)) => match e {
                    RtcEvent::ChannelOpen(id, _) => {
                        if id == app { opened[0] = true } else if id == ctl { opened[1] = true }
                        if opened == [true; 2] && !announced {
                            announced = true;
                            let tx = p.0.rtc.sessions.lock().unwrap().get(&sid).map(|(_, t)| t.clone());
                            if let Some(tx) = tx {
                                p.0.rtc.conns.lock().unwrap().insert(address.clone(), tx.clone());
                                if let Some(r) = &ready { let _ = r.send(()); } else { p.emit(Event::Open { from: address.clone(), conn: Conn { address: address.clone(), tx } }) }
                            }
                        }
                    }
                    RtcEvent::ChannelData(d) if d.id == ctl => {
                        if let Some(q) = std::str::from_utf8(&d.data).ok().and_then(|s| s.strip_prefix('K')).and_then(unb64) {
                            if let Some(r) = p.mesh_reply(&q) { if let Some(mut c) = rtc.channel(ctl) { let _ = c.write(false, format!("K{}", b64(&r)).as_bytes()); } }
                        }
                    }
                    RtcEvent::ChannelData(d) if d.id == app => p.emit(Event::Data { from: address.clone(), text: (!d.binary).then(|| String::from_utf8_lossy(&d.data).into_owned()), bytes: d.data }),
                    RtcEvent::ChannelClose(_) => break 'outer,
                    RtcEvent::IceConnectionStateChange(IceConnectionState::Disconnected) => break 'outer,
                    _ => {}
                },
                Err(e) => { log::debug!("rtc: {e}"); break 'outer }
            }
        };
        if !rtc.is_alive() || (!announced && started.elapsed() > Duration::from_secs(40)) { break }
        let wait = deadline.saturating_duration_since(Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(50));
        let _ = sock.set_read_timeout(Some(wait));
        match sock.recv_from(&mut buf) {
            Ok((n, source)) => {
                let local = if source.ip().is_loopback() { IpAddr::V4(Ipv4Addr::LOCALHOST) } else { *ips.iter().find(|i| !i.is_loopback()).unwrap_or(&ips[0]) };
                if let Ok(contents) = buf[..n].try_into() {
                    let _ = rtc.handle_input(Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: SocketAddr::new(local, port), contents }));
                }
            }
            Err(_) => { let _ = rtc.handle_input(Input::Timeout(Instant::now())); }
        }
    }
    p.0.rtc.sessions.lock().unwrap().remove(&sid);
    let mut conns = p.0.rtc.conns.lock().unwrap();
    if announced { conns.remove(&address); drop(conns); p.emit(Event::Closed { from: address }) }
}
