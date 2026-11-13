// CORVAX desktop: the Rust half of the host (SPEC §11). The node itself is CORVAX's JS bundle in a hidden window
// (web/node.html); this file only moves bytes: UDP, HTTP (+ WebSocket), files, DNS. Events go to the "node" window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::{Bytes, Incoming}, server::conn::http1, service::service_fn, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, net::SocketAddr, sync::{atomic::{AtomicU64, Ordering}, Arc}};
use tauri::{menu::{Menu, MenuItem}, tray::TrayIconBuilder, AppHandle, Emitter, Manager, State};
use tokio::{net::{TcpListener, UdpSocket}, sync::{mpsc, oneshot, Mutex}};
use tokio_tungstenite::{tungstenite::{handshake::derive_accept_key, protocol::Role, Message}, WebSocketStream};

const BODY_MAX: usize = 24 * 1024;            // SPEC: /krpc bodies ≤ 24 KB

struct Resp { status: u16, headers: HashMap<String, String>, body: Vec<u8> }

#[derive(Default)]
struct Host {
    next: AtomicU64,
    udp: Mutex<HashMap<u64, Arc<UdpSocket>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Resp>>>,              // HTTP requests waiting for the JS handler
    servers: Mutex<HashMap<u64, tauri::async_runtime::JoinHandle<()>>>,
    ws: Mutex<HashMap<u64, mpsc::UnboundedSender<Message>>>,
}
impl Host { fn id(&self) -> u64 { self.next.fetch_add(1, Ordering::Relaxed) + 1 } }
type H<'a> = State<'a, Arc<Host>>;
fn to_node(app: &AppHandle, event: &str, payload: serde_json::Value) { let _ = app.emit_to("node", event, payload); }

// ---- UDP ---------------------------------------------------------------------------------------
#[derive(Deserialize)] struct UdpBind { port: u16, address: String, reuse: bool, group: Option<String>, broadcast: bool }
#[tauri::command]
async fn udp_bind(app: AppHandle, host: H<'_>, args: UdpBind) -> Result<serde_json::Value, String> {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).map_err(|e| e.to_string())?;
    if args.reuse { s.set_reuse_address(true).ok(); #[cfg(unix)] s.set_reuse_port(true).ok(); }
    s.set_broadcast(args.broadcast).ok();
    s.set_nonblocking(true).map_err(|e| e.to_string())?;
    let addr: SocketAddr = format!("{}:{}", args.address, args.port).parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
    s.bind(&addr.into()).map_err(|e| e.to_string())?;
    if let Some(g) = &args.group { s.join_multicast_v4(&g.parse().map_err(|_| "bad group")?, &std::net::Ipv4Addr::UNSPECIFIED).ok(); s.set_multicast_loop_v4(true).ok(); }
    let sock = Arc::new(UdpSocket::from_std(s.into()).map_err(|e| e.to_string())?);
    let (id, port) = (host.id(), sock.local_addr().map_err(|e| e.to_string())?.port());
    host.udp.lock().await.insert(id, sock.clone());
    tauri::async_runtime::spawn(async move {
        let mut buf = vec![0u8; 65536];
        while let Ok((n, from)) = sock.recv_from(&mut buf).await {
            to_node(&app, "udp", json!({ "id": id, "data": B64.encode(&buf[..n]), "address": from.ip().to_string(), "port": from.port() }));
        }
    });
    Ok(json!({ "id": id, "port": port }))
}
#[derive(Deserialize)] struct UdpSend { id: u64, data: String, port: u16, host: String }
#[tauri::command]
async fn udp_send(host: H<'_>, args: UdpSend) -> Result<(), String> {
    let sock = host.udp.lock().await.get(&args.id).cloned().ok_or("closed")?;
    let bytes = B64.decode(args.data).map_err(|e| e.to_string())?;
    sock.send_to(&bytes, (args.host.as_str(), args.port)).await.map(|_| ()).map_err(|e| e.to_string())
}
#[derive(Deserialize)] struct Id { id: u64 }
#[tauri::command]
async fn udp_close(host: H<'_>, args: Id) -> Result<(), String> { host.udp.lock().await.remove(&args.id); Ok(()) }

// ---- HTTP + WebSocket ---------------------------------------------------------------------------
// Each request goes to the JS handler as an "http" event; the answer comes back through http_respond.
// If the client leaves first, "http-abort" tells JS (it ends long polls).
#[derive(Deserialize)] struct Listen { port: u16, address: String }
#[tauri::command]
async fn http_listen(app: AppHandle, host: H<'_>, args: Listen) -> Result<serde_json::Value, String> {
    let listener = TcpListener::bind((args.address.as_str(), args.port)).await.map_err(|e| e.to_string())?;
    let (sid, state) = (host.id(), host.inner().clone());
    let task = tauri::async_runtime::spawn(async move {
        while let Ok((stream, peer)) = listener.accept().await {
            let (app, state) = (app.clone(), state.clone());
            tokio::spawn(async move {
                let svc = service_fn(move |req| serve(app.clone(), state.clone(), sid, peer, req));
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).with_upgrades().await;
            });
        }
    });
    host.servers.lock().await.insert(sid, task);
    Ok(json!({ "sid": sid }))
}

struct AbortOnDrop { app: AppHandle, rid: u64, done: bool }
impl Drop for AbortOnDrop { fn drop(&mut self) { if !self.done { to_node(&self.app, "http-abort", json!({ "rid": self.rid })); } } }

async fn serve(app: AppHandle, host: Arc<Host>, sid: u64, peer: SocketAddr, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let path = req.uri().path().to_string();
    let headers: HashMap<String, String> = req.headers().iter().map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string())).collect();
    if headers.get("upgrade").map(|v| v.eq_ignore_ascii_case("websocket")).unwrap_or(false) {
        return Ok(websocket(app, host, sid, path, headers, req).await);
    }
    let method = req.method().to_string();
    let body = match Limited::new(req.into_body(), BODY_MAX).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(Response::builder().status(StatusCode::PAYLOAD_TOO_LARGE).body(Full::default()).unwrap()),
    };
    let (rid, (tx, rx)) = (host.id(), oneshot::channel());
    host.pending.lock().await.insert(rid, tx);
    let mut guard = AbortOnDrop { app: app.clone(), rid, done: false };
    to_node(&app, "http", json!({ "sid": sid, "rid": rid, "method": method, "path": path, "headers": headers,
        "ip": peer.ip().to_string(), "port": peer.port(), "body": B64.encode(&body) }));
    let resp = rx.await;
    guard.done = true;
    let r = resp.unwrap_or(Resp { status: 500, headers: HashMap::new(), body: vec![] });
    let mut b = Response::builder().status(r.status);
    for (k, v) in r.headers { b = b.header(k, v); }
    Ok(b.body(Full::new(Bytes::from(r.body))).unwrap())
}

