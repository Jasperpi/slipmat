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

pub fn is_controller(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("ddj") || n.contains("pioneer") || n.contains("xdj") || n.contains("alphatheta")
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

/// Names of the system's own mixing devices. They fold extra channels into
/// the speakers, so the headphones never get a pair of their own there.
const MIXERS: [&str; 5] = ["default", "pipewire", "pulse", "jack", "sysdefault"];

/// One way of reaching a sound card.
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    /// Device name; empty for the system default.
    name: String,
    /// The PipeWire sink to aim at, when going through PipeWire (Linux).
    node: Option<String>,
    /// What to call it on screen, if the device name won't do.
    label: Option<String>,
    /// It has channels of its own for the headphones.
    cue: bool,
}

/// PipeWire usually holds the controller's sound card, so the surest way to
/// all four of its channels is through PipeWire, aimed at that card.
#[cfg(target_os = "linux")]
pub mod pipewire {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    pub struct Sink {
        pub name: String,
        pub description: String,
    }

    /// `pw-dump`'s answer, or nothing if PipeWire isn't there or is slow.
    fn dump() -> Option<serde_json::Value> {
        let mut child = Command::new("pw-dump").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut pipe = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = tx.send(bytes);
        });
        let bytes = rx.recv_timeout(Duration::from_secs(2)).ok();
        let _ = child.kill();
        let _ = child.wait();
        serde_json::from_slice(&bytes?).ok()
    }

    /// The four-channel sink PipeWire made for a DJ controller, if any:
    /// channels 1-2 are its master output, 3-4 its headphone jack.
    pub fn controller_sink() -> Option<Sink> {
        pick(&dump()?)
    }

    pub fn pick(dump: &serde_json::Value) -> Option<Sink> {
        dump.as_array()?.iter().find_map(|o| {
            let p = o.get("info")?.get("props")?;
            if p.get("media.class")?.as_str()? != "Audio/Sink" {
                return None;
            }
            let name = p.get("node.name")?.as_str()?;
            let text = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let description = if text("node.description").is_empty() { name } else { text("node.description") };
            if ![name, description, text("api.alsa.card.name"), text("device.description")].iter().any(|t| super::is_controller(t)) {
                return None;
            }
            // Front pair then rear pair (the "surround 4.0" profile), or the
            // plain numbered channels of the "pro audio" profile.
            let position = text("audio.position");
            let places: Vec<&str> = position.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).collect();
            let four = p.get("audio.channels").and_then(|v| v.as_u64()).unwrap_or(places.len() as u64) >= 4;
            let layout = places.starts_with(&["FL", "FR", "RL", "RR"]) || (places.len() >= 4 && places.iter().all(|s| s.starts_with("AUX")));
            (four && layout).then(|| Sink { name: name.to_string(), description: description.to_string() })
        })
    }

    /// Point the next stream opened through PipeWire's ALSA device at `node`
    /// (or, with nothing, at wherever the system sends sound). The variable
    /// is created in `prepare` before any thread starts, so this only ever
    /// swaps its value.
    pub fn aim(node: Option<&str>) {
        // SAFETY: the variable already exists (see `prepare`), so the C
        // library replaces one pointer and never moves the environment.
        unsafe { std::env::set_var("PIPEWIRE_NODE", node.unwrap_or("")) }
    }

    /// Call once at start-up, before any thread exists.
    pub fn prepare() {
        if std::env::var_os("PIPEWIRE_NODE").is_none() {
            // SAFETY: nothing else is running yet.
            unsafe { std::env::set_var("PIPEWIRE_NODE", "") }
        }
    }
}

/// Which devices to try, best first: the one asked for; else the
/// controller's own card (through PipeWire where it runs the card, then
/// straight to the hardware); else the system default.
fn candidates(cfg: &OutputConfig) -> Vec<Candidate> {
    // For testing: run the decks without making a sound.
    if std::env::var_os("SLIPMAT_NO_AUDIO").is_some() {
        return Vec::new();
    }
    if !cfg.device.is_empty() {
        let mixer = MIXERS.iter().any(|m| cfg.device.eq_ignore_ascii_case(m));
        return vec![Candidate { name: cfg.device.clone(), node: None, label: None, cue: !mixer }];
    }
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    if let Some(sink) = pipewire::controller_sink() {
        out.push(Candidate { name: String::new(), node: Some(sink.name), label: Some(sink.description), cue: true });
    }
    let mut ddj: Vec<String> = devices().into_iter().filter(|d| d.controller && d.channels >= 4).map(|d| d.name).collect();
    let rank = |n: &str| {
        if n.starts_with("hw:") { 0 } else if n.starts_with("plughw:") { 1 } else if n.starts_with("surround40:") { 2 } else { 3 }
    };
    ddj.sort_by_key(|n| rank(n));
    out.extend(ddj.into_iter().map(|name| Candidate { name, node: None, label: None, cue: true }));
    out.push(Candidate { name: String::new(), node: None, label: None, cue: false });
    out
}

