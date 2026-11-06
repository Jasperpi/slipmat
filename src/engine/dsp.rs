//! Filters and small building blocks for the mixer.

use std::f32::consts::PI;

/// A stereo biquad (transposed direct form II).
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Low,
    High,
}

impl Biquad {
    pub fn new(kind: Kind, hz: f32, q: f32, rate: f32) -> Self {
        let mut b = Self::default();
        b.set(kind, hz, q, rate);
        b
    }

    /// Change the coefficients, keeping the state (so sweeps don't click).
    pub fn set(&mut self, kind: Kind, hz: f32, q: f32, rate: f32) {
        let hz = hz.clamp(10.0, rate * 0.45);
        let w = 2.0 * PI * hz / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = match kind {
            Kind::Low => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0),
            Kind::High => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    #[inline]
    pub fn run(&mut self, c: usize, x: f32) -> f32 {
        let y = self.b0 * x + self.z1[c];
        self.z1[c] = self.b1 * x - self.a1 * y + self.z2[c];
        self.z2[c] = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }
}

/// Fourth-order Linkwitz-Riley crossover half (two Butterworth stages).
#[derive(Clone, Copy, Default)]
pub struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    pub fn new(kind: Kind, hz: f32, rate: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        Self { a: Biquad::new(kind, hz, q, rate), b: Biquad::new(kind, hz, q, rate) }
    }
    #[inline]
    pub fn run(&mut self, c: usize, x: f32) -> f32 {
        self.b.run(c, self.a.run(c, x))
    }
}

/// A DJ mixer's three-band isolator EQ. With every band at unity the bands
/// sum back to the input (Linkwitz-Riley crossovers at 250 Hz and 3 kHz).
#[derive(Clone, Copy)]
pub struct Isolator {
    lo_lp: Lr4,
    lo_hp: Lr4,
    hi_lp: Lr4,
    hi_hp: Lr4,
    /// Phase-matching allpass for the low band: the low/high split of the
    /// upper crossover, applied to the lows too, keeps the bands summing flat.
    lo_ap_lp: Lr4,
    lo_ap_hp: Lr4,
    gain: [f32; 3],
}

pub const EQ_LOW_HZ: f32 = 250.0;
pub const EQ_HIGH_HZ: f32 = 3_000.0;

impl Isolator {
    pub fn new(rate: f32) -> Self {
        Self {
            lo_lp: Lr4::new(Kind::Low, EQ_LOW_HZ, rate),
            lo_hp: Lr4::new(Kind::High, EQ_LOW_HZ, rate),
            hi_lp: Lr4::new(Kind::Low, EQ_HIGH_HZ, rate),
            hi_hp: Lr4::new(Kind::High, EQ_HIGH_HZ, rate),
            lo_ap_lp: Lr4::new(Kind::Low, EQ_HIGH_HZ, rate),
            lo_ap_hp: Lr4::new(Kind::High, EQ_HIGH_HZ, rate),
            gain: [1.0; 3],
        }
    }

    /// Process a stereo block; `target` gains glide over the block.
    pub fn run(&mut self, buf: &mut [f32], target: [f32; 3]) {
        let n = (buf.len() / 2).max(1) as f32;
        let step = [(target[0] - self.gain[0]) / n, (target[1] - self.gain[1]) / n, (target[2] - self.gain[2]) / n];
        let flat = self.gain == [1.0; 3] && target == [1.0; 3];
        for f in buf.chunks_exact_mut(2) {
            for c in 0..2 {
                let x = f[c];
                let low = self.lo_lp.run(c, x);
                let rest = self.lo_hp.run(c, x);
                let mid = self.hi_lp.run(c, rest);
                let high = self.hi_hp.run(c, rest);
                let low = self.lo_ap_lp.run(c, low) + self.lo_ap_hp.run(c, low);
                if !flat {
                    f[c] = low * self.gain[0] + mid * self.gain[1] + high * self.gain[2];
                } else {
                    f[c] = low + mid + high;
                }
            }
            for b in 0..3 {
                self.gain[b] += step[b];
            }
        }
        self.gain = target;
    }
}