#[derive(Deserialize)] struct Respond { rid: u64, status: u16, headers: HashMap<String, String>, body: String }
#[tauri::command]
async fn http_respond(host: H<'_>, args: Respond) -> Result<(), String> {
    if let Some(tx) = host.pending.lock().await.remove(&args.rid) {
        let _ = tx.send(Resp { status: args.status, headers: args.headers, body: B64.decode(args.body).unwrap_or_default() });
    }
    Ok(())
}
#[derive(Deserialize)] struct Sid { sid: u64 }
#[tauri::command]
async fn http_close(host: H<'_>, args: Sid) -> Result<(), String> { if let Some(t) = host.servers.lock().await.remove(&args.sid) { t.abort(); } Ok(()) }

async fn websocket(app: AppHandle, host: Arc<Host>, sid: u64, path: String, headers: HashMap<String, String>, mut req: Request<Incoming>) -> Response<Full<Bytes>> {
    let Some(key) = headers.get("sec-websocket-key") else { return Response::builder().status(400).body(Full::default()).unwrap() };
    let accept = derive_accept_key(key.as_bytes());
    let wid = host.id();
    tokio::spawn(async move {
        let Ok(up) = hyper::upgrade::on(&mut req).await else { return };
        let ws = WebSocketStream::from_raw_socket(TokioIo::new(up), Role::Server, None).await;
        let (mut write, mut read) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        host.ws.lock().await.insert(wid, tx);
        to_node(&app, "ws-open", json!({ "sid": sid, "wid": wid, "path": path }));
        tokio::spawn(async move { while let Some(m) = rx.recv().await { if write.send(m).await.is_err() { break } } });
        while let Some(Ok(m)) = read.next().await {
            match m {
                Message::Text(t) => to_node(&app, "ws-msg", json!({ "wid": wid, "text": t.to_string() })),
                Message::Binary(b) => to_node(&app, "ws-msg", json!({ "wid": wid, "data": B64.encode(&b) })),
                Message::Close(_) => break,
                _ => {}
            }
        }
        host.ws.lock().await.remove(&wid);
        to_node(&app, "ws-close", json!({ "wid": wid }));
    });
    Response::builder().status(101).header("upgrade", "websocket").header("connection", "Upgrade")
        .header("sec-websocket-accept", accept).body(Full::default()).unwrap()
}
#[derive(Deserialize)] struct WsSend { wid: u64, text: Option<String>, data: Option<String> }
#[tauri::command]
async fn ws_send(host: H<'_>, args: WsSend) -> Result<(), String> {
    let m = match (args.text, args.data) { (Some(t), _) => Message::text(t), (_, Some(d)) => Message::binary(B64.decode(d).unwrap_or_default()), _ => return Ok(()) };
    if let Some(tx) = host.ws.lock().await.get(&args.wid) { let _ = tx.send(m); }
    Ok(())
}
#[derive(Deserialize)] struct Wid { wid: u64 }
#[tauri::command]
async fn ws_close(host: H<'_>, args: Wid) -> Result<(), String> { if let Some(tx) = host.ws.lock().await.remove(&args.wid) { let _ = tx.send(Message::Close(None)); } Ok(()) }

// ---- kv, DNS, readiness ------------------------------------------------------------------------
#[derive(Deserialize)] struct KvGet { name: String }
#[derive(Deserialize)] struct KvSet { name: String, value: String }
fn kv_path(app: &AppHandle, name: &str) -> Result<std::path::PathBuf, String> {
    if name.contains(['/', '\\']) || name.starts_with('.') { return Err("bad name".into()) }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name))
}
#[tauri::command]
fn kv_get(app: AppHandle, args: KvGet) -> Result<Option<String>, String> { Ok(std::fs::read_to_string(kv_path(&app, &args.name)?).ok()) }
#[tauri::command]
fn kv_set(app: AppHandle, args: KvSet) -> Result<(), String> { std::fs::write(kv_path(&app, &args.name)?, args.value).map_err(|e| e.to_string()) }
#[derive(Deserialize)] struct Dns { name: String }
#[tauri::command]
async fn dns_txt(args: Dns) -> Result<String, String> {
    let r = hickory_resolver::TokioAsyncResolver::tokio_from_system_conf().map_err(|e| e.to_string())?;
    let txt = r.txt_lookup(args.name).await.map_err(|e| e.to_string())?;
    Ok(txt.iter().map(|t| t.iter().map(|d| String::from_utf8_lossy(d).to_string()).collect::<String>()).collect::<Vec<_>>().join("\n"))
}
#[derive(Deserialize)] struct Ready { key: String, line: String, port: u16 }
#[tauri::command]
fn node_ready(app: AppHandle, args: Ready) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("CORVAX node {}…{}", &args.key[..6], if args.line.is_empty() { "" } else { " · anchor" })));
    }
    let _ = args.port;
}

