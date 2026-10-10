//! The whole kick pipeline as one streaming detector (docs/KICK_REALTIME_DESIGN.md section 1):
//! base candidates → f69 (f15 + the 54 extra features) → trees, the net, then the stage in
//! emission order. Mono audio of any rate in, fires stamped at the input sample where each
//! one is decided out.

use std::sync::OnceLock;

use super::base::{BaseDetector, Candidate, FEATURES};
use super::container::{Container, ContainerError};
use super::extra::{EXTRA_DIMS, ExtraFeatures};
use super::net::KickNet;
use super::stage::{DESCRIPTOR_LEN, Stage};
use super::trees::Forest;
use crate::analysis::LinearResampler;

/// The pipeline shape this code implements. A model file with another version needs Rust work.
pub const RECIPE_VERSION: i32 = 1;
const MODEL_RATE: u32 = 48_000;
const F69: usize = FEATURES + EXTRA_DIMS;
/// Input samples handled per internal step; bounds every scratch buffer.
const IN_STEP: usize = 512;
/// Candidates waiting for the extra features and the net (about 40 ms at most one per hop,
/// so 64 leaves a wide margin).
const PENDING: usize = 64;

static MODEL: OnceLock<Result<Container, ContainerError>> = OnceLock::new();

/// The embedded model, parsed once per process.
fn model() -> Result<&'static Container, ContainerError> {
    MODEL
        .get_or_init(|| Container::parse(include_bytes!("../../assets/kick_model.mkick")))
        .as_ref()
        .map_err(Clone::clone)
}

#[derive(Debug, Clone, PartialEq)]
pub enum KickError {
    Model(ContainerError),
    /// The model's `recipe_version` is not [`RECIPE_VERSION`].
    Recipe(i32),
    BadRate(u32),
}

impl std::fmt::Display for KickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::Recipe(v) => write!(f, "kick model recipe_version {v}, this build runs {RECIPE_VERSION}"),
            Self::BadRate(r) => write!(f, "kick detector: input rate {r} Hz"),
        }
    }
}

impl std::error::Error for KickError {}

impl From<ContainerError> for KickError {
    fn from(e: ContainerError) -> Self {
        Self::Model(e)
    }
}

/// One fire: the input-rate sample count at which it was decided, and its candidate's hops
/// on the 48 kHz model grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KickFire {
    pub sample: u64,
    pub cand_hop: u64,
    pub avail_hop: u64,
}

#[derive(Clone, Copy)]
struct Pending {
    cand: Candidate,
    ready: u64,
}

/// Streaming kick detector. No allocation after construction when the caller reserves
/// [`Self::max_fires`] in the output first.
pub struct KickDetector {
    base: BaseDetector,
    extra: ExtraFeatures,
    trees: Forest,
    net: KickNet,
    stage: Stage,
    /// `None` at 48 kHz. Linear interpolation without an anti-alias filter: downsampling folds
    /// content above 24 kHz back into the band, which reaches only the upper-band features and
    /// is small next to music's level up there. Upsampling adds no content above the source's
    /// Nyquist, so the model sees a band-limited 48 kHz stream.
    resampler: Option<LinearResampler>,
    /// Input samples per model sample.
    step: f64,
    resampled: Vec<f32>,
    block: Vec<f64>,
    cands: Vec<Candidate>,
    pending: [Option<Pending>; PENDING],
    head: usize,
    len: usize,
    samples48: u64,
    decided: u64,
    dropped: u64,
    f69: [f64; F69],
    extra54: [f64; EXTRA_DIMS],
}

impl KickDetector {
    /// Builds the detector for mono input at `input_rate` Hz from the embedded model.
    pub fn new(input_rate: u32) -> Result<Self, KickError> {
        Self::from_container(model()?, input_rate)
    }

    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, KickError> {
        let version = c.i32s("recipe_version", &[])?[0];
        if version != RECIPE_VERSION {
            return Err(KickError::Recipe(version));
        }
        if input_rate == 0 {
            return Err(KickError::BadRate(input_rate));
        }
        let trees = Forest::from_container(c, "trees.")?;
        if trees.n_features() != F69 {
            return Err(ContainerError::WrongShape { name: "trees.n_features".into(), want: vec![F69], got: vec![trees.n_features()] }.into());
        }
        let base = BaseDetector::new(c)?;
        let resampler = (input_rate != MODEL_RATE).then(|| LinearResampler::new(input_rate, MODEL_RATE));
        // Model samples one input step can produce, plus the resampler's carried edge.
        let max48 = (IN_STEP as u64 * MODEL_RATE as u64).div_ceil(input_rate as u64) as usize + 2;
        Ok(Self {
            cands: Vec::with_capacity(base.max_candidates(max48)),
            base,
            extra: ExtraFeatures::new(c)?,
            trees,
            net: KickNet::new(c)?,
            stage: Stage::new(c)?,
            resampler,
            step: input_rate as f64 / MODEL_RATE as f64,
            resampled: Vec::with_capacity(max48),
            block: Vec::with_capacity(max48),
            pending: [None; PENDING],
            head: 0,
            len: 0,
            samples48: 0,
            decided: 0,
            dropped: 0,
            f69: [0.0; F69],
            extra54: [0.0; EXTRA_DIMS],
        })
    }

