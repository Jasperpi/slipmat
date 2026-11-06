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
//! A song is worked through in 12-second pieces, in order, so the stems for
//! the start are ready within a second and the rest follow (a whole song takes
//! about ten seconds). Nothing is written to disk but the model files.
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
use crate::track::{SRC_RATE, TrackAudio};

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

/// The model files, in the networks' own order.
const MODELS: [&str; 4] = ["vocals", "drums", "bass", "other"];
const MODEL_URL: &str = "https://huggingface.co/Best-Practice/spleeter-4stems-onnx/resolve/main";

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
}

fn atomics(n: usize) -> Box<[AtomicI16]> {
    let mut v = Vec::with_capacity(n);
    v.resize_with(n, || AtomicI16::new(0));
    v.into_boxed_slice()
}

impl Stems {
    pub fn for_track(track: &TrackAudio) -> Self {
        let cap = track.expected + SRC_RATE as usize * 4;
        Self {
            data: [atomics(cap * 2), atomics(cap * 2), atomics(cap * 2)],
            cap,
            ready: AtomicUsize::new(0),
            status: AtomicU8::new(Status::Waiting as u8),
            cancelled: AtomicBool::new(false),
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
        let s = Self::for_track(track);
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

pub fn models_present() -> bool {
    MODELS.iter().all(|m| model_path(m).exists())
}

/// Fetch the four networks (about 80 MB) the first time stems are used.
pub async fn download_models() -> Result<()> {
    std::fs::create_dir_all(model_dir())?;
    let client = reqwest::Client::new();
    for m in MODELS {
        let path = model_path(m);
        if path.exists() {
            continue;
        }
        let url = format!("{MODEL_URL}/{m}.wide.fp16.onnx");
        let resp = client.get(&url).timeout(Duration::from_secs(300)).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("couldn't download the {m} stem model ({})", resp.status()));
        }
        let bytes = resp.bytes().await?;
        if bytes.len() < 1_000_000 {
            return Err(anyhow!("the {m} stem model came back too small"));
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        log::info!("downloaded stem model {m} ({} MB)", bytes.len() / 1_000_000);
    }
    Ok(())
}

struct Nets {
    sessions: Vec<ort::session::Session>,
    last_used: Instant,
}

fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("stem model: {e}")
}

fn load_nets() -> Result<Nets> {
    let mut sessions = Vec::new();
    for m in MODELS {
        // No memory arena: an arena keeps its high-water mark for good, and
        // four of them would hold most of a gigabyte between songs.
        let s = ort::session::Session::builder()
            .map_err(ort_err)?
            .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
            .map_err(ort_err)?
            .with_memory_pattern(false)
            .map_err(ort_err)?
            .with_intra_threads(3)
            .map_err(ort_err)?
            .commit_from_file(model_path(m))
            .map_err(ort_err)?;
        sessions.push(s);
    }
    Ok(Nets { sessions, last_used: Instant::now() })
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
    let started = Instant::now();
    stems.set_status(Status::Separating);
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
        if stems.cancelled.load(Ordering::Relaxed) {
            stems.set_status(Status::Off);
            return Ok(());
        }
        // Wait until the audio this piece reads has decoded.
        let first = split * SPLIT;
        let need = ((first + SPLIT) * HOP + N).saturating_sub(PAD);
        loop {
            let done = track.done.load(Ordering::Acquire);
            if done || track.len() >= need {
                break;
            }
            if track.failed.load(Ordering::Acquire) || stems.cancelled.load(Ordering::Relaxed) {
                stems.set_status(Status::Off);
                return Ok(());
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(50));
            guard = NETS.lock().unwrap();
        }
        if guard.is_none() {
            *guard = Some(load_nets()?);
        }
        let nets = guard.as_mut().expect("loaded above");
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
        split += 1;
        if done && (first + SPLIT) >= frames_total {
            break;
        }
    }
    stems.ready.store(track.len(), Ordering::Release);
    stems.set_status(Status::Done);
    log::info!("stems: {:.0} s of song separated in {:.1} s", track.len() as f64 / SRC_RATE, started.elapsed().as_secs_f64());
    Ok(())
}

/// Separate in the background, after making sure the models are here.
pub fn spawn(track: Arc<TrackAudio>, stems: Arc<Stems>) {
    tokio::spawn(async move {
        if !models_present() {
            stems.set_status(Status::Downloading);
            if let Err(e) = download_models().await {
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