// macOS would otherwise nap the hidden window and stall the node's timers.
#[cfg(target_os = "macos")]
fn keep_awake() {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    // SAFETY: a plain Foundation call with valid arguments; the returned token is kept for the life of the process.
    let token = unsafe { NSProcessInfo::processInfo().beginActivityWithOptions_reason(
        NSActivityOptions::NSActivityUserInitiatedAllowingIdleSystemSleep, &NSString::from_str("CORVAX node")) };
    std::mem::forget(token);
}
#[cfg(not(target_os = "macos"))]
fn keep_awake() {}

fn main() {
    keep_awake();
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .manage(Arc::new(Host::default()))
        .invoke_handler(tauri::generate_handler![udp_bind, udp_send, udp_close, http_listen, http_respond, http_close, ws_send, ws_close, kv_get, kv_set, dns_txt, node_ready])
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open CORVAX", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            TrayIconBuilder::with_id("main").menu(&Menu::with_items(app, &[&open, &quit])?)
                .icon(app.default_window_icon().cloned().expect("icon"))
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => if let Some(w) = app.get_webview_window("ui") { let _ = w.show(); let _ = w.set_focus(); },
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|w, e| if let tauri::WindowEvent::CloseRequested { api, .. } = e { if w.label() == "ui" { api.prevent_close(); let _ = w.hide(); } })
        .run(tauri::generate_context!())
        .expect("CORVAX desktop");
}
