//! The trained snare detector. It reuses the kick's container format and CNN family; every number
//! comes from the model container. Python reference: `tools/audio_analysis/snare_release.py`.

pub mod net;

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
