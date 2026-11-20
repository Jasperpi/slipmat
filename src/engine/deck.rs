//! One deck: a loaded song and everything about how it is being played.
//! Positions are in frames of the song (44.1 kHz), whatever the output rate.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use crate::stems::Stems;
use crate::track::{SRC_RATE, TrackAudio};

pub const HOTCUES: usize = 8;
/// Memory cues a song can keep (the CUE/LOOP CALL buttons step through them).
pub const MEMS: usize = 8;
/// Furthest the key can be shifted, in semitones each way.
pub const KEY_RANGE: i8 = 12;
/// Song frames one jog-wheel tick moves when scratching: a 720-tick platter
/// turning at 33⅓ rpm plays at normal speed.
pub const FRAMES_PER_TICK: f64 = SRC_RATE * (60.0 / (100.0 / 3.0)) / 720.0;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Grid {
    pub bpm: f64,
    /// A downbeat, in song frames.
    pub anchor: f64,
}

impl Grid {
    pub fn period(&self) -> f64 {
        SRC_RATE * 60.0 / self.bpm
    }
    /// Fractional beat number at `pos` (beat 0 is the anchor).
    pub fn beat(&self, pos: f64) -> f64 {
        (pos - self.anchor) / self.period()
    }
    pub fn at(&self, beat: f64) -> f64 {
        self.anchor + beat * self.period()
    }
    pub fn nearest(&self, pos: f64) -> f64 {
        self.at(self.beat(pos).round())
    }
    /// Beat phase, 0..1.
    pub fn phase(&self, pos: f64) -> f64 {
        self.beat(pos).rem_euclid(1.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Loop {
    pub start: f64,
    pub end: f64,
    pub active: bool,
}

impl Loop {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// A saved point in the song, with the loop that was running there if any.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mem {
    pub pos: f64,
    pub end: Option<f64>,
}

struct Scratch {
    target: f64,
    x: f64,
    v: f64,
}

struct Tail {
    pos: f64,
    left: usize,
    /// Step per output frame and loudness the old sound had.
    speed: f64,
    gain: f32,
}

const TAIL: usize = 192;
const FADE: f32 = 1.0 / 128.0;

/// Time-stretching (for key lock) by overlapping grains that are read at
/// normal speed while the deck moves at its own; each new grain is placed
/// where it best continues the last (WSOLA).
#[derive(Default)]
struct Stretch {
    active: bool,
    prev: f64,
    cur: f64,
    k: usize,
}

const GRAIN: usize = 1024; // half a grain (hop), output frames

pub struct Deck {
    pub track: Option<Arc<TrackAudio>>,
    /// The song split into vocals, melody, bass and drums, as far as done.
    pub stems: Option<Arc<Stems>>,
    /// Level of each stem asked for (vocals, melody, bass, drums), 0..1.
    pub stem_gain: [f32; 4],
    /// Level of each stem as heard, gliding to `stem_gain`.
    stem_now: [f32; 4],
    stems_ready: usize,
    pub grid: Option<Grid>,
    pub pos: f64,
    /// Asked for by the user.
    pub playing: bool,
    /// The platter is turning (audio is moving forward on its own).
    motor: bool,
    gain: f32,
    pub pitch: f32,
    pub range: f32,
    pub keylock: bool,
    /// Speed relative to the song's own tempo, set by the pitch fader or by sync.
    pub tempo: f64,
    pub sync: bool,
    /// Extra speed from the engine to pull a synced deck onto the beat.
    pub correction: f64,
    /// Actual playback speed right now (negative when going backwards).
    pub speed: f64,
    bend: f64,
    /// Held nudge from the on-screen buttons.
    pub nudge: f64,
    scratch: Option<Scratch>,
    pub touched: bool,
    pub cue: f64,
    cue_preview: bool,
    hot_preview: Option<usize>,
    play_latch: bool,
    pub hotcues: [Option<f64>; HOTCUES],
    pub memory: [Option<Mem>; MEMS],
    /// The memory cue last called, for deleting it.
    mem_last: Option<usize>,
    /// Semitones the key is shifted by (tempo unchanged).
    pub key_shift: i8,
    /// A loop roll is held: leaving it carries on where the song would be.
    pub rolling: bool,
    pub lp: Option<Loop>,
    pub loop_in: Option<f64>,
    pub slip: bool,
    slip_pos: Option<f64>,
    pub reverse: bool,
    tail: Option<Tail>,
    stretch: Stretch,
    stretching: bool,
    last_ratio: f64,
    /// Set when playback starts, so the engine can line up a synced deck.
    pub started: bool,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            track: None,
            stems: None,
            stem_gain: [1.0; 4],
            stem_now: [1.0; 4],
            stems_ready: 0,
            grid: None,
            pos: 0.0,
            playing: false,
            motor: false,
            gain: 0.0,
            pitch: 0.0,
            range: 0.08,
            keylock: false,
            tempo: 1.0,
            sync: false,
            correction: 0.0,
            speed: 0.0,
            bend: 0.0,
            nudge: 0.0,
            scratch: None,
            touched: false,
            cue: 0.0,
            cue_preview: false,
            hot_preview: None,
            play_latch: false,
            hotcues: [None; HOTCUES],
            memory: [None; MEMS],
            mem_last: None,
            key_shift: 0,
            rolling: false,
            lp: None,
            loop_in: None,
            slip: false,
            slip_pos: None,
            reverse: false,
            tail: None,
            stretch: Stretch::default(),
            stretching: false,
            last_ratio: 1.0,
            started: false,
        }
    }
}

#[inline]
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

impl Deck {
    pub fn own_tempo(&self) -> f64 {
        1.0 + self.pitch as f64 * self.range as f64
    }

