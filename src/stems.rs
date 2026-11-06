//! Stems: a song pulled apart into vocals, melody, bass and drums, so a DJ
//! can drop the drums out of one song and bring in the next one's.
//!
//! Done here, on this machine, with Deezer's Spleeter 4-stem model
//! (Apache-2.0, run through ONNX Runtime): the song's spectrum goes through
//! four small networks, one per stem; each guesses how loud its instrument is
//! at every frequency and moment, and those guesses become masks that divide
//! the original sound four ways. The masks always add up to one, so the four
//! stems add back up to the original song exactly.
//!
//! A song is worked through in pieces, in order, so the stems for the start
//! are ready within a second and the rest follow (a whole song takes about
//! ten seconds). Nothing is written to disk but the model files.
//!
//! On a computer with memory to spare (12 GB or more), Meta's Demucs
//! (HT-Demucs, MIT) is used instead: much cleaner stems, at about a gigabyte
//! of memory while it works. On Windows it runs on the graphics card through
//! DirectML when there is one.
//!
//! Decks and the interface order the stems vocals, melody, bass, drums.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

use crate::paths;
use crate::track::{SRC_RATE, TrackAudio, WAVE_HOP};

const N: usize = 4096;
const HOP: usize = 1024;
const PAD: usize = N - HOP;
/// STFT frames per network call (the size the networks were trained on).
const SPLIT: usize = 512;
/// Frequency bins the networks see (the "wide" models: up to ~16 kHz).
const BINS: usize = 1536;
const FULL: usize = N / 2 + 1;
/// A periodic Hann window, squared and overlapped at a quarter hop, sums to this.
const OLA_GAIN: f32 = 1.5;

/// Spleeter's model files, in the networks' own order.
const MODELS: [&str; 4] = ["vocals", "drums", "bass", "other"];
const MODEL_URL: &str = "https://huggingface.co/Best-Practice/spleeter-4stems-onnx/resolve/main";

/// HT-Demucs, exported to ONNX whole (its STFT included): stereo audio in,
/// [drums, bass, other, vocals] out, 7.8 s at a time.
const DEMUCS_FILE: &str = "htdemucs_fp16weights.onnx";
const DEMUCS_URL: &str = "https://huggingface.co/StemSplitio/htdemucs-onnx/resolve/main/htdemucs_fp16weights.onnx";
const DEMUCS_LEN: usize = 343_980;

/// Which separation the user wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Demucs where the computer can take it, Spleeter elsewhere.
    #[default]
    Auto,
    /// Spleeter: about ten seconds a song, 400 MB.
    Fast,
    /// Demucs: cleaner, needs about a gigabyte free.
    Best,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    Spleeter,
    Demucs,
}

/// Total memory in this computer, GB.
pub fn ram_gb() -> f64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() as f64 / 1e9
}

impl Model {
    pub fn choose(q: Quality) -> Self {
        match q {
            Quality::Fast => Model::Spleeter,
            Quality::Best => Model::Demucs,
            // 8 GB machines (like the laptop this was built on) run out of
            // memory with Demucs; 16 GB ones manage easily.
            Quality::Auto => {
                if ram_gb() >= 12.0 {
                    Model::Demucs
                } else {
                    Model::Spleeter
                }
            }
        }
    }

