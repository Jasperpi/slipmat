//! Auto-DJ: plays a list of songs and mixes each into the next by itself,
//! the way a DJ would: the next song waits on the idle deck at its first
//! downbeat, starts on a phrase boundary near the end of the playing one,
//! locked to its tempo and beat, comes in on the fader with its bass cut,
//! swaps basslines halfway, and takes over as the old song fades. With stems,
//! the new song's vocals wait until the old song has gone, so two singers
//! never clash. Afterwards the new song drifts back to its own tempo.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::app::{App, Row};
use crate::engine::{Control, DECKS, DeckState, State};
use crate::track::SRC_RATE;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub uri: String,
    pub id: String,
    pub name: String,
    pub artist: String,
    pub image: Option<String>,
    pub duration_ms: u32,
}

impl From<&Row> for Item {
    fn from(r: &Row) -> Self {
        Self { uri: r.uri.clone(), id: r.id.clone(), name: r.name.clone(), artist: r.artist.clone(), image: r.image.clone(), duration_ms: r.duration_ms }
    }
}

/// What the interface shows.
#[derive(Clone, Debug, Serialize, Default)]
pub struct Status {
    pub on: bool,
    pub queue: Vec<Item>,
    pub bars: u32,
    /// "waiting" | "playing" | "mixing"
    pub phase: &'static str,
    pub deck: Option<usize>,
    /// Seconds until the next mix starts.
    pub mix_in: Option<f64>,
    /// 0..1 through the mix.
    pub progress: Option<f64>,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Plan {
    Waiting,
    /// `forced`: a "Mix now" target (beat number), fixed when it was asked for.
    /// `drift`: easing back to the song's own tempo from pitch `.0`, since `.1`.
    Playing { d: usize, next: Option<usize>, drift: Option<(f32, Instant)>, forced: Option<f64> },
    Mixing { from: usize, to: usize, start: f64, beats: f64 },
}

pub struct AutoDj {
    pub on: AtomicBool,
    queue: Mutex<VecDeque<Item>>,
    bars: Mutex<u32>,
    skip: AtomicBool,
    status: Mutex<Status>,
}

impl Default for AutoDj {
    fn default() -> Self {
        Self { on: AtomicBool::new(false), queue: Mutex::default(), bars: Mutex::new(16), skip: AtomicBool::new(false), status: Mutex::default() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    On { on: bool },
    Add { items: Vec<Item>, #[serde(default)] next: bool },
    Remove { index: usize },
    Move { from: usize, to: usize },
    Clear,
    Skip,
    Bars { bars: u32 },
}

impl AutoDj {
    pub fn status(&self) -> Status {
        let mut s = self.status.lock().unwrap().clone();
        s.on = self.on.load(Ordering::Relaxed);
        s.queue = self.queue.lock().unwrap().iter().cloned().collect();
        s.bars = *self.bars.lock().unwrap();
        s
    }

    pub fn handle(&self, r: Request) {
        let mut q = self.queue.lock().unwrap();
        match r {
            Request::On { on } => self.on.store(on, Ordering::Relaxed),
            Request::Add { items, next } => {
                if next {
                    for it in items.into_iter().rev() {
                        q.push_front(it);
                    }
                } else {
                    q.extend(items);
                }
            }
            Request::Remove { index } => {
                q.remove(index);
            }
            Request::Move { from, to } => {
                if let Some(it) = q.remove(from) {
                    let at = to.min(q.len());
                    q.insert(at, it);
                }
            }
            Request::Clear => q.clear(),
            Request::Skip => self.skip.store(true, Ordering::Relaxed),
            Request::Bars { bars } => *self.bars.lock().unwrap() = bars.clamp(4, 64),
        }
    }

    fn pop(&self) -> Option<Item> {
        self.queue.lock().unwrap().pop_front()
    }

    fn note(&self, phase: &'static str, deck: Option<usize>, mix_in: Option<f64>, progress: Option<f64>, note: impl Into<String>) {
        let mut s = self.status.lock().unwrap();
        s.phase = phase;
        s.deck = deck;
        s.mix_in = mix_in;
        s.progress = progress;
        s.note = note.into();
    }
}

/// How long a song synced in takes to ease back to its own tempo.
const DRIFT_SECS: f32 = 30.0;

fn beat_of(d: &DeckState) -> Option<f64> {
    d.grid.map(|g| (d.pos - g.anchor) / (SRC_RATE * 60.0 / g.bpm))
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Sends a control only when the value changed, so the engine isn't flooded.
#[derive(Default)]
struct Sent {
    fader: [Option<f32>; DECKS],
    low: [Option<f32>; DECKS],
    vocals: [Option<f32>; DECKS],
}

impl Sent {
    fn fader(&mut self, app: &App, d: usize, v: f32) {
        if self.fader[d].is_none_or(|old| (old - v).abs() > 0.004) {
            self.fader[d] = Some(v);
            app.engine.send(Control::Fader { d, v });
        }
    }
    fn low(&mut self, app: &App, d: usize, v: f32, st: &DeckState) {
        if self.low[d].is_none_or(|old| (old - v).abs() > 0.004) {
            self.low[d] = Some(v);
            app.engine.send(Control::Eq { d, band: 0, v });
        }
        let _ = st;
    }
    fn vocals(&mut self, app: &App, d: usize, v: f32, st: &DeckState) {
        if self.vocals[d].is_none_or(|old| (old - v).abs() > 0.01) {
            self.vocals[d] = Some(v);
            let mut gains = st.stems;
            gains[0] = v;
            app.engine.send(Control::Stems { d, gains });
        }
    }
    fn forget(&mut self, d: usize) {
        self.fader[d] = None;
        self.low[d] = None;
        self.vocals[d] = None;
    }
}

/// Load `item` onto deck `d` and wait until it has a beat grid (or give up).
async fn prepare(app: &Arc<App>, d: usize, item: &Item) -> bool {
    if let Err(e) = app.load(d, &item.uri).await {
        app.notice(format!("Auto-DJ skipped “{}”: {e}", item.name), true);
        return false;
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(25) {
        let st = app.engine.snapshot();
        let deck = &st.decks[d];
        if deck.failed {
            break;
        }
        if deck.loaded && deck.grid.is_some() && deck.done {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    app.notice(format!("Auto-DJ couldn't get “{}” ready; skipping it", item.name), true);
    false
}

fn reset_deck(app: &App, sent: &mut Sent, d: usize) {
    app.engine.send(Control::Eq { d, band: 0, v: 0.5 });
    app.engine.send(Control::Fader { d, v: 1.0 });
    app.engine.send(Control::Stems { d, gains: [1.0; 4] });
    sent.forget(d);
}

/// When the playing deck should start mixing out (a beat number), and how far that is.
fn mix_point(d: &DeckState, beats: f64, skip: bool) -> Option<(f64, f64)> {
    let g = d.grid?;
    let now = beat_of(d)?;
    let period = SRC_RATE * 60.0 / g.bpm;
    let total = (d.len - g.anchor) / period;
    // Start on a phrase (8 bars), leaving a couple of bars after the mix.
    let mut start = ((total - beats - 8.0) / 32.0).floor() * 32.0;
    if start <= now + 0.5 {
        // Too late for that phrase: the next bar will do.
        start = (now / 4.0).floor() * 4.0 + 4.0;
    }
    if skip {
        start = start.min((now / 4.0).floor() * 4.0 + 4.0);
    }
    let secs = (start - now) * period / SRC_RATE / d.tempo.max(0.1);
    Some((start, secs))
}

pub async fn run(app: Arc<App>) {
    let mut plan = Plan::Waiting;
    // The next song is made ready in the background; this says on which
    // deck, once it is (or that it failed: None inside).
    let mut preparing: Option<tokio::task::JoinHandle<Option<usize>>> = None;
    let mut sent = Sent::default();
    let mut tick = tokio::time::interval(Duration::from_millis(40));
    loop {
        tick.tick().await;
        let dj = &app.autodj;
        if !dj.on.load(Ordering::Relaxed) {
            if plan != Plan::Waiting {
                plan = Plan::Waiting;
                dj.note("waiting", None, None, None, "Off");
            }
            if let Some(job) = preparing.take() {
                job.abort();
            }
            continue;
        }
        let st: State = app.engine.snapshot();
        if st.decks.len() < DECKS {
            continue;
        }
        plan = match plan {
            Plan::Waiting => {
                if let Some(d) = (0..DECKS).find(|&d| st.decks[d].playing) {
                    Plan::Playing { d, next: None, drift: None, forced: None }
                } else if let Some(item) = dj.pop() {
                    let d = (0..DECKS).find(|&d| !st.decks[d].loaded).unwrap_or(0);
                    dj.note("waiting", Some(d), None, None, format!("Loading “{}”…", item.name));
                    if prepare(&app, d, &item).await {
                        reset_deck(&app, &mut sent, d);
                        app.engine.send(Control::XfAssign { d, side: 0 });
                        app.engine.send(Control::Play { d });
                        Plan::Playing { d, next: None, drift: None, forced: None }
                    } else {
                        Plan::Waiting
                    }
                } else {
                    dj.note("waiting", None, None, None, "Add songs to the list to start");
                    Plan::Waiting
                }
            }
            Plan::Playing { d, next, mut drift, mut forced } => {
                let o = (d + 1) % DECKS;
                let deck = &st.decks[d];
                if !deck.playing {
                    // Paused by hand, or the song ran out.
                    let ended = deck.done && deck.pos >= deck.len - SRC_RATE * 0.2;
                    if ended && next.is_some() {
                        app.engine.send(Control::Play { d: o });
                        Plan::Playing { d: o, next: None, drift: None, forced: None }
                    } else if ended {
                        Plan::Waiting
                    } else {
                        dj.note("playing", Some(d), None, None, "Paused");
                        Plan::Playing { d, next, drift, forced }
                    }
                } else {
                    // Ease a song that was synced in back to its own tempo,
                    // over half a minute (absolute values, never read back).
                    if let Some((p0, since)) = drift {
                        let k = since.elapsed().as_secs_f32() / DRIFT_SECS;
                        let v = if k >= 1.0 { 0.0 } else { p0 * (1.0 - k) };
                        app.engine.send(Control::Pitch { d, v });
                        if k >= 1.0 {
                            drift = None;
                        }
                    }
                    // Get the next song ready on the other deck, in the background.
                    let mut next = next;
                    if let Some(job) = preparing.take_if(|j| j.is_finished()) {
                        next = job.await.ok().flatten();
                    }
                    if next.is_none() && preparing.is_none() && !st.decks[o].playing {
                        if let Some(item) = dj.pop() {
                            dj.note("playing", Some(d), None, None, format!("Getting “{}” ready…", item.name));
                            let app2 = app.clone();
                            preparing = Some(tokio::spawn(async move { prepare(&app2, o, &item).await.then_some(o) }));
                        }
                    }
                    let beats = *dj.bars.lock().unwrap() as f64 * 4.0;
                    if dj.skip.swap(false, Ordering::Relaxed) {
                        forced = mix_point(deck, beats, true).map(|(start, _)| start);
                    }
                    let point = match (forced, mix_point(deck, beats, false)) {
                        (Some(f), Some((start, _))) if f < start => beat_of(deck).map(|now| {
                            let period = deck.grid.map_or(1.0, |g| SRC_RATE * 60.0 / g.bpm);
                            (f, (f - now) * period / SRC_RATE / deck.tempo.max(0.1))
                        }),
                        (_, p) => p,
                    };
                    match (next, point) {
                        (Some(o), Some((start, secs))) => {
                            let now = beat_of(deck).unwrap_or(0.0);
                            if now >= start {
                                // Bring the next song in: on the beat, bass cut, fader down.
                                app.engine.send(Control::XfAssign { d: o, side: 0 });
                                app.engine.send(Control::XfAssign { d, side: 0 });
                                app.engine.send(Control::Stems { d: o, gains: [1.0; 4] });
                                sent.forget(o);
                                sent.fader(&app, o, 0.0);
                                sent.low(&app, o, 0.0, &st.decks[o]);
                                app.engine.send(Control::Sync { d: o, on: Some(true) });
                                app.engine.send(Control::Play { d: o });
                                Plan::Mixing { from: d, to: o, start, beats }
                            } else {
                                dj.note("playing", Some(d), Some(secs.max(0.0)), None, format!("Next: {}", st_name(&app, o)));
                                Plan::Playing { d, next: Some(o), drift, forced }
                            }
                        }
                        (None, _) => {
                            let left = (deck.len - deck.pos) / SRC_RATE / deck.tempo.max(0.1);
                            let note = if preparing.is_some() {
                                "Getting the next song ready…"
                            } else if left < 60.0 {
                                "Last song: add more to keep going"
                            } else {
                                "Playing"
                            };
                            dj.note("playing", Some(d), None, None, note);
                            Plan::Playing { d, next: None, drift, forced }
                        }
                        (Some(o), None) => {
                            dj.note("playing", Some(d), None, None, "Waiting for the beat grid…");
                            Plan::Playing { d, next: Some(o), drift, forced }
                        }
                    }
                }
            }
            Plan::Mixing { from, to, start, beats } => {
                let (a, b) = (&st.decks[from], &st.decks[to]);
                let now = beat_of(a).unwrap_or(start + beats);
                let p = ((now - start) / beats).clamp(0.0, 1.0);
                // In on the fader over the first 40%; bass swap around halfway;
                // the old song fades over the last 40%.
                sent.fader(&app, to, (p / 0.4).min(1.0) as f32);
                let swap = smooth((p - 0.45) / 0.1) as f32;
                sent.low(&app, from, 0.5 * (1.0 - swap), a);
                sent.low(&app, to, 0.5 * swap, b);
                sent.fader(&app, from, if p < 0.6 { 1.0 } else { (1.0 - (p - 0.6) / 0.4).max(0.0) as f32 });
                // Vocal guard: the new song's vocals wait for the old song to leave.
                let stems_ready = b.stems_status.is_some() && b.stems_ready > b.pos + SRC_RATE * 30.0;
                if stems_ready {
                    sent.vocals(&app, to, (((p - 0.7) / 0.3).clamp(0.0, 1.0)) as f32, b);
                }
                dj.note("mixing", Some(to), None, Some(p), format!("Mixing into {}", st_name(&app, to)));
                if p >= 1.0 || !a.playing {
                    app.engine.send(Control::Pause { d: from });
                    reset_deck(&app, &mut sent, from);
                    app.engine.send(Control::Eq { d: to, band: 0, v: 0.5 });
                    app.engine.send(Control::Stems { d: to, gains: [1.0; 4] });
                    app.engine.send(Control::Fader { d: to, v: 1.0 });
                    sent.forget(to);
                    app.engine.send(Control::Sync { d: to, on: Some(false) });
                    // Drift back to its own tempo, if it isn't far off; else stay.
                    let off = (b.tempo - 1.0).abs();
                    let pitch = ((b.tempo - 1.0) / b.range as f64).clamp(-1.0, 1.0) as f32;
                    app.engine.send(Control::Pitch { d: to, v: pitch });
                    let drift = (off > 0.0005 && off <= 0.08).then(|| (pitch, Instant::now()));
                    Plan::Playing { d: to, next: None, drift, forced: None }
                } else {
                    Plan::Mixing { from, to, start, beats }
                }
            }
        };
    }
}

fn st_name(app: &App, d: usize) -> String {
    app.decks.lock().unwrap().get(d).cloned().flatten().map(|o| format!("“{}”", o.track.name)).unwrap_or_else(|| "the next song".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::deck::Grid;

    fn deck(bpm: f64, secs: f64, at: f64) -> DeckState {
        DeckState {
            loaded: true,
            playing: true,
            tempo: 1.0,
            range: 0.08,
            len: secs * SRC_RATE,
            pos: at * SRC_RATE,
            grid: Some(Grid { bpm, anchor: 0.0 }),
            ..Default::default()
        }
    }

    #[test]
    fn mixes_out_on_a_phrase_before_the_end() {
        // 120 BPM, 4 minutes = 480 beats; a 16-bar (64 beat) mix.
        let (start, secs) = mix_point(&deck(120.0, 240.0, 10.0), 64.0, false).unwrap();
        assert_eq!(start % 32.0, 0.0, "starts on a phrase");
        assert!(start + 64.0 <= 480.0 - 8.0, "finishes before the end");
        assert!(start >= 480.0 - 64.0 - 8.0 - 32.0, "but not much earlier");
        assert!((secs - (start - 20.0) * 0.5).abs() < 1e-6);
        // Mix now: the next bar.
        let (start, _) = mix_point(&deck(120.0, 240.0, 10.0), 64.0, true).unwrap();
        assert_eq!(start, 24.0);
        // Past the phrase already: the next bar.
        let (start, _) = mix_point(&deck(120.0, 240.0, 235.0), 64.0, false).unwrap();
        assert_eq!(start, 472.0);
    }
}
