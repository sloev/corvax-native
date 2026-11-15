//! corvax-rs: a Rust CORVAX peer. Joins through the given anchor lines, publishes, prints messages, echoes on
//! DataChannels. Used for interop tests against the JS core, and as the reference for the ESP32 firmware.
//!   corvax-rs "<anchor line>" [name]
use corvax::{net::TcpNet, Event, Identity, Peer};
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let anchors = args.next().expect("usage: corvax-rs \"<anchor line>\" [name]");
    let (peer, events) = Peer::new(Identity::generate(), Arc::new(TcpNet));
    let n = peer.join(&anchors);
    peer.publish(args.next().as_deref()).expect("publish");
    peer.accept();
    peer.listen();
    println!("ready {} ({n} anchors)", peer.address());
    for e in events {
        match e {
            Event::Message { from, body } => { println!("message {from} {body}"); let _ = peer.send(&from, serde_json::json!({ "echo": body })); }
            Event::Open { from, .. } => println!("open {from}"),
            Event::Data { from, text, bytes } => println!("data {from} {}", text.unwrap_or_else(|| format!("{} bytes", bytes.len()))),
            Event::Closed { from } => println!("closed {from}"),
        }
    }
}