/// The one-knob filter on each channel: left of centre a lowpass closes,
/// right of centre a highpass opens.
#[derive(Clone, Copy)]
pub struct SweepFilter {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    rate: f32,
    value: f32,
}

impl SweepFilter {
    pub fn new(rate: f32) -> Self {
        Self { lp: [Biquad::default(); 2], hp: [Biquad::default(); 2], rate, value: 0.0 }
    }

    pub fn run(&mut self, buf: &mut [f32], target: f32) {
        let was = self.value;
        self.value = target;
        let dead = 0.02;
        if was.abs() < dead && target.abs() < dead {
            return;
        }
        // Glide the cutoff over the block in a few steps.
        let parts = 4;
        let frames = buf.len() / 2;
        let q = 0.9;
        for p in 0..parts {
            let v = was + (target - was) * (p + 1) as f32 / parts as f32;
            let a = frames * p / parts;
            let b = frames * (p + 1) / parts;
            if v < -dead {
                let t = (-v - dead) / (1.0 - dead);
                let hz = 20_000.0 * (60.0f32 / 20_000.0).powf(t);
                for s in &mut self.lp {
                    s.set(Kind::Low, hz, q, self.rate);
                }
                for f in buf[a * 2..b * 2].chunks_exact_mut(2) {
                    for c in 0..2 {
                        let y = self.lp[0].run(c, f[c]);
                        f[c] = self.lp[1].run(c, y);
                    }
                }
                for s in &mut self.hp {
                    s.reset();
                }
            } else if v > dead {
                let t = (v - dead) / (1.0 - dead);
                let hz = 20.0 * (9_000.0f32 / 20.0).powf(t);
                for s in &mut self.hp {
                    s.set(Kind::High, hz, q, self.rate);
                }
                for f in buf[a * 2..b * 2].chunks_exact_mut(2) {
                    for c in 0..2 {
                        let y = self.hp[0].run(c, f[c]);
                        f[c] = self.hp[1].run(c, y);
                    }
                }
                for s in &mut self.lp {
                    s.reset();
                }
            }
        }
    }
}

/// Knob position (0..1, centre 0.5) to EQ gain: full left kills the band,
/// centre is unity, full right is +6 dB.
pub fn eq_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v >= 0.5 {
        db((v - 0.5) * 2.0 * 6.0)
    } else if v < 0.01 {
        0.0
    } else {
        // -26 dB just above the bottom, rising to 0 dB at the centre.
        db(-26.0 * (1.0 - v / 0.5).powf(1.3))
    }
}

/// Trim knob (0..1, centre 0.5) to gain: ±12 dB.
pub fn trim_gain(v: f32) -> f32 {
    db((v.clamp(0.0, 1.0) - 0.5) * 24.0)
}

pub fn db(d: f32) -> f32 {
    10f32.powf(d / 20.0)
}

/// Channel fader position to gain: a taper close to a real mixer's.
pub fn fader_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0 { 0.0 } else { v * v * (3.0 - 2.0 * v) * 0.5 + v * v * 0.5 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    /// Constant power: both full in the middle, for long blends.
    #[default]
    Smooth,
    /// Straight lines; dips in the middle.
    Linear,
    /// Both sides full almost all the way: for cuts and scratching.
    Sharp,
}

/// Crossfader gains for the left and right sides.
pub fn crossfade(x: f32, curve: Curve) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0);
    match curve {
        Curve::Smooth => {
            let a = x * std::f32::consts::FRAC_PI_2;
            // Normalised so the middle is unity, not -3 dB.
            ((a.cos() * 1.4142).min(1.0), (a.sin() * 1.4142).min(1.0))
        }
        Curve::Linear => (1.0 - x, x),
        Curve::Sharp => (((1.0 - x) * 16.0).min(1.0), (x * 16.0).min(1.0)),
    }
}

