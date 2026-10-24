//! CPU per second of audio for every golden detector set, on each set's first clip, in the production pairing
//! (a model that reads kick verdicts runs fed by the kick set's model through `KickFed`).
//!
//! A measurement, not a gate: timing is not checked on shared machines (docs/DETECTOR_RUNTIME_DESIGN.md
//! section 7, invariants and enforcement). Run it in release and record the numbers by hand:
//!
//! ```text
//! cargo run --release -p manifold-audio --example detector_cost
//! ```

use manifold_audio::detectors::{KickFed, Side};
use manifold_audio::trained::container::Container;
use manifold_audio::trained::{Decision, Fire, TrainedDetector};

/// Every golden set on disk, as the parity tests name them.
const SETS: [&str; 4] = ["cover_kick", "cover_snare", "kick", "snare"];

fn fixtures(set: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/detectors")
        .join(set)
}

fn read(path: &std::path::Path) -> Container {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Container::parse(&bytes).unwrap()
}

fn first_clip(set: &str) -> String {
    let dir = fixtures(set);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
        .collect();
    names.sort();
    names
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no clip goldens in {}", dir.display()))
}

enum Run {
    Alone(Box<TrainedDetector>),
    Fed(Box<KickFed>),
}

impl Run {
    fn new(m: &Container) -> Self {
        let det = TrainedDetector::from_container(m, 48_000).unwrap();
        if det.wants_kick().is_none() {
            return Self::Alone(Box::new(det));
        }
        let kick =
            TrainedDetector::from_container(&read(&fixtures("kick").join("model.mkick")), 48_000)
                .unwrap();
        Self::Fed(Box::new(
            KickFed::new(kick, det).expect("the kick set is the kick this model reads"),
        ))
    }

    fn det(&self) -> &TrainedDetector {
        match self {
            Self::Alone(d) => d,
            Self::Fed(f) => f.reader(),
        }
    }

    fn push_traced(
        &mut self,
        samples: &[f32],
        mut on_decision: impl FnMut(&Decision<'_>, Option<Fire>),
    ) {
        match self {
            Self::Alone(d) => d.push_traced(samples, on_decision),
            Self::Fed(f) => f.push_traced(samples, |side, d, fire| {
                if side == Side::Reader {
                    on_decision(d, fire);
                }
            }),
        }
    }
}

fn main() {
    for set in SETS {
        let m = read(&fixtures(set).join("model.mkick"));
        let name = first_clip(set);
        let g = read(&fixtures(set).join(&name));
        let n = g.shape("audio_i16").unwrap()[0];
        let pcm: Vec<f32> = g
            .i16s("audio_i16", &[n])
            .unwrap()
            .iter()
            .map(|&v| v as f32 / 32768.0)
            .collect();
        let mut run = Run::new(&m);
        let mut fires = Vec::with_capacity(4096);
        let t0 = std::time::Instant::now();
        for chunk in pcm.chunks(800) {
            run.push_traced(chunk, |_, f| fires.extend(f));
        }
        let det = run.det();
        let secs = pcm.len() as f64 / 48_000.0;
        println!(
            "{set}: {:.2} ms CPU per second of audio ({:.1} candidates/s, {} nets{}) on {name}",
            t0.elapsed().as_secs_f64() / secs * 1e3,
            det.decided() as f64 / secs,
            det.spec().net.as_ref().map_or(0, |n| n.count),
            if det.wants_kick().is_some() {
                ", the kick it reads included"
            } else {
                ""
            }
        );
    }
}
