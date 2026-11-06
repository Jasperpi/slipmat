//! A whole song held in memory, decoded once and shared by the loader (which
//! writes it) and the audio thread (which reads it). DJ playback needs the
//! entire song at hand: cue points, loops, scratching and the waveform all
//! jump around it.

use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU32, AtomicUsize, Ordering};

/// Spotify's audio is always 44.1 kHz stereo.
pub const SRC_RATE: f64 = 44_100.0;
/// Frames summarised by one column of the waveform (about 5.8 ms).
pub const WAVE_HOP: usize = 256;

/// One waveform column: how loud the lows, mids and highs are, and the peak.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Column {
    pub low: u8,
    pub mid: u8,
    pub high: u8,
    pub peak: u8,
}

impl Column {
    fn pack(self) -> u32 {
        u32::from_le_bytes([self.low, self.mid, self.high, self.peak])
    }
    fn unpack(v: u32) -> Self {
        let [low, mid, high, peak] = v.to_le_bytes();
        Self { low, mid, high, peak }
    }
}

pub struct TrackAudio {
    /// Interleaved left/right, 16-bit. Atomics so the reader never sees a torn
    /// write; on every platform these compile to plain loads and stores.
    samples: Box<[AtomicI16]>,
    cap: usize,
    len: AtomicUsize,
    wave: Box<[AtomicU32]>,
    wave_len: AtomicUsize,
    pub done: AtomicBool,
    pub failed: AtomicBool,
    /// Length the metadata promised, in frames.
    pub expected: usize,
}

fn atomics<T, F: Fn() -> T>(n: usize, f: F) -> Box<[T]> {
    let mut v = Vec::with_capacity(n);
    v.resize_with(n, f);
    v.into_boxed_slice()
}

impl TrackAudio {
    pub fn new(duration_ms: u32) -> Self {
        let expected = (duration_ms as f64 / 1000.0 * SRC_RATE) as usize;
        // Room for a little more than promised; metadata is rounded.
        let cap = expected + SRC_RATE as usize * 4;
        Self {
            samples: atomics(cap * 2, || AtomicI16::new(0)),
            cap,
            len: AtomicUsize::new(0),
            wave: atomics(cap / WAVE_HOP + 1, || AtomicU32::new(0)),
            wave_len: AtomicUsize::new(0),
            done: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            expected,
        }
    }

    /// Frames decoded so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    /// Length to show and to plan around: the promised length until decoding
    /// finishes, then the real one.
    pub fn total(&self) -> usize {
        if self.done.load(Ordering::Acquire) { self.len() } else { self.expected.max(self.len()) }
    }

    #[inline]
    pub fn frame(&self, i: usize, len: usize) -> (f32, f32) {
        if i >= len {
            return (0.0, 0.0);
        }
        const S: f32 = 1.0 / 32768.0;
        (
            self.samples[2 * i].load(Ordering::Relaxed) as f32 * S,
            self.samples[2 * i + 1].load(Ordering::Relaxed) as f32 * S,
        )
    }

    /// Append decoded audio. Only the loader calls this, from one thread.
    pub fn push(&self, stereo: &[f32]) -> usize {
        let start = self.len.load(Ordering::Relaxed);
        let n = (stereo.len() / 2).min(self.cap - start);
        for i in 0..n {
            for c in 0..2 {
                let v = (stereo[2 * i + c] * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                self.samples[2 * (start + i) + c].store(v, Ordering::Relaxed);
            }
        }
        self.len.store(start + n, Ordering::Release);
        n
    }

    pub fn wave_len(&self) -> usize {
        self.wave_len.load(Ordering::Acquire)
    }

    pub fn column(&self, i: usize) -> Column {
        Column::unpack(self.wave[i].load(Ordering::Relaxed))
    }

    pub fn push_column(&self, c: Column) {
        let i = self.wave_len.load(Ordering::Relaxed);
        if i < self.wave.len() {
            self.wave[i].store(c.pack(), Ordering::Relaxed);
            self.wave_len.store(i + 1, Ordering::Release);
        }
    }

    /// Mono copy of everything decoded, for analysis.
    pub fn mono(&self) -> Vec<f32> {
        let len = self.len();
        (0..len).map(|i| {
            let (l, r) = self.frame(i, len);
            (l + r) * 0.5
        }).collect()
    }

    /// The waveform as bytes (four per column), from column `from`.
    pub fn wave_bytes(&self, from: usize) -> Vec<u8> {
        let n = self.wave_len();
        let mut out = Vec::with_capacity(n.saturating_sub(from) * 4);
        for i in from.min(n)..n {
            out.extend_from_slice(&self.wave[i].load(Ordering::Relaxed).to_le_bytes());
        }
        out
    }
}

/// Builds waveform columns from audio as it is decoded: a coarse three-way
/// split of the spectrum, the way DJ software colours its waveforms.
pub struct WaveBuilder {
    lp1: [f32; 2],
    lp2: [f32; 2],
    hp_prev_in: f32,
    hp: f32,
    n: usize,
    acc: [f32; 4],
}

impl Default for WaveBuilder {
    fn default() -> Self {
        Self { lp1: [0.0; 2], lp2: [0.0; 2], hp_prev_in: 0.0, hp: 0.0, n: 0, acc: [0.0; 4] }
    }
}

// Scale each band so a loud, modern master fills most of the height.
const LOW_GAIN: f32 = 1.7;
const MID_GAIN: f32 = 1.9;
const HIGH_GAIN: f32 = 4.5;

fn one_pole(hz: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * hz / SRC_RATE as f32).exp()
}

