//! Tempo, beat grid, key and loudness of a whole song.
//!
//! The grid assumes a steady tempo, as DJ software does: almost every song
//! made to be mixed has one, and a steady grid is what makes sync, quantize,
//! loops and beat jumps line up for the whole song rather than drift.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use serde::{Deserialize, Serialize};

use crate::track::SRC_RATE;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    pub bpm: f64,
    /// A downbeat (first beat of a bar), in source frames.
    pub anchor: f64,
    /// 0..24: 0-11 major from C, 12-23 minor from C.
    pub key: Option<u8>,
    /// Average loudness (RMS of the music), dBFS.
    pub loudness_db: f32,
    /// Where the music starts, in frames.
    pub first_sound: f64,
    /// How sure the tempo reading is, 0..1.
    pub confidence: f32,
}

impl Analysis {
    pub fn period(&self) -> f64 {
        SRC_RATE * 60.0 / self.bpm
    }
}

/// Tempos are reported in this range (like Serato's default 78-155): a
/// half-time or double-time reading is folded into it.
pub const RANGE: (f64, f64) = (78.0, 156.0);

/// Analysis rate: a quarter of the source rate is plenty for beats and keys.
const RATE: f64 = SRC_RATE / 4.0;

fn downsample(mono: &[f32]) -> Vec<f32> {
    // Crude anti-alias: average of four, after a gentle two-tap smoothing.
    mono.chunks_exact(4).map(|c| (c[0] + c[1] + c[2] + c[3]) * 0.25).collect()
}

pub fn analyse(mono: &[f32]) -> Option<Analysis> {
    if mono.len() < SRC_RATE as usize * 8 {
        return None;
    }
    let x = downsample(mono);
    let (loudness_db, first_sound) = loudness(mono);
    let (bpm, anchor, confidence) = beats(mono, &x)?;
    let key = key(&x);
    Some(Analysis { bpm, anchor, key, loudness_db, first_sound, confidence })
}

// ---- loudness -----------------------------------------------------------------

fn loudness(mono: &[f32]) -> (f32, f64) {
    let block = 4410; // 100 ms
    let mut levels: Vec<f32> = mono
        .chunks(block)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let first = levels.iter().position(|&l| l > 0.003).unwrap_or(0);
    // Ignore the quietest quarter (intros, breakdowns, silence).
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let loud = &levels[levels.len() / 4..];
    let mean_sq = loud.iter().map(|l| l * l).sum::<f32>() / loud.len().max(1) as f32;
    let db = 10.0 * mean_sq.max(1e-10).log10();
    // Refine where the sound starts, to the sample.
    let start = first * block;
    let end = (start + block).min(mono.len());
    let exact = (start..end).find(|&i| mono[i].abs() > 0.01).unwrap_or(start);
    (db, exact as f64)
}

// ---- tempo and grid ----------------------------------------------------------

const HOP: usize = 64;
const FRAME: usize = 1024;