    fn files(self) -> Vec<(PathBuf, String)> {
        match self {
            Model::Spleeter => MODELS.iter().map(|m| (model_path(m), format!("{MODEL_URL}/{m}.wide.fp16.onnx"))).collect(),
            Model::Demucs => vec![(model_dir().join(DEMUCS_FILE), DEMUCS_URL.to_string())],
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Waiting = 0,
    Downloading = 1,
    Separating = 2,
    Done = 3,
    Failed = 4,
    Off = 5,
}

impl Status {
    fn from(v: u8) -> Self {
        match v {
            1 => Status::Downloading,
            2 => Status::Separating,
            3 => Status::Done,
            4 => Status::Failed,
            5 => Status::Off,
            _ => Status::Waiting,
        }
    }
}

/// The separated stems of one song. Vocals, drums and bass are kept; the
/// melody is whatever is left (the song minus those three), which saves a
/// quarter of the memory and is exact because the stems sum to the song.
pub struct Stems {
    /// vocals, drums, bass: interleaved stereo, 16-bit.
    data: [Box<[AtomicI16]>; 3],
    cap: usize,
    /// Frames from the start that are separated.
    ready: AtomicUsize,
    status: AtomicU8,
    /// Set when the song leaves the deck: stop working on it.
    pub cancelled: AtomicBool,
    pub model: Model,
    /// Waveform per stem (vocals, melody, bass, drums: a byte each per
    /// column of `WAVE_HOP` frames), filled in as separation goes.
    wave: Box<[std::sync::atomic::AtomicU32]>,
    wave_len: AtomicUsize,
}

fn atomics(n: usize) -> Box<[AtomicI16]> {
    let mut v = Vec::with_capacity(n);
    v.resize_with(n, || AtomicI16::new(0));
    v.into_boxed_slice()
}

impl Stems {
    pub fn for_track(track: &TrackAudio, model: Model) -> Self {
        let cap = track.expected + SRC_RATE as usize * 4;
        Self {
            data: [atomics(cap * 2), atomics(cap * 2), atomics(cap * 2)],
            cap,
            ready: AtomicUsize::new(0),
            status: AtomicU8::new(Status::Waiting as u8),
            cancelled: AtomicBool::new(false),
            model,
            wave: {
                let mut v = Vec::with_capacity(cap / WAVE_HOP + 1);
                v.resize_with(cap / WAVE_HOP + 1, || std::sync::atomic::AtomicU32::new(0));
                v.into_boxed_slice()
            },
            wave_len: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn ready(&self) -> usize {
        self.ready.load(Ordering::Acquire)
    }

    pub fn status(&self) -> Status {
        Status::from(self.status.load(Ordering::Relaxed))
    }

    fn set_status(&self, s: Status) {
        self.status.store(s as u8, Ordering::Relaxed);
    }

    /// One frame of a kept stem (0 vocals, 1 drums, 2 bass).
    #[inline]
    pub fn frame(&self, stem: usize, i: usize) -> (f32, f32) {
        const S: f32 = 1.0 / 32768.0;
        let d = &self.data[stem];
        (d[2 * i].load(Ordering::Relaxed) as f32 * S, d[2 * i + 1].load(Ordering::Relaxed) as f32 * S)
    }

    pub fn wave_len(&self) -> usize {
        self.wave_len.load(Ordering::Acquire)
    }

    /// The stem waveform as bytes (vocals, melody, bass, drums per column), from `from`.
    pub fn wave_bytes(&self, from: usize) -> Vec<u8> {
        let n = self.wave_len();
        let mut out = Vec::with_capacity(n.saturating_sub(from) * 4);
        for i in from.min(n)..n {
            out.extend_from_slice(&self.wave[i].load(Ordering::Relaxed).to_le_bytes());
        }
        out
    }

    /// Add waveform columns for everything separated since last time.
    fn update_wave(&self, track: &TrackAudio) {
        let ready = self.ready();
        let len = track.len();
        let mut col = self.wave_len();
        while (col + 1) * WAVE_HOP <= ready && col < self.wave.len() {
            let mut sq = [0.0f32; 4];
            for i in col * WAVE_HOP..(col + 1) * WAVE_HOP {
                let (ml, mr) = track.frame(i, len);
                let v = self.frame(0, i);
                let d = self.frame(1, i);
                let b = self.frame(2, i);
                let mono = |x: (f32, f32)| (x.0 + x.1) * 0.5;
                let (vv, dd, bb) = (mono(v), mono(d), mono(b));
                let mel = (ml + mr) * 0.5 - vv - dd - bb;
                sq[0] += vv * vv;
                sq[1] += mel * mel;
                sq[2] += bb * bb;
                sq[3] += dd * dd;
            }
            let byte = |s: f32| (((s / WAVE_HOP as f32).sqrt() * 2.2).min(1.0).powf(0.8) * 255.0) as u8;
            let packed = u32::from_le_bytes([byte(sq[0]), byte(sq[1]), byte(sq[2]), byte(sq[3])]);
            self.wave[col].store(packed, Ordering::Relaxed);
            col += 1;
            self.wave_len.store(col, Ordering::Release);
        }
    }

    fn write(&self, stem: usize, c: usize, i: usize, v: f32) {
        if i < self.cap {
            let s = (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
            self.data[stem][2 * i + c].store(s, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
impl Stems {
    /// Stems with known contents, for tests: `parts[stem](frame) -> (l, r)`.
    pub fn filled(track: &TrackAudio, parts: [&dyn Fn(usize) -> (f32, f32); 3]) -> Self {
        let s = Self::for_track(track, Model::Spleeter);
        for i in 0..track.len() {
            for (k, f) in parts.iter().enumerate() {
                let (l, r) = f(i);
                s.write(k, 0, i, l);
                s.write(k, 1, i, r);
            }
        }
        s.ready.store(track.len(), Ordering::Release);
        s.set_status(Status::Done);
        s
    }
}

// ---- models -----------------------------------------------------------------------

fn model_dir() -> PathBuf {
    paths::cache().join("models")
}

fn model_path(name: &str) -> PathBuf {
    model_dir().join(format!("spleeter-{name}.wide.fp16.onnx"))
}

pub fn models_present(model: Model) -> bool {
    model.files().iter().all(|(p, _)| p.exists())
}

/// Fetch a model the first time it is used (Spleeter about 80 MB, Demucs 166 MB).
pub async fn download_models(model: Model) -> Result<()> {
    std::fs::create_dir_all(model_dir())?;
    let client = reqwest::Client::new();
    for (path, url) in model.files() {
        if path.exists() {
            continue;
        }
        let resp = client.get(&url).timeout(Duration::from_secs(900)).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("couldn't download {} ({})", url, resp.status()));
        }
        let bytes = resp.bytes().await?;
        if bytes.len() < 1_000_000 {
            return Err(anyhow!("{} came back too small", url));
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        log::info!("downloaded {} ({} MB)", path.display(), bytes.len() / 1_000_000);
    }
    Ok(())
}

struct Nets {
    model: Model,
    sessions: Vec<ort::session::Session>,
    last_used: Instant,
}

fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("stem model: {e}")
}

fn session(path: PathBuf, threads: usize, gpu: bool, optimise: bool) -> Result<ort::session::Session> {
    // No memory arena: an arena keeps its high-water mark for good, and
    // would hold most of a gigabyte between songs.
    let mut providers = Vec::new();
    #[cfg(windows)]
    if gpu {
        // The graphics card, through DirectML; ONNX Runtime falls back to the
        // processor if there isn't a usable one.
        providers.push(ort::ep::DirectML::default().build());
    }
    let _ = gpu;
    providers.push(ort::ep::CPU::default().with_arena_allocator(false).build());
    // Demucs is loaded without graph optimisation: optimising it briefly
    // takes over 2 GB, unoptimised it peaks under 1 GB and runs about as fast.
    let level = if optimise {
        ort::session::builder::GraphOptimizationLevel::Level3
    } else {
        ort::session::builder::GraphOptimizationLevel::Disable
    };
    ort::session::Session::builder()
        .map_err(ort_err)?
        .with_optimization_level(level)
        .map_err(ort_err)?
        .with_execution_providers(providers)
        .map_err(ort_err)?
        .with_memory_pattern(false)
        .map_err(ort_err)?
        .with_intra_threads(threads)
        .map_err(ort_err)?
        .commit_from_file(path)
        .map_err(ort_err)
}

fn load_nets(model: Model) -> Result<Nets> {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let sessions = match model {
        Model::Spleeter => MODELS.iter().map(|m| session(model_path(m), 3, false, true)).collect::<Result<Vec<_>>>()?,
        // Leave a couple of cores for the decks and the interface.
        Model::Demucs => vec![session(model_dir().join(DEMUCS_FILE), cores.saturating_sub(2).max(2), true, false)?],
    };
    log::info!("stem model {model:?} loaded");
    Ok(Nets { model, sessions, last_used: Instant::now() })
}

/// The loaded networks for `model`, loading (or swapping) them if needed.
fn nets_for(guard: &mut Option<Nets>, model: Model) -> Result<&mut Nets> {
    if guard.as_ref().is_none_or(|n| n.model != model) {
        *guard = None; // let the other model go first
        *guard = Some(load_nets(model)?);
    }
    Ok(guard.as_mut().expect("loaded above"))
}

/// The networks stay loaded between songs (about 200 MB), and are let go
/// after a few idle minutes: this machine has little memory to spare.
static NETS: Mutex<Option<Nets>> = Mutex::new(None);

pub fn release_idle() {
    let mut nets = NETS.lock().unwrap();
    if nets.as_ref().is_some_and(|n| n.last_used.elapsed() > Duration::from_secs(180)) {
        *nets = None;
        log::info!("stem models unloaded (idle)");
    }
}

// ---- separation ---------------------------------------------------------------------

/// Separate `track` into `stems`, waiting for it to decode as needed.
/// Blocking: run it on a thread of its own.
pub fn separate(track: &TrackAudio, stems: &Stems) -> Result<()> {
    crate::priority::background();
    let started = Instant::now();
    stems.set_status(Status::Separating);
    let finished = match stems.model {
        Model::Spleeter => spleeter(track, stems)?,
        Model::Demucs => demucs(track, stems)?,
    };
    if finished {
        stems.ready.store(track.len(), Ordering::Release);
        stems.update_wave(track);
        stems.set_status(Status::Done);
        log::info!(
            "stems ({:?}): {:.0} s of song separated in {:.1} s",
            stems.model,
            track.len() as f64 / SRC_RATE,
            started.elapsed().as_secs_f64()
        );
    } else {
        stems.set_status(Status::Off);
    }
    Ok(())
}

/// Wait until `track` has decoded `need` frames (or finished). False if the
/// song failed or left the deck meanwhile. The model lock is let go while waiting.
fn wait_for<'a>(
    track: &TrackAudio,
    stems: &Stems,
    need: usize,
    mut guard: std::sync::MutexGuard<'a, Option<Nets>>,
) -> Option<std::sync::MutexGuard<'a, Option<Nets>>> {
    loop {
        if stems.cancelled.load(Ordering::Relaxed) || track.failed.load(Ordering::Acquire) {
            return None;
        }
        if track.done.load(Ordering::Acquire) || track.len() >= need {
            return Some(guard);
        }
        drop(guard);
        std::thread::sleep(Duration::from_millis(50));
        guard = NETS.lock().unwrap();
    }
}

/// Demucs: 7.8-second pieces overlapping by a quarter, crossfaded.
fn demucs(track: &TrackAudio, stems: &Stems) -> Result<bool> {
    let n = DEMUCS_LEN;
    let overlap = n / 4;
    let stride = n - overlap;
    let window: Vec<f32> = (0..n)
        .map(|i| {
            let fade = |k: usize| k as f32 / (overlap - 1) as f32;
            if i < overlap { fade(i) } else if i >= n - overlap { fade(n - 1 - i) } else { 1.0 }
        })
        .collect();
    // Kept stems in Demucs's own order: vocals 3, drums 0, bass 1.
    const KEEP: [usize; 3] = [3, 0, 1];
    let mut acc = vec![0.0f32; 3 * 2 * n];
    let mut weight = vec![0.0f32; n];
    let mut input = vec![0.0f32; 2 * n];
    let mut guard = NETS.lock().unwrap();
    let mut start = 0usize;
    loop {
        guard = match wait_for(track, stems, start + n, guard) {
            Some(g) => g,
            None => return Ok(false),
        };
        let done = track.done.load(Ordering::Acquire);
        let len = track.len();
        if done && start >= len && start > 0 {
            break;
        }
        let end = if done { (start + n).min(len) } else { start + n };
        let clen = end - start;
        for k in 0..n {
            let (l, r) = if k < clen { track.frame(start + k, len) } else { (0.0, 0.0) };
            input[k] = l;
            input[n + k] = r;
        }
        let nets = nets_for(&mut guard, Model::Demucs)?;
        let tensor = ort::value::Tensor::from_array(([1usize, 2, n], input.clone())).map_err(ort_err)?;
        let out = nets.sessions[0].run(ort::inputs!["mix" => tensor]).map_err(ort_err)?;
        let (_, y) = out["stems"].try_extract_tensor::<f32>().map_err(ort_err)?;
        if y.len() != 4 * 2 * n {
            return Err(anyhow!("Demucs answered with {} values, expected {}", y.len(), 8 * n));
        }
        for (slot, &src) in KEEP.iter().enumerate() {
            for c in 0..2 {
                let from = &y[(src * 2 + c) * n..(src * 2 + c) * n + clen];
                let to = &mut acc[(slot * 2 + c) * n..(slot * 2 + c) * n + clen];
                for k in 0..clen {
                    to[k] += from[k] * window[k];
                }
            }
        }
        for k in 0..clen {
            weight[k] += window[k];
        }
        drop(out);
        nets.last_used = Instant::now();
        // Everything before the next piece starts is final.
        let last = done && end >= len;
        let fin = if last { clen } else { stride };
        for slot in 0..3 {
            for c in 0..2 {
                let a = &acc[(slot * 2 + c) * n..(slot * 2 + c + 1) * n];
                for k in 0..fin {
                    stems.write(slot, c, start + k, a[k] / weight[k].max(1e-8));
                }
            }
        }
        stems.ready.store(start + fin, Ordering::Release);
        stems.update_wave(track);
        if last {
            break;
        }
        for slot in 0..6 {
            let a = &mut acc[slot * n..(slot + 1) * n];
            a.copy_within(stride.., 0);
            a[overlap..].fill(0.0);
        }
        weight.copy_within(stride.., 0);
        weight[overlap..].fill(0.0);
        start += stride;
    }
    Ok(true)
}

/// Spleeter: the song's spectrum in 512-frame (12 s) pieces, four networks, masks.
fn spleeter(track: &TrackAudio, stems: &Stems) -> Result<bool> {
    let mut guard = NETS.lock().unwrap();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(N);
    let inv = planner.plan_fft_inverse(N);
    let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();

    // Overlap-add state for the three kept stems, two channels each: the
    // current piece plus the tail that spills into the next one.
    let span = SPLIT * HOP;
    let mut acc = vec![vec![0.0f32; span + PAD]; 6];
    let mut spec = vec![Complex32::new(0.0, 0.0); 2 * SPLIT * FULL];
    let mut input = vec![0.0f32; 2 * SPLIT * BINS];
    let mut buf = vec![Complex32::new(0.0, 0.0); N];
    let mut split = 0usize;
    loop {
        // Wait until the audio this piece reads has decoded.
        let first = split * SPLIT;
        let need = ((first + SPLIT) * HOP + N).saturating_sub(PAD);
        guard = match wait_for(track, stems, need, guard) {
            Some(g) => g,
            None => return Ok(false),
        };
        let nets = nets_for(&mut guard, Model::Spleeter)?;
        let done = track.done.load(Ordering::Acquire);
        let len = track.len();
        let frames_total = (PAD + len).div_ceil(HOP);
        if done && first >= frames_total {
            break;
        }
        let frames = SPLIT.min(frames_total.saturating_sub(first).max(if done { 0 } else { SPLIT }));

        // Spectra of this piece, both channels.
        input.fill(0.0);
        for c in 0..2 {
            for j in 0..SPLIT {
                let row = &mut spec[(c * SPLIT + j) * FULL..(c * SPLIT + j + 1) * FULL];
                if j >= frames {
                    row.fill(Complex32::new(0.0, 0.0));
                    continue;
                }
                let start = ((first + j) * HOP) as i64 - PAD as i64;
                for (k, b) in buf.iter_mut().enumerate() {
                    let i = start + k as i64;
                    let v = if i >= 0 && (i as usize) < len {
                        let (l, r) = track.frame(i as usize, len);
                        if c == 0 { l } else { r }
                    } else {
                        0.0
                    };
                    *b = Complex32::new(v * window[k], 0.0);
                }
                fwd.process(&mut buf);
                row.copy_from_slice(&buf[..FULL]);
                let at = (c * SPLIT + j) * BINS;
                for k in 0..BINS {
                    input[at + k] = buf[k].norm();
                }
            }
        }

        // Each network's estimate of its stem's magnitudes.
        let mut est: Vec<Vec<f32>> = Vec::with_capacity(4);
        for s in nets.sessions.iter_mut() {
            let tensor = ort::value::Tensor::from_array(([2usize, 1, SPLIT, BINS], input.clone())).map_err(ort_err)?;
            let out = s.run(ort::inputs!["x" => tensor]).map_err(ort_err)?;
            let (_, y) = out["y"].try_extract_tensor::<f32>().map_err(ort_err)?;
            if y.len() != input.len() {
                return Err(anyhow!("stem model answered with {} values, expected {}", y.len(), input.len()));
            }
            est.push(y.to_vec());
        }
        nets.last_used = Instant::now();

        // Masks, applied to the original spectrum; back to sound by overlap-add.
        let mut mask = vec![0.0f32; FULL];
        let mut total = vec![0.0f32; BINS];
        for c in 0..2 {
            for j in 0..frames {
                let at = (c * SPLIT + j) * BINS;
                let row = &spec[(c * SPLIT + j) * FULL..(c * SPLIT + j + 1) * FULL];
                for k in 0..BINS {
                    total[k] = est.iter().map(|x| x[at + k] * x[at + k]).sum::<f32>() + 1e-10;
                }
                // Kept stems: networks 0 (vocals), 1 (drums), 2 (bass).
                for (stem, e) in est.iter().enumerate().take(3) {
                    let mut sum = 0.0f32;
                    for k in 0..BINS {
                        let m = (e[at + k] * e[at + k] + 1e-10 / 4.0) / total[k];
                        mask[k] = m;
                        sum += m;
                    }
                    // Above the networks' range, each stem keeps its average share.
                    let mean = sum / BINS as f32;
                    mask[BINS..].fill(mean);
                    for k in 0..FULL {
                        buf[k] = row[k] * mask[k];
                    }
                    for k in 1..N / 2 {
                        buf[N - k] = buf[k].conj();
                    }
                    inv.process(&mut buf);
                    let a = &mut acc[stem * 2 + c];
                    let off = j * HOP;
                    for k in 0..N {
                        a[off + k] += buf[k].re / N as f32 * window[k];
                    }
                }
            }
        }

        // Everything before the next piece's first window is final.
        let base = (first * HOP) as i64 - PAD as i64;
        for stem in 0..3 {
            for c in 0..2 {
                let a = &mut acc[stem * 2 + c];
                for (k, v) in a[..span].iter().enumerate() {
                    let i = base + k as i64;
                    if i >= 0 && (i as usize) < len {
                        stems.write(stem, c, i as usize, *v / OLA_GAIN);
                    }
                }
                a.copy_within(span.., 0);
                a[PAD..].fill(0.0);
            }
        }
        let ready = ((base + span as i64).max(0) as usize).min(len);
        stems.ready.store(ready, Ordering::Release);
        stems.update_wave(track);
        split += 1;
        if done && (first + SPLIT) >= frames_total {
            break;
        }
    }
    Ok(true)
}

/// Separate in the background, after making sure the models are here.
pub fn spawn(track: Arc<TrackAudio>, stems: Arc<Stems>) {
    tokio::spawn(async move {
        if !models_present(stems.model) {
            stems.set_status(Status::Downloading);
            if let Err(e) = download_models(stems.model).await {
                log::warn!("stem models: {e}");
                stems.set_status(Status::Failed);
                return;
            }
        }
        let s = stems.clone();
        let result = tokio::task::spawn_blocking(move || separate(&track, &s)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                log::warn!("stems failed: {e}");
                stems.set_status(Status::Failed);
            }
            Err(e) => {
                log::warn!("stems task: {e}");
                stems.set_status(Status::Failed);
            }
        }
    });
}
