//! The trained kick detector (docs/KICK_REALTIME_DESIGN.md). Every number comes from
//! the model container; the Python recipe in `tools/audio_analysis/eval/` is the reference.

pub mod base;
pub mod container;
pub mod extra;
pub mod net;
pub mod stage;
pub mod trees;

#[cfg(test)]
pub(crate) mod golden {
    use super::container::Container;

    /// A parity golden from `tests/fixtures/kick/` (written by `tools/audio_analysis/kick_release.py`).
    pub fn load(name: &str) -> Container {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kick").join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Container::parse(&bytes).unwrap()
    }
}
