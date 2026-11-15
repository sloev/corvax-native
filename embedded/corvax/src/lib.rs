//! CORVAX for ESP32 and native hosts: a peer that speaks the same protocol as the JS core (SPEC.md).
pub mod bencode;
pub mod crypto;
pub mod net;
pub mod peer;
pub mod record;
pub mod text;
#[cfg(feature = "webrtc")]
pub mod rtc;

pub use crypto::Identity;
pub use peer::{Event, Peer};
pub use record::{Anchor, Beacon};
