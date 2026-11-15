# CORVAX in Rust (ESP32)

The CORVAX protocol for devices that can't run the JS core, with **WebRTC DataChannels** (str0m, pure-Rust crypto).

| Crate | What |
|---|---|
| `corvax` | identity, signatures, sealed messages (plain, compressed, kept), Beacons, anchor lines, KRPC over HTTP, a peer (join, publish, resolve, inbox polls, send), mesh queries on the control channel, WebRTC (answer and dial, channels 0 `app` and 1 `cx`) |
| `corvax-cli` | `corvax-rs "<anchor line>" [name]`: the peer on a desktop (3.9 MB); echoes messages, prints DataChannel data |
| `esp32` | firmware for the ESP32-S3 (PSRAM recommended): Wi-Fi, identity in NVS, HTTPS anchors, accepts DataChannels |

**Conformance:** `cargo test` checks every primitive against `corvax/tests/vectors.json`, made by the JS core (`node scripts/vectors.js` in CORVAX): identical keys, signatures and records; messages sealed in JS open here and the reverse.

**Tested against the JS core:** a JS peer resolved a Rust peer's record, exchanged sealed messages with it both ways, and opened a WebRTC DataChannel to it through a JS anchor.

**Not yet tested:** the firmware on real hardware (CI builds it). Not in the port: UDP DHT routing, topics, routing and carrying for others (it answers mesh queries for itself only).

## ESP32-S3

```sh
cargo install espup espflash ldproxy && espup install
cd esp32
WIFI_SSID=… WIFI_PASS=… cargo run --release        # builds, flashes, opens the monitor
```
It logs its CORVAX address; connect from any CORVAX app: `await me.connect(address)`.
