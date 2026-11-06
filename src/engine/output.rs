//! The sound card. A Pioneer DDJ is a four-channel sound card: the master
//! goes out on channels 1-2 (the RCA outputs) and the headphones get 3-4.
//! Any other card plays the master; with "split cue" the headphones go to
//! the left ear and the master to the right, for cueing on laptop speakers.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, SampleFormat, SizedSample, StreamConfig};
use serde::{Deserialize, Serialize};

use super::{Engine, MAX_BLOCK};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputConfig {
    /// Device name; empty picks a DDJ if one is plugged in, else the default.
    pub device: String,
    /// Channels (0-based) for the master and for the headphones.
    pub master: [usize; 2],
    pub cue: Option<[usize; 2]>,
    /// Headphones in the left ear, master in the right (mono each).
    pub split_cue: bool,
    pub buffer: u32,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self { device: String::new(), master: [0, 1], cue: Some([2, 3]), split_cue: false, buffer: 256 }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub channels: u16,
    pub controller: bool,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct OutputStatus {
    pub device: String,
    pub channels: u16,
    pub rate: u32,
    pub buffer: u32,
    pub latency_ms: f32,
    pub headphones: bool,
    pub error: Option<String>,
    pub silent: bool,
}

fn is_controller(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("ddj") || n.contains("pioneer") || n.contains("xdj")
}

pub fn devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let Ok(list) = host.output_devices() else { return Vec::new() };
    let mut out: Vec<DeviceInfo> = list
        .filter_map(|d| {
            let name = d.name().ok()?;
            let channels = d.supported_output_configs().ok()?.map(|c| c.channels()).max()?;
            Some(DeviceInfo { controller: is_controller(&name), name, channels })
        })
        .collect();
    out.sort_by_key(|d| !d.controller);
    out
}

/// Which device to try, in order: the one asked for, else a controller's
/// own card (direct hardware first), else the system default.
fn candidates(cfg: &OutputConfig) -> Vec<String> {
    // For testing: run the decks without making a sound.
    if std::env::var_os("SLIPMAT_NO_AUDIO").is_some() {
        return Vec::new();
    }
    if !cfg.device.is_empty() {
        return vec![cfg.device.clone()];
    }
    let mut ddj: Vec<String> = devices().into_iter().filter(|d| d.controller && d.channels >= 4).map(|d| d.name).collect();
    let rank = |n: &str| {
        if n.starts_with("hw:") { 0 } else if n.starts_with("plughw:") { 1 } else if n.starts_with("surround40:") { 2 } else { 3 }
    };
    ddj.sort_by_key(|n| rank(n));
    ddj.push(String::new()); // the default device
    ddj
}

struct Running {
    _stream: cpal::Stream,
}

enum Request {
    Open(OutputConfig, mpsc::Sender<OutputStatus>),
    /// The card went away (unplugged); fall back to whatever is there.
    Lost,
}

/// Owns the sound card stream on a thread of its own and swaps devices on request.
pub struct Output {
    tx: mpsc::Sender<Request>,
    pub status: Arc<Mutex<OutputStatus>>,
}

impl Output {
    pub fn start(engine: Arc<Mutex<Engine>>, cfg: OutputConfig) -> Self {
        let (tx, rx) = mpsc::channel::<Request>();
        let status = Arc::new(Mutex::new(OutputStatus::default()));
        let st = status.clone();
        let lost = tx.clone();
        std::thread::Builder::new()
            .name("slipmat-output".into())
            .spawn(move || {
                let mut running: Option<Running> = None;
                // Bumped on every open; a silent clock runs only while it matches.
                let silent = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let mut current = cfg.clone();
                let open = |cfg: &OutputConfig, running: &mut Option<Running>| -> OutputStatus {
                    *running = None;
                    let generation = silent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let mut last_err = None;
                    for name in candidates(cfg) {
                        match open_stream(&name, cfg, engine.clone(), lost.clone()) {
                            Ok((stream, status)) => {
                                *running = Some(Running { _stream: stream });
                                log::info!("audio out: {} ({} ch, {} Hz, {} frames)", status.device, status.channels, status.rate, status.buffer);
                                return status;
                            }
                            Err(e) => {
                                log::warn!("can't open {:?}: {e}", if name.is_empty() { "default" } else { &name });
                                last_err = Some(e.to_string());
                            }
                        }
                    }
                    // No sound card: keep time anyway so the decks still run.
                    spawn_silent(engine.clone(), silent.clone(), generation);
                    OutputStatus { error: last_err, silent: true, rate: 44_100, ..Default::default() }
                };
                let first = open(&cfg, &mut running);
                *st.lock().unwrap() = first;
                while let Ok(req) = rx.recv() {
                    match req {
                        Request::Open(cfg, reply) => {
                            current = cfg.clone();
                            let s = open(&cfg, &mut running);
                            *st.lock().unwrap() = s.clone();
                            let _ = reply.send(s);
                        }
                        Request::Lost => {
                            std::thread::sleep(Duration::from_millis(300));
                            // Errors come in bursts; one reopen is enough.
                            while rx.try_recv().is_ok() {}
                            let mut fallback = current.clone();
                            fallback.device.clear();
                            let s = open(&fallback, &mut running);
                            *st.lock().unwrap() = s;
                        }
                    }
                }
            })
            .expect("output thread");
        Self { tx, status }
    }

