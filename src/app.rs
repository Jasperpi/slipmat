//! Everything the server, the controller and the doctor share: the Spotify
//! session, the engine, the sound card, what's on each deck.

use std::collections::HashMap;
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
use crate::store::Store;
use crate::track::TrackAudio;
use crate::loader;

/// Loudness every song is brought to when auto gain is on (RMS, dBFS).
const TARGET_DB: f32 = -11.0;

#[derive(Clone)]
pub struct OnDeck {
    pub track: Track,
    pub audio: Arc<TrackAudio>,
    pub analysis: Option<Analysis>,
    pub analysing: bool,
}

/// One row in a song list.
#[derive(Clone, Debug, Serialize)]
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
    /// The browse knob was pushed.
    BrowsePress { shift: bool },
    /// A LOAD button: load the highlighted song onto deck `d`.
    LoadSelected { d: usize },
    /// Analysis finished for a song (refresh BPM/key columns).
    Analysed { id: String, bpm: f64, key: Option<u8> },
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
        let (events, _) = broadcast::channel(256);
        let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = Arc::new(Self {
            spotify: Spotify::new(),
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
        });
        app.spotify.resume();
        tokio::spawn(app.clone().keep_cues());
        tokio::spawn(app.clone().batch_worker(batch_rx));
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

    pub async fn liked(&self) -> Result<Vec<Row>> {
        let session = self.spotify.session().await?;
        self.cached_list("liked", || async move {
            let (mut items, _) = spot::collection(&session, "collection").await?;
            items.retain(|s| !s.removed && s.uri.starts_with("spotify:track:"));
            items.sort_by(|a, b| b.added_at.cmp(&a.added_at));
            let uris: Vec<String> = items.iter().map(|s| s.uri.clone()).collect();
            let mut tracks = spot::tracks(&session, &uris).await;
            for (t, s) in tracks.iter_mut().zip(&items) {
                t.added_at = Some(s.added_at);
            }
            Ok(tracks)
        })
        .await
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
        if d >= DECKS {
            bail!("no deck {}", d + 1);
        }
        if self.engine.snapshot().decks.get(d).is_some_and(|s| s.playing) {
            bail!("Deck {} is playing. Pause it first.", d + 1);
        }
        let session = self.spotify.session().await?;
        let track = spot::tracks(&session, &[uri.to_string()]).await.into_iter().next().ok_or_else(|| anyhow!("song not found"))?;
        if !track.playable {
            bail!("“{}” can't be played here", track.name);
        }
        let audio = loader::load(&session, uri, track.duration_ms)?;
        let saved = self.store.get(&track.id);
        let grid = saved.grid.or(saved.analysis.as_ref().map(|a| Grid { bpm: a.bpm, anchor: a.anchor }));
        let cue = saved.cue.or(saved.analysis.as_ref().map(auto_cue)).unwrap_or(0.0);
        let gain_db = saved.analysis.as_ref().map(auto_gain).unwrap_or(0.0);
        self.engine.tx.send(Cmd::Load { d, track: audio.clone(), grid, cue, hotcues: saved.hotcues, gain_db })?;
        let analysing = saved.analysis.is_none();
        self.decks.lock().unwrap()[d] = Some(OnDeck { track: track.clone(), audio: audio.clone(), analysis: saved.analysis.clone(), analysing });
        let app = self.clone();
        let id = track.id.clone();
        tokio::spawn(async move { app.after_load(d, id, audio, analysing).await });
        Ok(track)
    }

    /// Wait for the song to finish decoding, then analyse it if it's new.
    async fn after_load(self: Arc<Self>, d: usize, id: String, audio: Arc<TrackAudio>, analyse: bool) {
        while !audio.done.load(std::sync::atomic::Ordering::Acquire) {
            if audio.failed.load(std::sync::atomic::Ordering::Acquire) {
                self.notice("That song wouldn't download from Spotify. Try again, or pick another.", true);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !analyse {
            return;
        }
        let a = audio.clone();
        let Ok(Some(result)) = tokio::task::spawn_blocking(move || analysis::analyse(&a.mono())).await else {
            if let Some(Some(on)) = self.decks.lock().unwrap().get_mut(d) {
                on.analysing = false;
            }
            return;
        };
        self.store.update(&id, |t| t.analysis = Some(result.clone()));
        let grid = Grid { bpm: result.bpm, anchor: result.anchor };
        let _ = self.engine.tx.send(Cmd::Analysed { d, track: audio.clone(), grid, cue: Some(auto_cue(&result)), gain_db: auto_gain(&result) });
        if let Some(Some(on)) = self.decks.lock().unwrap().get_mut(d) {
            if Arc::ptr_eq(&on.audio, &audio) {
                on.analysis = Some(result.clone());
                on.analysing = false;
            }
        }
        let _ = self.events.send(Event::Analysed { id, bpm: result.bpm, key: result.key });
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
                    if let Ok(Some(result)) = tokio::task::spawn_blocking(move || analysis::analyse(&a.mono())).await {
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
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let st = self.engine.snapshot();
            let decks = self.decks.lock().unwrap().clone();
            for (d, on) in decks.iter().enumerate() {
                let (Some(on), Some(s)) = (on, st.decks.get(d)) else { continue };
                // Only once the engine has the same song.
                if s.track_key != Arc::as_ptr(&on.audio) as usize {
                    continue;
                }
                self.store.update(&on.track.id, |t| {
                    t.cue = Some(s.cue);
                    t.hotcues = s.hotcues;
                    if s.grid.is_some() {
                        t.grid = s.grid;
                    }
                });
            }
        }
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
