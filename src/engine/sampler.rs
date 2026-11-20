//! The sampler: sixteen one-shot sounds on the pads, mixed into the master.
//! Eight are made here from scratch (so there is something to play without
//! any files); a WAV file named `1.wav` … `16.wav` in the samples folder
//! takes that pad over.

use std::path::Path;
use std::sync::Arc;

use crate::track::SRC_RATE;

pub const SLOTS: usize = 16;

/// A sound held in memory: interleaved stereo at 44.1 kHz.
pub struct Sample {
    pub name: String,
    pub data: Vec<f32>,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    pos: f64,
    playing: bool,
    /// Fades out over a few milliseconds when stopped, so it never clicks.
    gain: f32,
    stopping: bool,
}

pub struct Sampler {
    slots: [Option<Arc<Sample>>; SLOTS],
    voices: [Voice; SLOTS],
    pub volume: f32,
}

impl Default for Sampler {
    fn default() -> Self {
        Self { slots: std::array::from_fn(|_| None), voices: [Voice::default(); SLOTS], volume: 0.8 }
    }
}

impl Sampler {
    /// Put a sound on a pad; returns the one it replaces.
    pub fn set(&mut self, n: usize, sample: Option<Arc<Sample>>) -> Option<Arc<Sample>> {
        if n >= SLOTS {
            return sample;
        }
        self.voices[n] = Voice::default();
        std::mem::replace(&mut self.slots[n], sample)
    }

    pub fn trigger(&mut self, n: usize) {
        if n < SLOTS && self.slots[n].is_some() {
            self.voices[n] = Voice { pos: 0.0, playing: true, gain: 1.0, stopping: false };
        }
    }

    pub fn stop(&mut self, n: usize) {
        if n < SLOTS {
            self.voices[n].stopping = true;
        }
    }

    pub fn playing(&self) -> [bool; SLOTS] {
        std::array::from_fn(|n| self.voices[n].playing && !self.voices[n].stopping)
    }

    pub fn loaded(&self) -> [bool; SLOTS] {
        std::array::from_fn(|n| self.slots[n].is_some())
    }

    /// Add whatever is playing to `out` (interleaved stereo at `rate`).
    pub fn render(&mut self, out: &mut [f32], rate: f64) {
        let step = SRC_RATE / rate;
        let volume = crate::engine::dsp::fader_gain(self.volume) * 1.2;
        for n in 0..SLOTS {
            let v = &mut self.voices[n];
            if !v.playing {
                continue;
            }
            let Some(sample) = self.slots[n].as_ref() else {
                v.playing = false;
                continue;
            };
            let frames = sample.data.len() / 2;
            for f in out.chunks_exact_mut(2) {
                let i = v.pos as usize;
                if i + 1 >= frames || v.gain <= 0.0 {
                    v.playing = false;
                    break;
                }
                let t = (v.pos - i as f64) as f32;
                let g = v.gain * volume;
                f[0] += (sample.data[2 * i] * (1.0 - t) + sample.data[2 * i + 2] * t) * g;
                f[1] += (sample.data[2 * i + 1] * (1.0 - t) + sample.data[2 * i + 3] * t) * g;
                v.pos += step;
                if v.stopping {
                    v.gain -= 1.0 / 256.0;
                }
            }
        }
    }
}

// ---- the built-in sounds -----------------------------------------------------

const R: f32 = SRC_RATE as f32;
const TAU: f32 = std::f32::consts::TAU;

/// Deterministic noise, so the sounds are the same every start.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn stereo(mono: Vec<f32>) -> Vec<f32> {
    // Normalise to a healthy level, with a short fade at each end.
    let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let n = mono.len();
    let mut out = Vec::with_capacity(n * 2);
    for (i, v) in mono.iter().enumerate() {
        let edge = (i.min(n - 1 - i) as f32 / 64.0).min(1.0);
        let s = v / peak * 0.7 * edge;
        out.push(s);
        out.push(s);
    }
    out
}

/// A band-limited-enough saw (a few harmonics), for the horn and siren.
fn saw(phase: f32) -> f32 {
    (1..=8).map(|k| (phase * k as f32 * TAU).sin() / k as f32).sum::<f32>() * 0.6
}

fn air_horn() -> Vec<f32> {
    // Two short blasts and a long one, three reedy notes a little apart.
    let hits = [(0.0, 0.13), (0.19, 0.13), (0.38, 0.75)];
    let n = (1.25 * R) as usize;
    let mut out = vec![0.0f32; n];
    let mut phase = [0.0f32; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / R;
        let Some((start, len)) = hits.iter().copied().find(|(s, l)| t >= *s && t < s + l) else { continue };
        let u = t - start;
        let env = (u / 0.012).min(1.0) * ((len - u) / 0.05).min(1.0);
        // Slides up into the note, sags a touch at the end.
        let bend = 1.0 - 0.06 * (-u * 40.0).exp() - 0.03 * (u / len).powi(3);
        let mut v = 0.0;
        for (k, hz) in [466.0f32, 587.0, 699.0].iter().enumerate() {
            phase[k] = (phase[k] + hz * bend * (1.0 + 0.004 * (t * 37.0 + k as f32).sin()) / R).fract();
            v += saw(phase[k]);
        }
        *o = (v * 0.9).tanh() * env;
    }
    out
}

