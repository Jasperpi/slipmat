//! Where Slipmat keeps things.

use std::path::PathBuf;

fn under(base: Option<PathBuf>) -> PathBuf {
    base.unwrap_or_else(|| PathBuf::from(".")).join("slipmat")
}

/// Login, downloaded audio: safe to delete.
pub fn cache() -> PathBuf {
    under(dirs::cache_dir())
}

/// Cue points, beat grids, analysis: worth keeping.
pub fn data() -> PathBuf {
    under(dirs::data_dir())
}

pub fn config() -> PathBuf {
    under(dirs::config_dir())
}
