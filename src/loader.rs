//! Decodes a whole Spotify song into memory.
//!
//! A librespot `Player` decodes as fast as its sink accepts audio, so a sink
//! that never waits turns it into a downloader: a song is ready in a few
//! seconds, and playback can start while the rest arrives.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use librespot_core::{Session, SpotifyUri};
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::NoOpVolume;
use librespot_playback::player::{Player, PlayerEvent};

use crate::track::{TrackAudio, WaveBuilder};

struct LoadSink {
    track: Arc<TrackAudio>,
    wave: WaveBuilder,
    buf: Vec<f32>,
}

impl Sink for LoadSink {
    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let Ok(samples) = packet.samples() else { return Ok(()) };
        self.buf.clear();
        self.buf.extend(samples.iter().map(|&s| s as f32));
        let n = self.track.push(&self.buf);
        let used = &self.buf[..n * 2];
        self.wave.feed(&self.track, used);
        Ok(())
    }
}

/// Start decoding `uri`. The returned buffer fills in the background;
/// `done` or `failed` is set when that ends.
pub fn load(session: &Session, uri: &str, duration_ms: u32) -> Result<Arc<TrackAudio>> {
    let id = SpotifyUri::from_uri(uri).map_err(|_| anyhow!("{uri} can't be loaded on a deck"))?;
    let track = Arc::new(TrackAudio::new(duration_ms.max(1000)));
    let config = PlayerConfig { bitrate: Bitrate::Bitrate320, normalisation: false, gapless: false, ..Default::default() };
    let sink_track = track.clone();
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || -> Box<dyn Sink> {
        Box::new(LoadSink { track: sink_track, wave: WaveBuilder::default(), buf: Vec::with_capacity(8192) })
    });
    let mut events = player.get_player_event_channel();
    player.load(id, true, 0);
    let watch = track.clone();
    let uri = uri.to_string();
    tokio::spawn(async move {
        let started = Instant::now();
        // Give up on a load that stops making progress.
        let mut last_len = 0;
        let mut last_progress = Instant::now();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
                Ok(Some(PlayerEvent::EndOfTrack { .. })) => {
                    watch.done.store(true, Ordering::Release);
                    break;
                }
                Ok(Some(PlayerEvent::Unavailable { .. })) => {
                    log::warn!("{uri} is unavailable");
                    watch.failed.store(true, Ordering::Release);
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    watch.failed.store(!watch.done.load(Ordering::Acquire), Ordering::Release);
                    break;
                }
                Err(_) => {}
            }
            let len = watch.len();
            if len != last_len {
                last_len = len;
                last_progress = Instant::now();
            } else if last_progress.elapsed() > Duration::from_secs(if len == 0 { 12 } else { 30 }) {
                // (One that never starts is given up on sooner: it's retried.)
                log::warn!("{uri} stalled at {len} frames");
                watch.failed.store(len == 0, Ordering::Release);
                watch.done.store(len > 0, Ordering::Release);
                break;
            }
        }
        player.stop();
        log::info!(
            "decoded {uri}: {:.1} s of audio in {:.1} s",
            watch.len() as f64 / crate::track::SRC_RATE,
            started.elapsed().as_secs_f64()
        );
        drop(player);
    });
    Ok(track)
}
