//! The Pioneer controller: its buttons, knobs and jog wheels in, its lights
//! out. Written for the DDJ-FLX4 and DDJ-400, which share one layout of
//! MIDI messages (numbers as documented by Pioneer and by Mixxx's mappings).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::app::{App, Event};
use crate::engine::fx::{ALL as FX_ALL, BEATS, FxKind};
use crate::engine::{Control, DECKS, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PadMode {
    HotCue,
    Keyboard,
    PadFx1,
    PadFx2,
    BeatJump,
    BeatLoop,
    Sampler,
    KeyShift,
}

impl PadMode {
    /// The note the mode's button sends (and lights on), and the first pad's note.
    fn button(self) -> u8 {
        match self {
            PadMode::HotCue => 0x1B,
            PadMode::Keyboard => 0x69,
            PadMode::PadFx1 => 0x1E,
            PadMode::PadFx2 => 0x6B,
            PadMode::BeatJump => 0x20,
            PadMode::BeatLoop => 0x6D,
            PadMode::Sampler => 0x22,
            PadMode::KeyShift => 0x6F,
        }
    }
    fn from_button(n: u8) -> Option<Self> {
        ALL_MODES.iter().copied().find(|m| m.button() == n)
    }
}

const ALL_MODES: [PadMode; 8] =
    [PadMode::HotCue, PadMode::Keyboard, PadMode::PadFx1, PadMode::PadFx2, PadMode::BeatJump, PadMode::BeatLoop, PadMode::Sampler, PadMode::KeyShift];

/// The KEYBOARD pad mode (SHIFT + HOT CUE) plays the stems: pads 1-4 turn
/// vocals, melody, bass and drums on and off (SHIFT: solo), pads 5-8 are
/// acapella, instrumental, no drums, and everything back.
const STEM_PRESETS: [[f32; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 0.0], [1.0; 4]];

fn stem_pad(now: [f32; 4], n: usize, shift: bool) -> [f32; 4] {
    if n >= 4 {
        return STEM_PRESETS[n - 4];
    }
    let mut g = now;
    if shift {
        let solo: [f32; 4] = std::array::from_fn(|k| if k == n { 1.0 } else { 0.0 });
        return if now == solo { [1.0; 4] } else { solo };
    }
    g[n] = if now[n] > 0.5 { 0.0 } else { 1.0 };
    g
}

const LOOP_BEATS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
const JUMP_BEATS: [f64; 8] = [-1.0, 1.0, -2.0, 2.0, -4.0, 4.0, -8.0, 8.0];
const RANGES: [f32; 4] = [0.06, 0.10, 0.16, 0.50];

/// The momentary effects on the pads in PAD FX modes: (effect, beat index).
const PAD_FX1: [(FxKind, usize); 8] = [
    (FxKind::Roll, 0),
    (FxKind::Roll, 1),
    (FxKind::Roll, 2),
    (FxKind::Roll, 4),
    (FxKind::Echo, 2),
    (FxKind::Echo, 4),
    (FxKind::Trans, 1),
    (FxKind::Wobble, 4),
];
const PAD_FX2: [(FxKind, usize); 8] = [
    (FxKind::Reverb, 4),
    (FxKind::Flanger, 5),
    (FxKind::Phaser, 5),
    (FxKind::Crush, 4),
    (FxKind::Echo, 3),
    (FxKind::Roll, 5),
    (FxKind::Trans, 0),
    (FxKind::Wobble, 2),
];
/// Pad effects borrow the second effect unit, and give it back on release.
const PAD_UNIT: usize = 1;

enum Out {
    Ctl(Control),
    Ev(Event),
}

pub struct Mapper {
    msb: HashMap<(u8, u8), u8>,
    pad_mode: [PadMode; DECKS],
    last_tempo: [Option<f32>; DECKS],
    /// Where the BEAT FX channel lever points: CH1, CH2 or both.
    fx_lever: [bool; 2],
    /// Pads held down in a PAD FX mode, and the unit settings to restore.
    pad_fx: Option<(usize, u8, crate::engine::FxState)>,
    pub vinyl: bool,
    pub invert_tempo: bool,
}

impl Default for Mapper {
    fn default() -> Self {
        Self {
            msb: HashMap::new(),
            pad_mode: [PadMode::HotCue; DECKS],
            last_tempo: [None; DECKS],
            fx_lever: [true, false],
            pad_fx: None,
            vinyl: true,
            invert_tempo: false,
        }
    }
}

fn rel(v: u8) -> f64 {
    v as f64 - 64.0
}

impl Mapper {
    /// A 14-bit control: remember the coarse half, act on the fine half.
    fn fine(&mut self, status: u8, cc: u8, v: u8) -> Option<f32> {
        if cc < 0x20 {
            self.msb.insert((status, cc), v);
            None
        } else {
            let msb = *self.msb.get(&(status, cc - 0x20))?;
            Some(((msb as u32) << 7 | v as u32) as f32 / 16383.0)
        }
    }

    fn handle(&mut self, msg: &[u8], st: &State) -> Vec<Out> {
        let mut out = Vec::new();
        if msg.len() < 3 {
            return out;
        }
        let (mut status, data, mut v) = (msg[0], msg[1], msg[2]);
        // A note-off is a note-on at zero velocity.
        if status & 0xF0 == 0x80 {
            status = 0x90 | (status & 0x0F);
            v = 0;
        }
        let kind = status & 0xF0;
        let ch = status & 0x0F;
        let down = v > 0 && kind == 0x90;
        let c = |c: Control| Out::Ctl(c);
        match (kind, ch) {
            // ---- deck buttons -------------------------------------------------
            (0x90, d) if (d as usize) < DECKS => {
                let d = d as usize;
                let deck = st.decks.get(d);
                match data {
                    0x0B if down => out.push(c(Control::PlayPause { d })),
                    0x0E | 0x47 => out.push(c(Control::Reverse { d, on: down })),
                    0x0C => out.push(c(Control::Cue { d, down })),
                    0x48 if down => out.push(c(Control::Start { d })),
                    0x36 => {
                        if self.vinyl || !down {
                            out.push(c(Control::JogTouch { d, on: down }));
                        }
                    }
                    0x67 if !down => out.push(c(Control::JogTouch { d, on: false })),
                    0x58 if down => out.push(c(Control::Sync { d, on: None })),
                    0x60 if down => {
                        let now = deck.map_or(0.08, |s| s.range);
                        let i = RANGES.iter().position(|r| (r - now).abs() < 0.001).map_or(0, |i| (i + 1) % RANGES.len());
                        out.push(c(Control::Range { d, range: RANGES[i] }));
                        out.push(Out::Ev(Event::Notice { text: format!("Deck {} tempo range ±{:.0}%", d + 1, RANGES[i] * 100.0), error: false }));
                    }
                    0x10 if down => out.push(c(Control::LoopIn { d })),
                    0x11 if down => out.push(c(Control::LoopOut { d })),
                    // 4 BEAT / EXIT: a four-beat loop, or out of the loop playing.
                    0x4D if down => {
                        let looping = deck.and_then(|s| s.lp).is_some_and(|l| l.active);
                        out.push(c(if looping { Control::Reloop { d } } else { Control::AutoLoop { d, beats: 4.0 } }));
                    }
                    // SHIFT + 4 BEAT / EXIT: back into the last loop.
                    0x50 if down => out.push(c(Control::Reloop { d })),
                    0x4C if down => out.push(c(Control::LoopHalve { d })),
                    0x4E if down => out.push(c(Control::LoopDouble { d })),
                    0x51 | 0x53 if down => {
                        let looping = deck.and_then(|s| s.lp).is_some_and(|l| l.active);
                        let back = data == 0x51;
                        out.push(c(match (looping, back) {
                            (true, true) => Control::LoopHalve { d },
                            (true, false) => Control::LoopDouble { d },
                            (false, true) => Control::BeatJump { d, beats: -4.0 },
                            (false, false) => Control::BeatJump { d, beats: 4.0 },
                        }));
                    }
                    0x3E if down => out.push(c(Control::BeatJump { d, beats: -16.0 })),
                    0x3D if down => out.push(c(Control::BeatJump { d, beats: 16.0 })),
                    0x54 if down => out.push(c(Control::Pfl { d, on: !deck.is_some_and(|s| s.pfl) })),
                    0x68 if down => out.push(c(Control::KeyLock { d, on: !deck.is_some_and(|s| s.keylock) })),
                    n if down => {
                        if let Some(m) = PadMode::from_button(n) {
                            self.pad_mode[d] = m;
                        }
                    }
                    _ => {}
                }
            }
            // ---- deck knobs, faders, jog wheels --------------------------------
            (0xB0, d) if (d as usize) < DECKS => {
                let d = d as usize;
                match data {
                    0x21 => out.push(c(Control::JogBend { d, ticks: rel(v) })),
                    0x22 => out.push(c(if self.vinyl { Control::JogScratch { d, ticks: rel(v) } } else { Control::JogBend { d, ticks: rel(v) } })),
                    0x23 => out.push(c(Control::JogBend { d, ticks: rel(v) })),
                    0x29 => out.push(c(Control::JogSearch { d, ticks: rel(v) })),
                    cc => {
                        if let Some(x) = self.fine(status, cc, v) {
                            match cc - 0x20 {
                                0x00 => {
                                    let p = if self.invert_tempo { 1.0 - 2.0 * x } else { 2.0 * x - 1.0 };
                                    // Soft takeover: only grab the tempo once the fader
                                    // meets where the deck actually is.
                                    let now = st.decks.get(d).map_or(0.0, |s| s.pitch);
                                    let owned = self.last_tempo[d].is_some_and(|l| (l - now).abs() < 0.01);
                                    let crossed = self.last_tempo[d].is_some_and(|l| (l - now).signum() != (p - now).signum());
                                    if owned || crossed || (p - now).abs() < 0.02 {
                                        out.push(c(Control::Pitch { d, v: p }));
                                    }
                                    self.last_tempo[d] = Some(p);
                                }
                                0x04 => out.push(c(Control::Trim { d, v: x })),
                                0x07 => out.push(c(Control::Eq { d, band: 2, v: x })),
                                0x0B => out.push(c(Control::Eq { d, band: 1, v: x })),
                                0x0F => out.push(c(Control::Eq { d, band: 0, v: x })),
                                0x13 => out.push(c(Control::Fader { d, v: x })),
                                _ => {}
                            }
                        }
                    }
                }
            }
            // ---- mixer section -------------------------------------------------
            (0xB0, 6) => match data {
                0x40 => out.push(Out::Ev(Event::Browse { delta: if v < 64 { v as i32 } else { v as i32 - 128 } })),
                // SHIFT + browse: zoom the waveforms.
                0x64 => out.push(Out::Ev(Event::Zoom { delta: if v < 64 { v as i32 } else { v as i32 - 128 } })),
                cc => {
                    if let Some(x) = self.fine(status, cc, v) {
                        match cc - 0x20 {
                            0x1F => out.push(c(Control::Crossfader { v: x })),
                            0x17 => out.push(c(Control::Filter { d: 0, v: x * 2.0 - 1.0 })),
                            0x18 => out.push(c(Control::Filter { d: 1, v: x * 2.0 - 1.0 })),
                            0x0C => out.push(c(Control::HeadMix { v: x })),
                            0x0D => out.push(c(Control::HeadLevel { v: x })),
                            0x08 => out.push(c(Control::Master { v: x })),
                            _ => {}
                        }
                    }
                }
            },
            (0x90, 6) if down => match data {
                0x41 => out.push(Out::Ev(Event::BrowsePress { shift: false })),
                0x42 => out.push(Out::Ev(Event::BrowsePress { shift: true })),
                0x46 => out.push(Out::Ev(Event::LoadSelected { d: 0 })),
                0x47 => out.push(Out::Ev(Event::LoadSelected { d: 1 })),
                0x63 => out.push(c(Control::MasterCue { on: !st.master_cue })),
                _ => {}
            },
            // ---- beat FX -------------------------------------------------------
            (0x90, 4) | (0x90, 5) => {
                let fx = st.fx.first().cloned().unwrap_or_default();
                match data {
                    0x63 | 0x64 if down => {
                        let i = FX_ALL.iter().position(|k| *k == fx.kind).unwrap_or(0);
                        let n = FX_ALL.len();
                        let next = if data == 0x63 { (i + 1) % n } else { (i + n - 1) % n };
                        out.push(c(Control::FxKind { u: 0, kind: FX_ALL[next] }));
                        out.push(Out::Ev(Event::Notice { text: format!("Beat FX: {}", FX_ALL[next].name()), error: false }));
                    }
                    0x4A if down => out.push(c(Control::FxBeats { u: 0, i: fx.beats.saturating_sub(1) })),
                    0x4B if down => out.push(c(Control::FxBeats { u: 0, i: (fx.beats + 1).min(BEATS.len() - 1) })),
                    // The CH SELECT lever (CH1 / CH2 / CH1&CH2) reports each
                    // side as on or off; the unit follows the lever.
                    0x10 | 0x11 => {
                        let side = match (ch, data) {
                            (4, 0x10) => Some(0),
                            (5, 0x11) => Some(1),
                            _ => None,
                        };
                        if let Some(side) = side {
                            self.fx_lever[side] = down;
                            let mask = (self.fx_lever[0] as u8) | (self.fx_lever[1] as u8) << 1;
                            if mask != 0 {
                                out.push(c(Control::FxAssign { u: 0, mask }));
                            }
                        }
                    }
                    0x47 if down => out.push(c(Control::FxOn { u: 0, on: !fx.on })),
                    0x43 if down => out.push(c(Control::FxOn { u: 0, on: false })),
                    _ => {}
                }
            }
            (0xB0, 4) => {
                if let Some(x) = self.fine(status, data, v) {
                    if data == 0x22 {
                        out.push(c(Control::FxDepth { u: 0, v: x }));
                    }
                }
            }
            // ---- performance pads ----------------------------------------------
            (0x90, 7..=10) => {
                let d = ((ch - 7) / 2) as usize;
                let shift = (ch - 7) % 2 == 1;
                let base = data & 0xF0;
                let n = (data & 0x0F) as usize;
                if n >= 8 || d >= DECKS {
                    return out;
                }
                match base {
                    0x00 if shift => {
                        if down {
                            out.push(c(Control::HotcueClear { d, n }));
                        }
                    }
                    0x00 => out.push(c(Control::Hotcue { d, n, down })),
                    0x60 if down => out.push(c(Control::AutoLoop { d, beats: LOOP_BEATS[n] })),
                    0x40 if down => {
                        let now = st.decks.get(d).map_or([1.0; 4], |s| s.stems);
                        out.push(c(Control::Stems { d, gains: stem_pad(now, n, shift) }));
                    }
                    0x20 if down && !shift => out.push(c(Control::BeatJump { d, beats: JUMP_BEATS[n] })),
                    0x10 | 0x50 => {
                        let (kind, beats) = if base == 0x10 { PAD_FX1[n] } else { PAD_FX2[n] };
                        if down {
                            let saved = self.pad_fx.take().map(|p| p.2).unwrap_or_else(|| st.fx.get(PAD_UNIT).cloned().unwrap_or_default());
                            out.push(c(Control::FxKind { u: PAD_UNIT, kind }));
                            out.push(c(Control::FxBeats { u: PAD_UNIT, i: beats }));
                            out.push(c(Control::FxAssign { u: PAD_UNIT, mask: 1 << d }));
                            out.push(c(Control::FxDepth { u: PAD_UNIT, v: 0.85 }));
                            out.push(c(Control::FxOn { u: PAD_UNIT, on: true }));
                            self.pad_fx = Some((d, data, saved));
                        } else if self.pad_fx.as_ref().is_some_and(|p| p.0 == d && p.1 == data) {
                            let (_, _, was) = self.pad_fx.take().unwrap();
                            out.push(c(Control::FxOn { u: PAD_UNIT, on: false }));
                            out.push(c(Control::FxKind { u: PAD_UNIT, kind: was.kind }));
                            out.push(c(Control::FxBeats { u: PAD_UNIT, i: was.beats }));
                            out.push(c(Control::FxAssign { u: PAD_UNIT, mask: was.assign }));
                            out.push(c(Control::FxDepth { u: PAD_UNIT, v: was.depth }));
                            if was.on {
                                out.push(c(Control::FxOn { u: PAD_UNIT, on: true }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        out
    }

    /// What every light should be showing.
    fn lights(&self, st: &State, blink: bool, loaded_flash: &[bool; DECKS]) -> HashMap<(u8, u8), u8> {
        let mut l = HashMap::new();
        let on = |b: bool| if b { 0x7F } else { 0x00 };
        for (d, s) in st.decks.iter().enumerate().take(DECKS) {
            let n = 0x90 | d as u8;
            l.insert((n, 0x0B), on(s.playing || (s.loaded && !s.playing && blink)));
            l.insert((n, 0x0C), on(s.loaded && (s.at_cue || (!s.playing && blink))));
            l.insert((n, 0x58), on(s.sync));
            l.insert((n, 0x54), on(s.pfl));
            let looping = s.lp.is_some_and(|lp| lp.active);
            l.insert((n, 0x10), on(looping || s.loop_in.is_some() && blink));
            l.insert((n, 0x11), on(looping));
            l.insert((n, 0x4D), on(looping || s.lp.is_some() && blink));
            for m in ALL_MODES {
                l.insert((n, m.button()), on(self.pad_mode[d] == m));
            }
            for shift in [0u8, 1] {
                let pads = 0x97 + 2 * d as u8 + shift;
                for i in 0..8u8 {
                    l.insert((pads, i), on(s.hotcues[i as usize].is_some()));
                    let size = LOOP_BEATS[i as usize];
                    let lit = match (s.lp, s.grid) {
                        (Some(lp), Some(g)) if lp.active => (lp.len() - size * g.period()).abs() < 2.0,
                        _ => false,
                    };
                    l.insert((pads, 0x60 + i), on(lit));
                    l.insert((pads, 0x20 + i), on(s.loaded));
                    let stem_lit = if i < 4 { s.stems[i as usize] > 0.5 } else { s.stems == STEM_PRESETS[i as usize - 4] };
                    l.insert((pads, 0x40 + i), on(s.loaded && stem_lit));
                    let held = |base: u8| self.pad_fx.as_ref().is_some_and(|p| p.0 == d && p.1 == base + i);
                    l.insert((pads, 0x10 + i), on(held(0x10)));
                    l.insert((pads, 0x50 + i), on(held(0x50)));
                }
            }
            let db = 20.0 * s.meter[0].max(s.meter[1]).max(1e-6).log10();
            l.insert((0xB0 | d as u8, 0x02), meter_value(db));
            // The unit's vinyl mode is set from here (it has no switch of its own).
            l.insert((n, 0x17), on(self.vinyl));
            if loaded_flash[d] {
                l.insert((0x9F, d as u8), 0x7F);
            }
        }
        if let Some(fx) = st.fx.first() {
            l.insert((0x94, 0x47), on(fx.on));
        }
        l
    }
}

/// Level in dB to the FLX4's channel meter: its segments light at
/// 0x26 (green), 0x41 (green 2), 0x57 (orange), 0x65 (orange 2), 0x77 (red).
fn meter_value(db: f32) -> u8 {
    const POINTS: [(f32, f32); 7] = [(-36.0, 0.0), (-24.0, 38.0), (-12.0, 65.0), (-6.0, 87.0), (-3.0, 101.0), (-0.5, 119.0), (0.0, 127.0)];
    if db <= POINTS[0].0 {
        return 0;
    }
    for w in POINTS.windows(2) {
        let ((a, va), (b, vb)) = (w[0], w[1]);
        if db <= b {
            return (va + (vb - va) * (db - a) / (b - a)).round() as u8;
        }
    }
    127
}

fn is_controller(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("ddj") || n.contains("pioneer")
}

struct Connected {
    name: String,
    _input: MidiInputConnection<()>,
    output: Option<MidiOutputConnection>,
    sent: HashMap<(u8, u8), u8>,
    keepalive: bool,
}

pub fn start(app: Arc<App>) {
    let mapper = Arc::new(Mutex::new(Mapper::default()));
    std::thread::Builder::new()
        .name("slipmat-midi".into())
        .spawn(move || run(app, mapper))
        .expect("midi thread");
}

fn connect(app: &Arc<App>, mapper: &Arc<Mutex<Mapper>>) -> Option<Connected> {
    let mut input = MidiInput::new("Slipmat").ok()?;
    input.ignore(Ignore::None);
    let ports = input.ports();
    let names: Vec<String> = ports.iter().filter_map(|p| input.port_name(p).ok()).collect();
    app.midi.lock().unwrap().ports = names.clone();
    let (port, name) = ports.iter().zip(&names).find(|(_, n)| is_controller(n))?;
    let name = name.clone();
    let cb_app = app.clone();
    let cb_mapper = mapper.clone();
    let conn = input
        .connect(
            port,
            "slipmat-in",
            move |_, msg, _| {
                let st = cb_app.engine.snapshot();
                let outs = cb_mapper.lock().unwrap().handle(msg, &st);
                if msg.len() >= 3 {
                    cb_app.midi.lock().unwrap().last = Some(format!("{:02X} {:02X} {:02X}", msg[0], msg[1], msg[2]));
                }
                for o in outs {
                    match o {
                        Out::Ctl(c) => cb_app.engine.send(c),
                        Out::Ev(e) => {
                            let _ = cb_app.events.send(e);
                        }
                    }
                }
            },
            (),
        )
        .ok()?;
    // The matching output port, for the lights.
    let base = name.split(':').next().unwrap_or(&name).to_string();
    let output = MidiOutput::new("Slipmat").ok().and_then(|out| {
        let port = out.ports().into_iter().find(|p| out.port_name(p).map(|n| n.contains(&base) || is_controller(&n)).unwrap_or(false))?;
        out.connect(&port, "slipmat-out").ok()
    });
    log::info!("controller connected: {name} (lights: {})", output.is_some());
    app.notice(format!("Controller connected: {base}"), false);
    Some(Connected { keepalive: name.to_uppercase().contains("FLX4"), name, _input: conn, output, sent: HashMap::new() })
}

fn run(app: Arc<App>, mapper: Arc<Mutex<Mapper>>) {
    let mut conn: Option<Connected> = None;
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut last_keepalive = Instant::now();
    let started = Instant::now();
    let mut keys = [0usize; DECKS];
    let mut flash_until = [Instant::now(); DECKS];
    loop {
        std::thread::sleep(Duration::from_millis(40));
        {
            let s = app.settings();
            let mut m = mapper.lock().unwrap();
            m.vinyl = s.vinyl;
            m.invert_tempo = s.invert_tempo;
        }
        if last_scan.elapsed() > Duration::from_secs(2) {
            last_scan = Instant::now();
            // Notice the controller being unplugged or plugged in.
            let present: Vec<String> = MidiInput::new("Slipmat-scan").map(|i| i.ports().iter().filter_map(|p| i.port_name(p).ok()).collect()).unwrap_or_default();
            if let Some(c) = &conn {
                if !present.iter().any(|n| *n == c.name) {
                    log::info!("controller disconnected: {}", c.name);
                    app.notice("Controller disconnected", true);
                    conn = None;
                }
            }
            if conn.is_none() {
                app.midi.lock().unwrap().ports = present.clone();
                if present.iter().any(|n| is_controller(n)) {
                    conn = connect(&app, &mapper);
                }
            }
            app.midi.lock().unwrap().connected = conn.as_ref().map(|c| c.name.split(':').next().unwrap_or(&c.name).to_string());
        }
        let Some(c) = conn.as_mut() else { continue };
        let Some(out) = c.output.as_mut() else { continue };
        let st = app.engine.snapshot();
        let mut flash = [false; DECKS];
        for d in 0..DECKS {
            if let Some(s) = st.decks.get(d) {
                if s.track_key != keys[d] {
                    keys[d] = s.track_key;
                    if s.loaded {
                        flash_until[d] = Instant::now() + Duration::from_millis(300);
                    }
                }
            }
            flash[d] = Instant::now() < flash_until[d];
        }
        let blink = (started.elapsed().as_millis() / 300) % 2 == 0;
        let want = mapper.lock().unwrap().lights(&st, blink, &flash);
        // Forget lights no longer asked for, so they're sent again next time.
        c.sent.retain(|k, _| want.contains_key(k));
        for (k, v) in want {
            if c.sent.get(&k) != Some(&v) {
                let kind = k.0 & 0xF0;
                let ok = if kind == 0x90 || kind == 0xB0 { out.send(&[k.0, k.1, v]).is_ok() } else { true };
                if ok {
                    c.sent.insert(k, v);
                }
            }
        }
        if c.keepalive && last_keepalive.elapsed() > Duration::from_millis(200) {
            last_keepalive = Instant::now();
            let _ = out.send(&[0xF0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x50, 0x02, 0xF7]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DeckState;

    fn state() -> State {
        let deck = DeckState { stems: [1.0; 4], ..Default::default() };
        State { decks: vec![deck.clone(), deck], fx: vec![Default::default(), Default::default()], ..Default::default() }
    }

    fn ctl(m: &mut Mapper, msg: &[u8], st: &State) -> Vec<String> {
        m.handle(msg, st).into_iter().map(|o| match o {
            Out::Ctl(c) => format!("{c:?}"),
            Out::Ev(e) => format!("{e:?}"),
        }).collect()
    }

    #[test]
    fn buttons_and_pads() {
        let mut m = Mapper::default();
        let st = state();
        assert_eq!(ctl(&mut m, &[0x90, 0x0B, 0x7F], &st), ["PlayPause { d: 0 }"]);
        assert_eq!(ctl(&mut m, &[0x91, 0x0C, 0x7F], &st), ["Cue { d: 1, down: true }"]);
        assert_eq!(ctl(&mut m, &[0x91, 0x0C, 0x00], &st), ["Cue { d: 1, down: false }"]);
        assert_eq!(ctl(&mut m, &[0x97, 0x02, 0x7F], &st), ["Hotcue { d: 0, n: 2, down: true }"]);
        assert_eq!(ctl(&mut m, &[0x9A, 0x01, 0x7F], &st), ["HotcueClear { d: 1, n: 1 }"]);
        assert_eq!(ctl(&mut m, &[0x99, 0x64, 0x7F], &st), ["AutoLoop { d: 1, beats: 4.0 }"]);
        assert_eq!(ctl(&mut m, &[0x97, 0x27, 0x7F], &st), ["BeatJump { d: 0, beats: 8.0 }"]);
        assert_eq!(ctl(&mut m, &[0x96, 0x47, 0x7F], &st), ["LoadSelected { d: 1 }"]);
        // Stems: pad 4 in KEYBOARD mode drops the drums, SHIFT + pad 1 solos vocals.
        assert_eq!(ctl(&mut m, &[0x97, 0x43, 0x7F], &st), ["Stems { d: 0, gains: [1.0, 1.0, 1.0, 0.0] }"]);
        assert_eq!(ctl(&mut m, &[0x98, 0x40, 0x7F], &st), ["Stems { d: 0, gains: [1.0, 0.0, 0.0, 0.0] }"]);
        assert_eq!(ctl(&mut m, &[0x99, 0x45, 0x7F], &st), ["Stems { d: 1, gains: [0.0, 1.0, 1.0, 1.0] }"]);
    }

    #[test]
    fn fourteen_bit_faders_and_jogs() {
        let mut m = Mapper::default();
        let st = state();
        assert!(ctl(&mut m, &[0xB1, 0x13, 0x7F], &st).is_empty());
        assert_eq!(ctl(&mut m, &[0xB1, 0x33, 0x7F], &st), ["Fader { d: 1, v: 1.0 }"]);
        ctl(&mut m, &[0xB6, 0x1F, 0x00], &st);
        assert_eq!(ctl(&mut m, &[0xB6, 0x3F, 0x00], &st), ["Crossfader { v: 0.0 }"]);
        assert_eq!(ctl(&mut m, &[0xB0, 0x22, 0x43], &st), ["JogScratch { d: 0, ticks: 3.0 }"]);
        assert_eq!(ctl(&mut m, &[0xB0, 0x21, 0x3E], &st), ["JogBend { d: 0, ticks: -2.0 }"]);
        assert_eq!(ctl(&mut m, &[0xB6, 0x40, 0x7F], &st), ["Browse { delta: -1 }"]);
    }

    #[test]
    fn flx4_specifics() {
        let mut m = Mapper::default();
        let mut st = state();
        // 4 BEAT / EXIT: a loop when there is none, out when there is.
        assert_eq!(ctl(&mut m, &[0x90, 0x4D, 0x7F], &st), ["AutoLoop { d: 0, beats: 4.0 }"]);
        st.decks[0].lp = Some(crate::engine::deck::Loop { start: 0.0, end: 1000.0, active: true });
        assert_eq!(ctl(&mut m, &[0x90, 0x4D, 0x7F], &st), ["Reloop { d: 0 }"]);
        // BEAT FX lever: CH2, then CH1&CH2 (as the unit reports them).
        ctl(&mut m, &[0x94, 0x10, 0x00], &st);
        assert_eq!(ctl(&mut m, &[0x95, 0x11, 0x7F], &st), ["FxAssign { u: 0, mask: 2 }"]);
        assert_eq!(ctl(&mut m, &[0x94, 0x10, 0x7F], &st), ["FxAssign { u: 0, mask: 3 }"]);
        // MASTER LEVEL is a 14-bit knob on the mixer channel.
        ctl(&mut m, &[0xB6, 0x08, 0x7F], &st);
        assert_eq!(ctl(&mut m, &[0xB6, 0x28, 0x7F], &st), ["Master { v: 1.0 }"]);
        assert_eq!(ctl(&mut m, &[0x96, 0x63, 0x7F], &st), ["MasterCue { on: true }"]);
        assert_eq!(ctl(&mut m, &[0xB6, 0x64, 0x7F], &st), ["Zoom { delta: -1 }"]);
        // Meter: silence dark, -12 dB in green 2, clipping red.
        assert_eq!(meter_value(-60.0), 0);
        assert!((0x41..0x57).contains(&meter_value(-10.0)));
        assert!(meter_value(0.0) >= 0x77);
    }

    #[test]
    fn tempo_fader_soft_takeover() {
        let mut m = Mapper::default();
        let mut st = state();
        st.decks[0].pitch = 0.5;
        // Fader at the top (-100%): far from the deck's +50%, ignored.
        ctl(&mut m, &[0xB0, 0x00, 0x00], &st);
        assert!(ctl(&mut m, &[0xB0, 0x20, 0x00], &st).is_empty());
        // Moved past +50%: taken over.
        ctl(&mut m, &[0xB0, 0x00, 0x62], &st);
        let got = ctl(&mut m, &[0xB0, 0x20, 0x00], &st);
        assert!(got[0].starts_with("Pitch { d: 0"), "{got:?}");
    }

    #[test]
    fn pad_fx_borrow_and_return_the_unit() {
        let mut m = Mapper::default();
        let st = state();
        let down = ctl(&mut m, &[0x97, 0x11, 0x7F], &st);
        assert!(down.iter().any(|s| s.contains("FxOn { u: 1, on: true }")));
        let up = ctl(&mut m, &[0x97, 0x11, 0x00], &st);
        assert!(up.iter().any(|s| s.contains("FxOn { u: 1, on: false }")));
        assert!(!up.iter().any(|s| s.contains("on: true")));
    }
}
