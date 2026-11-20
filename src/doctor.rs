//! `slipmat doctor`: live checks that never make a sound and never touch
//! what the Spotify account is playing elsewhere.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use librespot_protocol::extension_kind::ExtensionKind;

use crate::analysis;
use crate::engine::output;
use crate::loader;
use crate::session::{Login, Spotify};
use crate::spotify::spot;
use crate::track::SRC_RATE;

const G: &str = "\x1b[32m✓\x1b[0m";
const X: &str = "\x1b[31m✗\x1b[0m";

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

async fn signed_in() -> Result<std::sync::Arc<Spotify>> {
    let spotify = Spotify::new();
    spotify.resume();
    if matches!(spotify.login(), Login::SignedOut { riff: true }) {
        println!("  using riff's saved Spotify login");
        spotify.use_riff()?;
    }
    let mut rx = spotify.watch_login();
    let wait = async {
        loop {
            match rx.borrow_and_update().clone() {
                Login::Online { user } => return Ok(user),
                Login::Failed { message } => return Err(anyhow!(message)),
                Login::SignedOut { .. } => return Err(anyhow!("not signed in: run slipmat and sign in")),
                _ => {}
            }
            rx.changed().await?;
        }
    };
    let user: String = tokio::time::timeout(Duration::from_secs(25), wait).await.map_err(|_| anyhow!("Spotify didn't answer"))??;
    println!("{G} Spotify: signed in as {user}");
    Ok(spotify)
}

pub async fn run(args: &[String]) -> Result<()> {
    if let Some(q) = value(args, "--load") {
        return load(q).await;
    }
    if let Some(f) = value(args, "--stems") {
        let model = match value(args, "--model") {
            Some("demucs") => crate::stems::Model::Demucs,
            _ => crate::stems::Model::Spleeter,
        };
        return stems_file(f, model).await;
    }
    if args.iter().any(|a| a == "--fake-ddj") {
        return fake_ddj(value(args, "--name").unwrap_or("DDJ-FLX4 fake"), value(args, "--midi"));
    }
    if let Some(q) = value(args, "--attrs") {
        return attrs(q).await;
    }
    println!("Sound cards:");
    for d in output::devices() {
        println!("  {} {} ({} channels){}", if d.controller { "🎛" } else { "·" }, d.name, d.channels, if d.controller { "  ← controller" } else { "" });
    }
    println!("MIDI inputs:");
    match midir::MidiInput::new("slipmat-doctor") {
        Ok(m) => {
            let ports = m.ports();
            if ports.is_empty() {
                println!("  (none)");
            }
            for p in &ports {
                println!("  · {}", m.port_name(p).unwrap_or_default());
            }
        }
        Err(e) => println!("  {X} {e}"),
    }
    signed_in().await?;
    Ok(())
}

async fn attrs(q: &str) -> Result<()> {
    let spotify = signed_in().await?;
    let session = spotify.session().await?;
    let uris = spot::search_tracks(&session, q).await?;
    let uri = uris.first().ok_or_else(|| anyhow!("nothing found"))?.clone();
    for kind in [ExtensionKind::AUDIO_ATTRIBUTES, ExtensionKind::TRACK_EXTRA_AUDIO_ATTRIBUTES] {
        let (got, ok) = spot::batch(&session, kind, &[uri.clone()]).await;
        println!("{kind:?}: answered={ok} entries={}", got.len());
        for (u, bytes) in got {
            println!("  {u}: {} bytes {:02x?}", bytes.len(), &bytes[..bytes.len().min(96)]);
        }
    }
    Ok(())
}

async fn load(queries: &str) -> Result<()> {
    let spotify = signed_in().await?;
    let session = spotify.session().await?;
    for q in queries.split('|') {
        load_one(&session, q.trim()).await?;
    }
    Ok(())
}