    pub fn bpm(&self) -> Option<f64> {
        self.grid.map(|g| g.bpm * self.tempo)
    }

    pub fn len(&self) -> f64 {
        self.track.as_ref().map_or(0.0, |t| t.total() as f64)
    }

    pub fn at_cue(&self) -> bool {
        (self.pos - self.cue).abs() < 2.0
    }

    /// Put a song on the deck; returns what was there, to be freed elsewhere.
    pub fn load(
        &mut self,
        track: Arc<TrackAudio>,
        stems: Option<Arc<Stems>>,
        grid: Option<Grid>,
        cue: f64,
        hotcues: [Option<f64>; HOTCUES],
    ) -> (Option<Arc<TrackAudio>>, Option<Arc<Stems>>) {
        let old = (self.track.replace(track), std::mem::replace(&mut self.stems, stems));
        let keep = (self.pitch, self.range, self.keylock, self.slip);
        *self = Self { track: self.track.take(), stems: self.stems.take(), ..Self::default() };
        (self.pitch, self.range, self.keylock, self.slip) = keep;
        self.tempo = self.own_tempo();
        self.grid = grid;
        self.cue = cue;
        self.pos = cue;
        self.hotcues = hotcues;
        old
    }

    pub fn eject(&mut self) -> (Option<Arc<TrackAudio>>, Option<Arc<Stems>>) {
        let old = (self.track.take(), self.stems.take());
        let keep = (self.pitch, self.range, self.keylock, self.slip);
        *self = Self::default();
        (self.pitch, self.range, self.keylock, self.slip) = keep;
        old
    }

    /// Move the playhead, crossfading from where it was so it doesn't click.
    pub fn jump(&mut self, to: f64) {
        let to = to.max(0.0);
        if self.gain > 0.0 && self.speed.abs() > 0.0 {
            self.tail = Some(Tail { pos: self.pos, left: TAIL, speed: self.speed * self.last_ratio, gain: self.gain });
        }
        self.pos = to;
        self.stretch.active = false;
    }

    /// Jump and stop dead there (the old sound fades out by itself).
    fn stop_at(&mut self, to: f64) {
        self.jump(to);
        self.playing = false;
        self.motor = false;
        self.gain = 0.0;
        self.speed = 0.0;
        self.stretch.active = false;
    }

    fn snap(&self, pos: f64, quantize: bool) -> f64 {
        match (quantize, self.grid) {
            (true, Some(g)) => g.nearest(pos),
            _ => pos,
        }
    }

    // ---- transport ---------------------------------------------------------

