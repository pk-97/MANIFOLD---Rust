//! The whole snare pipeline as one streaming detector: candidates → noisiness frames and the
//! net → the fire rule (`snare_net.fires`). Mono audio of any rate in, fires stamped at their
//! emission sample on the input clock out.

use std::sync::OnceLock;

use super::cands::SnareCands;
use super::flat::Flatness;
use super::net::SnareNet;
use crate::kick::container::{Container, ContainerError};
use crate::kick::detector::ModelInput;

/// The pipeline shape this code implements. A model file with another version needs Rust work.
pub const RECIPE_VERSION: i32 = 1;
/// Candidates waiting for the net's look-ahead (deadline plus about 40 ms; at most one per hop,
/// so 64 leaves a wide margin).
const PENDING: usize = 64;

static MODEL: OnceLock<Result<Container, ContainerError>> = OnceLock::new();

/// The embedded model, parsed once per process.
fn model() -> Result<&'static Container, ContainerError> {
    MODEL
        .get_or_init(|| Container::parse(include_bytes!("../../assets/snare_model.mkick")))
        .as_ref()
        .map_err(Clone::clone)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SnareError {
    Model(ContainerError),
    /// The model's `recipe_version` is not [`RECIPE_VERSION`].
    Recipe(i32),
    BadRate(u32),
}

impl std::fmt::Display for SnareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::Recipe(v) => write!(f, "snare model recipe_version {v}, this build runs {RECIPE_VERSION}"),
            Self::BadRate(r) => write!(f, "snare detector: input rate {r} Hz"),
        }
    }
}

impl std::error::Error for SnareError {}

impl From<ContainerError> for SnareError {
    fn from(e: ContainerError) -> Self {
        Self::Model(e)
    }
}

/// One fire: the input sample count at which it was decided (the stamp, as for kicks, so a fire lands on
/// the same hop whatever the chunking), its emission on the input clock, and its candidate's hops on the
/// 48 kHz model grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnareFire {
    pub sample: u64,
    pub emission: u64,
    pub cand_hop: u64,
    pub avail_hop: u64,
}

#[derive(Clone, Copy)]
struct Pending {
    cand_hop: u64,
    avail_hop: u64,
    ready: u64,
}

/// Streaming snare detector. No allocation after construction when the caller reserves
/// [`Self::max_fires`] in the output first.
pub struct SnareDetector {
    cands: SnareCands,
    flat: Flatness,
    net: SnareNet,
    input: ModelInput,
    cutoff: f64,
    /// The fire rule's refractory, in model hops (`refractory_s - 1e-12` seconds).
    refractory_hops: f64,
    hop: u64,
    found: Vec<(u64, u64)>,
    pending: [Option<Pending>; PENDING],
    head: usize,
    len: usize,
    samples48: u64,
    /// The last fire's `avail_hop`.
    last: Option<u64>,
    decided: u64,
    dropped: u64,
}

impl SnareDetector {
    /// Builds the detector for mono input at `input_rate` Hz from the embedded model.
    pub fn new(input_rate: u32) -> Result<Self, SnareError> {
        Self::from_container(model()?, input_rate)
    }

    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, SnareError> {
        let version = c.i32s("recipe_version", &[])?[0];
        if version != RECIPE_VERSION {
            return Err(SnareError::Recipe(version));
        }
        let input = ModelInput::new(input_rate).ok_or(SnareError::BadRate(input_rate))?;
        let hop = c.i64s("net.hop", &[])?[0];
        let rate = c.i64s("net.sample_rate", &[])?[0];
        if hop <= 0 || rate <= 0 || hop != c.i64s("cand.hop", &[])?[0] {
            return Err(ContainerError::WrongType { name: "net.hop/cand.hop".into(), want: "one positive hop" }.into());
        }
        let refractory_s = c.f64s("refractory_s", &[])?[0];
        let max = ModelInput::max_model(input_rate);
        Ok(Self {
            cands: SnareCands::new(c)?,
            flat: Flatness::new(c)?,
            net: SnareNet::new(c)?,
            input,
            cutoff: c.f64s("cutoff", &[])?[0],
            refractory_hops: (refractory_s - 1e-12) * rate as f64 / hop as f64,
            hop: hop as u64,
            found: Vec::with_capacity(max / hop as usize + 2),
            pending: [None; PENDING],
            head: 0,
            len: 0,
            samples48: 0,
            last: None,
            decided: 0,
            dropped: 0,
        })
    }

    /// The most fires one `push` of `samples` input samples can append: one per candidate
    /// found in it, plus every candidate already waiting.
    pub fn max_fires(&self, samples: usize) -> usize {
        self.input.model_samples(samples) / self.hop as usize + 1 + PENDING
    }

    /// Candidates decided so far (fired or not).
    pub fn decided(&self) -> u64 {
        self.decided
    }