enum Request {
    Open(OutputConfig, mpsc::Sender<OutputStatus>),
    /// The card went away (unplugged); fall back to whatever is there.
    Lost,
    /// Something was plugged in or pulled out: move if there's a better
    /// card now (or, forced, open afresh whatever happens).
    Refresh { force: bool, reply: Option<mpsc::Sender<OutputStatus>> },
}

/// The open stream, and how it came to be the one in use.
struct Opener {
    engine: Arc<Mutex<Engine>>,
    lost: mpsc::Sender<Request>,
    stream: Option<cpal::Stream>,
    /// Bumped on every open; a silent clock runs only while it matches.
    silent: Arc<std::sync::atomic::AtomicU64>,
    opened: Option<Candidate>,
    /// Cards that wouldn't open last time, so a refresh doesn't keep
    /// interrupting the music to try them again.
    failed: Vec<Candidate>,
}

impl Opener {
    fn open(&mut self, cfg: &OutputConfig) -> OutputStatus {
        self.stream = None;
        self.opened = None;
        self.failed.clear();
        let generation = self.silent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let mut last_err = None;
        for cand in candidates(cfg) {
            match open_stream(&cand, cfg, self.engine.clone(), self.lost.clone()) {
                Ok((stream, status)) => {
                    self.stream = Some(stream);
                    log::info!(
                        "audio out: {} ({} ch, {} Hz, {} frames, headphones {})",
                        status.device, status.channels, status.rate, status.buffer, if status.headphones { "on their own channels" } else { "not routed" }
                    );
                    self.opened = Some(cand);
                    return status;
                }
                Err(e) => {
                    log::warn!("can't open {:?}: {e}", cand.label.as_deref().unwrap_or(if cand.name.is_empty() { "default" } else { &cand.name }));
                    last_err = Some(e.to_string());
                    self.failed.push(cand);
                }
            }
        }
        // No sound card: keep time anyway so the decks still run.
        spawn_silent(self.engine.clone(), self.silent.clone(), generation);
        OutputStatus { error: last_err, silent: true, rate: 44_100, ..Default::default() }
    }