async fn load_one(session: &librespot_core::Session, q: &str) -> Result<()> {
    let session = session.clone();
    let uris = spot::search_tracks(&session, q).await?;
    let tracks = spot::tracks(&session, &uris[..uris.len().min(10)]).await;
    let track = tracks.into_iter().find(|t| t.playable).ok_or_else(|| anyhow!("no playable song for {q:?}"))?;
    println!("  {} · {} ({:.0} s)", track.name, track.artist_line(), track.duration_ms as f64 / 1000.0);
    let started = Instant::now();
    let audio = loader::load(&session, &track.uri, track.duration_ms)?;
    let mut first_audio = None;
    loop {
        if first_audio.is_none() && audio.len() > SRC_RATE as usize {
            first_audio = Some(started.elapsed());
        }
        if audio.done.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        if audio.failed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(anyhow!("decode failed"));
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err(anyhow!("decode took over two minutes"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let took = started.elapsed();
    // SLIPMAT_DUMP=file.f32 saves 30 s from the one-minute mark (raw f32 stereo) for offline tests.
    if let Some(path) = std::env::var_os("SLIPMAT_DUMP") {
        let len = audio.len();
        let mut bytes = Vec::new();
        for i in (SRC_RATE as usize * 60)..(SRC_RATE as usize * 90).min(len) {
            let (l, r) = audio.frame(i, len);
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        std::fs::write(&path, bytes)?;
    }
    let secs = audio.len() as f64 / SRC_RATE;
    println!(
        "{G} decoded {secs:.1} s of audio in {:.1} s ({:.0}× real time); first second playable after {:.2} s",
        took.as_secs_f64(),
        secs / took.as_secs_f64(),
        first_audio.map_or(0.0, |d| d.as_secs_f64())
    );
    let mut bands: [Vec<u8>; 4] = Default::default();
    for i in 0..audio.wave_len() {
        let c = audio.column(i);
        for (b, v) in [c.low, c.mid, c.high, c.peak].into_iter().enumerate() {
            bands[b].push(v);
        }
    }
    let pct = |v: &mut Vec<u8>, p: f64| {
        v.sort();
        v.get(((v.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0)
    };
    let summary: Vec<String> = ["low", "mid", "high", "peak"].iter().zip(bands.iter_mut()).map(|(n, v)| format!("{n} p50 {} p95 {}", pct(v, 0.5), pct(v, 0.95))).collect();
    println!("{G} waveform: {} columns ({})", audio.wave_len(), summary.join(", "));
    let t = Instant::now();
    let mono = audio.mono();
    let a = tokio::task::spawn_blocking(move || analysis::analyse(&mono)).await?.ok_or_else(|| anyhow!("too short to analyse"))?;
    println!(
        "{G} analysis in {:.2} s: {:.2} BPM (confidence {:.2}), key {} / {}, loudness {:.1} dB, first beat at {:.3} s, music starts {:.3} s",
        t.elapsed().as_secs_f64(),
        a.bpm,
        a.confidence,
        a.key.map(analysis::key_name).unwrap_or("?".into()),
        a.key.map(analysis::camelot).unwrap_or("?".into()),
        a.loudness_db,
        a.anchor / SRC_RATE,
        a.first_sound / SRC_RATE
    );
    let mono = audio.mono();
    let drift = analysis::grid_drift(&mono, &a);
    println!("  kicks vs grid (median ms, start/middle/end): {:+.1} {:+.1} {:+.1}", drift[0], drift[1], drift[2]);
    Ok(())
}

#[cfg(not(unix))]
fn fake_ddj(_name: &str, _script: Option<&str>) -> Result<()> {
    Err(anyhow!("--fake-ddj needs virtual MIDI ports, which only Linux and macOS have"))
}

/// Pretend to be a DDJ-FLX4 (virtual MIDI ports), for testing the mapping
/// against a running Slipmat without the hardware.
#[cfg(unix)]
fn fake_ddj(name: &str, script: Option<&str>) -> Result<()> {
    use midir::os::unix::{VirtualInput, VirtualOutput};
    use std::sync::{Arc, Mutex};
    let lights: Arc<Mutex<Vec<[u8; 3]>>> = Arc::default();
    let seen = lights.clone();
    // A client name the installed Slipmat wouldn't take for a controller, when testing beside it.
    let client = if name.to_lowercase().contains("ddj") { "fake-ddj" } else { "test-rig" };
    let _input = midir::MidiInput::new(client)?
        .create_virtual(name, move |_, m, _| {
            if m.len() == 3 {
                seen.lock().unwrap().push([m[0], m[1], m[2]]);
            }
        }, ())
        .map_err(|e| anyhow!("{e}"))?;
    let mut out = midir::MidiOutput::new(client)?.create_virtual(name).map_err(|e| anyhow!("{e}"))?;
    println!("fake controller “{name}” up; waiting for Slipmat to find it…");
    std::thread::sleep(Duration::from_secs(5));
    let mut send = |m: &[u8]| {
        let _ = out.send(m);
        std::thread::sleep(Duration::from_millis(30));
    };
    // `--midi "90 0B 7F, wait 500, show 90 0B"`: send those messages
    // (hex), pause (milliseconds), and print lights as last set.
    if let Some(script) = script {
        for step in script.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(ms) = step.strip_prefix("wait ") {
                std::thread::sleep(Duration::from_millis(ms.trim().parse().unwrap_or(0)));
            } else if let Some(which) = step.strip_prefix("show ") {
                let b: Vec<u8> = which.split_whitespace().filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
                let got = lights.lock().unwrap();
                let v = got.iter().rev().find(|m| b.len() == 2 && m[0] == b[0] && m[1] == b[1]).map(|m| m[2]);
                println!("light {which}: {}", v.map_or("never set".to_string(), |v| format!("{v:02X}")));
            } else {
                let bytes: Vec<u8> = step.split_whitespace().filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
                send(&bytes);
            }
        }
        println!("{} light messages back from Slipmat", lights.lock().unwrap().len());
        return Ok(());
    }
    // Deck 1: fader full, crossfader centre, low EQ killed, play.
    send(&[0xB0, 0x13, 0x7F]);
    send(&[0xB0, 0x33, 0x7F]);
    send(&[0xB6, 0x1F, 0x40]);
    send(&[0xB6, 0x3F, 0x00]);
    send(&[0xB0, 0x0F, 0x00]);
    send(&[0xB0, 0x2F, 0x00]);
    send(&[0x90, 0x0B, 0x7F]);
    send(&[0x90, 0x0B, 0x00]);
    std::thread::sleep(Duration::from_millis(1500));
    // Hot cue 1 on deck 1, a 4-beat loop from the pads, headphone cue.
    send(&[0x97, 0x00, 0x7F]);
    send(&[0x97, 0x00, 0x00]);
    send(&[0x97, 0x64, 0x7F]);
    send(&[0x97, 0x64, 0x00]);
    send(&[0x90, 0x54, 0x7F]);
    send(&[0x90, 0x54, 0x00]);
    // Scratch: touch, turn back half a turn, let go.
    send(&[0x90, 0x36, 0x7F]);
    for _ in 0..30 {
        send(&[0xB0, 0x22, 0x40 - 12]);
    }
    send(&[0x90, 0x36, 0x00]);
    // Browse knob and load button.
    send(&[0xB6, 0x40, 0x01]);
    send(&[0x96, 0x47, 0x7F]);
    send(&[0x96, 0x47, 0x00]);
    std::thread::sleep(Duration::from_millis(1500));
    let got = lights.lock().unwrap().clone();
    println!("{} light messages back from Slipmat", got.len());
    let find = |s: u8, d: u8| got.iter().rev().find(|m| m[0] == s && m[1] == d).map(|m| m[2]);
    println!("  play deck 1: {:?}  hot cue pad 1: {:?}  4-beat loop pad: {:?}  headphone cue: {:?}  VU deck 1: {:?}",
        find(0x90, 0x0B), find(0x97, 0x00), find(0x97, 0x64), find(0x90, 0x54), find(0xB0, 0x02));
    Ok(())
}

/// Separate a raw f32 stereo file and, if `ref_<stem>.f32` files from the
/// Python reference sit beside it, report how far apart the two are.
async fn stems_file(path: &str, model: crate::stems::Model) -> Result<()> {
    use crate::stems::{Stems, separate};
    if !crate::stems::models_present(model) {
        println!("  downloading the {model:?} model…");
        crate::stems::download_models(model).await?;
    }
    use crate::track::TrackAudio;
    let bytes = std::fs::read(path)?;
    let samples: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let frames = samples.len() / 2;
    let track = TrackAudio::new((frames as f64 / SRC_RATE * 1000.0) as u32);
    track.push(&samples);
    track.done.store(true, std::sync::atomic::Ordering::Release);
    let stems = Stems::for_track(&track, model);
    let t = Instant::now();
    separate(&track, &stems)?;
    println!("{G} {model:?}: separated {:.1} s of audio in {:.2} s", frames as f64 / SRC_RATE, t.elapsed().as_secs_f64());
    let dir = std::path::Path::new(path).parent().unwrap_or(std::path::Path::new("."));
    let dir = if model == crate::stems::Model::Demucs { dir.join("demucs") } else { dir.to_path_buf() };
    for (k, name) in ["vocals", "drums", "bass"].iter().enumerate() {
        let Ok(r) = std::fs::read(dir.join(format!("ref_{name}.f32"))) else { continue };
        let reference: Vec<f32> = r.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in 0..frames.min(reference.len() / 2) {
            let (l, rr) = stems.frame(k, i);
            err += ((l - reference[2 * i]) as f64).powi(2) + ((rr - reference[2 * i + 1]) as f64).powi(2);
            sig += (reference[2 * i] as f64).powi(2) + (reference[2 * i + 1] as f64).powi(2);
        }
        println!("  {name}: differs from the Python reference by {:.1} dB", 10.0 * (err / sig.max(1e-12)).log10());
    }
    Ok(())
}
