//! Beat-synced effects. Each unit runs one effect on whatever it is
//! assigned to (a deck, or the master), timed in beats of that deck.

use serde::{Deserialize, Serialize};

use super::dsp::{Biquad, Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FxKind {
    #[default]
    Echo,
    Reverb,
    Flanger,
    Phaser,
    Roll,
    Trans,
    Crush,
    Wobble,
    /// Echo whose repeats lose their bass, so they never muddy the mix.
    LowCutEcho,
    /// Echo bouncing between left and right.
    PingPong,
    /// A long, darkening echo that spirals away.
    Spiral,
}

pub const ALL: [FxKind; 11] = [
    FxKind::Echo,
    FxKind::LowCutEcho,
    FxKind::PingPong,
    FxKind::Spiral,
    FxKind::Reverb,
    FxKind::Flanger,
    FxKind::Phaser,
    FxKind::Roll,
    FxKind::Trans,
    FxKind::Crush,
    FxKind::Wobble,
];

impl FxKind {
    pub fn name(self) -> &'static str {
        match self {
            FxKind::Echo => "Echo",
            FxKind::Reverb => "Reverb",
            FxKind::Flanger => "Flanger",
            FxKind::Phaser => "Phaser",
            FxKind::Roll => "Roll",
            FxKind::Trans => "Trans",
            FxKind::Crush => "Crush",
            FxKind::Wobble => "Wobble",
            FxKind::LowCutEcho => "Low Cut Echo",
            FxKind::PingPong => "Ping Pong",
            FxKind::Spiral => "Spiral",
        }
    }

    /// Keeps ringing after it is switched off (the others stop at once).
    pub fn tails(self) -> bool {
        matches!(self, FxKind::Echo | FxKind::LowCutEcho | FxKind::PingPong | FxKind::Spiral | FxKind::Reverb)
    }
}

/// Beat lengths the beat buttons step through.
pub const BEATS: [f32; 9] = [0.125, 0.25, 0.5, 0.75, 1.0, 2.0, 4.0, 8.0, 16.0];

/// First-order allpass, the phaser's building block.
#[derive(Clone, Copy, Default)]
struct AllPass {
    a: f32,
    x1: [f32; 2],
    y1: [f32; 2],
}

impl AllPass {
    fn set(&mut self, hz: f32, rate: f32) {
        let t = (std::f32::consts::PI * hz / rate).tan();
        self.a = (t - 1.0) / (t + 1.0);
    }
    #[inline]
    fn run(&mut self, c: usize, x: f32) -> f32 {
        let y = self.a * x + self.x1[c] - self.a * self.y1[c];
        self.x1[c] = x;
        self.y1[c] = y;
        y
    }
    fn reset(&mut self) {
        self.x1 = [0.0; 2];
        self.y1 = [0.0; 2];
    }
}

struct Delay {
    buf: Vec<f32>,
    w: usize,
}

