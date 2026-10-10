use super::spectrum::np_sum;
use super::*;
use crate::kick::golden;

fn model() -> Container {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kick_model.mkick");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Container::parse(&bytes).unwrap()
}

fn clip_names() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kick");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no golden clips in {}", dir.display());
    names
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

fn run(det: &mut BaseDetector, audio: &[f64], seed: u64) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut chunks = Chunks(seed);
    let mut at = 0;
    while at < audio.len() {
        let n = chunks.next().min(audio.len() - at);
        out.reserve(det.max_candidates(n));
        let cap = out.capacity();
        det.push(&audio[at..at + n], &mut out);
        assert_eq!(out.capacity(), cap, "push grew the output");
        at += n;
    }
    out
}

#[test]
fn parity_against_goldens() {
    let m = model();
    for name in clip_names() {
        let g = golden::load(&name);
        assert_eq!(g.text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}: golden is from another model");
        assert_eq!(g.i64s("sample_rate", &[]).unwrap(), &[48_000]);
        let audio: Vec<f64> = g.i16s("audio_i16", &[g.shape("audio_i16").unwrap()[0]]).unwrap().iter().map(|&v| v as f64 / 32768.0).collect();
        let k = g.shape("cand_hop").unwrap()[0];
        let cand = g.i64s("cand_hop", &[k]).unwrap();
        let avail = g.i64s("avail_hop", &[k]).unwrap();
        let f15 = g.f64s("f15", &[k, FEATURES]).unwrap();
        let mut det = BaseDetector::new(&m).unwrap();
        assert_eq!(det.hop() as i64, g.i64s("hop", &[]).unwrap()[0]);
        let t0 = std::time::Instant::now();
        let got = run(&mut det, &audio, 0x9E37_79B9_7F4A_7C15 ^ k as u64);
        let elapsed = t0.elapsed();
        assert_eq!(got.len(), k, "{name}: candidate count");
        let mut worst = [0.0f64; FEATURES];
        for (i, c) in got.iter().enumerate() {
            assert_eq!(c.cand_hop as i64, cand[i], "{name}: cand_hop[{i}]");
            assert_eq!(c.avail_hop as i64, avail[i], "{name}: avail_hop[{i}]");
            assert_eq!(det.ready_sample(c.avail_hop), (avail[i] as u64 + 1) * 256);
            for (j, w) in worst.iter_mut().enumerate() {
                *w = w.max((c.features[j] - f15[i * FEATURES + j]).abs());
            }
        }
        let max_err = worst.iter().copied().fold(0.0, f64::max);
        println!(
            "{name}: {k} candidates, cand/avail exact, f15 max abs error {max_err:.3e} per column {worst:?}; {:.1} ms for {:.1} s of audio",
            elapsed.as_secs_f64() * 1e3,
            audio.len() as f64 / 48_000.0
        );
        assert!(max_err <= 1e-6, "{name}: f15 error {max_err:e} per column {worst:?}");
    }
}

#[test]
fn chunking_does_not_change_candidates() {
    let m = model();
    let g = golden::load(&clip_names()[0]);
    let audio: Vec<f64> = g.i16s("audio_i16", &[g.shape("audio_i16").unwrap()[0]]).unwrap().iter().map(|&v| v as f64 / 32768.0).collect();
    let a = run(&mut BaseDetector::new(&m).unwrap(), &audio, 1);
    let mut whole = Vec::with_capacity(audio.len() / 256 + 1);
    BaseDetector::new(&m).unwrap().push(&audio, &mut whole);
    assert_eq!(a, whole);
}

/// Values from numpy 2.4 `np.sum` (pairwise summation), where a naive left fold rounds differently.
#[test]
fn np_sum_matches_numpy_pairwise_order() {
    let v: Vec<f64> = (0..340).map(|i| if i % 9 == 0 { 1.0e16 } else { 1.0 + i as f64 * 0.25 }).collect();
    let naive = v.iter().fold(0.0, |a, b| a + b);
    let pw = np_sum(&v);
    assert_ne!(naive.to_bits(), pw.to_bits(), "test vector must separate the orders");
    assert_eq!(pw.to_bits(), NUMPY_SUM_340.to_bits());
    assert_eq!(np_sum(&v[..37]).to_bits(), NUMPY_SUM_37.to_bits());
    assert_eq!(np_sum(&v[..5]).to_bits(), NUMPY_SUM_5.to_bits());
}

const NUMPY_SUM_340: f64 = 3.8000000000001306e17;
const NUMPY_SUM_37: f64 = 5.000000000000018e16;
const NUMPY_SUM_5: f64 = 1.0000000000000008e16;
