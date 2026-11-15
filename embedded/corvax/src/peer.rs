//! A CORVAX peer: joins through anchors, publishes its Beacon, listens on its inboxes, sends and receives sealed
//! messages, answers the mesh queries of connected peers and (feature `webrtc`) opens DataChannels.
//! Blocking and thread-based, so the same code runs on a desktop and on an ESP32 (ESP-IDF std).
use crate::bencode::{dict, encode, Value};
use crate::crypto::{open, random, seal, sha1, sha256, Identity};
use crate::net::{rpc, Net};
use crate::record::{Anchor, Beacon};
use crate::text::{hex, key32, z32};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }
const T: Duration = Duration::from_secs(8);

/// What happens to a peer, in order. `Open` hands over a connection (see `rtc::Conn`).
#[derive(Debug)]
pub enum Event {
    Message { from: String, body: J },
    #[cfg(feature = "webrtc")]
    Open { from: String, conn: crate::rtc::Conn },
    #[cfg(feature = "webrtc")]
    Data { from: String, text: Option<String>, bytes: Vec<u8> },
    #[cfg(feature = "webrtc")]
    Closed { from: String },
}

#[derive(Clone)]
pub struct Peer(pub Arc<Inner>);
pub struct Inner {
    pub id: Identity,
    pub net: Arc<dyn Net>,
    pub anchors: Mutex<Vec<Anchor>>,              // verified anchors
    pub inboxes: Mutex<Vec<Anchor>>,              // ≤ 2, where our Beacon says to send to us
    pub name: Mutex<Option<String>>,
    pub mine: Mutex<Option<Beacon>>,
    pub beacons: Mutex<HashMap<String, (Beacon, Instant)>>,
    pub held: Mutex<HashMap<String, Beacon>>,     // Beacons connected peers gave us (mesh, SPEC §13)
    pub seen: Mutex<HashMap<String, u64>>,        // message id -> valid until
    pub events: Mutex<mpsc::Sender<Event>>,
    pub stop: AtomicBool,
    #[cfg(feature = "webrtc")]
    pub rtc: crate::rtc::Rtcs,
}

impl Peer {
    /// A peer with this identity; events arrive on the returned receiver.
    pub fn new(id: Identity, net: Arc<dyn Net>) -> (Peer, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let p = Peer(Arc::new(Inner {
            id, net, anchors: Mutex::default(), inboxes: Mutex::default(), name: Mutex::default(), mine: Mutex::default(),
            beacons: Mutex::default(), held: Mutex::default(), seen: Mutex::default(), events: Mutex::new(tx), stop: AtomicBool::new(false),
            #[cfg(feature = "webrtc")]
            rtc: crate::rtc::Rtcs::default(),
        }));
        (p, rx)
    }
    pub fn address(&self) -> String { self.0.id.address() }
    pub fn seed(&self) -> [u8; 32] { self.0.id.seed }
    pub(crate) fn emit(&self, e: Event) { let _ = self.0.events.lock().unwrap().send(e); }

    /// Verify anchors from any text holding anchor lines (cx_hi: they must sign a fresh nonce with the key in their
    /// line) and choose 2 inboxes: the verified anchors XOR-closest to our key. Returns how many answered.
    pub fn join(&self, text: &str) -> usize {
        for a in text.lines().filter_map(Anchor::parse) {
            let n = random::<16>();
            let ok = rpc(&*self.0.net, &a.url(), "cx_hi", dict([("n", Value::from(&n[..]))]), Duration::from_secs(5)).ok()
                .filter(|r| r.get("k").and_then(Value::bytes).map(|k| z32(k) == a.key).unwrap_or(false))
                .filter(|r| crate::crypto::verify(&key32(&a.key).unwrap(), r.get("sig").and_then(Value::bytes).unwrap_or_default(), &[b"cx-hi".as_slice(), &n].concat()));
            let mut list = self.0.anchors.lock().unwrap();
            if ok.is_some() && !list.iter().any(|x| x.key == a.key) { list.push(a) }
        }
        let mut list = self.0.anchors.lock().unwrap().clone();
        let me = self.0.id.ed_pub;
        list.sort_by_key(|a| key32(&a.key).map(|k| k.iter().zip(me).map(|(x, y)| x ^ y).collect::<Vec<_>>()).unwrap_or_default());
        *self.0.inboxes.lock().unwrap() = list.into_iter().take(2).collect();
        self.0.anchors.lock().unwrap().len()
    }