    pub fn play(&mut self) {
        if self.track.is_none() {
            return;
        }
        if self.cue_preview || self.hot_preview.is_some() {
            self.play_latch = true;
            return;
        }
        if !self.playing {
            self.playing = true;
            self.started = true;
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.cue_preview = false;
        self.hot_preview = None;
        self.play_latch = false;
    }

    /// Stop and go back to the cue point (fader start, pulled back down).
    pub fn back_cue(&mut self) {
        if self.track.is_none() {
            return;
        }
        self.pause();
        self.stop_at(self.cue);
    }

    pub fn set_key_shift(&mut self, semitones: i8) {
        self.key_shift = semitones.clamp(-KEY_RANGE, KEY_RANGE);
    }

    pub fn play_pause(&mut self) {
        if self.cue_preview || self.hot_preview.is_some() {
            self.play_latch = true;
        } else if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// The CUE button, as on a CDJ.
    pub fn cue_button(&mut self, down: bool, quantize: bool) {
        if self.track.is_none() {
            return;
        }
        if down {
            if self.playing {
                self.pause();
                self.stop_at(self.cue);
            } else if self.at_cue() {
                self.cue_preview = true;
                self.play_latch = false;
                self.playing = true;
                self.started = true;
            } else {
                self.cue = self.snap(self.pos, quantize);
                self.jump(self.cue);
            }
        } else if self.cue_preview {
            self.cue_preview = false;
            if self.play_latch {
                self.play_latch = false;
            } else {
                self.stop_at(self.cue);
            }
        }
    }

    pub fn hotcue(&mut self, n: usize, down: bool, quantize: bool) {
        if self.track.is_none() || n >= HOTCUES {
            return;
        }
        if down {
            match self.hotcues[n] {
                None => self.hotcues[n] = Some(self.snap(self.pos, quantize)),
                Some(at) => {
                    if self.playing && self.hot_preview.is_none() && !self.cue_preview {
                        let to = match (quantize, self.grid) {
                            (true, Some(g)) => at + (self.pos - g.nearest(self.pos)),
                            _ => at,
                        };
                        self.jump(to);
                    } else {
                        self.jump(at);
                        self.hot_preview = Some(n);
                        self.play_latch = false;
                        self.playing = true;
                        self.started = true;
                    }
                }
            }
        } else if self.hot_preview == Some(n) {
            self.hot_preview = None;
            if self.play_latch {
                self.play_latch = false;
            } else if let Some(at) = self.hotcues[n] {
                self.stop_at(at);
            } else {
                self.playing = false;
            }
        }
    }

    pub fn seek_fraction(&mut self, frac: f64) {
        let len = self.len();
        if len > 0.0 {
            self.jump((frac.clamp(0.0, 1.0) * len).min(len - 1.0));
        }
    }

    // ---- loops ---------------------------------------------------------------

    pub fn auto_loop(&mut self, beats: f64, quantize: bool) {
        let Some(g) = self.grid else { return };
        if let Some(l) = self.lp {
            if l.active && (l.len() - beats * g.period()).abs() < 1.0 {
                self.loop_exit();
                return;
            }
        }
        let start = self.loop_start(g, beats, quantize);
        self.set_loop(start, start + beats * g.period());
    }

    fn loop_start(&self, g: Grid, beats: f64, quantize: bool) -> f64 {
        let start = if quantize {
            // The beat just passed (or one about to land).
            g.at((g.beat(self.pos) + 0.03).floor())
        } else {
            self.pos
        };
        if beats < 1.0 && quantize {
            // Short loops start on the current subdivision.
            let sub = g.period() * beats;
            start + ((self.pos - start) / sub).floor() * sub
        } else {
            start
        }
    }

    /// Loop roll: loops while held; on release the song carries on from
    /// where it would have been had it never looped.
    pub fn loop_roll(&mut self, beats: f64, down: bool, quantize: bool) {
        if down {
            let Some(g) = self.grid else { return };
            if self.slip_pos.is_none() {
                self.slip_pos = Some(self.pos);
            }
            self.rolling = true;
            let start = self.loop_start(g, beats, quantize);
            self.set_loop(start, start + beats * g.period());
        } else if self.rolling {
            self.rolling = false;
            self.loop_exit();
        }
    }

    // ---- memory cues -----------------------------------------------------------

    /// Save where the deck is (its loop, if one is running) as a memory cue.
    pub fn mem_store(&mut self, quantize: bool) -> bool {
        if self.track.is_none() {
            return false;
        }
        let mem = match self.lp.filter(|l| l.active) {
            Some(l) => Mem { pos: l.start, end: Some(l.end) },
            None => Mem { pos: if self.playing { self.snap(self.pos, quantize) } else { self.pos }, end: None },
        };
        // One already there is replaced; else the first free slot.
        let near = self.memory.iter().position(|m| m.is_some_and(|m| (m.pos - mem.pos).abs() < 200.0));
        let Some(i) = near.or_else(|| self.memory.iter().position(|m| m.is_none())) else { return false };
        self.memory[i] = Some(mem);
        self.mem_last = Some(i);
        true
    }

    /// Go to the next (`dir` > 0) or previous memory cue. A paused deck
    /// waits there with its cue point moved to it; a playing deck jumps and
    /// keeps playing.
    pub fn mem_call(&mut self, dir: i8) -> bool {
        let here = self.pos;
        let pick = self
            .memory
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.map(|m| (i, m)))
            .filter(|(_, m)| if dir > 0 { m.pos > here + 64.0 } else { m.pos < here - 64.0 })
            .min_by(|a, b| (a.1.pos - here).abs().total_cmp(&(b.1.pos - here).abs()));
        let Some((i, m)) = pick else { return false };
        self.mem_last = Some(i);
        if !self.playing {
            self.cue = m.pos;
        }
        self.jump(m.pos);
        self.lp = match m.end {
            Some(end) => Some(Loop { start: m.pos, end, active: true }),
            None => self.lp.map(|l| Loop { active: false, ..l }),
        };
        true
    }

    /// Delete the memory cue last called or saved, or the one right here.
    pub fn mem_delete(&mut self) -> bool {
        let here = self.pos;
        let last = self.mem_last.take().filter(|&i| self.memory[i].is_some());
        let near = || self.memory.iter().position(|m| m.is_some_and(|m| (m.pos - here).abs() < SRC_RATE * 0.5));
        match last.or_else(near) {
            Some(i) => {
                self.memory[i] = None;
                true
            }
            None => false,
        }
    }

    fn set_loop(&mut self, start: f64, end: f64) {
        if end - start < 32.0 {
            return;
        }
        if self.slip && self.slip_pos.is_none() {
            self.slip_pos = Some(self.pos);
        }
        self.lp = Some(Loop { start, end, active: true });
        self.loop_in = None;
        if self.pos >= end {
            self.jump(start + (self.pos - start) % (end - start));
        }
    }

    pub fn loop_in_button(&mut self, quantize: bool) {
        if self.track.is_none() {
            return;
        }
        let at = self.snap(self.pos, quantize);
        match self.lp {
            Some(l) if l.active => self.lp = Some(Loop { start: at.min(l.end - 32.0), ..l }),
            _ => self.loop_in = Some(at),
        }
    }

    pub fn loop_out_button(&mut self, quantize: bool) {
        let start = match (self.loop_in, self.lp) {
            (Some(s), _) => s,
            (None, Some(l)) => l.start,
            _ => return,
        };
        let mut end = self.snap(self.pos, quantize);
        if let (true, Some(g)) = (quantize, self.grid) {
            // Whole beats long, at least one.
            let beats = ((end - start) / g.period()).round().max(1.0);
            end = start + beats * g.period();
        }
        if end > start {
            self.set_loop(start, end);
        }
    }

    pub fn reloop(&mut self) {
        match self.lp {
            Some(l) if l.active => self.loop_exit(),
            Some(l) => {
                self.lp = Some(Loop { active: true, ..l });
                self.jump(l.start);
            }
            None => {}
        }
    }

    pub fn loop_exit(&mut self) {
        if let Some(l) = self.lp.as_mut() {
            l.active = false;
        }
        if let Some(back) = self.slip_pos.take() {
            self.jump(back);
        }
    }

    pub fn loop_resize(&mut self, factor: f64) {
        let Some(l) = self.lp else { return };
        let len = (l.len() * factor).max(64.0);
        if len > SRC_RATE * 120.0 {
            return;
        }
        let end = l.start + len;
        self.lp = Some(Loop { end, ..l });
        if l.active && self.pos >= end {
            self.jump(l.start + (self.pos - l.start) % len);
        }
    }

    pub fn beat_jump(&mut self, beats: f64) {
        let Some(g) = self.grid else {
            // Without a grid, jump by half-seconds.
            self.jump(self.pos + beats * SRC_RATE * 0.5);
            return;
        };
        let by = beats * g.period();
        if let Some(l) = self.lp.as_mut() {
            if l.active {
                l.start += by;
                l.end += by;
            }
        }
        if self.pos + by >= 0.0 {
            self.jump(self.pos + by);
        }
    }

    // ---- jog wheel -----------------------------------------------------------

    pub fn jog_touch(&mut self, on: bool) {
        self.touched = on;
        if on {
            if self.slip && self.slip_pos.is_none() {
                self.slip_pos = Some(self.pos);
            }
            self.scratch = Some(Scratch { target: self.pos, x: self.pos, v: self.speed * SRC_RATE });
        } else {
            self.scratch = None;
            if let Some(back) = self.slip_pos.take_if(|_| !self.lp.is_some_and(|l| l.active)) {
                self.jump(back);
            }
        }
    }

    pub fn jog_scratch(&mut self, ticks: f64) {
        match self.scratch.as_mut() {
            Some(s) => s.target += ticks * FRAMES_PER_TICK,
            None => self.jog_bend(ticks),
        }
    }

    pub fn jog_bend(&mut self, ticks: f64) {
        if self.playing {
            self.bend = (self.bend + ticks * 0.0025).clamp(-0.5, 0.5);
        } else if self.track.is_some() {
            // A paused deck moves with the wheel, for finding a spot.
            self.jump(self.pos + ticks * FRAMES_PER_TICK);
        }
    }

    pub fn jog_search(&mut self, ticks: f64) {
        let step = self.grid.map_or(SRC_RATE * 0.06, |g| g.period() / 8.0);
        self.jump(self.pos + ticks * step);
    }

    pub fn reverse_hold(&mut self, on: bool) {
        if on && !self.reverse {
            self.reverse = true;
            if self.slip_pos.is_none() {
                self.slip_pos = Some(self.pos);
            }
        } else if !on && self.reverse {
            self.reverse = false;
            if let Some(back) = self.slip_pos.take() {
                self.jump(back);
            }
        }
    }

    // ---- rendering -------------------------------------------------------------

    #[inline]
    fn frame_at(&self, track: &TrackAudio, len: usize, i: i64) -> (f32, f32) {
        let mut i = i;
        if let Some(l) = self.lp.filter(|l| l.active) {
            let (s, e) = (l.start.floor() as i64, l.end.floor() as i64);
            if i >= e && e > s {
                i = s + (i - e) % (e - s);
            }
        }
        if i < 0 {
            return (0.0, 0.0);
        }
        let i = i as usize;
        let (l, r) = track.frame(i, len);
        // With a stem turned down, take it (scaled) out of the song. The
        // melody is the song minus the other three, so with every gain g:
        // out = g_mel·song + Σ (g_s − g_mel)·stem_s over vocals, drums, bass.
        if self.stem_now != [1.0; 4] && i < self.stems_ready && i < len {
            if let Some(st) = self.stems.as_ref() {
                let [gv, gm, gb, gd] = self.stem_now;
                let (vl, vr) = st.frame(0, i);
                let (dl, dr) = st.frame(1, i);
                let (bl, br) = st.frame(2, i);
                return (
                    gm * l + (gv - gm) * vl + (gd - gm) * dl + (gb - gm) * bl,
                    gm * r + (gv - gm) * vr + (gd - gm) * dr + (gb - gm) * br,
                );
            }
        }
        (l, r)
    }

    #[inline]
    fn read(&self, track: &TrackAudio, len: usize, pos: f64) -> (f32, f32) {
        let i = pos.floor();
        let t = (pos - i) as f32;
        let i = i as i64;
        let a = self.frame_at(track, len, i - 1);
        let b = self.frame_at(track, len, i);
        let c = self.frame_at(track, len, i + 1);
        let d = self.frame_at(track, len, i + 2);
        (hermite(a.0, b.0, c.0, d.0, t), hermite(a.1, b.1, c.1, d.1, t))
    }

    fn mono_at(&self, track: &TrackAudio, len: usize, i: i64) -> f32 {
        let (l, r) = self.frame_at(track, len, i);
        l + r
    }

    /// Where a new grain should start: near `want`, matching `natural`.
    fn best_grain(&self, track: &TrackAudio, len: usize, natural: f64, want: f64) -> f64 {
        const SPAN: i64 = 400;
        const WIN: i64 = 256;
        let nat = natural.round() as i64;
        let mut reference = [0.0f32; (WIN / 2) as usize];
        for (k, j) in (0..WIN).step_by(2).enumerate() {
            reference[k] = self.mono_at(track, len, nat + j);
        }
        let score = |start: i64| -> f32 {
            let mut dot = 0.0;
            let mut energy = 1e-6;
            for (k, j) in (0..WIN).step_by(2).enumerate() {
                let v = self.mono_at(track, len, start + j);
                dot += v * reference[k];
                energy += v * v;
            }
            dot / energy.sqrt()
        };
        let base = want.round() as i64;
        let mut best = (f32::MIN, base);
        let mut d = -SPAN;
        while d <= SPAN {
            let s = score(base + d);
            if s > best.0 {
                best = (s, base + d);
            }
            d += 8;
        }
        let coarse = best.1;
        for d in -7..=7 {
            let s = score(coarse + d);
            if s > best.0 {
                best = (s, coarse + d);
            }
        }
        best.1 as f64
    }

    /// `ratio` is how fast the grains themselves are read (their pitch);
    /// the deck's position moves on at its own speed regardless.
    fn stretched(&mut self, track: &TrackAudio, len: usize, ratio: f64) -> (f32, f32) {
        let h = GRAIN;
        if !self.stretch.active {
            self.stretch = Stretch { active: true, prev: self.pos - h as f64 * ratio, cur: self.pos, k: 0 };
        }
        if self.stretch.k >= h {
            let natural = self.stretch.cur + h as f64 * ratio;
            let next = self.best_grain(track, len, natural, self.pos);
            self.stretch.prev = self.stretch.cur;
            self.stretch.cur = next;
            self.stretch.k = 0;
        }
        let k = self.stretch.k;
        let w = 0.5 - 0.5 * (std::f64::consts::PI * k as f64 / h as f64).cos();
        let a = self.read(track, len, self.stretch.cur + k as f64 * ratio);
        let b = self.read(track, len, self.stretch.prev + (k + h) as f64 * ratio);
        self.stretch.k += 1;
        let w = w as f32;
        (a.0 * w + b.0 * (1.0 - w), a.1 * w + b.1 * (1.0 - w))
    }

    fn target_speed(&mut self, dt: f64) -> f64 {
        // Decays: jog bends fade, scratch release glides.
        self.bend *= (-dt / 0.08).exp();
        if let Some(s) = self.scratch.as_mut() {
            let (alpha, beta) = (0.1, 0.004);
            let predicted = s.x + s.v * dt;
            let r = s.target - predicted;
            s.x = predicted + alpha * r;
            s.v += beta * r / dt;
            return s.v / SRC_RATE;
        }
        let base = if self.motor { self.tempo } else { 0.0 };
        let base = if self.reverse { -base.abs().max(self.tempo) } else { base };
        if base == 0.0 {
            return 0.0;
        }
        base * (1.0 + self.bend + self.nudge) + self.correction
    }

    /// Render `out.len() / 2` stereo frames at output rate `rate`.
    pub fn render(&mut self, out: &mut [f32], rate: f64) {
        out.fill(0.0);
        let Some(track) = self.track.clone() else {
            self.speed = 0.0;
            return;
        };
        let len = track.len();
        let done = track.done.load(Ordering::Acquire);
        let ratio = SRC_RATE / rate;
        self.last_ratio = ratio;
        self.stems_ready = self.stems.as_ref().map_or(0, |s| s.ready());
        // Stem levels glide over about 5 ms, so muting one never clicks.
        let stem_step = (1.0 / (0.005 * rate)) as f32;
        let sub = 64;
        let dt = sub as f64 / rate;
        let key = 2f64.powf(self.key_shift as f64 / 12.0);
        // Start the motor at once; stop it only after fading out.
        if self.playing {
            self.motor = true;
        }
        for (i, f) in out.chunks_exact_mut(2).enumerate() {
            if i % sub == 0 {
                let target = self.target_speed(dt);
                // Coming off the platter, the deck glides back up to speed.
                if self.scratch.is_none() && (target - self.speed).abs() > 0.02 && self.motor && !self.reverse && self.speed.abs() > 0.001 {
                    self.speed += (target - self.speed) * (1.0 - (-dt / 0.03).exp());
                } else {
                    self.speed = target;
                }
            }
            if self.stem_now != self.stem_gain {
                for k in 0..4 {
                    let d = self.stem_gain[k] - self.stem_now[k];
                    self.stem_now[k] += d.clamp(-stem_step, stem_step);
                }
            }
            let want = if self.playing || self.scratch.is_some() { 1.0 } else { 0.0 };
            if self.gain < want {
                self.gain = (self.gain + FADE).min(1.0);
            } else if self.gain > want {
                self.gain = (self.gain - FADE).max(0.0);
                if self.gain == 0.0 {
                    self.motor = false;
                }
            }
            if !self.playing && self.gain == 0.0 && self.scratch.is_none() {
                self.motor = false;
                self.speed = 0.0;
                // Still let the sound we jumped away from fade out.
                if let Some((pos, mix, gain)) = self.tail.as_mut().map(|t| {
                    let at = (t.pos, t.left as f32 / TAIL as f32, t.gain);
                    t.pos += t.speed;
                    t.left -= 1;
                    at
                }) {
                    let (tl, tr) = self.read(&track, len, pos);
                    f[0] = tl * mix * gain;
                    f[1] = tr * mix * gain;
                    if self.tail.as_ref().is_some_and(|t| t.left == 0) {
                        self.tail = None;
                    }
                }
                continue;
            }
            let step = self.speed * ratio;
            // The pitch the song should sound at: its own with key lock,
            // else following the speed; either way moved by the key shift.
            let pitch = if self.keylock { key } else { self.speed * key };
            let stretch = (self.keylock || self.key_shift != 0)
                && self.scratch.is_none()
                && !self.reverse
                && self.speed > 0.25
                && (pitch - self.speed).abs() > 0.002;
            if stretch != self.stretching {
                self.stretching = stretch;
                self.stretch.active = false;
                if self.tail.is_none() && self.gain > 0.0 {
                    self.tail = Some(Tail { pos: self.pos, left: TAIL, speed: step, gain: 1.0 });
                }
            }
            let (mut l, mut r) = if stretch { self.stretched(&track, len, ratio * pitch) } else { self.read(&track, len, self.pos) };
            if let Some(t) = self.tail.as_mut() {
                let mix = t.left as f32 / TAIL as f32;
                let pos = t.pos;
                t.pos += step;
                t.left -= 1;
                let tg = t.gain;
                let (tl, tr) = self.read(&track, len, pos);
                l = l * (1.0 - mix) + tl * mix * tg;
                r = r * (1.0 - mix) + tr * mix * tg;
                if self.tail.as_ref().is_some_and(|t| t.left == 0) {
                    self.tail = None;
                }
            }
            f[0] = l * self.gain;
            f[1] = r * self.gain;
            self.pos += step;
            if let Some(s) = self.slip_pos.as_mut() {
                if self.playing {
                    *s += self.tempo * ratio;
                }
            }
            if let Some(lp) = self.lp.filter(|l| l.active) {
                if step > 0.0 && self.pos >= lp.end {
                    self.pos = lp.start + (self.pos - lp.start) % lp.len();
                }
            }
            if self.pos < 0.0 {
                self.pos = 0.0;
            }
            if self.pos >= len as f64 {
                self.pos = len as f64;
                if done {
                    self.playing = false;
                    self.cue_preview = false;
                    self.hot_preview = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_track(secs: f64) -> Arc<TrackAudio> {
        let t = TrackAudio::new((secs * 1000.0) as u32);
        let n = (secs * SRC_RATE) as usize;
        let data: Vec<f32> = (0..n).flat_map(|i| {
            let v = (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 44_100.0).sin() * 0.5;
            [v, v]
        }).collect();
        t.push(&data);
        t.done.store(true, Ordering::Release);
        Arc::new(t)
    }

    fn run(d: &mut Deck, secs: f64, rate: f64) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = vec![0.0f32; 512];
        for _ in 0..((secs * rate) as usize / 256) {
            d.render(&mut buf, rate);
            all.extend_from_slice(&buf);
        }
        all
    }

    #[test]
    fn plays_at_tempo_and_output_rate() {
        let mut d = Deck::default();
        d.load(tone_track(10.0), None, None, 0.0, [None; 8]);
        d.play();
        run(&mut d, 1.0, 48_000.0);
        assert!((d.pos - SRC_RATE).abs() < 600.0, "{}", d.pos);
        d.pitch = 1.0; // +8%
        d.tempo = d.own_tempo();
        let before = d.pos;
        run(&mut d, 1.0, 48_000.0);
        assert!(((d.pos - before) / SRC_RATE - 1.08).abs() < 0.01);
    }

    #[test]
    fn cdj_cue_behaviour() {
        let mut d = Deck::default();
        d.load(tone_track(20.0), None, None, 0.0, [None; 8]);
        d.jump(SRC_RATE * 2.0);
        // Paused away from cue: CUE sets the cue here.
        d.cue_button(true, false);
        d.cue_button(false, false);
        assert!((d.cue - SRC_RATE * 2.0).abs() < 1.0);
        // Holding CUE at the cue point previews, release returns.
        d.cue_button(true, false);
        run(&mut d, 0.5, 44_100.0);
        assert!(d.pos > d.cue + 10_000.0);
        d.cue_button(false, false);
        run(&mut d, 0.05, 44_100.0);
        assert!(d.at_cue() && !d.playing);
        // Hold CUE, press PLAY, release CUE: keeps playing.
        d.cue_button(true, false);
        d.play();
        d.cue_button(false, false);
        assert!(d.playing);
        // CUE while playing: back to cue and stop.
        run(&mut d, 0.2, 44_100.0);
        d.cue_button(true, false);
        assert!(!d.playing && d.at_cue());
    }

    #[test]
    fn hotcues_and_loops_on_the_grid() {
        let mut d = Deck::default();
        let grid = Grid { bpm: 120.0, anchor: 0.0 };
        d.load(tone_track(30.0), None, Some(grid), 0.0, [None; 8]);
        d.jump(grid.period() * 4.0 + 300.0);
        d.hotcue(0, true, true);
        d.hotcue(0, false, true);
        assert_eq!(d.hotcues[0], Some(grid.period() * 4.0));
        d.jump(grid.period() * 9.0 + 100.0);
        d.play();
        d.auto_loop(4.0, true);
        let l = d.lp.unwrap();
        assert!((l.start - grid.period() * 9.0).abs() < 1.0);
        assert!((l.len() - grid.period() * 4.0).abs() < 1.0);
        run(&mut d, 5.0, 44_100.0);
        assert!(d.pos >= l.start && d.pos < l.end, "stayed in the loop");
        d.loop_resize(0.5);
        assert!((d.lp.unwrap().len() - grid.period() * 2.0).abs() < 1.0);
        d.reloop();
        assert!(!d.lp.unwrap().active);
        // Quantized hot cue jump keeps the beat phase.
        let phase = grid.phase(d.pos);
        d.hotcue(0, true, true);
        assert!((grid.phase(d.pos) - phase).abs() < 1e-6);
    }

    #[test]
    fn scratch_follows_the_platter() {
        let mut d = Deck::default();
        d.load(tone_track(10.0), None, None, 0.0, [None; 8]);
        d.jump(SRC_RATE * 3.0);
        d.jog_touch(true);
        let start = d.pos;
        // Half a turn forward over ~0.25 s, then let it settle.
        for _ in 0..50 {
            d.jog_scratch(7.2);
            run(&mut d, 0.005, 44_100.0);
        }
        run(&mut d, 0.3, 44_100.0);
        let moved = d.pos - start;
        let want = 360.0 * FRAMES_PER_TICK;
        assert!((moved - want).abs() < want * 0.05, "moved {moved}, wanted {want}");
        d.jog_touch(false);
        assert!(!d.playing);
    }

    #[test]
    fn dropping_a_stem_takes_out_just_that_part() {
        // A song made of four tones; each "stem" is one of them.
        let hz = [220.0f32, 330.0, 55.0, 1_000.0]; // vocals, melody, bass, drums
        let tone = |k: usize, i: usize| (i as f32 * 2.0 * std::f32::consts::PI * hz[k] / 44_100.0).sin() * 0.2;
        let secs = 3.0;
        let n = (secs * SRC_RATE) as usize;
        let t = TrackAudio::new((secs * 1000.0) as u32);
        let song: Vec<f32> = (0..n).flat_map(|i| {
            let v: f32 = (0..4).map(|k| tone(k, i)).sum();
            [v, v]
        }).collect();
        t.push(&song);
        t.done.store(true, Ordering::Release);
        let t = Arc::new(t);
        let (v, dr, b) = (|i: usize| (tone(0, i), tone(0, i)), |i: usize| (tone(3, i), tone(3, i)), |i: usize| (tone(2, i), tone(2, i)));
        let stems = Arc::new(Stems::filled(&t, [&v, &dr, &b]));
        let mut d = Deck::default();
        d.load(t.clone(), Some(stems), None, 0.0, [None; 8]);
        d.stem_gain = [1.0, 1.0, 1.0, 0.0]; // no drums
        d.play();
        let out = run(&mut d, 1.0, 44_100.0);
        // After the glide, the output is the song without the 1 kHz tone.
        let start = d.pos as usize - 22_050;
        let frames = &out[out.len() - 2 * 22_050..];
        let mut err = 0.0f32;
        for (j, f) in frames.chunks(2).enumerate() {
            let i = start + j;
            let want = tone(0, i) + tone(1, i) + tone(2, i);
            err = err.max((f[0] - want).abs());
        }
        assert!(err < 0.002, "residual {err}");
    }

    #[test]
    fn key_lock_keeps_time_and_stays_clean() {
        let mut d = Deck::default();
        d.load(tone_track(20.0), None, None, 0.0, [None; 8]);
        d.keylock = true;
        d.pitch = 0.75; // +6%
        d.tempo = d.own_tempo();
        d.play();
        let out = run(&mut d, 4.0, 44_100.0);
        assert!(((d.pos / SRC_RATE) / 4.0 - 1.06).abs() < 0.01, "{}", d.pos / SRC_RATE);
        // Still a 440 Hz tone: count zero crossings over a second.
        let mono: Vec<f32> = out.chunks(2).map(|c| c[0]).skip(44_100).take(44_100).collect();
        let crossings = mono.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((crossings as i32 - 440).abs() <= 6, "{crossings} Hz with key lock");
    }

    #[test]
    fn key_shift_moves_the_pitch_and_not_the_tempo() {
        let hz = |shift: i8, keylock: bool, pitch: f32| -> (f64, f64) {
            let mut d = Deck::default();
            d.load(tone_track(20.0), None, None, 0.0, [None; 8]);
            d.keylock = keylock;
            d.pitch = pitch;
            d.tempo = d.own_tempo();
            d.set_key_shift(shift);
            d.play();
            let out = run(&mut d, 4.0, 44_100.0);
            let mono: Vec<f32> = out.chunks(2).map(|c| c[0]).skip(44_100).take(88_200).collect();
            let crossings = mono.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            (crossings as f64 / 2.0, d.pos / SRC_RATE / 4.0)
        };
        // Up two semitones: 440 Hz becomes ~494 Hz, still at normal speed.
        let (f, speed) = hz(2, true, 0.0);
        assert!((f - 493.9).abs() < 6.0, "{f} Hz");
        assert!((speed - 1.0).abs() < 0.01, "speed {speed}");
        // Down three, with the tempo up 6% and key lock: ~370 Hz at 1.06x.
        let (f, speed) = hz(-3, true, 0.75);
        assert!((f - 370.0).abs() < 6.0, "{f} Hz");
        assert!((speed - 1.06).abs() < 0.01, "speed {speed}");
        // No shift, no key lock: pitch follows the speed, as before.
        let (f, _) = hz(0, false, 0.75);
        assert!((f - 466.4).abs() < 4.0, "{f} Hz");
    }

    #[test]
    fn loop_roll_carries_on_where_the_song_would_be() {
        let mut d = Deck::default();
        let grid = Grid { bpm: 120.0, anchor: 0.0 };
        d.load(tone_track(30.0), None, Some(grid), 0.0, [None; 8]);
        d.jump(grid.period() * 8.0);
        d.play();
        run(&mut d, 0.1, 44_100.0);
        d.loop_roll(1.0, true, true);
        assert!(d.rolling && d.lp.is_some_and(|l| l.active));
        run(&mut d, 3.0, 44_100.0);
        assert!(d.pos < grid.period() * 9.0 + 10.0, "held in the one-beat loop");
        d.loop_roll(1.0, false, true);
        // 3.1 s after beat 8 at 120 BPM is about beat 14.2.
        let beat = grid.beat(d.pos);
        assert!((beat - 14.2).abs() < 0.1, "came out at beat {beat}");
        assert!(!d.rolling && !d.lp.unwrap().active);
    }

    #[test]
    fn memory_cues_store_call_and_delete() {
        let mut d = Deck::default();
        let grid = Grid { bpm: 120.0, anchor: 0.0 };
        d.load(tone_track(60.0), None, Some(grid), 0.0, [None; 8]);
        let beat = |n: f64| grid.period() * n;
        d.jump(beat(16.0));
        assert!(d.mem_store(true));
        d.jump(beat(64.0));
        d.auto_loop(4.0, true);
        assert!(d.mem_store(true), "a loop is saved with its length");
        d.reloop();
        d.jump(0.0);
        assert!(d.mem_call(1));
        assert!((d.pos - beat(16.0)).abs() < 1.0 && (d.cue - beat(16.0)).abs() < 1.0, "paused deck waits there, cue moved");
        assert!(d.mem_call(1));
        assert!((d.pos - beat(64.0)).abs() < 1.0);
        let l = d.lp.unwrap();
        assert!(l.active && (l.len() - beat(4.0)).abs() < 1.0, "the loop came back");
        assert!(!d.mem_call(1), "nothing further on");
        assert!(d.mem_call(-1));
        assert!((d.pos - beat(16.0)).abs() < 1.0 && !d.lp.unwrap().active);
        assert!(d.mem_delete());
        assert_eq!(d.memory.iter().flatten().count(), 1);
    }

    #[test]
    fn back_cue_stops_at_the_cue_point() {
        let mut d = Deck::default();
        d.load(tone_track(20.0), None, None, 4_000.0, [None; 8]);
        d.play();
        run(&mut d, 0.5, 44_100.0);
        d.back_cue();
        run(&mut d, 0.05, 44_100.0);
        assert!(!d.playing && d.at_cue());
    }
}