impl Delay {
    fn new(frames: usize) -> Self {
        Self { buf: vec![0.0; frames * 2], w: 0 }
    }
    #[inline]
    fn read(&self, c: usize, back: f32) -> f32 {
        let n = self.buf.len() / 2;
        let back = back.clamp(1.0, (n - 2) as f32);
        let pos = self.w as f32 - back;
        let pos = if pos < 0.0 { pos + n as f32 } else { pos };
        let i = pos as usize;
        let t = pos - i as f32;
        let a = self.buf[(i % n) * 2 + c];
        let b = self.buf[((i + 1) % n) * 2 + c];
        a + (b - a) * t
    }
    #[inline]
    fn write(&mut self, l: f32, r: f32) {
        self.buf[self.w * 2] = l;
        self.buf[self.w * 2 + 1] = r;
        self.w = (self.w + 1) % (self.buf.len() / 2);
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Freeverb-style reverb.
struct Reverb {
    combs: Vec<(Vec<f32>, usize, f32)>,
    allpasses: Vec<(Vec<f32>, usize)>,
}

impl Reverb {
    fn new(rate: f32) -> Self {
        let scale = rate / 44_100.0;
        let combs = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617]
            .iter()
            .flat_map(|&n| {
                let n = (n as f32 * scale) as usize;
                [(vec![0.0; n], 0, 0.0), (vec![0.0; n + 23], 0, 0.0)]
            })
            .collect();
        let allpasses = [556, 441, 341, 225]
            .iter()
            .flat_map(|&n| {
                let n = (n as f32 * scale) as usize;
                [(vec![0.0; n], 0), (vec![0.0; n + 23], 0)]
            })
            .collect();
        Self { combs, allpasses }
    }

    fn run(&mut self, l: f32, r: f32, room: f32) -> (f32, f32) {
        let damp = 0.3;
        let input = [l, r];
        let mut out = [0.0f32; 2];
        for (i, (buf, pos, store)) in self.combs.iter_mut().enumerate() {
            let c = i % 2;
            let y = buf[*pos];
            *store = y * (1.0 - damp) + *store * damp;
            buf[*pos] = input[c] * 0.015 + *store * room;
            *pos = (*pos + 1) % buf.len();
            out[c] += y;
        }
        for (i, (buf, pos)) in self.allpasses.iter_mut().enumerate() {
            let c = i % 2;
            let b = buf[*pos];
            buf[*pos] = out[c] + b * 0.5;
            out[c] = b - out[c];
            *pos = (*pos + 1) % buf.len();
        }
        (out[0], out[1])
    }

    fn clear(&mut self) {
        for (b, _, s) in &mut self.combs {
            b.fill(0.0);
            *s = 0.0;
        }
        for (b, _) in &mut self.allpasses {
            b.fill(0.0);
        }
    }
}

pub struct Unit {
    pub kind: FxKind,
    pub on: bool,
    /// Index into `BEATS`.
    pub beats: usize,
    /// Wet amount 0..1.
    pub depth: f32,
    /// Bitmask of what this unit is on: bit d = deck d, bit 7 = master.
    pub assign: u8,
    rate: f32,
    delay: Delay,
    reverb: Reverb,
    phaser: [AllPass; 6],
    phaser_fb: [f32; 2],
    lfo: f64,
    roll_len: usize,
    roll_pos: usize,
    roll_filled: bool,
    crush_hold: [f32; 2],
    crush_count: f32,
    wobble: [Biquad; 2],
    /// Filter in the echo's feedback loop (low cut, or the spiral's darkening).
    cut: Biquad,
    /// Dry/wet as heard, gliding to its target; also carries tails after "off".
    wet: f32,
    gate: f32,
}

pub const MASTER: u8 = 1 << 7;

impl Unit {
    pub fn new(rate: f32, kind: FxKind) -> Self {
        Self {
            kind,
            on: false,
            beats: 4,
            depth: 0.5,
            assign: 0,
            rate,
            delay: Delay::new((rate * 6.0) as usize),
            reverb: Reverb::new(rate),
            phaser: [AllPass::default(); 6],
            phaser_fb: [0.0; 2],
            lfo: 0.0,
            roll_len: 0,
            roll_pos: 0,
            roll_filled: false,
            crush_hold: [0.0; 2],
            crush_count: 0.0,
            wobble: [Biquad::default(); 2],
            cut: Biquad::default(),
            wet: 0.0,
            gate: 1.0,
        }
    }