    /// Candidates lost because the pending queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Feeds mono input samples; appends each fire as it is decided.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<SnareFire>) {
        self.push_with(samples, |f| out.push(f));
    }

    /// Feeds mono input samples; hands each fire to `on_fire` as it is decided.
    pub fn push_with(&mut self, samples: &[f32], mut on_fire: impl FnMut(SnareFire)) {
        for chunk in samples.chunks(ModelInput::STEP) {
            self.input.feed(chunk);
            self.step_block(&mut on_fire);
        }
    }

    fn step_block(&mut self, on_fire: &mut impl FnMut(SnareFire)) {
        self.found.clear();
        let block = &self.input.block;
        self.cands.push(block, &mut self.found);
        self.flat.push(block);
        self.net.push(block);
        self.samples48 += block.len() as u64;
        for &(cand_hop, avail_hop) in &self.found {
            if self.len == PENDING {
                self.dropped += 1;
                continue;
            }
            // `ready_sample` covers the slice end frame, which the noisiness stream also holds by then.
            let ready = self.net.ready_sample(avail_hop);
            self.pending[(self.head + self.len) % PENDING] = Some(Pending { cand_hop, avail_hop, ready });
            self.len += 1;
        }
        while self.len > 0 {
            let Some(p) = self.pending[self.head].filter(|p| p.ready <= self.samples48) else { break };
            self.head = (self.head + 1) % PENDING;
            self.len -= 1;
            if self.decide(&p) {
                on_fire(SnareFire {
                    sample: self.input.input_count(p.ready),
                    emission: self.input.input_count((p.avail_hop + 1) * self.hop),
                    cand_hop: p.cand_hop,
                    avail_hop: p.avail_hop,
                });
            }
        }
    }

    /// `snare_net.fires`: p at or over the cutoff, and the emission at least the refractory
    /// after the last fire's. A NaN p (slice frames not held) never fires.
    fn decide(&mut self, p: &Pending) -> bool {
        self.decided += 1;
        let prob = self.net.probability(p.cand_hop, p.avail_hop, &self.flat);
        let clear = self.last.is_none_or(|l| (p.avail_hop as f64 - l as f64) >= self.refractory_hops);
        let fire = prob >= self.cutoff && clear;
        if fire {
            self.last = Some(p.avail_hop);
        }
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snare::golden;

    fn clips() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snare");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no snare goldens in {}", dir.display());
        names
    }

    fn audio(g: &Container) -> Vec<f32> {
        let n = g.shape("audio_i16").unwrap()[0];
        g.i16s("audio_i16", &[n]).unwrap().iter().map(|&v| v as f32 / 32768.0).collect()
    }

    /// Seeded chunk sizes in 256..=1024.
    struct Chunks(u64);

    impl Chunks {
        fn next(&mut self) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            256 + (self.0 % 769) as usize
        }
    }

    fn run(det: &mut SnareDetector, audio: &[f32], seed: u64) -> Vec<SnareFire> {
        let mut fires = Vec::new();
        let mut chunks = Chunks(seed);
        let mut at = 0;
        while at < audio.len() {
            let n = chunks.next().min(audio.len() - at);
            fires.reserve(det.max_fires(n));
            let cap = fires.capacity();
            det.push(&audio[at..at + n], &mut fires);
            assert_eq!(fires.capacity(), cap, "push grew the output");
            at += n;
        }
        fires
    }

    #[test]
    fn embedded_model_matches_the_goldens() {
        let m = model().unwrap();
        for name in clips() {
            assert_eq!(golden::load(&name).text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}");
        }
    }

    /// Fire samples from audio alone, fed in random chunks, against the golden fires
    /// (`(avail_hop + 1) * hop`). Golden fires whose candidate is still waiting at the clip
    /// end have no decision yet and are left out.
    #[test]
    fn fires_match_every_golden_clip_end_to_end() {
        for (ci, name) in clips().iter().enumerate() {
            let g = golden::load(name);
            let hop = g.i64s("hop", &[]).unwrap()[0] as u64;
            let nf = g.shape("fires").unwrap()[0];
            let golden_fires = g.i64s("fires", &[nf]).unwrap();
            let pcm = audio(&g);
            let mut det = SnareDetector::new(48_000).unwrap();
            let fires = run(&mut det, &pcm, 0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
            let want: Vec<u64> = golden_fires
                .iter()
                .map(|&a| a as u64)
                .filter(|&a| det.net.ready_sample(a) <= pcm.len() as u64)
                .map(|a| (a + 1) * hop)
                .collect();
            let got: Vec<u64> = fires.iter().map(|f| f.emission).collect();
            let lag_ms: Vec<f64> = fires.iter().map(|f| (f.sample - f.emission) as f64 / 48.0).collect();
            let (lo, hi) = lag_ms.iter().fold((f64::INFINITY, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
            println!(
                "{name}: {} decided, fires {}/{} (golden total {nf}), decision lag after emission {lo:.1}..{hi:.1} ms",
                det.decided(),
                got.len(),
                want.len()
            );
            assert_eq!(det.dropped(), 0, "{name}: pending queue overflowed");
            assert_eq!(got, want, "{name}: fire samples");
        }
    }

    #[test]
    fn refuses_another_recipe() {
        use crate::kick::trees::tests::container;
        let c = container(&[("recipe_version".into(), 2, vec![], 2i32.to_le_bytes().to_vec())]);
        assert_eq!(SnareDetector::from_container(&c, 48_000).err(), Some(SnareError::Recipe(2)));
    }

    /// CPU per second of dense audio. Only a release run means anything:
    /// `cargo test --release -p manifold-audio snare_detector_cost -- --nocapture`.
    #[test]
    fn snare_detector_cost() {
        let g = golden::load(&clips()[0]);
        let pcm = audio(&g);
        let mut det = SnareDetector::new(48_000).unwrap();
        let mut fires = Vec::with_capacity(1024);
        let reps = 3;
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            for chunk in pcm.chunks(800) {
                det.push(chunk, &mut fires);
            }
        }
        let secs = reps as f64 * pcm.len() as f64 / 48_000.0;
        let per_s = t0.elapsed().as_secs_f64() / secs;
        println!(
            "snare detector: {:.2} ms CPU per second of audio ({:.1} candidates/s, {} fires) on {}",
            per_s * 1e3,
            det.decided() as f64 / secs,
            fires.len(),
            clips()[0]
        );
    }
}
