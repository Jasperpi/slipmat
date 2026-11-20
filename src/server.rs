//! The local web server the decks are drawn by: http://127.0.0.1:4141.
//! Nothing here is reachable from other machines.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::App;
use crate::engine::Control;
use crate::engine::output;
use crate::settings::Settings;
use crate::port;

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Ui;

type Shared = Arc<App>;

struct Fail(anyhow::Error);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for Fail {
    fn from(e: E) -> Self {
        Fail(e.into())
    }
}

type Answer<T> = std::result::Result<T, Fail>;

pub async fn run(open: bool) -> Result<()> {
    let port = port();
    let url = format!("http://127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(_) => {
            println!("Slipmat is already running: {url}");
            if open {
                crate::spotify::auth::open_in_browser(&url);
            }
            return Ok(());
        }
    };
    let app = App::start();
    crate::midi::start(app.clone());
    let router = Router::new()
        .route("/", get(index))
        .route("/{*path}", get(asset))
        .route("/api/ws", get(ws))
        .route("/api/liked", get(liked))
        .route("/api/playlists", get(playlists))
        .route("/api/playlist/{id}", get(playlist))
        .route("/api/search", get(search))
        .route("/api/history", get(history).delete(clear_history))
        .route("/api/load", post(load))
        .route("/api/analyze", post(analyze))
        .route("/api/deck/{d}/wave", get(wave))
        .route("/api/deck/{d}/stemwave", get(stem_wave))
        .route("/api/login/browser", post(login_browser))
        .route("/api/login/riff", post(login_riff))
        .route("/api/logout", post(logout))
        .route("/api/settings", get(settings).post(save_settings))
        .route("/api/devices", get(devices))
        .route("/api/output/reconnect", post(reconnect))
        .route("/api/control", post(control))
        .route("/api/quit", post(quit))
        .route("/api/autodj", get(autodj_status).post(autodj))
        .layer(middleware::from_fn(local_only))
        .with_state(app.clone());
    println!("Slipmat is running: {url}  (Ctrl-C to quit)");
    if open {
        crate::spotify::auth::open_in_browser(&url);
    }
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Only pages served from this machine may talk to it: another website open
/// in the same browser can't, and neither can a DNS-rebinding trick.
async fn local_only(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let port = port();
    let host_ok = [format!("127.0.0.1:{port}"), format!("localhost:{port}")].iter().any(|h| h == host);
    let origin_ok = match req.headers().get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        None => true,
        Some(o) => o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}"),
    };
    if !host_ok || !origin_ok {
        return (StatusCode::FORBIDDEN, "Slipmat only answers pages it served itself.").into_response();
    }
    next.run(req).await
}

fn serve_file(path: &str) -> Response {
    match Ui::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, "no-cache".into())], file.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn index() -> Response {
    serve_file("index.html")
}

async fn asset(Path(path): Path<String>) -> Response {
    serve_file(&path)
}

async fn liked(State(app): State<Shared>) -> Answer<Json<Value>> {
    let rows = app.liked().await?;
    Ok(Json(json!({ "title": "Liked Songs", "rows": rows })))
}

async fn playlists(State(app): State<Shared>) -> Answer<Json<Value>> {
    Ok(Json(json!({ "playlists": app.playlists().await? })))
}

async fn playlist(State(app): State<Shared>, Path(id): Path<String>) -> Answer<Json<Value>> {
    let (title, rows) = app.playlist(&id).await?;
    Ok(Json(json!({ "title": title, "rows": rows })))
}

async fn history(State(app): State<Shared>) -> Json<Value> {
    Json(json!({ "title": "History", "rows": app.history_rows() }))
}

