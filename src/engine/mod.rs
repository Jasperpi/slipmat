//! The mixer: decks, channel strips, effects, master and headphone cue.
//! Runs inside the sound card's callback; everything reaches it as commands.

pub mod deck;
pub mod dsp;
pub mod fx;
pub mod output;
pub mod sampler;

use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use deck::{Deck, Grid, HOTCUES, Loop, MEMS, Mem};
use sampler::{SLOTS, Sample, Sampler};
use dsp::{Curve, Isolator, Limiter, Meter, SweepFilter};
use fx::{FxKind, Unit};

use crate::stems::{self, Stems};
use crate::track::{SRC_RATE, TrackAudio};

pub const DECKS: usize = 2;
pub const FX_UNITS: usize = 2;
pub const MAX_BLOCK: usize = 1024;

/// Things the interface and the controller ask for.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "c", rename_all = "snake_case")]
pub enum Control {
    Play { d: usize },
    Pause { d: usize },
    PlayPause { d: usize },
    Cue { d: usize, down: bool },
    Start { d: usize },
    Hotcue { d: usize, n: usize, down: bool },
    HotcueClear { d: usize, n: usize },
    Seek { d: usize, frac: f64 },
    Pitch { d: usize, v: f32 },
    Range { d: usize, range: f32 },
    KeyLock { d: usize, on: bool },
    PitchReset { d: usize },
    Nudge { d: usize, v: f64 },
    JogTouch { d: usize, on: bool },
    JogScratch { d: usize, ticks: f64 },
    JogBend { d: usize, ticks: f64 },
    JogSearch { d: usize, ticks: f64 },
    Sync { d: usize, on: Option<bool> },
    Quantize { on: bool },
    Slip { d: usize, on: bool },
    Reverse { d: usize, on: bool },
    AutoLoop { d: usize, beats: f64 },
    LoopIn { d: usize },
    LoopOut { d: usize },
    Reloop { d: usize },
    LoopHalve { d: usize },
    LoopDouble { d: usize },
    BeatJump { d: usize, beats: f64 },
    Trim { d: usize, v: f32 },
    Eq { d: usize, band: usize, v: f32 },
    Filter { d: usize, v: f32 },
    Fader { d: usize, v: f32 },
    Pfl { d: usize, on: bool },
    XfAssign { d: usize, side: i8 },
    Crossfader { v: f32 },
    Curve { curve: Curve },
    Master { v: f32 },
    HeadMix { v: f32 },
    HeadLevel { v: f32 },
    /// Hear the master in the headphones too (the MASTER CUE button).
    MasterCue { on: bool },
    FxKind { u: usize, kind: FxKind },
    FxOn { u: usize, on: bool },
    FxBeats { u: usize, i: usize },
    FxDepth { u: usize, v: f32 },
    FxAssign { u: usize, mask: u8 },
    GridNudge { d: usize, ms: f64 },
    GridDownbeat { d: usize },
    GridBpm { d: usize, bpm: f64 },
    Eject { d: usize },
    AutoGain { on: bool },
    /// Levels of vocals, melody, bass and drums on deck `d`, 0..1 each.
    Stems { d: usize, gains: [f32; 4] },
    /// Stop and go back to the cue point (fader start, pulled back down).
    BackCue { d: usize },
    /// A loop held down that the song carries on past when let go.
    LoopRoll { d: usize, beats: f64, down: bool },
    /// Shift the key by semitones without changing the tempo.
    KeyShift { d: usize, semitones: i8 },
    MemStore { d: usize },
    MemCall { d: usize, dir: i8 },
    MemDelete { d: usize },
    /// Smart Fader: moving between the decks also glides the tempo from one
    /// song's to the other's and swaps their basslines.
    SmartFader { on: bool },
    /// Smart CFX: the filter knobs add an effect on top of the filter.
    SmartCfx { on: bool },
    CfxKind { kind: Cfx },
    /// A sampler pad.
    Sample { n: usize },
    SampleStop { n: usize },
    SamplerVolume { v: f32 },
    /// Instant double: copy the other deck (song, place, tempo) onto `d`.
    Double { d: usize },
}

/// What Smart CFX adds to the filter as its knob is turned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Cfx {
    #[default]
    Echo,
    Space,
    Noise,
    Crush,
    Jet,
}

pub const CFX_ALL: [Cfx; 5] = [Cfx::Echo, Cfx::Space, Cfx::Noise, Cfx::Crush, Cfx::Jet];

impl Cfx {
    pub fn name(self) -> &'static str {
        match self {
            Cfx::Echo => "Dub Echo",
            Cfx::Space => "Space",
            Cfx::Noise => "Noise",
            Cfx::Crush => "Crush",
            Cfx::Jet => "Jet",
        }
    }
}

/// A song taken off a deck, on its way to be freed off the audio thread.
pub struct Garbage(
    #[allow(dead_code)] Option<Arc<TrackAudio>>,
    #[allow(dead_code)] Option<Arc<Stems>>,
    #[allow(dead_code)] Option<Arc<Sample>>,
);

pub enum Cmd {
    Control(Control),
    Load {
        d: usize,
        track: Arc<TrackAudio>,
        stems: Option<Arc<Stems>>,
        grid: Option<Grid>,
        cue: f64,
        hotcues: [Option<f64>; HOTCUES],
        memory: [Option<Mem>; MEMS],
        gain_db: f32,
    },
    /// Put a sound on sampler pad `n` (or clear it).
    Sample { n: usize, sample: Option<Arc<Sample>> },
    /// Analysis finished for the song now on deck `d` (ignored if it changed).
    Analysed { d: usize, track: Arc<TrackAudio>, grid: Grid, cue: Option<f64>, gain_db: f32 },
}

struct Strip {
    trim: f32,
    eq: [f32; 3],
    filter: f32,
    fader: f32,
    pfl: bool,
    /// -1 left, 0 thru, 1 right.
    side: i8,
    auto_db: f32,
    isolator: Isolator,
    sweep: SweepFilter,
    /// Smart CFX's effect on this channel, and its noise generator.
    cfx: Unit,
    noise: u32,
    meter: [Meter; 2],
}

