//! Everything the server, the controller and the doctor share: the Spotify
//! session, the engine, the sound card, what's on each deck.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::analysis::{self, Analysis};
use crate::engine::deck::Grid;
use crate::engine::output::Output;
use crate::engine::{Cmd, Control, DECKS, Engine, Handle};
use crate::session::Spotify;
use crate::settings::Settings;
use crate::spotify::model::Track;
use crate::spotify::spot;
use crate::stems::{self, Stems};
use crate::store::Store;
use crate::track::TrackAudio;
use crate::loader;

/// Loudness every song is brought to when auto gain is on (RMS, dBFS).
const TARGET_DB: f32 = -11.0;

#[derive(Clone)]
pub struct OnDeck {
    pub track: Track,
    pub audio: Arc<TrackAudio>,
    pub stems: Option<Arc<Stems>>,
    pub analysis: Option<Analysis>,
    pub analysing: bool,
}

fn list_path(name: &str) -> std::path::PathBuf {
    crate::paths::cache().join("lists").join(format!("{name}.json"))
}

fn disk_list<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_slice(&std::fs::read(list_path(name)).ok()?).ok()
}

fn save_list<T: Serialize>(name: &str, v: &T) {
    let path = list_path(name);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Ok(bytes) = serde_json::to_vec(v) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// One row in a song list.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct Row {
    pub uri: String,
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub image: Option<String>,
    pub explicit: bool,
    pub playable: bool,
    pub bpm: Option<f64>,
    pub key: Option<u8>,
    pub added_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    Notice { text: String, error: bool },
    /// The controller's browse knob turned.
    Browse { delta: i32 },
    /// SHIFT + browse knob: zoom the waveforms in (negative) or out.
    Zoom { delta: i32 },
    /// The browse knob was pushed.
    BrowsePress { shift: bool },
    /// A LOAD button: load the highlighted song onto deck `d`.
    LoadSelected { d: usize },
    /// Analysis finished for a song (refresh BPM/key columns).
    Analysed { id: String, bpm: f64, key: Option<u8> },
    /// A list shown from the saved copy turned out different: fetch it again.
    ListChanged { list: String },
    /// SHIFT + LOAD on the controller: sort the song list by this column.
    Sort { by: &'static str },
}

/// Things the controller asks for that need to know what's on the decks.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    /// Instant double: the other deck's song onto `d`, at the same place.
    Double { d: usize },
    /// Shift deck `d`'s key to go with the other deck's.
    KeySync { d: usize },
    AutoDjToggle,
}

/// A song that was played to the room, and when.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct Played {
    pub at: i64,
    pub row: Row,
}

/// Seconds a song has to be heard before it counts as played.
const PLAYED_AFTER: u32 = 20;
const HISTORY_MAX: usize = 1000;

fn history_path() -> std::path::PathBuf {
    crate::paths::data().join("history.json")
}

