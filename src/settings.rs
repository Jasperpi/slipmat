//! Preferences, kept in ~/.config/slipmat/settings.json.

use serde::{Deserialize, Serialize};

use crate::engine::output::OutputConfig;
use crate::paths;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub output: OutputConfig,
    /// Tempo fader: pulling it toward you speeds up (Pioneer/Technics). Turn
    /// on to flip it.
    pub invert_tempo: bool,
    /// Pitch range new decks start with (0.08 = ±8%).
    pub range: f32,
    pub keylock: bool,
    pub quantize: bool,
    pub auto_gain: bool,
    /// Jog wheels scratch when touched on top (vinyl mode); off, they bend.
    pub vinyl: bool,
    /// Show keys as Camelot codes (8A) instead of names (Am).
    pub camelot: bool,
    /// Split every song into vocals, melody, bass and drums when it loads.
    pub stems: bool,
    /// Which separation: auto (Demucs on computers with 12 GB+), fast, best.
    pub stem_quality: crate::stems::Quality,
    /// Draw the big waveforms in stem colours once stems are ready.
    pub stem_colors: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output: OutputConfig::default(),
            invert_tempo: false,
            range: 0.08,
            keylock: true,
            quantize: true,
            auto_gain: true,
            vinyl: true,
            camelot: true,
            stems: true,
            stem_quality: crate::stems::Quality::Auto,
            stem_colors: true,
        }
    }
}

fn path() -> std::path::PathBuf {
    paths::config().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(paths::config());
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path(), bytes);
        }
    }
}