impl WaveBuilder {
    pub fn feed(&mut self, track: &TrackAudio, stereo: &[f32]) {
        let a_low = one_pole(180.0);
        let a_mid = one_pole(2_200.0);
        let hp_r = 1.0 / (1.0 + 2.0 * std::f32::consts::PI * 3_500.0 / SRC_RATE as f32);
        for f in stereo.chunks_exact(2) {
            let x = (f[0] + f[1]) * 0.5;
            // Two cascaded one-pole lowpasses for each split.
            self.lp1[0] += a_low * (x - self.lp1[0]);
            self.lp1[1] += a_low * (self.lp1[0] - self.lp1[1]);
            self.lp2[0] += a_mid * (x - self.lp2[0]);
            self.lp2[1] += a_mid * (self.lp2[0] - self.lp2[1]);
            self.hp = hp_r * (self.hp + x - self.hp_prev_in);
            self.hp_prev_in = x;
            let low = self.lp1[1];
            let mid = self.lp2[1] - low;
            let high = self.hp;
            // Energy per band (RMS reads truer than peaks), plus the peak.
            self.acc[0] += low * low;
            self.acc[1] += mid * mid;
            self.acc[2] += high * high;
            self.acc[3] = self.acc[3].max(x.abs());
            self.n += 1;
            if self.n == WAVE_HOP {
                let n = WAVE_HOP as f32;
                let b = |sq: f32, gain: f32| (((sq / n).sqrt() * gain).min(1.0).powf(0.8) * 255.0) as u8;
                track.push_column(Column {
                    low: b(self.acc[0], LOW_GAIN),
                    mid: b(self.acc[1], MID_GAIN),
                    high: b(self.acc[2], HIGH_GAIN),
                    peak: (self.acc[3].min(1.0) * 255.0) as u8,
                });
                self.n = 0;
                self.acc = [0.0; 4];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_read_back() {
        let t = TrackAudio::new(1000);
        let data: Vec<f32> = (0..2000).map(|i| ((i as f32) / 2000.0) - 0.5).collect();
        assert_eq!(t.push(&data), 1000);
        assert_eq!(t.len(), 1000);
        let (l, r) = t.frame(10, t.len());
        assert!((l - data[20]).abs() < 1e-4 && (r - data[21]).abs() < 1e-4);
        assert_eq!(t.frame(5000, t.len()), (0.0, 0.0));
    }

    #[test]
    fn waveform_columns_follow_audio() {
        let t = TrackAudio::new(2000);
        let mut w = WaveBuilder::default();
        // A loud 60 Hz tone then a loud 8 kHz one.
        let tone = |hz: f32, n: usize| -> Vec<f32> {
            (0..n).flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * hz * i as f32 / 44_100.0).sin() * 0.8;
                [v, v]
            }).collect()
        };
        w.feed(&t, &tone(60.0, 22_050));
        w.feed(&t, &tone(8_000.0, 22_050));
        let n = t.wave_len();
        assert!(n > 150);
        let early = t.column(n / 4);
        let late = t.column(3 * n / 4);
        assert!(early.low > early.high, "{early:?}");
        assert!(late.high > late.low, "{late:?}");
    }
}
