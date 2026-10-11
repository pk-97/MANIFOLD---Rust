//! Finder parity is in `trained::tests` (candidates and f15 on every kick golden set).

use super::spectrum::np_sum;
use super::*;
use crate::trained::golden::{self, Chunks};

fn model() -> Container {
    golden::model("kick")
}

fn clip_names() -> Vec<String> {
    golden::clips("detectors/kick")
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
fn chunking_does_not_change_candidates() {
    let m = model();
    let g = golden::load("detectors/kick", &clip_names()[0]);
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