    /// The most fires one `push` of `samples` input samples can append: one per base candidate
    /// emitted in it, plus every candidate already waiting.
    pub fn max_fires(&self, samples: usize) -> usize {
        let model = (samples as f64 / self.step).ceil() as usize + 2;
        self.base.max_candidates(model) + PENDING
    }

    /// Candidates decided so far (fired or not).
    pub fn decided(&self) -> u64 {
        self.decided
    }

    /// Candidates lost because the pending queue was full (zero unless the model's latency
    /// grows past what [`PENDING`] holds).
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Feeds mono input samples; appends each fire as it is decided.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<KickFire>) {
        self.push_with(samples, |f| out.push(f));
    }

    /// Feeds mono input samples; hands each fire to `on_fire` as it is decided.
    pub fn push_with(&mut self, samples: &[f32], mut on_fire: impl FnMut(KickFire)) {
        for chunk in samples.chunks(IN_STEP) {
            self.block.clear();
            match self.resampler.as_mut() {
                Some(r) => {
                    self.resampled.clear();
                    r.process(chunk, &mut self.resampled);
                    self.block.extend(self.resampled.iter().map(|&x| x as f64));
                }
                None => self.block.extend(chunk.iter().map(|&x| x as f64)),
            }
            self.step_block(&mut on_fire);
        }
    }

    fn step_block(&mut self, on_fire: &mut impl FnMut(KickFire)) {
        self.cands.clear();
        self.base.push(&self.block, &mut self.cands);
        self.extra.push(&self.block);
        self.net.push(&self.block);
        self.samples48 += self.block.len() as u64;
        for i in 0..self.cands.len() {
            let cand = self.cands[i];
            let ready = self
                .base
                .ready_sample(cand.avail_hop)
                .max(self.extra.ready_sample(cand.cand_hop, cand.avail_hop))
                .max(self.net.ready_sample(cand.avail_hop));
            if self.len == PENDING {
                self.dropped += 1;
                continue;
            }
            self.pending[(self.head + self.len) % PENDING] = Some(Pending { cand, ready });
            self.len += 1;
        }
        while self.len > 0 {
            let Some(p) = self.pending[self.head].filter(|p| p.ready <= self.samples48) else { break };
            self.head = (self.head + 1) % PENDING;
            self.len -= 1;
            if self.decide(&p.cand) {
                on_fire(KickFire { sample: self.input_count(p.ready), cand_hop: p.cand.cand_hop, avail_hop: p.cand.avail_hop });
            }
        }
    }

    fn decide(&mut self, c: &Candidate) -> bool {
        self.decided += 1;
        self.extra.features(c.cand_hop, c.avail_hop, &mut self.extra54);
        self.f69[..FEATURES].copy_from_slice(&c.features);
        self.f69[FEATURES..].copy_from_slice(&self.extra54);
        let tree_p = self.trees.predict_proba(&self.f69);
        let net_p = self.net.probability(c.cand_hop, c.avail_hop);
        let mut descriptor = [0.0; DESCRIPTOR_LEN];
        // hstack(prof32, low16): the extra features' last 48 columns.
        descriptor.copy_from_slice(&self.extra54[EXTRA_DIMS - DESCRIPTOR_LEN..]);
        self.stage.decide(tree_p, net_p, &descriptor, c.features[0], c.avail_hop + 1, c.avail_hop).fire
    }

    /// The input sample count by which `model_count` model samples exist. The resampler emits
    /// model sample `j` once input index `floor(j * step)` has arrived.
    fn input_count(&self, model_count: u64) -> u64 {
        if self.resampler.is_none() {
            return model_count;
        }
        ((model_count.saturating_sub(1)) as f64 * self.step).floor() as u64 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kick::golden;

    fn clips() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kick");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no kick goldens in {}", dir.display());
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

    fn run(det: &mut KickDetector, audio: &[f32], seed: u64) -> Vec<KickFire> {
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
        assert_eq!(m.i32s("recipe_version", &[]).unwrap(), &[RECIPE_VERSION]);
        for name in clips() {
            assert_eq!(golden::load(&name).text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}");
        }
    }

    /// Fires from audio alone, fed in random chunks, against the golden `fires` (candidate
    /// indices, mapped through `cand_hop`/`avail_hop`). Candidates still waiting at the clip
    /// end have no decision yet and are left out.
    #[test]
    fn fires_match_every_golden_clip_end_to_end() {
        for (ci, name) in clips().iter().enumerate() {
            let g = golden::load(name);
            let k = g.shape("cand_hop").unwrap()[0];
            let cand = g.i64s("cand_hop", &[k]).unwrap();
            let avail = g.i64s("avail_hop", &[k]).unwrap();
            let nf = g.shape("fires").unwrap()[0];
            let want_idx = g.i64s("fires", &[nf]).unwrap();
            let pcm = audio(&g);
            let mut det = KickDetector::new(48_000).unwrap();
            let fires = run(&mut det, &pcm, 0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
            let decided = det.decided() as usize;
            let want: Vec<(u64, u64)> = want_idx
                .iter()
                .filter(|&&i| (i as usize) < decided)
                .map(|&i| (cand[i as usize] as u64, avail[i as usize] as u64))
                .collect();
            let got: Vec<(u64, u64)> = fires.iter().map(|f| (f.cand_hop, f.avail_hop)).collect();
            let lag_ms: Vec<f64> =
                fires.iter().map(|f| (f.sample as f64 - ((f.avail_hop + 1) * 256) as f64) / 48.0).collect();
            let (lo, hi) = lag_ms.iter().fold((f64::INFINITY, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
            println!(
                "{name}: {k} candidates, {decided} decided, fires {}/{} (golden total {nf}), decision lag after emission {lo:.1}..{hi:.1} ms",
                got.len(),
                want.len()
            );
            assert_eq!(det.dropped(), 0, "{name}: pending queue overflowed");
            assert!(k - decided <= PENDING, "{name}: only {decided} of {k} decided");
            assert_eq!(got, want, "{name}: fired candidates");
            for f in &fires {
                assert!(f.sample >= (f.avail_hop + 1) * 256, "{name}: fire stamped before its emission");
            }
        }
    }

    /// Any input rate works and stamps fires on the input grid: a 44.1 kHz stream of the dense
    /// clip fires near where the 48 kHz stream does (resampling moves features slightly, so
    /// this checks the clock, not exact parity).
    #[test]
    fn resampled_input_stamps_on_the_input_grid() {
        let g = golden::load(&clips()[0]);
        let pcm48 = audio(&g);
        let mut r = LinearResampler::new(48_000, 44_100);
        let mut pcm44 = Vec::new();
        r.process(&pcm48, &mut pcm44);
        let a = run(&mut KickDetector::new(48_000).unwrap(), &pcm48, 7);
        let b = run(&mut KickDetector::new(44_100).unwrap(), &pcm44, 7);
        assert!(!a.is_empty());
        let near = b
            .iter()
            .filter(|f| a.iter().any(|g| (f.sample as f64 / 44_100.0 - g.sample as f64 / 48_000.0).abs() < 0.012))
            .count();
        println!("48k fires {}, 44.1k fires {}, 44.1k within 12 ms of a 48k fire {near}", a.len(), b.len());
        assert!(near * 10 >= a.len().max(b.len()) * 8, "44.1 kHz fires drifted from the 48 kHz clock");
    }

    #[test]
    fn refuses_another_recipe() {
        use crate::kick::trees::tests::container;
        let c = container(&[("recipe_version".into(), 2, vec![], 2i32.to_le_bytes().to_vec())]);
        assert_eq!(KickDetector::from_container(&c, 48_000).err(), Some(KickError::Recipe(2)));
    }

    /// CPU per second of dense audio. Only a release run means anything:
    /// `cargo test --release -p manifold-audio kick_detector_cost -- --nocapture`.
    #[test]
    fn kick_detector_cost() {
        let g = golden::load(&clips()[0]);
        let pcm = audio(&g);
        let mut det = KickDetector::new(48_000).unwrap();
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
            "kick detector: {:.2} ms CPU per second of audio ({:.1} candidates/s, {} fires) on {}",
            per_s * 1e3,
            det.decided() as f64 / secs,
            fires.len(),
            clips()[0]
        );
    }
}