    pub fn set_kind(&mut self, kind: FxKind) {
        if kind != self.kind {
            self.kind = kind;
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.delay.clear();
        self.reverb.clear();
        for b in &mut self.phaser {
            b.reset();
        }
        self.phaser_fb = [0.0; 2];
        self.cut.reset();
        self.roll_filled = false;
        self.roll_pos = 0;
        self.wet = 0.0;
    }

    /// Whether this unit still has sound to make (on, or a tail ringing out).
    pub fn active(&self) -> bool {
        self.on || self.wet > 0.001
    }

    /// Process a stereo block in place. `beat_secs` is the length of one beat
    /// of whatever this unit is following, as heard.
    pub fn run(&mut self, buf: &mut [f32], beat_secs: f32) {
        if !self.active() {
            if self.roll_filled {
                self.roll_filled = false;
            }
            return;
        }
        let beats = BEATS[self.beats.min(BEATS.len() - 1)];
        let time = (beat_secs * beats).clamp(0.01, 5.5);
        let frames = buf.len() / 2;
        // Echo and reverb keep ringing after "off"; the others stop at once.
        let tails = self.kind.tails();
        let target = if self.on { 1.0 } else { 0.0 };
        let glide = 1.0 - (-1.0 / (0.01 * self.rate)).exp();
        let depth = self.depth.clamp(0.0, 1.0);
        let rate = self.rate;
        match self.kind {
            kind @ (FxKind::Echo | FxKind::LowCutEcho | FxKind::PingPong | FxKind::Spiral) => {
                let back = time * rate;
                let feedback = if kind == FxKind::Spiral { 0.6 + 0.35 * depth } else { 0.35 + 0.5 * depth };
                let filtered = match kind {
                    FxKind::LowCutEcho => {
                        self.cut.set(Kind::High, 450.0, 0.7, rate);
                        true
                    }
                    FxKind::Spiral => {
                        self.cut.set(Kind::Low, 3_200.0, 0.7, rate);
                        true
                    }
                    _ => false,
                };
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    let (mut dl, mut dr) = (self.delay.read(0, back), self.delay.read(1, back));
                    if filtered {
                        dl = self.cut.run(0, dl);
                        dr = self.cut.run(1, dr);
                    }
                    // Only feed new sound in while on; the tail keeps repeating.
                    let send = self.wet;
                    if kind == FxKind::PingPong {
                        // In on the left, then across and back.
                        self.delay.write((f[0] + f[1]) * 0.5 * send + dr * feedback, dl * feedback);
                    } else {
                        self.delay.write(f[0] * send + dl * feedback, f[1] * send + dr * feedback);
                    }
                    f[0] += dl * depth.max(0.3);
                    f[1] += dr * depth.max(0.3);
                }
                if !self.on {
                    // Let the repeats fade; stop once inaudible.
                    let level = self.delay.buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                    self.wet = if level < 1e-3 { 0.0 } else { self.wet.max(0.002) };
                }
            }
            FxKind::Reverb => {
                let room = (0.7 + 0.27 * (beats / 16.0).sqrt()).min(0.98);
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    let (l, r) = self.reverb.run(f[0] * self.wet, f[1] * self.wet, room);
                    f[0] = f[0] * (1.0 - 0.3 * depth * self.wet) + l * depth * 2.5;
                    f[1] = f[1] * (1.0 - 0.3 * depth * self.wet) + r * depth * 2.5;
                }
                if !self.on {
                    let mut probe = [0.0f32; 2];
                    let (l, r) = self.reverb.run(0.0, 0.0, room);
                    probe[0] = l;
                    probe[1] = r;
                    self.wet = if probe[0].abs() + probe[1].abs() < 1e-4 { 0.0 } else { self.wet.max(0.002) };
                }
            }
            FxKind::Flanger => {
                let lfo_step = 1.0 / (time as f64 * rate as f64);
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    self.lfo = (self.lfo + lfo_step) % 1.0;
                    let m = 0.5 - 0.5 * (self.lfo * std::f64::consts::TAU).cos() as f32;
                    let back = (0.0008 + 0.006 * m) * rate;
                    let (dl, dr) = (self.delay.read(0, back), self.delay.read(1, back));
                    self.delay.write(f[0] + dl * 0.6, f[1] + dr * 0.6);
                    let w = self.wet * (0.3 + 0.7 * depth);
                    f[0] = f[0] * (1.0 - 0.5 * w) + dl * w * 0.7;
                    f[1] = f[1] * (1.0 - 0.5 * w) + dr * w * 0.7;
                }
            }
            FxKind::Phaser => {
                let lfo_step = 1.0 / (time as f64 * rate as f64);
                let mut i = 0;
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    self.lfo = (self.lfo + lfo_step) % 1.0;
                    if i % 32 == 0 {
                        let m = 0.5 - 0.5 * (self.lfo * std::f64::consts::TAU).cos() as f32;
                        let hz = 250.0 * (3_500.0f32 / 250.0).powf(m);
                        for (k, b) in self.phaser.iter_mut().enumerate() {
                            b.set(hz * (1.0 + k as f32 * 0.25), rate);
                        }
                    }
                    i += 1;
                    let mut y = [0.0f32; 2];
                    for c in 0..2 {
                        let mut v = f[c] + self.phaser_fb[c] * 0.5;
                        for b in self.phaser.iter_mut() {
                            v = b.run(c, v);
                        }
                        self.phaser_fb[c] = v;
                        y[c] = v;
                    }
                    let w = self.wet * (0.4 + 0.6 * depth) * 0.5;
                    f[0] = f[0] * (1.0 - w) + y[0] * w;
                    f[1] = f[1] * (1.0 - w) + y[1] * w;
                }
            }
            FxKind::Roll => {
                let len = (time * rate) as usize;
                if !self.on {
                    self.wet = 0.0;
                    self.roll_filled = false;
                    return;
                }
                if !self.roll_filled || self.roll_len != len {
                    // Start capturing now; repeat once a whole slice is in.
                    if !self.roll_filled {
                        self.roll_pos = 0;
                    }
                    self.roll_len = len.max(64);
                    self.roll_filled = true;
                    self.wet = 0.0;
                }
                for f in buf.chunks_exact_mut(2) {
                    let n = self.roll_len;
                    let capturing = self.wet < 0.5;
                    if capturing {
                        self.delay.buf[self.roll_pos * 2] = f[0];
                        self.delay.buf[self.roll_pos * 2 + 1] = f[1];
                    } else {
                        // Tiny fade at the loop seam.
                        let edge = 64;
                        let p = self.roll_pos;
                        let g = if p < edge { p as f32 / edge as f32 } else if p > n - edge { (n - p) as f32 / edge as f32 } else { 1.0 };
                        let (l, r) = (self.delay.buf[p * 2] * g, self.delay.buf[p * 2 + 1] * g);
                        f[0] = f[0] * (1.0 - depth.max(0.85)) + l * depth.max(0.85);
                        f[1] = f[1] * (1.0 - depth.max(0.85)) + r * depth.max(0.85);
                    }
                    self.roll_pos += 1;
                    if self.roll_pos >= n {
                        self.roll_pos = 0;
                        self.wet = 1.0;
                    }
                }
                let _ = frames;
            }
            FxKind::Trans => {
                let lfo_step = 1.0 / (time as f64 * rate as f64);
                let duty = 0.5;
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    self.lfo = (self.lfo + lfo_step) % 1.0;
                    let open = if self.lfo < duty { 1.0 } else { 1.0 - depth.max(0.5) };
                    self.gate += (open - self.gate) * (1.0 - (-1.0 / (0.002 * rate)).exp());
                    let g = 1.0 - self.wet * (1.0 - self.gate);
                    f[0] *= g;
                    f[1] *= g;
                }
            }
            FxKind::Crush => {
                let bits = 16.0 - 12.0 * depth;
                let levels = 2f32.powf(bits);
                let hold = 1.0 + depth * 20.0 * (beats / 4.0).min(2.0);
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    self.crush_count += 1.0;
                    if self.crush_count >= hold {
                        self.crush_count -= hold;
                        self.crush_hold = [(f[0] * levels).round() / levels, (f[1] * levels).round() / levels];
                    }
                    f[0] = f[0] * (1.0 - self.wet) + self.crush_hold[0] * self.wet;
                    f[1] = f[1] * (1.0 - self.wet) + self.crush_hold[1] * self.wet;
                }
            }
            FxKind::Wobble => {
                let lfo_step = 1.0 / (time as f64 * rate as f64);
                let mut i = 0;
                for f in buf.chunks_exact_mut(2) {
                    self.wet += (target - self.wet) * glide;
                    self.lfo = (self.lfo + lfo_step) % 1.0;
                    if i % 16 == 0 {
                        let m = 0.5 + 0.5 * (self.lfo * std::f64::consts::TAU).sin() as f32;
                        let hz = 150.0 * (6_000.0f32 / 150.0).powf(m * (0.3 + 0.7 * (1.0 - depth * 0.5)));
                        for b in &mut self.wobble {
                            b.set(Kind::Low, hz, 2.5, rate);
                        }
                    }
                    i += 1;
                    for c in 0..2 {
                        let y0 = self.wobble[0].run(c, f[c]);
                        let y = self.wobble[1].run(c, y0);
                        f[c] = f[c] * (1.0 - self.wet) + y * self.wet * 0.8;
                    }
                }
            }
        }
        if !tails && !self.on {
            self.wet = if self.wet < 0.001 { 0.0 } else { self.wet };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_runs_and_stays_finite() {
        let rate = 48_000.0;
        for kind in ALL {
            let mut u = Unit::new(rate, kind);
            u.on = true;
            u.depth = 0.8;
            let mut buf: Vec<f32> = (0..9600).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
            for chunk in buf.chunks_mut(512) {
                u.run(chunk, 0.47);
            }
            assert!(buf.iter().all(|v| v.is_finite() && v.abs() < 4.0), "{kind:?}");
            u.on = false;
            for _ in 0..400 {
                let mut silent = vec![0.0f32; 512];
                u.run(&mut silent, 0.47);
            }
            // Effects with tails eventually stop running.
            assert!(!u.active() || kind.tails(), "{kind:?}");
        }
    }

    #[test]
    fn echo_repeats_on_the_beat() {
        let rate = 48_000.0;
        let mut u = Unit::new(rate, FxKind::Echo);
        u.on = true;
        u.depth = 1.0;
        u.beats = 4; // one beat
        let beat = 0.5; // 120 BPM
        let mut buf = vec![0.0f32; 96_000];
        buf[4000] = 1.0;
        buf[4001] = 1.0;
        u.run(&mut buf, beat);
        // First repeat one beat (24000 frames) later.
        let at = (2_000 + 24_000) * 2;
        let peak = buf[at - 20..at + 20].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.2, "{peak}");
    }
}
