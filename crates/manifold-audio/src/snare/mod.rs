//! The trained snare detector (docs/SNARE_REALTIME_DESIGN.md). Every number comes from the model container
//! (`assets/snare_model.mkick`); `tools/audio_analysis/snare_release.py` (`model_entries`,
//! `reference`) is the reference and writes the parity goldens under `tests/fixtures/snare/`.
//! The net reuses the kick's CNN family (`kick::net`).

pub mod cands;
pub mod detector;
pub mod flat;
pub mod net;

pub use detector::{SnareDetector, SnareError, SnareFire};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod golden {
    use crate::kick::container::Container;

    /// A parity golden from `tests/fixtures/snare/` (written by `tools/audio_analysis/snare_release.py`).
    pub fn load(name: &str) -> Container {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snare").join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Container::parse(&bytes).unwrap()
    }
}