fn siren() -> Vec<f32> {
    let n = (1.8 * R) as usize;
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / R;
            let hz = 650.0 + 550.0 * (0.5 - 0.5 * (t * TAU / 0.9).cos());
            phase = (phase + hz / R).fract();
            let env = (t / 0.02).min(1.0) * ((1.8 - t) / 0.15).min(1.0);
            (saw(phase) * 0.6 + (phase * TAU).sin() * 0.6) * env
        })
        .collect()
}

fn laser() -> Vec<f32> {
    // A falling zap, echoed three times.
    let zap = (0.22 * R) as usize;
    let n = (0.9 * R) as usize;
    let mut out = vec![0.0f32; n];
    for (rep, level) in [1.0f32, 0.5, 0.25, 0.12].iter().enumerate() {
        let at = rep * (0.17 * R) as usize;
        let mut phase = 0.0f32;
        for i in 0..zap {
            let u = i as f32 / zap as f32;
            let hz = 3200.0 * (160.0f32 / 3200.0).powf(u);
            phase = (phase + hz / R).fract();
            if at + i < n {
                out[at + i] += (phase * TAU).sin() * (1.0 - u).powf(0.6) * level;
            }
        }
    }
    out
}

fn kick() -> Vec<f32> {
    let n = (0.7 * R) as usize;
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / R;
            let hz = 46.0 + 130.0 * (-t * 38.0).exp();
            phase = (phase + hz / R).fract();
            let click = if t < 0.004 { 1.0 - t / 0.004 } else { 0.0 };
            ((phase * TAU).sin() * (-t * 5.5).exp() * 1.4).tanh() + click * 0.3
        })
        .collect()
}

fn clap() -> Vec<f32> {
    let n = (0.35 * R) as usize;
    let mut noise = Noise(0x1234_5678);
    let (mut lp, mut hp) = (0.0f32, 0.0f32);
    (0..n)
        .map(|i| {
            let t = i as f32 / R;
            // Three quick slaps and then the room.
            let slap = [0.0f32, 0.011, 0.023].iter().map(|s| if t >= *s { (-(t - s) * 320.0).exp() } else { 0.0 }).sum::<f32>();
            let tail = if t >= 0.03 { (-(t - 0.03) * 16.0).exp() } else { 0.0 };
            let x = noise.next();
            // Band around 1-2 kHz.
            lp += 0.35 * (x - lp);
            hp += 0.06 * (lp - hp);
            (lp - hp) * (slap + tail * 0.8)
        })
        .collect()
}

fn snare() -> Vec<f32> {
    let n = (0.3 * R) as usize;
    let mut noise = Noise(0x0bad_cafe);
    let mut phase = 0.0f32;
    let mut hp = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / R;
            phase = (phase + (185.0 + 90.0 * (-t * 60.0).exp()) / R).fract();
            let x = noise.next();
            hp += 0.2 * (x - hp);
            (phase * TAU).sin() * (-t * 22.0).exp() * 0.8 + (x - hp) * (-t * 16.0).exp() * 0.7
        })
        .collect()
}

fn riser() -> Vec<f32> {
    // Noise opening up through a resonant filter over three seconds.
    let secs = 3.0;
    let n = (secs * R) as usize;
    let mut noise = Noise(0x5eed_1e55);
    let (mut low, mut band) = (0.0f32, 0.0f32);
    (0..n)
        .map(|i| {
            let u = i as f32 / n as f32;
            let hz = 200.0 * (9000.0f32 / 200.0).powf(u);
            let f = 2.0 * (std::f32::consts::PI * hz / R).sin();
            let x = noise.next();
            // State-variable filter, taken at its band-pass.
            low += f * band;
            let high = x - low - 0.25 * band;
            band += f * high;
            band * (0.15 + 0.85 * u * u) * ((1.0 - u) / 0.01).min(1.0)
        })
        .collect()
}

fn boom() -> Vec<f32> {
    let n = (2.2 * R) as usize;
    let mut noise = Noise(0x00dd_ba11);
    let mut phase = 0.0f32;
    let mut lp = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / R;
            phase = (phase + (32.0 + 55.0 * (-t * 9.0).exp()) / R).fract();
            lp += 0.02 * (noise.next() - lp);
            ((phase * TAU).sin() * (-t * 1.9).exp() * 1.6).tanh() + lp * 6.0 * (-t * 3.0).exp()
        })
        .collect()
}

/// The eight sounds every Slipmat starts with.
pub fn built_in() -> Vec<Sample> {
    let make: [(&str, fn() -> Vec<f32>); 8] = [
        ("Air horn", air_horn),
        ("Siren", siren),
        ("Laser", laser),
        ("Kick", kick),
        ("Clap", clap),
        ("Snare", snare),
        ("Riser", riser),
        ("Boom", boom),
    ];
    make.iter().map(|(name, f)| Sample { name: name.to_string(), data: stereo(f()) }).collect()
}