    pub fn status(&self) -> OutputStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn reopen(&self, cfg: OutputConfig) -> Result<OutputStatus> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Request::Open(cfg, tx)).map_err(|_| anyhow!("output thread gone"))?;
        rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the sound card didn't answer"))
    }
}

/// Render in real time with nowhere to play it, so the decks keep moving.
fn spawn_silent(engine: Arc<Mutex<Engine>>, silent: Arc<std::sync::atomic::AtomicU64>, generation: u64) {
    std::thread::spawn(move || {
        let n = 512;
        let mut m = vec![0.0f32; n * 2];
        let mut c = vec![0.0f32; n * 2];
        let _realtime = audio_thread_priority::promote_current_thread_to_real_time(n as u32, 44_100).ok();
        let mut next = Instant::now();
        while silent.load(std::sync::atomic::Ordering::Relaxed) == generation {
            let rate = {
                let mut e = engine.lock().unwrap();
                e.render(&mut m, &mut c);
                e.rate
            };
            next += Duration::from_secs_f64(n as f64 / rate);
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            } else {
                next = Instant::now();
            }
        }
    });
}

fn open_stream(name: &str, cfg: &OutputConfig, engine: Arc<Mutex<Engine>>, lost: mpsc::Sender<Request>) -> Result<(cpal::Stream, OutputStatus)> {
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_output_device().ok_or_else(|| anyhow!("no default sound card"))?
    } else {
        host.output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("{name} isn't connected"))?
    };
    let dev_name = device.name().unwrap_or_else(|_| name.to_string());
    // Only a controller, or a multi-channel card picked by hand, gets a
    // separate headphone pair. The system's mixing devices (PipeWire's
    // "default") would fold channels 3-4 into the speakers, and the cue
    // would leak into the room.
    let mixing = name.is_empty() || ["default", "pipewire", "pulse", "jack", "sysdefault"].iter().any(|v| dev_name.eq_ignore_ascii_case(v));
    let cfg = &if is_controller(&dev_name) || (!cfg.device.is_empty() && !mixing) {
        cfg.clone()
    } else {
        OutputConfig { master: [0, 1], cue: None, ..cfg.clone() }
    };
    // Enough channels for the headphones if the card has them.
    let wanted = cfg.cue.map_or(cfg.master[1], |c| c[1].max(cfg.master[1])) as u16 + 1;
    let configs: Vec<_> = device.supported_output_configs()?.collect();
    let pick = configs
        .iter()
        .filter(|c| c.channels() >= 2)
        .min_by_key(|c| {
            let ch = c.channels();
            let fits = if ch >= wanted { 0 } else { 1 };
            let fmt = match c.sample_format() {
                SampleFormat::F32 => 0,
                SampleFormat::I32 => 1,
                SampleFormat::I16 => 2,
                _ => 3,
            };
            let rate_ok = if c.min_sample_rate().0 <= 44_100 && c.max_sample_rate().0 >= 44_100 { 0 } else { 1 };
            (fits, (ch as i32 - wanted as i32).abs(), rate_ok, fmt)
        })
        .ok_or_else(|| anyhow!("{dev_name} has no usable output format"))?
        .clone();
    let rate = if pick.min_sample_rate().0 <= 44_100 && pick.max_sample_rate().0 >= 44_100 {
        44_100
    } else if pick.min_sample_rate().0 <= 48_000 && pick.max_sample_rate().0 >= 48_000 {
        48_000
    } else {
        pick.max_sample_rate().0
    };
    let channels = pick.channels();
    let format = pick.sample_format();
    let supported = pick.with_sample_rate(cpal::SampleRate(rate));
    let mut config: StreamConfig = supported.config();
    let buffer = cfg.buffer.clamp(64, MAX_BLOCK as u32);
    config.buffer_size = BufferSize::Fixed(buffer);
    engine.lock().unwrap().set_rate(rate as f64);
    let route = Route::new(cfg, channels as usize);
    let headphones = route.cue.is_some() || route.split;
    let stream = match build(&device, &config, format, engine.clone(), route.clone(), lost.clone()) {
        Ok(s) => s,
        Err(_) => {
            // Some cards won't take a fixed buffer size.
            config.buffer_size = BufferSize::Default;
            build(&device, &config, format, engine, route, lost)?
        }
    };
    stream.play()?;
    let frames = match config.buffer_size {
        BufferSize::Fixed(n) => n,
        BufferSize::Default => 1024,
    };
    Ok((
        stream,
        OutputStatus {
            device: dev_name,
            channels,
            rate,
            buffer: frames,
            latency_ms: frames as f32 * 2.0 / rate as f32 * 1000.0,
            headphones,
            error: None,
            silent: false,
        },
    ))
}