    /// Our Beacon: inbox lines, v=2 (we read compressed bodies and batched candidates), an optional name.
    fn beacon(&self) -> Option<Beacon> {
        let mut fields: Vec<(&str, String)> = Vec::new();
        let name = self.0.name.lock().unwrap().clone();
        if let Some(n) = &name { fields.push(("n", n.clone())) }
        for a in self.0.inboxes.lock().unwrap().iter() { fields.push(("c", a.line())) }
        fields.push(("v", "2".into()));
        let b = Beacon::make(&self.0.id, &fields, now_ms() * 1000)?;
        *self.0.mine.lock().unwrap() = Some(b.clone());
        Some(b)
    }
    /// Make our address resolvable: put our Beacon on our inbox anchors (they put it in the DHT).
    pub fn publish(&self, name: Option<&str>) -> Result<()> {
        if let Some(n) = name { *self.0.name.lock().unwrap() = Some(n.into()) }
        let b = self.beacon().ok_or_else(|| Error::other("beacon too large"))?;
        let put = dict([("k", Value::from(&b.key[..])), ("seq", Value::Int(b.seq as i64)), ("sig", Value::from(&b.sig[..])), ("v", b.v.clone().into())]);
        let ok = self.0.inboxes.lock().unwrap().clone().iter().filter(|a| rpc(&*self.0.net, &a.url(), "put", put.clone(), T).is_ok()).count();
        if ok == 0 { return Err(Error::other("no anchor took the beacon")) }
        Ok(())
    }

    /// A peer's Beacon: held from the mesh, cached (10 min) or fetched from our anchors; the newest valid one wins.
    pub fn resolve(&self, address: &str) -> Option<Beacon> {
        if let Some((b, t)) = self.0.beacons.lock().unwrap().get(address) { if t.elapsed() < Duration::from_secs(600) { return Some(b.clone()) } }
        let key = key32(address)?;
        let mut best = self.0.held.lock().unwrap().get(address).cloned();
        let ask = dict([("target", Value::from(&sha1(&key)[..])), ("k", Value::from(&key[..]))]);
        for a in self.0.anchors.lock().unwrap().clone().iter().take(3) {
            let Ok(r) = rpc(&*self.0.net, &a.url(), "get", ask.clone(), T) else { continue };
            let (Some(v), Some(seq), Some(sig)) = (r.get("v").and_then(Value::bytes), r.get("seq").and_then(Value::int), r.get("sig").and_then(Value::bytes)) else { continue };
            let b = Beacon { key, seq: seq as u64, sig: sig.try_into().ok()?, v: v.to_vec() };
            if b.valid() && best.as_ref().map(|x| b.seq > x.seq).unwrap_or(true) { best = Some(b) }
        }
        let b = best?;
        self.0.beacons.lock().unwrap().insert(address.into(), (b.clone(), Instant::now()));
        Some(b)
    }

    /// Send an app message (any JSON). keep: ms to keep trying (store-carry-forward is the JS core's; here it is
    /// a kept message that stays valid longer, so carriers and late inbox polls can still deliver it).
    pub fn send(&self, address: &str, body: J) -> Result<()> { self.signal(address, json!({ "app": body }), None) }
    pub fn send_kept(&self, address: &str, body: J, keep_ms: u64) -> Result<()> { self.signal(address, json!({ "app": body }), Some(now_ms() + keep_ms.min(crate::crypto::KEEP_MAX))) }