impl Strip {
    fn new(rate: f32, side: i8) -> Self {
        Self {
            trim: 0.5,
            eq: [0.5; 3],
            filter: 0.0,
            fader: 1.0,
            pfl: false,
            side,
            auto_db: 0.0,
            isolator: Isolator::new(rate),
            sweep: SweepFilter::new(rate),
            cfx: Unit::new(rate, FxKind::Echo),
            noise: 0x9e37_79b9,
            meter: [Meter::default(); 2],
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct DeckState {
    pub loaded: bool,
    /// Identifies the song buffer on the deck (changes on every load).
    pub track_key: usize,
    pub pos: f64,
    pub len: f64,
    pub decoded: f64,
    pub done: bool,
    pub failed: bool,
    pub playing: bool,
    pub speed: f64,
    pub tempo: f64,
    pub pitch: f32,
    pub range: f32,
    pub keylock: bool,
    pub sync: bool,
    pub grid: Option<Grid>,
    pub cue: f64,
    pub at_cue: bool,
    pub hotcues: [Option<f64>; HOTCUES],
    pub memory: [Option<Mem>; MEMS],
    pub key_shift: i8,
    pub rolling: bool,
    #[serde(rename = "loop")]
    pub lp: Option<Loop>,
    pub loop_in: Option<f64>,
    pub slip: bool,
    pub reverse: bool,
    pub touched: bool,
    pub trim: f32,
    pub eq: [f32; 3],
    pub filter: f32,
    pub fader: f32,
    pub pfl: bool,
    pub side: i8,
    pub auto_db: f32,
    pub meter: [f32; 2],
    pub stems: [f32; 4],
    /// Frames separated into stems so far, and how that is going.
    pub stems_ready: f64,
    pub stems_status: Option<stems::Status>,
    /// Separated with Demucs (the high-quality model).
    pub stems_hq: bool,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct FxState {
    pub kind: FxKind,
    pub on: bool,
    pub beats: usize,
    pub depth: f32,
    pub assign: u8,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct State {
    pub decks: Vec<DeckState>,
    pub crossfader: f32,
    pub curve: Curve,
    pub master: f32,
    pub head_mix: f32,
    pub head_level: f32,
    pub master_cue: bool,
    pub master_meter: [f32; 2],
    pub limiting: bool,
    pub fx: Vec<FxState>,
    pub quantize: bool,
    pub auto_gain: bool,
    pub smart_fader: bool,
    pub smart_cfx: bool,
    pub cfx: Cfx,
    pub sampler_loaded: [bool; SLOTS],
    pub sampler_playing: [bool; SLOTS],
    pub sampler_volume: f32,
    /// Seconds of audio rendered since start: the clock positions belong to.
    pub clock: f64,
    pub rate: f64,
    /// Share of the time available that mixing takes (worst block of the
    /// last second): over 1.0 means the sound card is being starved.
    pub load: f32,
}

pub struct Engine {
    pub rate: f64,
    decks: Vec<Deck>,
    strips: Vec<Strip>,
    fx: Vec<Unit>,
    crossfader: f32,
    curve: Curve,
    master: f32,
    head_mix: f32,
    head_level: f32,
    master_cue: bool,
    quantize: bool,
    auto_gain: bool,
    smart_fader: bool,
    smart_cfx: bool,
    cfx: Cfx,
    sampler: Sampler,
    /// Tempo (BPM) synced decks follow.
    sync_bpm: Option<f64>,
    limiter: Limiter,
    limiting: f32,
    master_meter: [Meter; 2],
    clock: f64,
    since_publish: f64,
    load_peak: f32,
    load_age: f64,
    load_shown: f32,
    rx: Receiver<Cmd>,
    trash: Sender<Garbage>,
    state: Arc<Mutex<State>>,
    bufs: Vec<Vec<f32>>,
}

/// What the rest of the app holds to talk to the engine.
#[derive(Clone)]
pub struct Handle {
    pub tx: Sender<Cmd>,
    pub state: Arc<Mutex<State>>,
}

impl Handle {
    pub fn send(&self, c: Control) {
        let _ = self.tx.send(Cmd::Control(c));
    }
    pub fn snapshot(&self) -> State {
        self.state.lock().unwrap().clone()
    }
}

impl Engine {
    pub fn new(rate: f64) -> (Self, Handle, Receiver<Garbage>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (trash, garbage) = crossbeam_channel::unbounded();
        let state = Arc::new(Mutex::new(State::default()));
        let r = rate as f32;
        let engine = Self {
            rate,
            decks: (0..DECKS).map(|_| Deck::default()).collect(),
            strips: (0..DECKS).map(|d| Strip::new(r, if d == 0 { -1 } else { 1 })).collect(),
            fx: (0..FX_UNITS).map(|u| {
                let mut unit = Unit::new(r, if u == 0 { FxKind::Echo } else { FxKind::Reverb });
                unit.assign = 1 << u;
                unit
            }).collect(),
            crossfader: 0.5,
            curve: Curve::Smooth,
            master: 0.8,
            head_mix: 0.0,
            head_level: 0.8,
            master_cue: false,
            quantize: true,
            auto_gain: true,
            smart_fader: false,
            smart_cfx: false,
            cfx: Cfx::default(),
            sampler: Sampler::default(),
            sync_bpm: None,
            limiter: Limiter::new(r),
            limiting: 1.0,
            master_meter: [Meter::default(); 2],
            clock: 0.0,
            since_publish: 1.0,
            load_peak: 0.0,
            load_age: 0.0,
            load_shown: 0.0,
            rx,
            trash,
            state: state.clone(),
            bufs: (0..DECKS + 2).map(|_| vec![0.0; MAX_BLOCK * 2]).collect(),
        };
        (engine, Handle { tx, state }, garbage)
    }

    /// Move to a new output rate (a different sound card), keeping every setting.
    pub fn set_rate(&mut self, rate: f64) {
        if (rate - self.rate).abs() < 1.0 {
            return;
        }
        self.rate = rate;
        let r = rate as f32;
        for s in &mut self.strips {
            s.isolator = Isolator::new(r);
            s.sweep = SweepFilter::new(r);
            s.cfx = Unit::new(r, FxKind::Echo);
        }
        for u in &mut self.fx {
            let mut fresh = Unit::new(r, u.kind);
            (fresh.on, fresh.beats, fresh.depth, fresh.assign) = (u.on, u.beats, u.depth, u.assign);
            *u = fresh;
        }
        self.limiter = Limiter::new(r);
    }

    /// Hand a song (and its stems) to another thread to free: freeing a
    /// few hundred megabytes here would stall the sound card.
    fn discard(&self, (track, stems): (Option<Arc<TrackAudio>>, Option<Arc<Stems>>)) {
        // Stop separating it, unless the other deck plays the same song
        // (an instant double shares the stems).
        if let Some(s) = stems.as_ref().filter(|s| !self.decks.iter().any(|k| k.stems.as_ref().is_some_and(|x| Arc::ptr_eq(x, s)))) {
            s.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = self.trash.send(Garbage(track, stems, None));
    }

    fn other(d: usize) -> usize {
        (d + 1) % DECKS
    }

    /// Engage sync on `d`: match the tempo of the deck it follows, and line up the beats.
    fn sync_on(&mut self, d: usize) {
        let Some(g) = self.decks[d].grid else { return };
        let o = Self::other(d);
        let leader = &self.decks[o];
        let lead_bpm = if leader.sync { self.sync_bpm } else { None }
            .or_else(|| leader.track.as_ref().and(leader.bpm()));
        self.sync_bpm = Some(lead_bpm.unwrap_or(g.bpm * self.decks[d].tempo));
        self.decks[d].sync = true;
        self.apply_sync();
        // A waiting deck stays exactly on its cue point: it is lined up
        // with the beat when it starts, not before.
        if self.decks[d].playing {
            self.align(d);
        }
    }

    /// Smart Fader. Returns each deck's bass level.
    fn smart(&mut self, xl: f32, xr: f32) -> [f32; DECKS] {
        let low = [1.0; DECKS];
        if !self.smart_fader {
            return low;
        }
        let level = |d: usize| {
            let s = &self.strips[d];
            dsp::fader_gain(s.fader) * match s.side {
                -1 => xl,
                1 => xr,
                _ => 1.0,
            }
        };
        let (ga, gb) = (level(0), level(1));
        let grid = |d: usize| self.decks[d].grid.filter(|_| self.decks[d].track.is_some());
        let (Some(a), Some(b)) = (grid(0), grid(1)) else { return low };
        if ga + gb <= 0.0 {
            return low;
        }
        // How far the mix has moved from deck 1 (0) to deck 2 (1).
        let m = (gb / (ga + gb)) as f64;
        let from = a.bpm * self.decks[0].own_tempo();
        let mut to = b.bpm * self.decks[1].own_tempo();
        while to > from * 1.5 {
            to /= 2.0;
        }
        while to < from * 0.67 {
            to *= 2.0;
        }
        self.sync_bpm = Some(from + (to - from) * m);
        self.decks[0].sync = true;
        self.decks[1].sync = true;
        if !(self.decks[0].playing && self.decks[1].playing) {
            return low;
        }
        // Basslines swap around the middle, so two never play at once.
        let x = ((m - 0.35) / 0.3).clamp(0.0, 1.0);
        let swap = (x * x * (3.0 - 2.0 * x)) as f32;
        [1.0 - swap, swap]
    }

    /// Put `d`'s beats on the other deck's, if that one is playing.
    fn align(&mut self, d: usize) {
        let o = Self::other(d);
        let (Some(fg), Some(lg)) = (self.decks[d].grid, self.decks[o].grid) else { return };
        if !self.decks[o].playing || self.decks[o].track.is_none() {
            return;
        }
        let lead_phase = lg.phase(self.decks[o].pos);
        // Account for half/double tempo pairs: compare in the follower's beats.
        let ratio = (fg.bpm * self.decks[d].tempo) / (lg.bpm * self.decks[o].tempo);
        let lead_phase = if ratio > 1.5 { (lead_phase * 2.0).fract() } else { lead_phase };
        let mut diff = lead_phase - fg.phase(self.decks[d].pos);
        if diff > 0.5 {
            diff -= 1.0;
        }
        if diff < -0.5 {
            diff += 1.0;
        }
        let mut to = self.decks[d].pos + diff * fg.period();
        if to < 0.0 {
            to += fg.period(); // can't go before the start: the next beat instead
        }
        self.decks[d].jump(to);
    }

    fn sync_tempo_for(bpm: f64, target: f64) -> f64 {
        // Match at the nearest octave: a 87 BPM song follows 174 at half speed.
        let mut t = target / bpm;
        while t > 1.5 {
            t /= 2.0;
        }
        while t < 0.67 {
            t *= 2.0;
        }
        t
    }

    fn apply_sync(&mut self) {
        for d in 0..DECKS {
            let deck = &mut self.decks[d];
            if deck.sync {
                if let (Some(g), Some(bpm)) = (deck.grid, self.sync_bpm) {
                    deck.tempo = Self::sync_tempo_for(g.bpm, bpm);
                    continue;
                }
            }
            deck.tempo = deck.own_tempo();
        }
    }

    /// Small speed corrections keep synced decks on the beat (after a nudge,
    /// for instance). Returns nothing; sets each deck's `correction`.
    fn phase_lock(&mut self) {
        for d in 0..DECKS {
            self.decks[d].correction = 0.0;
            let o = Self::other(d);
            let (f, l) = (&self.decks[d], &self.decks[o]);
            if !f.sync || !f.playing || f.touched || !l.playing || l.track.is_none() {
                continue;
            }
            let (Some(fg), Some(lg)) = (f.grid, l.grid) else { continue };
            // The leader is whichever deck has played longer: the one that
            // isn't synced, or deck 1 when both are.
            if l.sync && d < o {
                continue;
            }
            let ratio = (fg.bpm * f.tempo) / (lg.bpm * l.tempo);
            // Only hold the beats together when the tempos agree.
            if ![0.5, 1.0, 2.0].iter().any(|r| (ratio / r - 1.0).abs() < 0.002) {
                continue;
            }
            let lead = if ratio > 1.5 { (lg.phase(l.pos) * 2.0).fract() } else { lg.phase(l.pos) };
            let mut err = lead - fg.phase(f.pos);
            if err > 0.5 {
                err -= 1.0;
            }
            if err < -0.5 {
                err += 1.0;
            }
            if err.abs() > 0.004 {
                self.decks[d].correction = (err * 0.8).clamp(-0.03, 0.03);
            }
        }
    }

    fn control(&mut self, c: Control) {
        use Control::*;
        let q = self.quantize;
        let ok = |d: usize| d < DECKS;
        match c {
            Play { d } if ok(d) => self.decks[d].play(),
            Pause { d } if ok(d) => self.decks[d].pause(),
            PlayPause { d } if ok(d) => self.decks[d].play_pause(),
            Cue { d, down } if ok(d) => self.decks[d].cue_button(down, q),
            Start { d } if ok(d) => self.decks[d].jump(0.0),
            Hotcue { d, n, down } if ok(d) => self.decks[d].hotcue(n, down, q),
            HotcueClear { d, n } if ok(d) && n < HOTCUES => self.decks[d].hotcues[n] = None,
            Seek { d, frac } if ok(d) => self.decks[d].seek_fraction(frac),
            Pitch { d, v } if ok(d) => {
                let deck = &mut self.decks[d];
                deck.pitch = v.clamp(-1.0, 1.0);
                if deck.sync {
                    // Moving a synced deck's tempo moves every synced deck.
                    if let Some(g) = deck.grid {
                        self.sync_bpm = Some(g.bpm * deck.own_tempo());
                    }
                }
            }
            Range { d, range } if ok(d) => {
                let deck = &mut self.decks[d];
                // Keep the speed, re-expressed on the new range.
                let tempo = deck.own_tempo();
                deck.range = range.clamp(0.02, 1.0);
                deck.pitch = ((tempo - 1.0) / deck.range as f64).clamp(-1.0, 1.0) as f32;
            }
            KeyLock { d, on } if ok(d) => self.decks[d].keylock = on,
            PitchReset { d } if ok(d) => {
                self.decks[d].pitch = 0.0;
                if self.decks[d].sync {
                    self.sync_bpm = self.decks[d].grid.map(|g| g.bpm);
                }
            }
            Nudge { d, v } if ok(d) => self.decks[d].nudge = v.clamp(-0.2, 0.2),
            JogTouch { d, on } if ok(d) => self.decks[d].jog_touch(on),
            JogScratch { d, ticks } if ok(d) => self.decks[d].jog_scratch(ticks),
            JogBend { d, ticks } if ok(d) => self.decks[d].jog_bend(ticks),
            JogSearch { d, ticks } if ok(d) => self.decks[d].jog_search(ticks),
            Sync { d, on } if ok(d) => {
                let on = on.unwrap_or(!self.decks[d].sync);
                if on {
                    self.sync_on(d);
                } else {
                    let deck = &mut self.decks[d];
                    // Keep the tempo it was synced to.
                    deck.sync = false;
                    deck.pitch = ((deck.tempo - 1.0) / deck.range as f64).clamp(-1.0, 1.0) as f32;
                }
            }
            Quantize { on } => self.quantize = on,
            Slip { d, on } if ok(d) => self.decks[d].slip = on,
            Reverse { d, on } if ok(d) => self.decks[d].reverse_hold(on),
            AutoLoop { d, beats } if ok(d) => self.decks[d].auto_loop(beats, q),
            LoopIn { d } if ok(d) => self.decks[d].loop_in_button(q),
            LoopOut { d } if ok(d) => self.decks[d].loop_out_button(q),
            Reloop { d } if ok(d) => self.decks[d].reloop(),
            LoopHalve { d } if ok(d) => self.decks[d].loop_resize(0.5),
            LoopDouble { d } if ok(d) => self.decks[d].loop_resize(2.0),
            BeatJump { d, beats } if ok(d) => self.decks[d].beat_jump(beats),
            Trim { d, v } if ok(d) => self.strips[d].trim = v.clamp(0.0, 1.0),
            Eq { d, band, v } if ok(d) && band < 3 => self.strips[d].eq[band] = v.clamp(0.0, 1.0),
            Filter { d, v } if ok(d) => self.strips[d].filter = v.clamp(-1.0, 1.0),
            Fader { d, v } if ok(d) => self.strips[d].fader = v.clamp(0.0, 1.0),
            Pfl { d, on } if ok(d) => self.strips[d].pfl = on,
            XfAssign { d, side } if ok(d) => self.strips[d].side = side.clamp(-1, 1),
            Crossfader { v } => self.crossfader = v.clamp(0.0, 1.0),
            Curve { curve } => self.curve = curve,
            Master { v } => self.master = v.clamp(0.0, 1.0),
            HeadMix { v } => self.head_mix = v.clamp(0.0, 1.0),
            HeadLevel { v } => self.head_level = v.clamp(0.0, 1.0),
            MasterCue { on } => self.master_cue = on,
            FxKind { u, kind } if u < FX_UNITS => self.fx[u].set_kind(kind),
            FxOn { u, on } if u < FX_UNITS => self.fx[u].on = on,
            FxBeats { u, i } if u < FX_UNITS => self.fx[u].beats = i.min(fx::BEATS.len() - 1),
            FxDepth { u, v } if u < FX_UNITS => self.fx[u].depth = v.clamp(0.0, 1.0),
            FxAssign { u, mask } if u < FX_UNITS => self.fx[u].assign = mask,
            GridNudge { d, ms } if ok(d) => {
                if let Some(g) = self.decks[d].grid.as_mut() {
                    g.anchor += ms / 1000.0 * SRC_RATE;
                }
            }
            GridDownbeat { d } if ok(d) => {
                let deck = &mut self.decks[d];
                if let Some(g) = deck.grid.as_mut() {
                    g.anchor = deck.pos;
                }
            }
            GridBpm { d, bpm } if ok(d) && (40.0..=250.0).contains(&bpm) => {
                let deck = &mut self.decks[d];
                match deck.grid.as_mut() {
                    Some(g) => {
                        // Keep the beat under the playhead where it is.
                        let beat = g.beat(deck.pos);
                        g.bpm = bpm;
                        g.anchor = deck.pos - beat * g.period();
                    }
                    None => deck.grid = Some(Grid { bpm, anchor: deck.pos }),
                }
            }
            Eject { d } if ok(d) && !self.decks[d].playing => {
                let old = self.decks[d].eject();
                self.discard(old);
            }
            AutoGain { on } => self.auto_gain = on,
            Stems { d, gains } if ok(d) => self.decks[d].stem_gain = gains.map(|g| g.clamp(0.0, 1.0)),
            BackCue { d } if ok(d) => self.decks[d].back_cue(),
            LoopRoll { d, beats, down } if ok(d) => self.decks[d].loop_roll(beats, down, q),
            KeyShift { d, semitones } if ok(d) => self.decks[d].set_key_shift(semitones),
            MemStore { d } if ok(d) => {
                self.decks[d].mem_store(q);
            }
            MemCall { d, dir } if ok(d) => {
                self.decks[d].mem_call(dir);
            }
            MemDelete { d } if ok(d) => {
                self.decks[d].mem_delete();
            }
            SmartFader { on } => {
                if self.smart_fader && !on {
                    // Each deck goes back to its own tempo fader.
                    for deck in &mut self.decks {
                        deck.sync = false;
                    }
                }
                self.smart_fader = on;
            }
            SmartCfx { on } => self.smart_cfx = on,
            CfxKind { kind } => self.cfx = kind,
            Sample { n } => self.sampler.trigger(n),
            SampleStop { n } => self.sampler.stop(n),
            SamplerVolume { v } => self.sampler.volume = v.clamp(0.0, 1.0),
            Double { d } if ok(d) && !self.decks[d].playing => {
                let o = Self::other(d);
                let src = &self.decks[o];
                let Some(track) = src.track.clone() else { return };
                let (stems, grid, cue, hotcues, memory) = (src.stems.clone(), src.grid, src.cue, src.hotcues, src.memory);
                let (pos, playing, sync) = (src.pos, src.playing, src.sync);
                let (pitch, range, keylock, key_shift, tempo) = (src.pitch, src.range, src.keylock, src.key_shift, src.tempo);
                let old = self.decks[d].load(track, stems, grid, cue, hotcues);
                let deck = &mut self.decks[d];
                deck.memory = memory;
                (deck.pitch, deck.range, deck.keylock, deck.key_shift, deck.tempo, deck.sync) = (pitch, range, keylock, key_shift, tempo, sync);
                deck.pos = pos;
                if playing {
                    deck.play();
                }
                self.strips[d].auto_db = self.strips[o].auto_db;
                self.discard(old);
            }
            _ => {}
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Control(c) => self.control(c),
            Cmd::Sample { n, sample } => {
                let old = self.sampler.set(n, sample);
                let _ = self.trash.send(Garbage(None, None, old));
            }
            Cmd::Load { d, track, stems, grid, cue, hotcues, memory, gain_db } if d < DECKS => {
                if self.decks[d].playing {
                    return; // never swap the song out from under a playing deck
                }
                let was_synced = self.decks[d].sync;
                let old = self.decks[d].load(track, stems, grid, cue, hotcues);
                self.decks[d].memory = memory;
                self.strips[d].auto_db = gain_db;
                self.discard(old);
                if was_synced {
                    self.sync_on(d);
                }
            }
            Cmd::Analysed { d, track, grid, cue, gain_db } if d < DECKS => {
                let deck = &mut self.decks[d];
                let same = deck.track.as_ref().is_some_and(|t| Arc::ptr_eq(t, &track));
                if !same {
                    return;
                }
                if deck.grid.is_none() {
                    deck.grid = Some(grid);
                }
                if let Some(cue) = cue {
                    if !deck.playing && deck.at_cue() && deck.cue == 0.0 {
                        deck.cue = cue;
                        deck.jump(cue);
                    }
                }
                self.strips[d].auto_db = gain_db;
                if deck.sync {
                    self.sync_on(d);
                }
            }
            _ => {}
        }
    }

    /// Fill `master` and `cue` (interleaved stereo, same length).
    pub fn render(&mut self, master: &mut [f32], cue: &mut [f32]) {
        let frames = master.len() / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            let (m, c) = (&mut master[done * 2..(done + n) * 2], &mut cue[done * 2..(done + n) * 2]);
            self.block(m, c);
            done += n;
        }
    }

    fn block(&mut self, master: &mut [f32], cue: &mut [f32]) {
        let began = std::time::Instant::now();
        while let Ok(cmd) = self.rx.try_recv() {
            self.command(cmd);
        }
        let frames = master.len() / 2;
        let secs = frames as f64 / self.rate;
        let (xl, xr) = dsp::crossfade(self.crossfader, self.curve);
        let bass = self.smart(xl, xr);
        self.apply_sync();
        // A deck that just started playing while synced lands on the beat.
        for d in 0..DECKS {
            if std::mem::take(&mut self.decks[d].started) && self.decks[d].sync {
                self.align(d);
            }
        }
        self.phase_lock();
        master.fill(0.0);
        cue.fill(0.0);
        let mut bufs = std::mem::take(&mut self.bufs);
        for d in 0..DECKS {
            let buf = &mut bufs[d][..frames * 2];
            self.decks[d].render(buf, self.rate);
            let s = &mut self.strips[d];
            let auto = if self.auto_gain { dsp::db(s.auto_db) } else { 1.0 };
            let gain = dsp::trim_gain(s.trim) * auto;
            for v in buf.iter_mut() {
                *v *= gain;
            }
            s.isolator.run(buf, [dsp::eq_gain(s.eq[0]) * bass[d], dsp::eq_gain(s.eq[1]), dsp::eq_gain(s.eq[2])]);
            let beat = self.decks[d].bpm().map_or(0.5, |b| 60.0 / b) as f32;
            // Smart CFX: how far the knob is from the middle drives an
            // effect as well as the filter.
            let amount = if self.smart_cfx { ((s.filter.abs() - 0.02) / 0.98).max(0.0) } else { 0.0 };
            if amount > 0.0 && self.cfx == Cfx::Noise {
                // A hiss that swells with the knob, swept by the filter.
                let g = amount * amount * 0.22;
                for v in buf.iter_mut() {
                    s.noise ^= s.noise << 13;
                    s.noise ^= s.noise >> 17;
                    s.noise ^= s.noise << 5;
                    *v += (s.noise as f32 / u32::MAX as f32 * 2.0 - 1.0) * g;
                }
            }
            s.sweep.run(buf, s.filter);
            let (kind, beats) = match self.cfx {
                Cfx::Echo | Cfx::Noise => (FxKind::LowCutEcho, 3),
                Cfx::Space => (FxKind::Reverb, 6),
                Cfx::Crush => (FxKind::Crush, 4),
                Cfx::Jet => (FxKind::Flanger, 6),
            };
            let on = amount > 0.01 && self.cfx != Cfx::Noise;
            if on {
                s.cfx.set_kind(kind);
                s.cfx.beats = beats;
                s.cfx.depth = amount;
            }
            s.cfx.on = on;
            if s.cfx.active() {
                s.cfx.run(buf, beat);
            }
            // Effects on this deck, timed by its beat.
            for u in &mut self.fx {
                if u.assign & (1 << d) != 0 {
                    u.run(buf, beat);
                }
            }
            // Channel meters read before the fader, like a real mixer's.
            let mut peak = [0.0f32; 2];
            for f in buf.chunks_exact(2) {
                peak[0] = peak[0].max(f[0].abs());
                peak[1] = peak[1].max(f[1].abs());
            }
            for c in 0..2 {
                s.meter[c].feed(&[peak[c]], secs as f32);
            }
            if s.pfl {
                for (o, v) in cue.iter_mut().zip(buf.iter()) {
                    *o += *v;
                }
            }
            let side = match s.side {
                -1 => xl,
                1 => xr,
                _ => 1.0,
            };
            let g = dsp::fader_gain(s.fader) * side;
            if g > 0.0 {
                for (o, v) in master.iter_mut().zip(buf.iter()) {
                    *o += *v * g;
                }
            }
        }
        self.sampler.render(master, self.rate);
        // Master effects, timed by whichever deck is loudest in the mix.
        let lead = (0..DECKS)
            .filter(|&d| self.decks[d].playing)
            .max_by(|&a, &b| self.strips[a].fader.partial_cmp(&self.strips[b].fader).unwrap())
            .unwrap_or(0);
        let beat = self.decks[lead].bpm().map_or(0.5, |b| 60.0 / b) as f32;
        for u in &mut self.fx {
            if u.assign & fx::MASTER != 0 {
                u.run(master, beat);
            }
        }
        let mg = dsp::fader_gain(self.master);
        for v in master.iter_mut() {
            *v *= mg;
        }
        self.limiting = self.limiter.run(master);
        let mut peak = [0.0f32; 2];
        for f in master.chunks_exact(2) {
            peak[0] = peak[0].max(f[0].abs());
            peak[1] = peak[1].max(f[1].abs());
        }
        for c in 0..2 {
            self.master_meter[c].feed(&[peak[c]], secs as f32);
        }
        // MASTER CUE puts the master in the headphones' cue side as well.
        if self.master_cue {
            for (c, m) in cue.iter_mut().zip(master.iter()) {
                *c += *m;
            }
        }
        // Headphones: the cued channels blended with the master.
        let (cg, mgain) = ((1.0 - self.head_mix).sqrt(), self.head_mix.sqrt());
        let hl = dsp::fader_gain(self.head_level) * 1.2;
        for (c, m) in cue.iter_mut().zip(master.iter()) {
            *c = ((*c * cg + *m * mgain) * hl).clamp(-1.0, 1.0);
        }
        self.bufs = bufs;
        let load = (began.elapsed().as_secs_f64() / secs) as f32;
        self.load_peak = self.load_peak.max(load);
        self.load_age += secs;
        if self.load_age >= 1.0 {
            self.load_shown = self.load_peak;
            self.load_peak = 0.0;
            self.load_age = 0.0;
        }
        self.clock += secs;
        self.since_publish += secs;
        if self.since_publish >= 0.01 {
            if let Ok(mut st) = self.state.try_lock() {
                self.publish(&mut st);
                self.since_publish = 0.0;
            }
        }
    }

    fn publish(&self, st: &mut State) {
        st.decks.resize_with(DECKS, DeckState::default);
        for d in 0..DECKS {
            let deck = &self.decks[d];
            let s = &self.strips[d];
            let t = deck.track.as_ref();
            st.decks[d] = DeckState {
                loaded: t.is_some(),
                track_key: t.map_or(0, |t| Arc::as_ptr(t) as usize),
                pos: deck.pos,
                len: deck.len(),
                decoded: t.map_or(0.0, |t| t.len() as f64),
                done: t.is_some_and(|t| t.done.load(std::sync::atomic::Ordering::Relaxed)),
                failed: t.is_some_and(|t| t.failed.load(std::sync::atomic::Ordering::Relaxed)),
                playing: deck.playing,
                speed: deck.speed,
                tempo: deck.tempo,
                pitch: deck.pitch,
                range: deck.range,
                keylock: deck.keylock,
                sync: deck.sync,
                grid: deck.grid,
                cue: deck.cue,
                at_cue: deck.at_cue(),
                hotcues: deck.hotcues,
                memory: deck.memory,
                key_shift: deck.key_shift,
                rolling: deck.rolling,
                lp: deck.lp,
                loop_in: deck.loop_in,
                slip: deck.slip,
                reverse: deck.reverse,
                touched: deck.touched,
                trim: s.trim,
                eq: s.eq,
                filter: s.filter,
                fader: s.fader,
                pfl: s.pfl,
                side: s.side,
                auto_db: s.auto_db,
                stems: deck.stem_gain,
                stems_ready: deck.stems.as_ref().map_or(0.0, |s| s.ready() as f64),
                stems_status: deck.stems.as_ref().map(|s| s.status()),
                stems_hq: deck.stems.as_ref().is_some_and(|s| s.model == stems::Model::Demucs),
                meter: [s.meter[0].level, s.meter[1].level],
            };
        }
        st.crossfader = self.crossfader;
        st.curve = self.curve;
        st.master = self.master;
        st.head_mix = self.head_mix;
        st.head_level = self.head_level;
        st.master_cue = self.master_cue;
        st.master_meter = [self.master_meter[0].level, self.master_meter[1].level];
        st.limiting = self.limiting < 0.97;
        // Written in place: this runs in the audio callback, which must not allocate.
        st.fx.resize_with(self.fx.len(), FxState::default);
        for (slot, u) in st.fx.iter_mut().zip(&self.fx) {
            *slot = FxState { kind: u.kind, on: u.on, beats: u.beats, depth: u.depth, assign: u.assign };
        }
        st.quantize = self.quantize;
        st.auto_gain = self.auto_gain;
        st.smart_fader = self.smart_fader;
        st.smart_cfx = self.smart_cfx;
        st.cfx = self.cfx;
        st.sampler_loaded = self.sampler.loaded();
        st.sampler_playing = self.sampler.playing();
        st.sampler_volume = self.sampler.volume;
        st.clock = self.clock;
        st.load = self.load_shown;
        st.rate = self.rate;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn click_track(bpm: f64, secs: f64) -> Arc<TrackAudio> {
        let t = TrackAudio::new((secs * 1000.0) as u32);
        let n = (secs * SRC_RATE) as usize;
        let period = SRC_RATE * 60.0 / bpm;
        let data: Vec<f32> = (0..n).flat_map(|i| {
            let ph = (i as f64 % period) / SRC_RATE;
            let v = if ph < 0.02 { ((ph * 2.0 * std::f64::consts::PI * 100.0).sin() * 0.6) as f32 } else { 0.0 };
            [v, v]
        }).collect();
        t.push(&data);
        t.done.store(true, Ordering::Release);
        Arc::new(t)
    }

    fn render(e: &mut Engine, secs: f64) -> Vec<f32> {
        let n = 256;
        let mut out = Vec::new();
        let mut m = vec![0.0; n * 2];
        let mut c = vec![0.0; n * 2];
        for _ in 0..(secs * e.rate) as usize / n {
            e.render(&mut m, &mut c);
            out.extend_from_slice(&m);
        }
        out
    }

    #[test]
    fn sync_matches_tempo_and_phase() {
        let (mut e, h, _g) = Engine::new(48_000.0);
        let a = click_track(124.0, 60.0);
        let b = click_track(120.0, 60.0);
        let ga = Grid { bpm: 124.0, anchor: 0.0 };
        let gb = Grid { bpm: 120.0, anchor: 0.0 };
        h.tx.send(Cmd::Load { d: 0, track: a, stems: None, grid: Some(ga), cue: 0.0, hotcues: [None; 8], memory: [None; MEMS], gain_db: 0.0 }).unwrap();
        h.tx.send(Cmd::Load { d: 1, track: b, stems: None, grid: Some(gb), cue: 1234.0, hotcues: [None; 8], memory: [None; MEMS], gain_db: 0.0 }).unwrap();
        h.send(Control::Play { d: 0 });
        render(&mut e, 2.3);
        h.send(Control::Sync { d: 1, on: Some(true) });
        h.send(Control::Play { d: 1 });
        render(&mut e, 0.5);
        let st = h.snapshot();
        let (d0, d1) = (&st.decks[0], &st.decks[1]);
        assert!(d1.sync);
        assert!((d1.tempo * 120.0 - 124.0).abs() < 0.01, "tempo {}", d1.tempo);
        let p0 = ga.phase(d0.pos);
        let p1 = gb.phase(d1.pos);
        let diff = (p0 - p1).abs().min(1.0 - (p0 - p1).abs());
        assert!(diff < 0.01, "phase off by {diff} beats");
        // Moving the synced deck's tempo moves the shared sync tempo.
        h.send(Control::Pitch { d: 1, v: 0.5 });
        render(&mut e, 0.1);
        let st = h.snapshot();
        assert!((st.decks[1].tempo - 1.04).abs() < 0.001, "{}", st.decks[1].tempo);
    }

    #[test]
    fn crossfader_and_cue_routing() {
        let (mut e, h, _g) = Engine::new(44_100.0);
        h.tx.send(Cmd::Load { d: 0, track: click_track(120.0, 10.0), stems: None, grid: None, cue: 0.0, hotcues: [None; 8], memory: [None; MEMS], gain_db: 0.0 }).unwrap();
        h.send(Control::Play { d: 0 });
        h.send(Control::Crossfader { v: 1.0 });
        h.send(Control::Curve { curve: Curve::Sharp });
        let out = render(&mut e, 1.0);
        assert!(out.iter().all(|v| v.abs() < 1e-6), "deck 1 is cut by the crossfader");
        h.send(Control::Pfl { d: 0, on: true });
        let mut m = vec![0.0; 512];
        let mut c = vec![0.0; 512];
        let mut heard = 0.0f32;
        for _ in 0..200 {
            e.render(&mut m, &mut c);
            heard = heard.max(c.iter().fold(0.0, |a, v| a.max(v.abs())));
        }
        assert!(heard > 0.1, "cue bus carries the deck: {heard}");
    }

    fn load(h: &Handle, d: usize, bpm: f64, cue: f64) -> Grid {
        let grid = Grid { bpm, anchor: 0.0 };
        h.tx.send(Cmd::Load { d, track: click_track(bpm, 60.0), stems: None, grid: Some(grid), cue, hotcues: [None; 8], memory: [None; MEMS], gain_db: 0.0 }).unwrap();
        grid
    }

    #[test]
    fn a_waiting_synced_deck_stays_on_its_cue() {
        // The pre-cue case: deck 1 plays, deck 2 waits at its cue with SYNC
        // on. Turning sync on (or loading with it on) must not move deck 2
        // off the cue point; it is lined up when it starts.
        let (mut e, h, _g) = Engine::new(44_100.0);
        load(&h, 0, 124.0, 0.0);
        let gb = load(&h, 1, 120.0, 5_000.0);
        h.send(Control::Play { d: 0 });
        render(&mut e, 1.7);
        h.send(Control::Sync { d: 1, on: Some(true) });
        render(&mut e, 0.3);
        let st = h.snapshot();
        assert!(st.decks[1].at_cue && (st.decks[1].pos - 5_000.0).abs() < 1.0, "moved to {}", st.decks[1].pos);
        assert!((st.decks[1].tempo * 120.0 - 124.0).abs() < 0.01, "tempo already matched");
        // Loading another song onto the synced deck: same.
        load(&h, 1, 120.0, 7_000.0);
        render(&mut e, 0.2);
        let st = h.snapshot();
        assert!(st.decks[1].sync && st.decks[1].at_cue, "at {}", st.decks[1].pos);
        // Holding CUE previews from the cue, in time; letting go returns.
        h.send(Control::Cue { d: 1, down: true });
        render(&mut e, 0.5);
        let st = h.snapshot();
        let (p0, p1) = (Grid { bpm: 124.0, anchor: 0.0 }.phase(st.decks[0].pos), gb.phase(st.decks[1].pos));
        let diff = (p0 - p1).abs().min(1.0 - (p0 - p1).abs());
        assert!(st.decks[1].playing && diff < 0.02, "preview {diff} beats off");
        h.send(Control::Cue { d: 1, down: false });
        render(&mut e, 0.1);
        let st = h.snapshot();
        assert!(!st.decks[1].playing && (st.decks[1].pos - 7_000.0).abs() < 1.0);
    }

    #[test]
    fn headphones_hear_the_cued_deck_before_the_room_does() {
        let (mut e, h, _g) = Engine::new(44_100.0);
        load(&h, 0, 124.0, 0.0);
        load(&h, 1, 120.0, 0.0);
        h.send(Control::Play { d: 0 });
        // Deck 2: fader down, headphone cue on, playing.
        h.send(Control::Fader { d: 1, v: 0.0 });
        h.send(Control::Pfl { d: 1, on: true });
        h.send(Control::Play { d: 1 });
        h.send(Control::Fader { d: 0, v: 0.0 });
        let mut m = vec![0.0; 512];
        let mut c = vec![0.0; 512];
        let (mut room, mut phones) = (0.0f32, 0.0f32);
        for _ in 0..300 {
            e.render(&mut m, &mut c);
            room = room.max(m.iter().fold(0.0, |a, v| a.max(v.abs())));
            phones = phones.max(c.iter().fold(0.0, |a, v| a.max(v.abs())));
        }
        assert!(room < 1e-6, "nothing in the room with both faders down: {room}");
        assert!(phones > 0.1, "deck 2 in the headphones: {phones}");
    }

    #[test]
    fn instant_double_copies_the_other_deck() {
        let (mut e, h, _g) = Engine::new(44_100.0);
        load(&h, 0, 124.0, 0.0);
        h.send(Control::Pitch { d: 0, v: 0.5 });
        h.send(Control::Play { d: 0 });
        render(&mut e, 1.0);
        h.send(Control::Double { d: 1 });
        render(&mut e, 0.5);
        let st = h.snapshot();
        let (a, b) = (&st.decks[0], &st.decks[1]);
        assert!(b.loaded && b.playing && b.track_key == a.track_key);
        assert!((a.pos - b.pos).abs() < 2.0, "{} vs {}", a.pos, b.pos);
        assert!((a.tempo - b.tempo).abs() < 1e-9 && b.pitch == 0.5);
        // Never over a playing deck.
        h.send(Control::Eject { d: 0 });
        h.send(Control::Double { d: 0 });
        render(&mut e, 0.1);
        assert!(h.snapshot().decks[0].loaded);
    }

    #[test]
    fn smart_fader_glides_the_tempo_and_swaps_the_bass() {
        let (mut e, h, _g) = Engine::new(44_100.0);
        load(&h, 0, 120.0, 0.0);
        load(&h, 1, 128.0, 0.0);
        h.send(Control::SmartFader { on: true });
        h.send(Control::Play { d: 0 });
        h.send(Control::Play { d: 1 });
        let bpm = |st: &State, d: usize| st.decks[d].grid.unwrap().bpm * st.decks[d].tempo;
        for (x, want) in [(0.0, 120.0), (1.0, 128.0)] {
            h.send(Control::Crossfader { v: x });
            render(&mut e, 0.2);
            let st = h.snapshot();
            assert!((bpm(&st, 0) - want).abs() < 0.01 && (bpm(&st, 1) - want).abs() < 0.01, "at {x}: {} / {}", bpm(&st, 0), bpm(&st, 1));
        }
        h.send(Control::Crossfader { v: 0.5 });
        render(&mut e, 0.2);
        let st = h.snapshot();
        assert!((bpm(&st, 0) - 124.0).abs() < 0.01, "halfway: {}", bpm(&st, 0));
        // Off: each deck back on its own tempo fader.
        h.send(Control::SmartFader { on: false });
        render(&mut e, 0.2);
        let st = h.snapshot();
        assert!(!st.decks[0].sync && !st.decks[1].sync && (bpm(&st, 1) - 128.0).abs() < 0.01);
    }

    #[test]
    fn sampler_pads_play_into_the_master() {
        let (mut e, h, _g) = Engine::new(44_100.0);
        let tone: Vec<f32> = (0..44_100).flat_map(|i| { let v = (i as f32 * 0.1).sin() * 0.5; [v, v] }).collect();
        h.tx.send(Cmd::Sample { n: 2, sample: Some(Arc::new(Sample { name: "tone".into(), data: tone })) }).unwrap();
        let quiet = render(&mut e, 0.2);
        assert!(quiet.iter().all(|v| v.abs() < 1e-6));
        h.send(Control::Sample { n: 2 });
        let out = render(&mut e, 0.2);
        assert!(out.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.1);
        assert!(h.snapshot().sampler_playing[2] && h.snapshot().sampler_loaded[2]);
        h.send(Control::SampleStop { n: 2 });
        render(&mut e, 0.2);
        assert!(!h.snapshot().sampler_playing[2]);
        let after = render(&mut e, 0.1);
        assert!(after.iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn smart_cfx_adds_an_effect_only_when_on() {
        // A click, then silence: with Smart CFX the echo answers back.
        let run = |smart: bool| -> f32 {
            let (mut e, h, _g) = Engine::new(44_100.0);
            let t = TrackAudio::new(4_000);
            let mut data = vec![0.0f32; 44_100 * 2 * 4];
            for v in data.iter_mut().take(4_000) {
                *v = 0.6;
            }
            t.push(&data);
            t.done.store(true, Ordering::Release);
            h.tx.send(Cmd::Load { d: 0, track: Arc::new(t), stems: None, grid: Some(Grid { bpm: 120.0, anchor: 0.0 }), cue: 0.0, hotcues: [None; 8], memory: [None; MEMS], gain_db: 0.0 }).unwrap();
            h.send(Control::SmartCfx { on: smart });
            h.send(Control::Filter { d: 0, v: 0.3 });
            h.send(Control::Play { d: 0 });
            let out = render(&mut e, 1.5);
            // Loudest thing after the click has passed.
            out[44_100..].iter().fold(0.0f32, |m, v| m.max(v.abs()))
        };
        assert!(run(false) < 0.01, "plain filter leaves silence: {}", run(false));
        assert!(run(true) > 0.02, "echo repeats: {}", run(true));
    }
}