#[derive(Clone)]
struct Route {
    master: [usize; 2],
    cue: Option<[usize; 2]>,
    split: bool,
}

impl Route {
    fn new(cfg: &OutputConfig, channels: usize) -> Self {
        let fits = |p: [usize; 2]| p[0] < channels && p[1] < channels;
        let master = if fits(cfg.master) { cfg.master } else { [0, 1] };
        let cue = cfg.cue.filter(|c| fits(*c) && *c != master);
        Self { master, cue, split: cfg.split_cue && cue.is_none() }
    }
}

fn build(device: &cpal::Device, config: &StreamConfig, format: SampleFormat, engine: Arc<Mutex<Engine>>, route: Route, lost: mpsc::Sender<Request>) -> Result<cpal::Stream> {
    match format {
        SampleFormat::F32 => stream::<f32>(device, config, engine, route, lost),
        SampleFormat::I32 => stream::<i32>(device, config, engine, route, lost),
        SampleFormat::I16 => stream::<i16>(device, config, engine, route, lost),
        SampleFormat::U16 => stream::<u16>(device, config, engine, route, lost),
        other => Err(anyhow!("unsupported sample format {other:?}")),
    }
}

fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &StreamConfig,
    engine: Arc<Mutex<Engine>>,
    route: Route,
    lost: mpsc::Sender<Request>,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let mut master = vec![0.0f32; MAX_BLOCK * 2];
    let mut cue = vec![0.0f32; MAX_BLOCK * 2];
    let rate = config.sample_rate.0;
    let mut realtime: Option<Option<audio_thread_priority::RtPriorityHandle>> = None;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            // First time through: ask for real-time priority (kept while the
            // stream lives). Without it a busy computer can starve the music.
            if realtime.is_none() {
                let handle = audio_thread_priority::promote_current_thread_to_real_time(frames as u32, rate);
                match &handle {
                    Ok(_) => log::info!("audio thread: real-time priority"),
                    Err(e) => log::warn!("audio thread: no real-time priority ({e})"),
                }
                realtime = Some(handle.ok());
            }
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(MAX_BLOCK);
                let (m, c) = (&mut master[..n * 2], &mut cue[..n * 2]);
                match engine.lock() {
                    Ok(mut e) => e.render(m, c),
                    Err(_) => {
                        m.fill(0.0);
                        c.fill(0.0);
                    }
                }
                for i in 0..n {
                    let frame = &mut data[(done + i) * channels..(done + i + 1) * channels];
                    for s in frame.iter_mut() {
                        *s = T::from_sample(0.0);
                    }
                    if route.split {
                        frame[0] = T::from_sample((c[2 * i] + c[2 * i + 1]) * 0.5);
                        frame[1] = T::from_sample((m[2 * i] + m[2 * i + 1]) * 0.5);
                    } else {
                        frame[route.master[0]] = T::from_sample(m[2 * i]);
                        frame[route.master[1]] = T::from_sample(m[2 * i + 1]);
                        if let Some(cc) = route.cue {
                            frame[cc[0]] = T::from_sample(c[2 * i]);
                            frame[cc[1]] = T::from_sample(c[2 * i + 1]);
                        }
                    }
                }
                done += n;
            }
        },
        move |e| {
            log::error!("audio stream error: {e}");
            if matches!(e, cpal::StreamError::DeviceNotAvailable) {
                let _ = lost.send(Request::Lost);
            }
        },
        None,
    )?;
    Ok(stream)
}