/// Peak meter with a quick rise and slow fall.
#[derive(Clone, Copy, Default)]
pub struct Meter {
    pub level: f32,
}

impl Meter {
    pub fn feed(&mut self, buf: &[f32], seconds: f32) {
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let fall = (-seconds / 0.35).exp();
        self.level = peak.max(self.level * fall);
    }
}

/// A gentle brickwall so the master never clips the converter.
pub struct Limiter {
    gain: f32,
    rate: f32,
}

impl Limiter {
    pub fn new(rate: f32) -> Self {
        Self { gain: 1.0, rate }
    }
    pub fn run(&mut self, buf: &mut [f32]) -> f32 {
        let ceiling = 0.98;
        let release = 1.0 - (-1.0 / (0.25 * self.rate)).exp();
        let mut reduction = 1.0f32;
        for f in buf.chunks_exact_mut(2) {
            let peak = f[0].abs().max(f[1].abs());
            let need = if peak * self.gain > ceiling { ceiling / peak } else { 1.0 };
            if need < self.gain {
                self.gain = need;
            } else {
                self.gain += (1.0 - self.gain) * release;
            }
            f[0] *= self.gain;
            f[1] *= self.gain;
            reduction = reduction.min(self.gain);
        }
        reduction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, rate: f32, n: usize) -> Vec<f32> {
        (0..n).flat_map(|i| {
            let v = (2.0 * PI * hz * i as f32 / rate).sin() * 0.5;
            [v, v]
        }).collect()
    }

    fn rms(b: &[f32]) -> f32 {
        (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt()
    }

    #[test]
    fn isolator_is_flat_at_unity_and_kills_bands() {
        let rate = 48_000.0;
        for hz in [60.0, 1_000.0, 8_000.0] {
            let mut eq = Isolator::new(rate);
            let mut b = tone(hz, rate, 48_000);
            let before = rms(&b[48_000..]);
            for c in b.chunks_mut(512) {
                eq.run(c, [1.0; 3]);
            }
            let after = rms(&b[48_000..]);
            assert!((after / before - 1.0).abs() < 0.03, "{hz} Hz: {before} -> {after}");
        }
        let mut eq = Isolator::new(rate);
        let mut b = tone(60.0, rate, 48_000);
        for c in b.chunks_mut(512) {
            eq.run(c, [0.0, 1.0, 1.0]);
        }
        assert!(rms(&b[48_000..]) < 0.02, "bass kill left {}", rms(&b[48_000..]));
    }

    #[test]
    fn filter_knob_cuts_the_right_side() {
        let rate = 48_000.0;
        let mut f = SweepFilter::new(rate);
        let mut highs = tone(8_000.0, rate, 24_000);
        for chunk in highs.chunks_mut(512) {
            f.run(chunk, -0.8);
        }
        assert!(rms(&highs[24_000..]) < 0.02);
        let mut f = SweepFilter::new(rate);
        let mut lows = tone(60.0, rate, 24_000);
        for chunk in lows.chunks_mut(512) {
            f.run(chunk, 0.8);
        }
        assert!(rms(&lows[24_000..]) < 0.02);
    }

    #[test]
    fn knob_tapers() {
        assert!((eq_gain(0.5) - 1.0).abs() < 1e-6);
        assert_eq!(eq_gain(0.0), 0.0);
        assert!((trim_gain(0.5) - 1.0).abs() < 1e-6);
        assert_eq!(fader_gain(0.0), 0.0);
        assert!((fader_gain(1.0) - 1.0).abs() < 1e-6);
        let (l, r) = crossfade(0.5, Curve::Smooth);
        assert!((l - 1.0).abs() < 0.01 && (r - 1.0).abs() < 0.01);
        assert_eq!(crossfade(0.0, Curve::Sharp), (1.0, 0.0));
    }
}
