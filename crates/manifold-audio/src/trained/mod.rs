//! The generic detector runtime: one [`TrainedDetector`] runs any
//! lab-built detector from its contract-v2 model file. The file names its parts (finder, measures, net,
//! trees, combine, history stage, cutoff); [`spec::Spec`] refuses anything this build cannot run, and only
//! the named parts are built. `tools/audio_analysis/lab/release.py` writes the files and the parity goldens.

pub mod container;
pub mod detector;
pub mod finder;
pub mod frames;
pub mod history;
pub(crate) mod input;
pub mod measure;
pub mod net;
pub mod spec;
pub mod trees;

pub use detector::{Decision, DetectorError, Fire, TrainedDetector};
pub use spec::{Spec, SpecError};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod golden {
    use super::container::Container;

    fn read(rel: &str) -> Container {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Container::parse(&bytes).unwrap()
    }

    /// A clip golden from `tests/fixtures/<set>/`.
    pub fn load(set: &str, name: &str) -> Container {
        read(&format!("tests/fixtures/{set}/{name}"))
    }

    /// The clip goldens of `tests/fixtures/<set>/`, sorted.
    pub fn clips(set: &str) -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(set);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no clip goldens in {}", dir.display());
        names
    }

    /// A unit fixture from `tests/fixtures/detectors/unit/`.
    pub fn unit(name: &str) -> Container {
        read(&format!("tests/fixtures/detectors/unit/{name}"))
    }

    /// The model file of a golden set (`kick` and `snare` are the shipped ones).
    pub fn model(set: &str) -> Container {
        read(&format!("tests/fixtures/detectors/{set}/model.mkick"))
    }

    /// A golden's audio as the stream sees it after `ModelInput` (f32 widened).
    pub fn audio_f64(g: &Container) -> Vec<f64> {
        audio_f32(g).into_iter().map(f64::from).collect()
    }

    pub fn audio_f32(g: &Container) -> Vec<f32> {
        let n = g.shape("audio_i16").unwrap()[0];
        g.i16s("audio_i16", &[n])
            .unwrap()
            .iter()
            .map(|&v| v as f32 / 32768.0)
            .collect()
    }

    /// Seeded chunk sizes in 256..=1024 (xorshift; the crate has no rand dependency).
    pub struct Chunks(pub u64);

    impl Chunks {
        pub fn next(&mut self) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            256 + (self.0 % 769) as usize
        }
    }
}