    pub(crate) fn signal(&self, address: &str, body: J, exp: Option<u64>) -> Result<()> {
        let b = self.resolve(address).ok_or_else(|| Error::other(format!("no record for {address}")))?;
        let (to, x) = (b.key, b.x_pub().ok_or_else(|| Error::other("record has no x"))?);
        let msg = seal(&self.0.id, &to, &x, &body, b.version() >= 2, now_ms(), exp).ok_or_else(|| Error::other("message too large"))?;
        #[cfg(feature = "webrtc")]
        if self.0.rtc.query(address, "cx_send", dict([("to", Value::from(&to[..])), ("msg", msg.clone().into())])) { return Ok(()) }   // over an open connection: no anchor
        let mut last = Error::other("no inbox line");
        for a in b.field("c").iter().filter_map(|l| Anchor::parse(l)) {
            match rpc(&*self.0.net, &a.url(), "cx_send", dict([("to", Value::from(&to[..])), ("msg", msg.clone().into())]), T) { Ok(_) => return Ok(()), Err(e) => last = e }
        }
        Err(last)
    }

    /// Poll every inbox for messages (one thread each; 50 s long polls, signed for that anchor and that ack).
    pub fn listen(&self) {
        for a in self.0.inboxes.lock().unwrap().clone() {
            let me = self.clone();
            std::thread::Builder::new().name("corvax-inbox".into()).stack_size(96 * 1024).spawn(move || {
                let (mut after, mut fails) = (0u64, 0u32);
                while !me.0.stop.load(Ordering::Relaxed) {
                    let ts = now_ms();
                    let sig = me.0.id.sign(format!("cx-listen:{ts}:{after}:{}", a.key).as_bytes());
                    let q = dict([("k", Value::from(&me.0.id.ed_pub[..])), ("ts", Value::Int(ts as i64)), ("sig", Value::from(&sig[..])),
                        ("after", Value::Int(after as i64)), ("v", Value::Int(2)), ("w", Value::Int(50))]);
                    match rpc(&*me.0.net, &a.url(), "cx_listen", q, Duration::from_secs(65)) {
                        Ok(r) => {
                            fails = 0;
                            for m in r.get("msgs").and_then(Value::list).unwrap_or_default() {
                                after = after.max(m.get("seq").and_then(Value::int).unwrap_or(0) as u64);
                                if let Some(w) = m.get("msg").and_then(Value::bytes) { me.on_wire(w) }
                            }
                        }
                        Err(_) => { fails += 1; std::thread::sleep(Duration::from_millis(500 * 2u64.pow(fails.min(6)))) }
                    }
                }
            }).expect("thread");
        }
    }

    /// A sealed message from anywhere (inbox, connection, by hand).
    pub fn on_wire(&self, wire: &[u8]) {
        let now = now_ms();
        let Some(m) = open(&self.0.id, wire, now) else { return };
        {
            let mut seen = self.0.seen.lock().unwrap();
            if seen.contains_key(&m.id) { return }
            seen.retain(|_, until| *until > now);
            seen.insert(m.id.clone(), (now + crate::crypto::MAX_AGE).max(m.exp));
        }
        let from = z32(&m.from);
        if m.exp > 0 { let _ = self.signal(&from, json!({ "cx": "got", "id": &hex(&sha256(wire))[..32] }), None); }   // a kept message: receipt
        match m.body.get("cx").and_then(J::as_str) {
            #[cfg(feature = "webrtc")]
            Some(_) => crate::rtc::on_signal(self, &from, &m.body),
            #[cfg(not(feature = "webrtc"))]
            Some(_) => {}
            None => if let Some(app) = m.body.get("app") { self.emit(Event::Message { from, body: app.clone() }) },
        }
    }