    /// Whether the best card available now is other than the one in use.
    fn stale(&self, cfg: &OutputConfig) -> bool {
        let want = candidates(cfg);
        want.iter().find(|c| !self.failed.contains(c)) != self.opened.as_ref()
    }
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
                let mut opener = Opener { engine, lost, stream: None, silent: Arc::default(), opened: None, failed: Vec::new() };
                let mut current = cfg.clone();
                let first = opener.open(&cfg);
                *st.lock().unwrap() = first;
                while let Ok(req) = rx.recv() {
                    match req {
                        Request::Open(cfg, reply) => {
                            current = cfg.clone();
                            let s = opener.open(&cfg);
                            *st.lock().unwrap() = s.clone();
                            let _ = reply.send(s);
                        }
                        Request::Lost => {
                            std::thread::sleep(Duration::from_millis(300));
                            // Errors come in bursts; one reopen is enough.
                            while let Ok(req) = rx.try_recv() {
                                if let Request::Open(cfg, _) = req {
                                    current = cfg;
                                }
                            }
                            // A card picked by hand that has gone: use what's there.
                            let mut fallback = current.clone();
                            fallback.device.clear();
                            let s = opener.open(&fallback);
                            *st.lock().unwrap() = s;
                        }
                        Request::Refresh { force, reply } => {
                            if force || opener.stale(&current) {
                                let s = opener.open(&current);
                                *st.lock().unwrap() = s;
                            }
                            if let Some(reply) = reply {
                                let _ = reply.send(st.lock().unwrap().clone());
                            }
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

    /// A controller came or went: move to the best card there is now. The
    /// music is only interrupted if the card actually changes.
    pub fn refresh(&self) {
        let _ = self.tx.send(Request::Refresh { force: false, reply: None });
    }

    /// Open the sound card afresh (the "reconnect" button).
    pub fn reconnect(&self) -> Result<OutputStatus> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Request::Refresh { force: true, reply: Some(tx) }).map_err(|_| anyhow!("output thread gone"))?;
        rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the sound card didn't answer"))
    }
}

/// Render in real time with nowhere to play it, so the decks keep moving.
fn spawn_silent(engine: Arc<Mutex<Engine>>, silent: Arc<std::sync::atomic::AtomicU64>, generation: u64) {
    std::thread::spawn(move || {
        let n = 512;
        let mut m = vec![0.0f32; n * 2];
        let mut c = vec![0.0f32; n * 2];
        if let Ok(handle) = audio_thread_priority::promote_current_thread_to_real_time(n as u32, 44_100) {
            std::mem::forget(handle); // for the life of this thread
        }
        let mut next = Instant::now();
        while silent.load(std::sync::atomic::Ordering::Relaxed) == generation {
            let rate = {
                let mut e = engine.lock().unwrap_or_else(|e| e.into_inner());
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

fn open_stream(cand: &Candidate, cfg: &OutputConfig, engine: Arc<Mutex<Engine>>, lost: mpsc::Sender<Request>) -> Result<(cpal::Stream, OutputStatus)> {
    #[cfg(target_os = "linux")]
    pipewire::aim(cand.node.as_deref());
    let opened = open_candidate(cand, cfg, engine, lost);
    #[cfg(target_os = "linux")]
    pipewire::aim(None);
    opened
}

fn open_candidate(cand: &Candidate, cfg: &OutputConfig, engine: Arc<Mutex<Engine>>, lost: mpsc::Sender<Request>) -> Result<(cpal::Stream, OutputStatus)> {
    let name = cand.name.as_str();
    let host = cpal::default_host();
    let device = if name.is_empty() {
        host.default_output_device().ok_or_else(|| anyhow!("no default sound card"))?
    } else {
        host.output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("{name} isn't connected"))?
    };
    let dev_name = device.name().unwrap_or_else(|_| name.to_string());
    // Only a card with channels of its own for them gets the headphones.
    // The system's mixing devices (PipeWire's "default") would fold channels
    // 3-4 into the speakers, and the cue would leak into the room.
    let cfg = &if cand.cue || (cand.name.is_empty() && cand.node.is_none() && is_controller(&dev_name)) {
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
    engine.lock().unwrap_or_else(|e| e.into_inner()).set_rate(rate as f64);
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
            device: cand.label.clone().unwrap_or(dev_name),
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
    let mut realtime = false;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            // First time through: ask for real-time priority (kept while the
            // stream lives). Without it a busy computer can starve the music.
            if !realtime {
                realtime = true;
                match audio_thread_priority::promote_current_thread_to_real_time(frames as u32, rate) {
                    // Kept for the life of this thread (it ends with the
                    // stream); the handle itself may not cross threads.
                    Ok(handle) => {
                        std::mem::forget(handle);
                        log::info!("audio thread: real-time priority");
                    }
                    Err(e) => log::warn!("audio thread: no real-time priority ({e})"),
                }
            }
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(MAX_BLOCK);
                let (m, c) = (&mut master[..n * 2], &mut cue[..n * 2]);
                // A panic elsewhere while holding the engine must not
                // silence the room: carry on with the engine as it is.
                engine.lock().unwrap_or_else(|e| e.into_inner()).render(m, c);
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::pipewire::pick;

    fn sink(name: &str, description: &str, channels: u64, position: &str) -> serde_json::Value {
        serde_json::json!({ "id": 1, "info": { "props": {
            "media.class": "Audio/Sink", "node.name": name, "node.description": description,
            "audio.channels": channels, "audio.position": position,
        }}})
    }

    #[test]
    fn finds_the_controllers_four_channel_sink() {
        let laptop = sink("alsa_output.platform-sound.pro-output-0", "Built-in Audio Pro", 2, "[ AUX0, AUX1 ]");
        let ddj = sink("alsa_output.usb-AlphaTheta_Corporation_DDJ-FLX4_X-00.analog-surround-40", "DDJ-FLX4 Analog Surround 4.0", 4, "[ FL, FR, RL, RR ]");
        let dump = serde_json::json!([laptop.clone(), { "id": 9, "info": { "props": { "media.class": "Audio/Source" } } }, ddj]);
        let found = pick(&dump).expect("the DDJ");
        assert!(found.name.contains("DDJ-FLX4") && found.description.contains("Surround"));
        // The pro-audio profile's numbered channels are fine too.
        assert!(pick(&serde_json::json!([sink("alsa_output.usb-Pioneer_DDJ-400.pro-output-0", "DDJ-400 Pro", 4, "[ AUX0, AUX1, AUX2, AUX3 ]")])).is_some());
        // In stereo there is no headphone pair: not this way.
        assert!(pick(&serde_json::json!([sink("alsa_output.usb-AlphaTheta_Corporation_DDJ-FLX4_X-00.analog-stereo", "DDJ-FLX4 Analog Stereo", 2, "[ FL, FR ]")])).is_none());
        // Four channels on something that isn't a controller: no.
        assert!(pick(&serde_json::json!([sink("alsa_output.platform-sound.pro-output-1", "Built-in Audio Pro 1", 4, "[ AUX0, AUX1, AUX2, AUX3 ]"), laptop])).is_none());
    }
}