// ---- your own sounds -----------------------------------------------------------

/// Read a WAV file (8/16/24/32-bit PCM or 32-bit float, any sample rate,
/// mono or stereo) into a sample. At most 30 seconds are kept.
pub fn read_wav(path: &Path) -> Option<Sample> {
    let bytes = std::fs::read(path).ok()?;
    let data = decode_wav(&bytes)?;
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some(Sample { name, data })
}

fn decode_wav(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0usize, 0u32, 0usize);
    let mut pcm: Option<&[u8]> = None;
    let mut at = 12;
    while at + 8 <= b.len() {
        let size = u32_at(at + 4)? as usize;
        let body = at + 8;
        let end = (body + size).min(b.len());
        match &b[at..at + 4] {
            b"fmt " => {
                format = u16_at(body)?;
                channels = u16_at(body + 2)? as usize;
                rate = u32_at(body + 4)?;
                bits = u16_at(body + 14)? as usize;
                // "Extensible" files keep the real format further in.
                if format == 0xFFFE {
                    format = u16_at(body + 24)?;
                }
            }
            b"data" => pcm = Some(&b[body..end]),
            _ => {}
        }
        at = body + size + (size & 1);
    }
    let pcm = pcm?;
    if channels == 0 || channels > 8 || rate < 4_000 || !matches!((format, bits), (1, 8 | 16 | 24 | 32) | (3, 32)) {
        return None;
    }
    let width = bits / 8;
    let frames = pcm.len() / (width * channels);
    let sample = |f: usize, c: usize| -> f32 {
        let s = &pcm[(f * channels + c) * width..];
        match (format, bits) {
            (1, 8) => (s[0] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            _ => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        }
    };
    // To 44.1 kHz stereo, by straight-line interpolation.
    let step = rate as f64 / SRC_RATE;
    let out_frames = ((frames as f64 / step) as usize).min(SRC_RATE as usize * 30);
    let right = if channels > 1 { 1 } else { 0 };
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as f64 * step;
        let f = pos as usize;
        let t = (pos - f as f64) as f32;
        let g = (f + 1).min(frames.saturating_sub(1));
        for c in [0, right] {
            out.push((sample(f, c) * (1.0 - t) + sample(g, c) * t).clamp(-1.0, 1.0));
        }
    }
    (out_frames > 0).then_some(out)
}

/// What goes on each pad: your `N.wav` if there is one, else a built-in
/// (pads 9-16, the second deck's, repeat the first eight).
pub fn bank(dir: &Path) -> Vec<Arc<Sample>> {
    let built: Vec<Arc<Sample>> = built_in().into_iter().map(Arc::new).collect();
    (0..SLOTS)
        .map(|n| match read_wav(&dir.join(format!("{}.wav", n + 1))) {
            Some(s) => Arc::new(s),
            None => built[n % built.len()].clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_sounds_are_sane() {
        let all = built_in();
        assert_eq!(all.len(), 8);
        for s in &all {
            let peak = s.data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(s.data.iter().all(|v| v.is_finite()), "{}", s.name);
            assert!((0.5..=0.75).contains(&peak), "{} peaks at {peak}", s.name);
            assert!(s.data.len() > 8_000 && s.data.len() % 2 == 0, "{}", s.name);
        }
    }

    #[test]
    fn a_pad_plays_once_and_stops() {
        let mut sampler = Sampler::default();
        let click = Arc::new(Sample { name: "click".into(), data: vec![0.5; 2000] });
        assert!(sampler.set(3, Some(click)).is_none());
        let mut out = vec![0.0f32; 512];
        sampler.render(&mut out, 44_100.0);
        assert!(out.iter().all(|v| *v == 0.0), "silent until triggered");
        sampler.trigger(3);
        assert!(sampler.playing()[3]);
        sampler.render(&mut out, 44_100.0);
        assert!(out[0] > 0.1);
        for _ in 0..8 {
            sampler.render(&mut out, 44_100.0);
        }
        assert!(!sampler.playing()[3], "one-shot ends by itself");
        // An empty pad does nothing.
        sampler.trigger(9);
        assert!(!sampler.playing()[9]);
    }

    #[test]
    fn reads_wav_files() {
        // 16-bit mono at 22.05 kHz: comes back stereo at 44.1 kHz.
        let samples: Vec<i16> = (0..2205).map(|i| ((i as f32 * 0.1).sin() * 20_000.0) as i16).collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + samples.len() * 2) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&22_050u32.to_le_bytes());
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for s in &samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        let out = decode_wav(&b).unwrap();
        assert_eq!(out.len(), 2205 * 2 * 2);
        assert!((out[0] - out[1]).abs() < 1e-6, "mono lands on both sides");
        assert!(decode_wav(b"not a wav file at all").is_none());
    }
}
