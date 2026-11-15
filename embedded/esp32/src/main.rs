//! CORVAX on an ESP32-S3: joins Wi-Fi, then the CORVAX network through anchors (HTTPS), publishes its record,
//! receives sealed messages, and accepts WebRTC DataChannels from browsers and other peers (echoing what it gets).
//! Wi-Fi and anchors at build time: WIFI_SSID, WIFI_PASS, CORVAX_ANCHORS (anchor lines; default: the public anchor).
use corvax::{net::Net, Event, Identity, Peer};
use esp_idf_svc::{eventloop::EspSystemEventLoop, hal::peripherals::Peripherals, nvs::{EspDefaultNvsPartition, EspNvs},
    http::client::{Configuration, EspHttpConnection}, io::{Read, Write}, wifi::{BlockingWifi, ClientConfiguration, EspWifi}};
use std::{sync::Arc, time::Duration};

/// HTTP(S) for KRPC through ESP-IDF's client (mbedTLS with the certificate bundle).
struct EspNet;
impl Net for EspNet {
    fn post(&self, url: &str, body: &[u8], timeout: Duration) -> std::io::Result<Vec<u8>> {
        let e = |x: esp_idf_svc::sys::EspError| std::io::Error::other(x.to_string());
        let mut c = EspHttpConnection::new(&Configuration { timeout: Some(timeout), crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach), ..Default::default() }).map_err(e)?;
        c.initiate_request(esp_idf_svc::http::Method::Post, url, &[("content-type", "text/plain"), ("content-length", &body.len().to_string())]).map_err(e)?;
        c.write_all(body).map_err(|x| std::io::Error::other(format!("{x:?}")))?;
        c.initiate_response().map_err(e)?;
        if c.status() != 200 { return Err(std::io::Error::other(format!("http {}", c.status()))) }
        let (mut out, mut buf) = (Vec::new(), [0u8; 1024]);
        loop { match c.read(&mut buf) { Ok(0) => break, Ok(n) => out.extend(&buf[..n]), Err(x) => return Err(std::io::Error::other(format!("{x:?}"))) } }
        Ok(out)
    }
}

fn main() -> anyhow_free::Result {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let (p, sys, nvs) = (Peripherals::take()?, EspSystemEventLoop::take()?, EspDefaultNvsPartition::take()?);
    let mut wifi = BlockingWifi::wrap(EspWifi::new(p.modem, sys.clone(), Some(nvs.clone()))?, sys)?;
    wifi.set_configuration(&esp_idf_svc::wifi::Configuration::Client(ClientConfiguration {
        ssid: env!("WIFI_SSID").try_into().unwrap(), password: env!("WIFI_PASS").try_into().unwrap(), ..Default::default() }))?;
    wifi.start()?; wifi.connect()?; wifi.wait_netif_up()?;
    let ip = wifi.wifi().sta_netif().get_ip_info()?.ip;
    log::info!("wifi up: {ip}");

    // One identity for the life of the device, kept in NVS.
    let mut store = EspNvs::new(nvs, "corvax", true)?;
    let mut seed = [0u8; 32];
    if store.get_raw("seed", &mut seed)?.is_none() { seed = corvax::crypto::random(); store.set_raw("seed", &seed)?; }
    let (peer, events) = Peer::new(Identity::new(seed), Arc::new(EspNet));
    *peer.0.rtc.ips.lock().unwrap() = vec![std::net::IpAddr::V4(ip)];
    let anchors = option_env!("CORVAX_ANCHORS").unwrap_or("corvax-anchor:w3nd11t347pqgy89seiwcfxen7x8ywcf7dka6rz9f6esp9g5bkey@corvax.sloev.workers.dev:443;web=https://corvax.sloev.workers.dev");
    log::info!("{} anchors answered", peer.join(anchors));
    peer.publish(Some("esp32"))?;
    peer.accept();
    peer.listen();
    log::info!("CORVAX address {}", peer.address());
    for e in events {
        match e {
            Event::Message { from, body } => { log::info!("message from {from}: {body}"); let _ = peer.send(&from, serde_json::json!({ "esp32": body })); }
            Event::Open { from, conn } => { log::info!("connected: {from}"); conn.send_text("hello from an ESP32"); }
            Event::Data { from, text, .. } => log::info!("{from}: {}", text.unwrap_or_default()),
            Event::Closed { from } => log::info!("closed: {from}"),
        }
    }
    Ok(())
}

mod anyhow_free { pub type Result = std::result::Result<(), Box<dyn std::error::Error>>; }
