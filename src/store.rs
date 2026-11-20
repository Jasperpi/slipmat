//! What Slipmat remembers about each song: its analysis, any grid fix, its
//! cue point and hot cues. One small JSON file per song.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::analysis::Analysis;
use crate::engine::deck::{Grid, HOTCUES, MEMS, Mem};
use crate::paths;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrackData {
    pub analysis: Option<Analysis>,
    /// The grid as last set on a deck (analysis, or the user's correction).
    pub grid: Option<Grid>,
    pub cue: Option<f64>,
    pub hotcues: [Option<f64>; HOTCUES],
    /// Memory cues (the CUE/LOOP CALL buttons).
    pub memory: [Option<Mem>; MEMS],
}

/// What library rows show.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub bpm: f64,
    pub key: Option<u8>,
}

pub struct Store {
    dir: PathBuf,
    cache: Mutex<HashMap<String, TrackData>>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

impl Store {
    pub fn open() -> Self {
        let dir = paths::data().join("tracks");
        let _ = std::fs::create_dir_all(&dir);
        let mut cache = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let path = e.path();
                let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
                if let Ok(data) = std::fs::read(&path).map(|b| serde_json::from_slice::<TrackData>(&b)) {
                    if let Ok(data) = data {
                        cache.insert(id, data);
                    }
                }
            }
        }
        Self { dir, cache: Mutex::new(cache) }
    }

    pub fn get(&self, id: &str) -> TrackData {
        self.cache.lock().unwrap().get(id).cloned().unwrap_or_default()
    }

    pub fn summary(&self, id: &str) -> Option<Summary> {
        let cache = self.cache.lock().unwrap();
        let t = cache.get(id)?;
        let bpm = t.grid.map(|g| g.bpm).or(t.analysis.as_ref().map(|a| a.bpm))?;
        Some(Summary { bpm, key: t.analysis.as_ref().and_then(|a| a.key) })
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut TrackData)) {
        if !valid_id(id) {
            return;
        }
        let data = {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.entry(id.to_string()).or_default();
            let before = entry.clone();
            f(entry);
            if *entry == before {
                return;
            }
            entry.clone()
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&data) {
            let path = self.dir.join(format!("{id}.json"));
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}