async fn clear_history(State(app): State<Shared>) -> Json<Value> {
    app.clear_history();
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct Q {
    q: String,
}

async fn search(State(app): State<Shared>, Query(q): Query<Q>) -> Answer<Json<Value>> {
    let rows = app.search(&q.q).await?;
    Ok(Json(json!({ "title": format!("“{}”", q.q.trim()), "rows": rows })))
}

#[derive(Deserialize)]
struct LoadReq {
    d: usize,
    uri: String,
}

async fn load(State(app): State<Shared>, Json(req): Json<LoadReq>) -> Answer<Json<Value>> {
    let t = app.load(req.d, &req.uri).await?;
    Ok(Json(json!({ "ok": true, "name": t.name })))
}

#[derive(Deserialize)]
struct AnalyzeReq {
    uris: Vec<String>,
}

async fn analyze(State(app): State<Shared>, Json(req): Json<AnalyzeReq>) -> Json<Value> {
    app.analyse_later(req.uris);
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct WaveQ {
    from: Option<usize>,
}

async fn wave(State(app): State<Shared>, Path(d): Path<usize>, Query(q): Query<WaveQ>) -> Response {
    let audio = app.decks.lock().unwrap().get(d).cloned().flatten().map(|o| o.audio);
    match audio {
        Some(a) => {
            let bytes = a.wave_bytes(q.from.unwrap_or(0));
            let total = a.wave_len();
            let done = a.done.load(std::sync::atomic::Ordering::Acquire);
            Response::builder()
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header("x-wave-total", total.to_string())
                .header("x-wave-done", if done { "1" } else { "0" })
                .header("x-track-key", (Arc::as_ptr(&a) as usize).to_string())
                .body(Body::from(bytes))
                .unwrap()
        }
        None => (StatusCode::NOT_FOUND, "empty deck").into_response(),
    }
}

/// The deck's stem waveform so far (4 bytes per column: vocals, melody, bass, drums).
async fn stem_wave(State(app): State<Shared>, Path(d): Path<usize>, Query(q): Query<WaveQ>) -> Response {
    let on = app.decks.lock().unwrap().get(d).cloned().flatten();
    match on.and_then(|o| o.stems.map(|s| (o.audio, s))) {
        Some((audio, stems)) => Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("x-wave-total", stems.wave_len().to_string())
            .header("x-wave-done", if stems.status() == crate::stems::Status::Done { "1" } else { "0" })
            .header("x-track-key", (Arc::as_ptr(&audio) as usize).to_string())
            .body(Body::from(stems.wave_bytes(q.from.unwrap_or(0))))
            .unwrap(),
        None => (StatusCode::NOT_FOUND, "no stems on this deck").into_response(),
    }
}

async fn login_browser(State(app): State<Shared>) -> Json<Value> {
    app.spotify.browser_login();
    Json(json!({ "ok": true }))
}

async fn login_riff(State(app): State<Shared>) -> Answer<Json<Value>> {
    app.spotify.use_riff()?;
    Ok(Json(json!({ "ok": true })))
}

async fn logout(State(app): State<Shared>) -> Json<Value> {
    app.spotify.logout();
    app.forget_lists();
    Json(json!({ "ok": true }))
}

async fn settings(State(app): State<Shared>) -> Json<Settings> {
    Json(app.settings())
}

async fn save_settings(State(app): State<Shared>, Json(s): Json<Settings>) -> Answer<Json<Value>> {
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || app2.save_settings(s)).await.map_err(|e| anyhow!(e))??;
    Ok(Json(json!({ "ok": true, "output": app.output.status() })))
}

async fn devices() -> Json<Value> {
    let list = tokio::task::spawn_blocking(output::devices).await.unwrap_or_default();
    Json(json!({ "devices": list }))
}

/// Open the sound card afresh: finds the controller if it was plugged in late.
async fn reconnect(State(app): State<Shared>) -> Answer<Json<Value>> {
    let app2 = app.clone();
    let status = tokio::task::spawn_blocking(move || app2.output.reconnect()).await.map_err(|e| anyhow!(e))??;
    Ok(Json(json!({ "ok": true, "output": status })))
}

async fn autodj_status(State(app): State<Shared>) -> Json<crate::autodj::Status> {
    Json(app.autodj.status())
}

async fn autodj(State(app): State<Shared>, Json(r): Json<crate::autodj::Request>) -> Json<crate::autodj::Status> {
    app.autodj.handle(r);
    Json(app.autodj.status())
}

async fn quit() -> Json<Value> {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(json!({ "ok": true }))
}

async fn control(State(app): State<Shared>, Json(c): Json<Control>) -> Json<Value> {
    app.engine.send(c);
    Json(json!({ "ok": true }))
}

fn deck_meta(app: &App) -> Vec<Value> {
    let decks = app.decks.lock().unwrap().clone();
    decks
        .iter()
        .map(|on| match on {
            None => Value::Null,
            Some(o) => {
                let row = app.row(&o.track);
                json!({
                    "uri": row.uri, "id": row.id, "name": row.name, "artist": row.artist,
                    "album": row.album, "image": row.image, "duration_ms": row.duration_ms,
                    "key": o.analysis.as_ref().and_then(|a| a.key),
                    "bpm": o.analysis.as_ref().map(|a| a.bpm),
                    "analysing": o.analysing,
                    "wave": o.audio.wave_len(),
                    "track_key": Arc::as_ptr(&o.audio) as usize,
                })
            }
        })
        .collect()
}

async fn ws(State(app): State<Shared>, upgrade: WebSocketUpgrade, headers: HeaderMap) -> Response {
    let _ = headers;
    upgrade.on_upgrade(move |socket| session(app, socket))
}

async fn session(app: Shared, mut socket: WebSocket) {
    let mut events = app.events.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    let mut slow = 0u32;
    let mut extras = json!({});
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // The slower-changing parts every ~half second.
                if slow % 15 == 0 {
                    extras = json!({
                        "autodj": app.autodj.status(),
                        "login": app.spotify.login(),
                        "output": app.output.status(),
                        "midi": app.midi.lock().unwrap().clone(),
                        "batch": app.batch.lock().unwrap().clone(),
                        "sampler": app.sampler_names,
                        "played": app.played.lock().unwrap().clone(),
                    });
                }
                slow += 1;
                extras["autodj"] = serde_json::to_value(app.autodj.status()).unwrap_or_default();
                let msg = json!({
                    "t": "state",
                    "engine": app.engine.snapshot(),
                    "decks": deck_meta(&app),
                    "extras": extras,
                });
                if socket.send(Message::Text(msg.to_string().into())).await.is_err() {
                    break;
                }
            }
            ev = events.recv() => {
                match ev {
                    Ok(ev) => {
                        if socket.send(Message::Text(serde_json::to_string(&ev).unwrap_or_default().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                handle(&app, &text).await;
            }
        }
    }
}

async fn handle(app: &Shared, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    if let Some(t) = v.get("t").and_then(|t| t.as_str()) {
        match t {
            "load" => {
                let d = v.get("d").and_then(|d| d.as_u64()).unwrap_or(0) as usize;
                let uri = v.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                // The first of two LOAD presses (an instant double).
                if app.doubled_recently(d) {
                    return;
                }
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = app.load(d, &uri).await {
                        app.notice(e.to_string(), true);
                    }
                });
            }
            "double" => app.double(v.get("d").and_then(|d| d.as_u64()).unwrap_or(0) as usize),
            "key_sync" => app.key_sync(v.get("d").and_then(|d| d.as_u64()).unwrap_or(0) as usize),
            _ => {}
        }
        return;
    }
    match serde_json::from_value::<Control>(v) {
        Ok(c) => app.engine.send(c),
        Err(e) => log::warn!("bad control {text}: {e}"),
    }
}