    /// Mesh queries from a connected peer (SPEC §13): Beacons, anchor list, messages for us. Others: 204.
    pub fn mesh(&self, q: &str, a: &Value) -> std::result::Result<Value, (i64, &'static str)> {
        let key = |v: Option<&Value>| v.and_then(Value::bytes).and_then(|b| <[u8; 32]>::try_from(b).ok());
        match q {
            "cx_hi" => {
                let n = a.get("n").and_then(Value::bytes).filter(|n| n.len() <= 64).ok_or((203, "bad nonce"))?;
                Ok(dict([("k", Value::from(&self.0.id.ed_pub[..])), ("sig", Value::from(&self.0.id.sign(&[b"cx-hi".as_slice(), n].concat())[..]))]))
            }
            "get" => {
                let k = key(a.get("k")).ok_or((203, "bad key"))?;
                let b = if k == self.0.id.ed_pub { self.0.mine.lock().unwrap().clone() } else { self.0.held.lock().unwrap().get(&z32(&k)).cloned() };
                Ok(b.map(|b| dict([("k", Value::from(&b.key[..])), ("seq", Value::Int(b.seq as i64)), ("sig", Value::from(&b.sig[..])), ("v", b.v.into())])).unwrap_or(dict([])))
            }
            "put" => {
                let b = Beacon { key: key(a.get("k")).ok_or((206, "bad signature"))?, seq: a.get("seq").and_then(Value::int).ok_or((206, "bad signature"))? as u64,
                    sig: a.get("sig").and_then(Value::bytes).and_then(|s| s.try_into().ok()).ok_or((206, "bad signature"))?, v: a.get("v").and_then(Value::bytes).ok_or((206, "bad signature"))?.to_vec() };
                if !b.valid() { return Err((206, "bad signature")) }
                let mut held = self.0.held.lock().unwrap();
                if held.get(&b.address()).map(|x| x.seq < b.seq).unwrap_or(true) { if held.len() >= 64 { held.clear() } held.insert(b.address(), b); }
                Ok(dict([]))
            }
            "cx_anchors" => Ok(dict([("a", self.0.anchors.lock().unwrap().iter().map(Anchor::line).collect::<Vec<_>>().join("\n").as_str().into())])),
            "cx_peers" => Ok(dict([("p", Value::List(vec![]))])),
            "cx_send" => {
                let (to, msg) = (key(a.get("to")).ok_or((203, "bad message"))?, a.get("msg").and_then(Value::bytes).ok_or((203, "bad message"))?);
                if to != self.0.id.ed_pub { return Err((204, "no path to that peer")) }      // this peer doesn't pass messages on
                self.on_wire(msg);
                Ok(dict([("queued", Value::Int(1))]))
            }
            _ => Err((204, "not available on this peer")),
        }
    }
    pub(crate) fn mesh_reply(&self, query: &[u8]) -> Option<Vec<u8>> {
        let m = crate::bencode::decode(query)?;
        if m.get("y")?.str()? != "q" { return None }
        let t = m.get("t")?.clone();
        let r = match self.mesh(m.get("q")?.str()?, m.get("a").unwrap_or(&dict([]))) {
            Ok(r) => dict([("t", t), ("y", "r".into()), ("r", r)]),
            Err((c, e)) => dict([("t", t), ("y", "e".into()), ("e", Value::List(vec![Value::Int(c), e.into()]))]),
        };
        Some(encode(&r))
    }

    /// Answer incoming connection offers (DataChannels).
    #[cfg(feature = "webrtc")]
    pub fn accept(&self) { self.0.rtc.accept.store(true, Ordering::Relaxed) }
    #[cfg(feature = "webrtc")]
    pub fn connect(&self, address: &str) -> Result<crate::rtc::Conn> { crate::rtc::connect(self, address, Duration::from_secs(20)) }
    pub fn close(&self) { self.0.stop.store(true, Ordering::Relaxed); #[cfg(feature = "webrtc")] self.0.rtc.close_all(); }
}