/// Spectral-flux onset strength at RATE / HOP frames per second.
fn novelty(x: &[f32]) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FRAME);
    let window: Vec<f32> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FRAME as f32).cos())
        .collect();
    let bins = FRAME / 2;
    let frames = x.len().saturating_sub(FRAME) / HOP;
    let mut prev = vec![0.0f32; bins];
    let mut out = Vec::with_capacity(frames);
    let mut buf = vec![Complex32::new(0.0, 0.0); FRAME];
    // Up to ~5.5 kHz; weight the lows a little more, where the beat lives.
    for f in 0..frames {
        let s = &x[f * HOP..f * HOP + FRAME];
        for i in 0..FRAME {
            buf[i] = Complex32::new(s[i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut flux = 0.0;
        for k in 1..bins {
            let m = (1.0 + 1000.0 * buf[k].norm()).ln();
            let d = m - prev[k];
            if d > 0.0 {
                flux += if k < 24 { 2.0 * d } else { d };
            }
            prev[k] = m;
        }
        out.push(flux);
    }
    // Remove the slowly varying part so loud sections don't dominate.
    let w = 40;
    let mut cum = vec![0.0f64; out.len() + 1];
    for (i, v) in out.iter().enumerate() {
        cum[i + 1] = cum[i] + *v as f64;
    }
    (0..out.len())
        .map(|i| {
            let a = i.saturating_sub(w);
            let b = (i + w + 1).min(out.len());
            let mean = (cum[b] - cum[a]) / (b - a) as f64;
            (out[i] as f64 - mean).max(0.0) as f32
        })
        .collect()
}

/// Kick-drum onsets at 1 ms resolution, for placing the grid exactly.
fn kick_onsets(mono: &[f32]) -> Vec<f32> {
    let a = 1.0 - (-2.0 * std::f32::consts::PI * 120.0 / SRC_RATE as f32).exp();
    let (mut l1, mut l2) = (0.0f32, 0.0f32);
    let step = 44; // ~1 ms
    let mut env = Vec::with_capacity(mono.len() / step + 1);
    let mut acc = 0.0f32;
    for (i, &v) in mono.iter().enumerate() {
        l1 += a * (v - l1);
        l2 += a * (l1 - l2);
        acc = acc.max(l2.abs());
        if i % step == step - 1 {
            env.push(acc);
            acc = 0.0;
        }
    }
    // Rise around each moment marks an attack, at the moment it starts.
    let mut out = vec![0.0; env.len()];
    for i in 1..env.len().saturating_sub(4) {
        let rise = env[i + 4] - env[i - 1];
        out[i] = rise.max(0.0);
    }
    out
}

fn periodicity(nov: &[f32], period: f64) -> Complex32 {
    // Sum of nov[t] * e^{-2πi t / period}, by rotating a phasor.
    let w = -2.0 * std::f64::consts::PI / period;
    let (step_s, step_c) = w.sin_cos();
    let (mut s, mut c) = (0.0f64, 1.0f64);
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (t, &v) in nov.iter().enumerate() {
        re += v as f64 * c;
        im += v as f64 * s;
        let nc = c * step_c - s * step_s;
        let ns = s * step_c + c * step_s;
        c = nc;
        s = ns;
        if t % 1024 == 1023 {
            // Renormalise against rounding drift.
            let n = (c * c + s * s).sqrt();
            c /= n;
            s /= n;
        }
    }
    Complex32::new(re as f32, im as f32)
}

fn comb_score(nov: &[f32], period: f64) -> f64 {
    // The beat and its subdivisions together.
    [1.0, 2.0, 4.0]
        .iter()
        .zip([1.0, 0.5, 0.25])
        .map(|(h, w)| periodicity(nov, period / h).norm() as f64 * w)
        .sum()
}

/// Steady tempo, a downbeat, and a confidence.
fn beats(mono: &[f32], x: &[f32]) -> Option<(f64, f64, f32)> {
    let nov = novelty(x);
    if nov.len() < 500 {
        return None;
    }
    let fps = RATE / HOP as f64;
    // Coarse: autocorrelation over 70-180 BPM.
    let lag = |bpm: f64| fps * 60.0 / bpm;
    let acf = |l: f64| -> f64 {
        let li = l.floor() as usize;
        let t = l - li as f64;
        let mut a = 0.0;
        let mut b = 0.0;
        for i in 0..nov.len().saturating_sub(li + 1) {
            a += (nov[i] * nov[i + li]) as f64;
            b += (nov[i] * nov[i + li + 1]) as f64;
        }
        a * (1.0 - t) + b * t
    };
    // Each candidate scored by the beat and its multiples together, so a
    // tempo and its double score alike; the DJ range below picks between them.
    let mut best = (0.0, 0.0);
    let mut bpm = 60.0;
    while bpm <= 200.0 {
        let l = lag(bpm);
        let mut score = (1..=4).map(|k| acf(l * k as f64)).sum::<f64>();
        score *= (-0.5 * ((bpm / 120.0).log2() / 1.2).powi(2)).exp();
        if score > best.0 {
            best = (score, bpm);
        }
        bpm += 0.5;
    }
    let mut folded = best.1;
    while folded >= RANGE.1 {
        folded /= 2.0;
    }
    while folded < RANGE.0 {
        folded *= 2.0;
    }
    best.1 = folded;
    let coarse = best.1;

    // Fine: strength of the beat frequency, searched ±2% in tiny steps.
    let p0 = lag(coarse);
    let mut fine = (0.0, p0);
    let steps = 400;
    for i in 0..=steps {
        let p = p0 * (0.98 + 0.04 * i as f64 / steps as f64);
        let s = comb_score(&nov, p);
        if s > fine.0 {
            fine = (s, p);
        }
    }
    // Polish around the best step.
    let mut lo = fine.1 * 0.9998;
    let mut hi = fine.1 * 1.0002;
    for _ in 0..20 {
        let a = lo + (hi - lo) / 3.0;
        let b = hi - (hi - lo) / 3.0;
        if comb_score(&nov, a) < comb_score(&nov, b) { lo = a } else { hi = b }
    }
    let period_frames = (lo + hi) / 2.0;
    let mut bpm = fps * 60.0 / period_frames;
    let strength = comb_score(&nov, period_frames);
    // Most songs are made at a whole (or half) BPM; prefer it when it fits as well.
    for snap in [bpm.round(), (bpm * 2.0).round() / 2.0] {
        if (snap - bpm).abs() < 0.06 && comb_score(&nov, lag(snap)) >= strength * 0.985 {
            bpm = snap;
            break;
        }
    }
    let total: f64 = nov.iter().map(|&v| v as f64).sum();
    let confidence = (strength / total.max(1e-9) * 2.5).min(1.0) as f32;

    // Place the beats on the kicks, at 1 ms resolution.
    let kicks = kick_onsets(mono);
    // Kick onsets come one per 44 source frames.
    let kps = SRC_RATE / 44.0;
    let period_k = kps * 60.0 / bpm;
    let z = periodicity(&kicks, period_k);
    let mut phase = (-z.im.atan2(z.re) as f64 / (2.0 * std::f64::consts::PI)).rem_euclid(1.0) * period_k;
    // The fundamental gives the neighbourhood; the attack peaks give the spot.
    let fit = |ph: f64| -> f64 {
        let mut s = 0.0;
        let mut t = ph;
        while t < kicks.len() as f64 - 2.0 {
            let i = t.round() as usize;
            s += kicks[i] as f64 + 0.5 * (kicks[i.saturating_sub(1)] + kicks[i + 1]) as f64;
            t += period_k;
        }
        s
    };
    let mut best_ph = (fit(phase), phase);
    let span = (period_k / 4.0) as i64;
    for d in -span..=span {
        let ph = (phase + d as f64).rem_euclid(period_k);
        let s = fit(ph);
        if s > best_ph.0 {
            best_ph = (s, ph);
        }
    }
    phase = best_ph.1;
    // If the kicks are weak (no four-on-the-floor), fall back to the full-band onsets.
    let kick_total: f32 = kicks.iter().sum();
    let beat0 = if kick_total > 1e-3 {
        phase * 44.0
    } else {
        let zf = periodicity(&nov, period_frames);
        let ph = (-zf.im.atan2(zf.re) as f64 / (2.0 * std::f64::consts::PI)).rem_euclid(1.0) * period_frames;
        (ph * HOP as f64 + FRAME as f64 / 2.0) * 4.0
    };
    let period = SRC_RATE * 60.0 / bpm;

    // Pick which beat of four starts the bar: where things change most.
    let full_at = |frame: f64| -> f64 {
        let i = ((frame / 4.0 - FRAME as f64 / 2.0) / HOP as f64).round();
        if i < 0.0 || i as usize >= nov.len() { 0.0 } else { nov[i as usize] as f64 }
    };
    let kick_at = |frame: f64| -> f64 {
        let i = (frame / 44.0).round() as usize;
        kicks.get(i).copied().unwrap_or(0.0) as f64
    };
    let mut bar_scores = [0.0f64; 4];
    let beats_total = (mono.len() as f64 / period) as usize;
    for k in 0..beats_total {
        let at = beat0 + k as f64 * period;
        bar_scores[k % 4] += full_at(at) + 0.3 * kick_at(at);
    }
    let down = (0..4).max_by(|&a, &b| bar_scores[a].partial_cmp(&bar_scores[b]).unwrap()).unwrap_or(0);
    let anchor = beat0 + down as f64 * period;
    Some((bpm, anchor % (period * 4.0), confidence))
}

// ---- key -------------------------------------------------------------------------

// Krumhansl-Kessler key profiles.
const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

pub fn chroma(x: &[f32]) -> [f64; 12] {
    let n = 4096;
    let hop = 2048;
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).collect();
    // Which pitch class each bin feeds, between C2 and C7.
    let pcs: Vec<Option<usize>> = (0..n / 2)
        .map(|k| {
            let f = k as f64 * RATE / n as f64;
            if !(65.0..2100.0).contains(&f) {
                return None;
            }
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            let near = midi.round();
            // Only bins close to a semitone centre: in-between bins blur keys.
            ((midi - near).abs() < 0.35).then_some((near as i64).rem_euclid(12) as usize)
        })
        .collect();
    let mut chroma = [0.0f64; 12];
    let mut buf = vec![Complex32::new(0.0, 0.0); n];
    let mut start = 0;
    while start + n <= x.len() {
        for i in 0..n {
            buf[i] = Complex32::new(x[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut frame = [0.0f64; 12];
        for (k, pc) in pcs.iter().enumerate() {
            if let Some(pc) = pc {
                frame[*pc] += buf[k].norm() as f64;
            }
        }
        // Each moment counts by its shape, not its volume.
        let sum: f64 = frame.iter().sum();
        if sum > 1e-6 {
            for i in 0..12 {
                chroma[i] += (frame[i] / sum).powf(1.5);
            }
        }
        start += hop;
    }
    chroma
}

fn correlate(a: &[f64; 12], b: &[f64; 12], shift: usize) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let x = a[(i + shift) % 12] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    num / (da * db).sqrt().max(1e-12)
}

pub fn key_of(chroma: &[f64; 12]) -> Option<u8> {
    if chroma.iter().sum::<f64>() < 1e-6 {
        return None;
    }
    let mut best = (f64::MIN, 0u8);
    for tonic in 0..12 {
        for (mode, profile) in [(0u8, &MAJOR), (12u8, &MINOR)] {
            let r = correlate(chroma, profile, tonic);
            if r > best.0 {
                best = (r, mode + tonic as u8);
            }
        }
    }
    Some(best.1)
}

fn key(x: &[f32]) -> Option<u8> {
    key_of(&chroma(x))
}

const NAMES: [&str; 12] = ["C", "D♭", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];
const MINOR_NAMES: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "G♯", "A", "B♭", "B"];

pub fn key_name(k: u8) -> String {
    if k < 12 { NAMES[k as usize].to_string() } else { format!("{}m", MINOR_NAMES[(k - 12) as usize]) }
}

/// Camelot wheel code, e.g. 8A for A minor.
pub fn camelot(k: u8) -> String {
    // Position of each tonic on the wheel, majors (B) and minors (A).
    const MAJ: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
    const MIN: [u8; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
    if k < 12 { format!("{}B", MAJ[k as usize]) } else { format!("{}A", MIN[(k - 12) as usize]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drum loop: kick on every beat, hat on the off-beats, a clap on 2 and 4.
    fn beat_track(bpm: f64, secs: f64, offset: f64) -> Vec<f32> {
        let n = (secs * SRC_RATE) as usize;
        let mut v = vec![0.0f32; n];
        let period = SRC_RATE * 60.0 / bpm;
        let mut k = 0;
        loop {
            let at = offset + k as f64 * period;
            if at as usize >= n {
                break;
            }
            let start = at.round() as usize;
            for i in 0..6000.min(n - start) {
                let t = i as f32 / SRC_RATE as f32;
                let f = 50.0 + 90.0 * (-t * 30.0).exp();
                v[start + i] += (2.0 * std::f32::consts::PI * f * t).sin() * (-t * 12.0).exp() * 0.8;
            }
            let hat = (at + period / 2.0).round() as usize;
            let mut seed = 1u32;
            for i in 0..1500.min(n.saturating_sub(hat)) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 9) as f32 / (1 << 23) as f32 - 1.0;
                v[hat + i] += noise * (-(i as f32) / 300.0).exp() * 0.15;
            }
            k += 1;
        }
        v
    }

    #[test]
    fn finds_tempo_and_grid() {
        for (bpm, offset) in [(128.0, 1234.0), (122.0, 20_000.0), (174.0, 500.0), (95.0, 7_000.0), (140.0, 3_000.0)] {
            let audio = beat_track(bpm, 60.0, offset);
            let a = analyse(&audio).unwrap();
            // Drum & bass reads at half time, like the usual DJ range.
            let want = if bpm >= RANGE.1 { bpm / 2.0 } else { bpm };
            assert!((a.bpm - want).abs() < 0.02, "wanted {want}, got {}", a.bpm);
            // The grid lands on a kick, within 3 ms.
            // Every kick is a beat (at half time, every other one; either is fine).
            let period = SRC_RATE * 60.0 / bpm;
            let off = ((a.anchor - offset) / period).rem_euclid(1.0) * period;
            let err = off.min(period - off);
            let signed = if off < period / 2.0 { off } else { off - period };
            eprintln!("bpm {bpm}: signed grid error {:.2} ms", signed / SRC_RATE * 1000.0);
            assert!(err < SRC_RATE * 0.003, "bpm {bpm}: grid off by {:.1} ms", err / SRC_RATE * 1000.0);
        }
    }

    #[test]
    fn half_bpm_tempo_is_not_snapped_wrongly() {
        let audio = beat_track(123.5, 60.0, 0.0);
        let a = analyse(&audio).unwrap();
        assert!((a.bpm - 123.5).abs() < 0.02, "{}", a.bpm);
    }

    #[test]
    fn finds_key() {
        // A minor: i - iv - v - i chords (Am, Dm, Em, Am) as sine stacks.
        let chord = |notes: &[f64], secs: f64| -> Vec<f32> {
            let n = (secs * SRC_RATE) as usize;
            (0..n).map(|i| {
                let t = i as f64 / SRC_RATE;
                notes.iter().map(|m| (2.0 * std::f64::consts::PI * 440.0 * 2f64.powf((m - 69.0) / 12.0) * t).sin()).sum::<f64>() as f32 * 0.2
            }).collect()
        };
        let mut audio = Vec::new();
        for _ in 0..3 {
            audio.extend(chord(&[57.0, 60.0, 64.0, 45.0], 2.0)); // Am
            audio.extend(chord(&[62.0, 65.0, 69.0, 50.0], 2.0)); // Dm
            audio.extend(chord(&[64.0, 67.0, 71.0, 52.0], 2.0)); // Em
            audio.extend(chord(&[57.0, 60.0, 64.0, 45.0], 2.0)); // Am
        }
        let k = key_of(&chroma(&downsample(&audio))).unwrap();
        assert_eq!(key_name(k), "Am");
        assert_eq!(camelot(k), "8A");
    }

    #[test]
    fn camelot_codes() {
        assert_eq!(camelot(0), "8B"); // C
        assert_eq!(camelot(7), "9B"); // G
        assert_eq!(camelot(12 + 9), "8A"); // Am
        assert_eq!(camelot(12 + 4), "9A"); // Em
        assert_eq!(camelot(12), "5A"); // Cm
    }
}

/// How far the kicks sit from the grid (median, ms) in each third of the
/// song: steady numbers near zero mean the tempo reading holds all the way.
pub fn grid_drift(mono: &[f32], a: &Analysis) -> [f64; 3] {
    let kicks = kick_onsets(mono);
    let period = a.period();
    let mut thirds: [Vec<f64>; 3] = Default::default();
    let n = mono.len() as f64;
    let mut beat = a.anchor;
    while beat < n {
        let c = (beat / 44.0).round() as i64;
        let (mut best, mut at) = (0.0f32, None);
        for d in -40i64..=40 {
            if let Some(&v) = kicks.get((c + d).max(0) as usize) {
                if v > best {
                    best = v;
                    at = Some(d);
                }
            }
        }
        if let (Some(d), true) = (at, best > 0.002) {
            let third = ((beat / n) * 3.0).min(2.0) as usize;
            thirds[third].push(d as f64 * 44.0 / SRC_RATE * 1000.0);
        }
        beat += period;
    }
    thirds.map(|mut v| {
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    })
}