/// Semitones (the fewest, at most six either way) that bring key `from`
/// to `to` or to its relative major/minor. Keys are 0-11 major from C,
/// 12-23 minor from C.
pub fn key_match(from: u8, to: u8) -> i8 {
    let shifted = |k: u8, by: i8| -> u8 {
        let base = if k >= 12 { 12 } else { 0 };
        base + ((k - base) as i8 + by).rem_euclid(12) as u8
    };
    // C major and A minor share their notes: minor root = major root + 9.
    let relative = |k: u8| if k >= 12 { (k - 12 + 3) % 12 } else { 12 + (k + 9) % 12 };
    let mut best = 0i8;
    for by in [0i8, 1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6] {
        let k = shifted(from, by);
        if k == to || k == relative(to) {
            best = by;
            break;
        }
    }
    best
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct MidiStatus {
    pub connected: Option<String>,
    pub ports: Vec<String>,
    pub last: Option<String>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct BatchStatus {
    pub queued: usize,
    pub done: usize,
    pub current: Option<String>,
}

pub struct App {
    pub spotify: Arc<Spotify>,
    pub autodj: crate::autodj::AutoDj,
    pub engine: Handle,
    pub output: Output,
    pub store: Store,
    pub settings: Mutex<Settings>,
    pub decks: Mutex<Vec<Option<OnDeck>>>,
    pub events: broadcast::Sender<Event>,
    pub midi: Mutex<MidiStatus>,
    pub batch: Mutex<BatchStatus>,
    batch_tx: tokio::sync::mpsc::UnboundedSender<String>,
    lists: Mutex<HashMap<String, (Instant, Vec<Row>)>>,
    /// Counts loads per deck, so only the latest one asked for lands.
    load_gen: [AtomicU64; DECKS],
    doubled_at: Mutex<[Option<Instant>; DECKS]>,
    /// Names of the sounds on the sampler pads.
    pub sampler_names: Vec<String>,
    pub history: Mutex<Vec<Played>>,
    /// Songs played since Slipmat started (their ids).
    pub played: Mutex<Vec<String>>,
}

fn auto_gain(a: &Analysis) -> f32 {
    (TARGET_DB - a.loudness_db).clamp(-12.0, 9.0)
}

fn auto_cue(a: &Analysis) -> f64 {
    let g = Grid { bpm: a.bpm, anchor: a.anchor };
    let beat = g.nearest(a.first_sound);
    if (beat - a.first_sound).abs() < g.period() / 4.0 && beat >= 0.0 { beat } else { a.first_sound }
}

impl App {
    pub fn start() -> Arc<Self> {
        let settings = Settings::load();
        let (mut engine, handle, garbage) = Engine::new(44_100.0);
        // Songs taken off a deck are freed here, never in the audio callback.
        std::thread::spawn(move || while garbage.recv().is_ok() {});
        let _ = &mut engine;
        let engine = Arc::new(Mutex::new(engine));
        let output = Output::start(engine.clone(), settings.output.clone());
        for d in 0..DECKS {
            handle.send(Control::Range { d, range: settings.range });
            handle.send(Control::KeyLock { d, on: settings.keylock });
        }
        handle.send(Control::Quantize { on: settings.quantize });
        handle.send(Control::AutoGain { on: settings.auto_gain });
        // The sampler's sounds: built in, or your own WAV files.
        let samples_dir = crate::paths::config().join("samples");
        let _ = std::fs::create_dir_all(&samples_dir);
        let bank = crate::engine::sampler::bank(&samples_dir);
        let sampler_names = bank.iter().map(|s| s.name.clone()).collect();
        for (n, sample) in bank.into_iter().enumerate() {
            let _ = handle.tx.send(Cmd::Sample { n, sample: Some(sample) });
        }
        let history: Vec<Played> = std::fs::read(history_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let (events, _) = broadcast::channel(256);
        let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = Arc::new(Self {
            spotify: Spotify::new(),
            autodj: crate::autodj::AutoDj::default(),
            engine: handle,
            output,
            store: Store::open(),
            settings: Mutex::new(settings),
            decks: Mutex::new(vec![None; DECKS]),
            events,
            midi: Mutex::new(MidiStatus::default()),
            batch: Mutex::new(BatchStatus::default()),
            batch_tx,
            lists: Mutex::new(HashMap::new()),
            load_gen: Default::default(),
            doubled_at: Mutex::new([None; DECKS]),
            sampler_names,
            history: Mutex::new(history),
            played: Mutex::new(Vec::new()),
        });
        app.spotify.resume();
        tokio::spawn(app.clone().keep_cues());
        tokio::spawn(app.clone().batch_worker(batch_rx));
        tokio::spawn(crate::autodj::run(app.clone()));
        app
    }

    pub fn notice(&self, text: impl Into<String>, error: bool) {
        let _ = self.events.send(Event::Notice { text: text.into(), error });
    }

    pub fn row(&self, t: &Track) -> Row {
        let s = self.store.summary(&t.id);
        Row {
            uri: t.uri.clone(),
            id: t.id.clone(),
            name: t.name.clone(),
            artist: t.artist_line(),
            album: t.album.clone(),
            duration_ms: t.duration_ms,
            image: t.image.clone(),
            explicit: t.explicit,
            playable: t.playable && t.uri.starts_with("spotify:track:"),
            bpm: s.as_ref().map(|s| s.bpm),
            key: s.and_then(|s| s.key),
            added_at: t.added_at,
        }
    }

    fn rows(&self, tracks: &[Track]) -> Vec<Row> {
        tracks.iter().map(|t| self.row(t)).collect()
    }

    /// A song list, fetched at most once a few minutes; BPM/key always fresh.
    async fn cached_list<F, Fut>(&self, key: &str, fetch: F) -> Result<Vec<Row>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<Track>>>,
    {
        if let Some((at, rows)) = self.lists.lock().unwrap().get(key) {
            if at.elapsed() < Duration::from_secs(300) {
                let mut rows = rows.clone();
                for r in &mut rows {
                    let s = self.store.summary(&r.id);
                    r.bpm = s.as_ref().map(|s| s.bpm);
                    r.key = s.and_then(|s| s.key);
                }
                return Ok(rows);
            }
        }
        let tracks = fetch().await?;
        let rows = self.rows(&tracks);
        self.lists.lock().unwrap().insert(key.to_string(), (Instant::now(), rows.clone()));
        Ok(rows)
    }

    /// Liked Songs. A copy kept on disk shows at once (even before Spotify
    /// has answered); the real list is fetched behind it and the page told
    /// if anything changed.
    pub async fn liked(self: &Arc<Self>) -> Result<Vec<Row>> {
        if let Some(rows) = self.fresh_list("liked") {
            return Ok(rows);
        }
        if let Some(rows) = disk_list::<Vec<Row>>("liked") {
            let app = self.clone();
            let before: Vec<String> = rows.iter().map(|r| r.uri.clone()).collect();
            tokio::spawn(async move {
                if let Ok(fresh) = app.fetch_liked().await {
                    if fresh.iter().map(|r| &r.uri).ne(before.iter()) {
                        let _ = app.events.send(Event::ListChanged { list: "liked".into() });
                    }
                }
            });
            return Ok(self.with_analysis(rows));
        }
        self.fetch_liked().await
    }

    async fn fetch_liked(&self) -> Result<Vec<Row>> {
        let session = self.spotify.session().await?;
        let (mut items, _) = spot::collection(&session, "collection").await?;
        items.retain(|s| !s.removed && s.uri.starts_with("spotify:track:"));
        items.sort_by(|a, b| b.added_at.cmp(&a.added_at));
        let uris: Vec<String> = items.iter().map(|s| s.uri.clone()).collect();
        let mut tracks = spot::tracks(&session, &uris).await;
        for (t, s) in tracks.iter_mut().zip(&items) {
            t.added_at = Some(s.added_at);
        }
        let rows = self.rows(&tracks);
        self.lists.lock().unwrap().insert("liked".into(), (Instant::now(), rows.clone()));
        save_list("liked", &rows);
        Ok(rows)
    }

    /// The playlist sidebar, the same way: from disk first, then fresh.
    pub async fn playlists(self: &Arc<Self>) -> Result<serde_json::Value> {
        if let Some(v) = disk_list::<serde_json::Value>("playlists") {
            if self.fresh_list("playlists").is_none() {
                let app = self.clone();
                let saved = v.clone();
                self.lists.lock().unwrap().insert("playlists".into(), (Instant::now(), Vec::new()));
                tokio::spawn(async move {
                    if let Ok(fresh) = app.fetch_playlists().await {
                        if fresh != saved {
                            let _ = app.events.send(Event::ListChanged { list: "playlists".into() });
                        }
                    }
                });
            }
            return Ok(v);
        }
        self.fetch_playlists().await
    }

    async fn fetch_playlists(&self) -> Result<serde_json::Value> {
        let session = self.spotify.session().await?;
        let entries = spot::rootlist(&session).await?;
        let list: Vec<serde_json::Value> = entries
            .into_iter()
            .filter_map(|e| match e {
                crate::spotify::model::SideEntry::Folder { name, depth } => Some(serde_json::json!({ "folder": name, "depth": depth })),
                crate::spotify::model::SideEntry::Playlist { playlist, depth } => Some(serde_json::json!({
                    "id": playlist.id, "name": playlist.name, "owner": playlist.owner, "len": playlist.len, "depth": depth
                })),
                _ => None,
            })
            .collect();
        let v = serde_json::Value::Array(list);
        save_list("playlists", &v);
        Ok(v)
    }

    fn fresh_list(&self, key: &str) -> Option<Vec<Row>> {
        let lists = self.lists.lock().unwrap();
        let (at, rows) = lists.get(key)?;
        (at.elapsed() < Duration::from_secs(300)).then(|| self.with_analysis(rows.clone()))
    }

    fn with_analysis(&self, mut rows: Vec<Row>) -> Vec<Row> {
        for r in &mut rows {
            let s = self.store.summary(&r.id);
            r.bpm = s.as_ref().map(|s| s.bpm);
            r.key = s.and_then(|s| s.key);
        }
        rows
    }

    /// Forget saved lists (on sign-out: they belong to that account).
    pub fn forget_lists(&self) {
        self.lists.lock().unwrap().clear();
        let _ = std::fs::remove_dir_all(crate::paths::cache().join("lists"));
    }

    pub async fn playlist(&self, id: &str) -> Result<(String, Vec<Row>)> {
        let session = self.spotify.session().await?;
        let head = spot::playlist(&session, id).await?;
        let name = head.playlist.name.clone();
        let rows = self
            .cached_list(&format!("pl:{id}:{}", head.revision), || async move {
                let uris: Vec<String> = head.items.iter().map(|(u, _)| u.clone()).collect();
                let mut tracks = spot::tracks(&session, &uris).await;
                for (t, (_, at)) in tracks.iter_mut().zip(&head.items) {
                    t.added_at = *at;
                }
                tracks.retain(|t| t.uri.starts_with("spotify:track:"));
                Ok(tracks)
            })
            .await?;
        Ok((name, rows))
    }

    pub async fn search(&self, q: &str) -> Result<Vec<Row>> {
        let session = self.spotify.session().await?;
        let q = q.trim().to_string();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        self.cached_list(&format!("q:{}", q.to_lowercase()), || async move {
            let uris = spot::search_tracks(&session, &q).await?;
            Ok(spot::tracks(&session, &uris).await)
        })
        .await
    }

    /// Load a Spotify song onto a deck.
    pub async fn load(self: &Arc<Self>, d: usize, uri: &str) -> Result<Track> {
        self.load_attempt(d, uri, 0).await
    }

    async fn load_attempt(self: &Arc<Self>, d: usize, uri: &str, attempt: u32) -> Result<Track> {
        if d >= DECKS {
            bail!("no deck {}", d + 1);
        }
        if self.engine.snapshot().decks.get(d).is_some_and(|s| s.playing) {
            bail!("Deck {} is playing. Pause it first.", d + 1);
        }
        // If another song is asked for while this one is being fetched,
        // that one wins.
        let turn = self.load_gen[d].fetch_add(1, Ordering::SeqCst) + 1;
        let session = self.spotify.session().await?;
        let track = spot::tracks(&session, &[uri.to_string()]).await.into_iter().next().ok_or_else(|| anyhow!("song not found"))?;
        if !track.playable {
            bail!("“{}” can't be played here", track.name);
        }
        if self.load_gen[d].load(Ordering::SeqCst) != turn {
            bail!("“{}” was replaced by the next song loaded onto deck {}", track.name, d + 1);
        }
        let audio = loader::load(&session, uri, track.duration_ms)?;
        // Split into stems in the background, as the song arrives.
        let settings = self.settings();
        let stems = settings.stems.then(|| {
            let st = Arc::new(Stems::for_track(&audio, stems::Model::choose(settings.stem_quality)));
            stems::spawn(audio.clone(), st.clone());
            st
        });
        let saved = self.store.get(&track.id);
        let grid = saved.grid.or(saved.analysis.as_ref().map(|a| Grid { bpm: a.bpm, anchor: a.anchor }));
        let cue = saved.cue.or(saved.analysis.as_ref().map(auto_cue)).unwrap_or(0.0);
        let gain_db = saved.analysis.as_ref().map(auto_gain).unwrap_or(0.0);
        self.engine.tx.send(Cmd::Load { d, track: audio.clone(), stems: stems.clone(), grid, cue, hotcues: saved.hotcues, memory: saved.memory, gain_db })?;
        let analysing = saved.analysis.is_none();
        self.decks.lock().unwrap()[d] = Some(OnDeck { track: track.clone(), audio: audio.clone(), stems, analysis: saved.analysis.clone(), analysing });
        let app = self.clone();
        let uri = uri.to_string();
        let id = track.id.clone();
        tokio::spawn(async move { app.after_load(d, uri, id, audio, analysing, attempt).await });
        Ok(track)
    }

    /// Try a song that wouldn't download once more (boxed: it loops back
    /// into loading).
    fn retry(self: Arc<Self>, d: usize, uri: String, attempt: u32) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            if let Err(e) = self.load_attempt(d, &uri, attempt).await {
                self.notice(e.to_string(), true);
            }
        })
    }

    /// Wait for the song to finish decoding, then analyse it if it's new.
    async fn after_load(self: Arc<Self>, d: usize, uri: String, id: String, audio: Arc<TrackAudio>, analyse: bool, attempt: u32) {
        while !audio.done.load(Ordering::Acquire) {
            if audio.failed.load(Ordering::Acquire) {
                // Spotify drops a download now and then: try again, as long
                // as the deck is still waiting for this song.
                let waiting = self.engine.snapshot().decks.get(d).is_some_and(|s| s.track_key == Arc::as_ptr(&audio) as usize && !s.playing);
                if waiting && attempt < 2 {
                    self.notice("Spotify dropped that download; trying again…", false);
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    self.clone().retry(d, uri, attempt + 1).await;
                } else {
                    self.notice("That song wouldn't download from Spotify. Try again, or pick another.", true);
                }
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !analyse {
            return;
        }
        let a = audio.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::priority::background();
            analysis::analyse(&a.mono())
        })
        .await
        .ok()
        .flatten();
        // The same song may be on both decks (an instant double).
        let on_decks = |f: &dyn Fn(&mut OnDeck)| {
            for on in self.decks.lock().unwrap().iter_mut().flatten() {
                if Arc::ptr_eq(&on.audio, &audio) {
                    f(on);
                }
            }
        };
        let Some(result) = result else {
            on_decks(&|on| on.analysing = false);
            return;
        };
        self.store.update(&id, |t| t.analysis = Some(result.clone()));
        let grid = Grid { bpm: result.bpm, anchor: result.anchor };
        for deck in 0..DECKS {
            let _ = self.engine.tx.send(Cmd::Analysed { d: deck, track: audio.clone(), grid, cue: Some(auto_cue(&result)), gain_db: auto_gain(&result) });
        }
        on_decks(&|on| {
            on.analysis = Some(result.clone());
            on.analysing = false;
        });
        let _ = self.events.send(Event::Analysed { id, bpm: result.bpm, key: result.key });
    }

    /// LOAD pressed twice asks for a double; the first press also asked the
    /// page to load the highlighted song, which must then be ignored.
    pub fn doubled_recently(&self, d: usize) -> bool {
        self.doubled_at.lock().unwrap().get(d).copied().flatten().is_some_and(|at| at.elapsed() < Duration::from_millis(900))
    }

    pub fn act(self: &Arc<Self>, action: Action) {
        match action {
            Action::Double { d } => self.double(d),
            Action::KeySync { d } => self.key_sync(d),
            Action::AutoDjToggle => {
                let on = !self.autodj.on.load(Ordering::Relaxed);
                self.autodj.on.store(on, Ordering::Relaxed);
                self.notice(if on { "Auto-DJ on" } else { "Auto-DJ off" }, false);
            }
        }
    }

    /// Instant double: the other deck's song, at the same place and tempo.
    pub fn double(self: &Arc<Self>, d: usize) {
        if d >= DECKS {
            return;
        }
        let o = (d + 1) % DECKS;
        if self.engine.snapshot().decks.get(d).is_some_and(|s| s.playing) {
            return self.notice(format!("Deck {} is playing. Pause it first.", d + 1), true);
        }
        let Some(src) = self.decks.lock().unwrap()[o].clone() else {
            return self.notice(format!("Nothing on deck {} to double", o + 1), true);
        };
        // Cancels a load still on its way to this deck.
        self.load_gen[d].fetch_add(1, Ordering::SeqCst);
        self.doubled_at.lock().unwrap()[d] = Some(Instant::now());
        let name = src.track.name.clone();
        self.decks.lock().unwrap()[d] = Some(src);
        self.engine.send(Control::Double { d });
        self.notice(format!("Deck {}: instant double of “{name}”", d + 1), false);
    }

    /// Key sync: shift deck `d` the few semitones that make it harmonise
    /// with the other deck.
    pub fn key_sync(&self, d: usize) {
        if d >= DECKS {
            return;
        }
        let o = (d + 1) % DECKS;
        let keys: Vec<Option<u8>> = self.decks.lock().unwrap().iter().map(|on| on.as_ref().and_then(|on| on.analysis.as_ref()).and_then(|a| a.key)).collect();
        let (Some(mine), Some(theirs)) = (keys[d], keys[o]) else {
            return self.notice("Key sync needs both songs' keys (still analysing?)", true);
        };
        // The other deck may itself be shifted.
        let st = self.engine.snapshot();
        let theirs_now = {
            let by = st.decks.get(o).map_or(0, |s| s.key_shift);
            let base = if theirs >= 12 { 12 } else { 0 };
            base + ((theirs - base) as i8 + by).rem_euclid(12) as u8
        };
        let by = key_match(mine, theirs_now);
        self.engine.send(Control::KeyShift { d, semitones: by });
        let text = match by {
            0 => format!("Deck {}: already in a key that fits", d + 1),
            _ => format!("Deck {}: key shifted {by:+} to fit deck {}", d + 1, o + 1),
        };
        self.notice(text, false);
    }

    /// Analyse songs without loading them (for the BPM and key columns).
    pub fn analyse_later(&self, uris: Vec<String>) {
        let fresh: Vec<String> = uris
            .into_iter()
            .filter(|u| u.starts_with("spotify:track:") && self.store.summary(crate::spotify::model::id_of(u)).is_none())
            .collect();
        self.batch.lock().unwrap().queued += fresh.len();
        for u in fresh {
            let _ = self.batch_tx.send(u);
        }
    }

    async fn batch_worker(self: Arc<Self>, mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
        while let Some(uri) = rx.recv().await {
            let id = crate::spotify::model::id_of(&uri).to_string();
            if self.store.summary(&id).is_some() {
                self.batch_progress(None);
                continue;
            }
            let Ok(session) = self.spotify.session().await else {
                self.batch_progress(None);
                continue;
            };
            let Some(track) = spot::tracks(&session, &[uri.clone()]).await.into_iter().next() else {
                self.batch_progress(None);
                continue;
            };
            self.batch.lock().unwrap().current = Some(format!("{} · {}", track.name, track.artist_line()));
            if let Ok(audio) = loader::load(&session, &uri, track.duration_ms) {
                let started = Instant::now();
                while !audio.done.load(std::sync::atomic::Ordering::Acquire)
                    && !audio.failed.load(std::sync::atomic::Ordering::Acquire)
                    && started.elapsed() < Duration::from_secs(120)
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if audio.done.load(std::sync::atomic::Ordering::Acquire) {
                    let a = audio.clone();
                    if let Ok(Some(result)) = tokio::task::spawn_blocking(move || {
            crate::priority::background();
            analysis::analyse(&a.mono())
        }).await {
                        self.store.update(&id, |t| t.analysis = Some(result.clone()));
                        let _ = self.events.send(Event::Analysed { id: id.clone(), bpm: result.bpm, key: result.key });
                    }
                }
            }
            self.batch_progress(None);
        }
    }

    fn batch_progress(&self, current: Option<String>) {
        let mut b = self.batch.lock().unwrap();
        b.done += 1;
        b.current = current;
        if b.done >= b.queued {
            *b = BatchStatus::default();
        }
    }

    /// Save cue points, hot cues and grid edits as they change.
    async fn keep_cues(self: Arc<Self>) {
        // Per deck: the song being counted, seconds heard, already logged.
        let mut heard = [(0usize, 0u32, false); DECKS];
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            stems::release_idle();
            let st = self.engine.snapshot();
            let decks = self.decks.lock().unwrap().clone();
            let (xl, xr) = crate::engine::dsp::crossfade(st.crossfader, st.curve);
            for (d, on) in decks.iter().enumerate() {
                let (Some(on), Some(s)) = (on, st.decks.get(d)) else { continue };
                // Only once the engine has the same song.
                if s.track_key != Arc::as_ptr(&on.audio) as usize {
                    continue;
                }
                // History: a song counts once the room has heard it a while.
                let h = &mut heard[d];
                if h.0 != s.track_key {
                    *h = (s.track_key, 0, false);
                }
                let side = match s.side {
                    -1 => xl,
                    1 => xr,
                    _ => 1.0,
                };
                if s.playing && s.fader * side > 0.05 && st.master > 0.0 {
                    h.1 += 1;
                    if h.1 >= PLAYED_AFTER && !h.2 {
                        h.2 = true;
                        self.log_played(&on.track);
                    }
                }
                self.store.update(&on.track.id, |t| {
                    t.cue = Some(s.cue);
                    t.hotcues = s.hotcues;
                    t.memory = s.memory;
                    if s.grid.is_some() {
                        t.grid = s.grid;
                    }
                });
            }
        }
    }

    fn log_played(&self, track: &Track) {
        // Both decks playing the same song (a double) is one play.
        let mut played = self.played.lock().unwrap();
        if played.last() == Some(&track.id) {
            return;
        }
        played.push(track.id.clone());
        drop(played);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut history = self.history.lock().unwrap();
        history.push(Played { at, row: self.row(track) });
        let extra = history.len().saturating_sub(HISTORY_MAX);
        history.drain(..extra);
        if let Ok(bytes) = serde_json::to_vec(&*history) {
            let path = history_path();
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// Everything played, newest first, with BPM and key as known now.
    pub fn history_rows(&self) -> Vec<Row> {
        let rows = self.history.lock().unwrap().iter().rev().map(|p| Row { added_at: Some(p.at), ..p.row.clone() }).collect();
        self.with_analysis(rows)
    }

    pub fn clear_history(&self) {
        self.history.lock().unwrap().clear();
        self.played.lock().unwrap().clear();
        let _ = std::fs::remove_file(history_path());
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn save_settings(&self, new: Settings) -> Result<()> {
        let old = self.settings();
        if new.output != old.output {
            self.output.reopen(new.output.clone())?;
        }
        if new.quantize != old.quantize {
            self.engine.send(Control::Quantize { on: new.quantize });
        }
        if new.auto_gain != old.auto_gain {
            self.engine.send(Control::AutoGain { on: new.auto_gain });
        }
        new.save();
        *self.settings.lock().unwrap() = new;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::key_match;

    #[test]
    fn key_sync_finds_the_nearest_fitting_key() {
        // C major (0) fits A minor (21) as it is.
        assert_eq!(key_match(0, 21), 0);
        assert_eq!(key_match(5, 5), 0);
        // D major (2) to C major: down two.
        assert_eq!(key_match(2, 0), -2);
        // B minor (23) to C major (0): up one makes C minor? No: the fit is
        // A minor, two down.
        assert_eq!(key_match(23, 0), -2);
        // F# major (6) to C major: six away, the furthest it ever goes.
        assert_eq!(key_match(6, 0).abs(), 6);
        // Whatever comes back really does fit.
        for from in 0..24u8 {
            for to in 0..24u8 {
                let by = key_match(from, to);
                let base = if from >= 12 { 12 } else { 0 };
                let k = base + ((from - base) as i8 + by).rem_euclid(12) as u8;
                let relative = if to >= 12 { (to - 12 + 3) % 12 } else { 12 + (to + 9) % 12 };
                assert!(k == to || k == relative, "{from} -> {to} by {by} gave {k}");
                assert!(by.abs() <= 6);
            }
        }
    }
}
