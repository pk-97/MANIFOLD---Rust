//! The shipped snare end to end against its v1 goldens (`tests/fixtures/snare/`, written by
//! `tools/audio_analysis/snare_release.py`), through the generic runtime.

use super::*;
use crate::trained::golden::{self, Chunks};
use crate::trained::net::Net;

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
    for name in golden::clips("snare") {
        assert_eq!(golden::load("snare", &name).text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}");
    }
}

/// Fire emissions from audio alone, fed in random chunks, against the golden fires (`(avail_hop + 1) * hop`).
/// Golden fires whose candidate is still waiting at the clip end have no decision yet and are left out.
#[test]
fn fires_match_every_golden_clip_end_to_end() {
    let net = Net::new(&golden::snare_v2(), 1).unwrap();
    for (ci, name) in golden::clips("snare").iter().enumerate() {
        let g = golden::load("snare", name);
        let hop = g.i64s("hop", &[]).unwrap()[0] as u64;
        let nf = g.shape("fires").unwrap()[0];
        let golden_fires = g.i64s("fires", &[nf]).unwrap();
        let pcm = golden::audio_f32(&g);
        let mut det = SnareDetector::new(48_000).unwrap();
        let fires = run(&mut det, &pcm, 0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
        let want: Vec<u64> = golden_fires
            .iter()
            .map(|&a| a as u64)
            .filter(|&a| net.ready_sample(a) <= pcm.len() as u64)
            .map(|a| (a + 1) * hop)
            .collect();
        let got: Vec<u64> = fires.iter().map(|f| f.emission).collect();
        let lag_s = det.0.decision_lag_s();
        let lag_ms: Vec<f64> = fires.iter().map(|f| (f.sample - f.emission) as f64 / 48.0).collect();
        let (lo, hi) = lag_ms.iter().fold((f64::INFINITY, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
        println!(
            "{name}: {} decided, fires {}/{} (golden total {nf}), decision lag after emission {lo:.1}..{hi:.1} ms (bound {:.1} ms)",
            det.decided(),
            got.len(),
            want.len(),
            lag_s * 1e3
        );
        assert_eq!(det.dropped(), 0, "{name}: pending queue overflowed");
        assert_eq!(got, want, "{name}: fire samples");
        for f in &fires {
            assert!(f.sample >= f.emission, "{name}: fire stamped before its emission");
            // The bound holds once the net has a full slice of stream behind it (v1 clips start on music).
            if f.emission >= 24_000 {
                assert!((f.sample - f.emission) as f64 <= lag_s * 48_000.0, "{name}: fire later than decision_lag_s");
            }
        }
    }
}

#[test]
fn refuses_another_recipe() {
    let mut c = golden::asset("snare");
    c.insert("recipe_version", crate::trained::container::Array::i32_scalar(2));
    assert_eq!(SnareDetector::from_container(&c, 48_000).err(), Some(SnareError::V1(V1Error::Recipe(2))));
}

/// CPU per second of dense audio. Only a release run means anything:
/// `cargo test --release -p manifold-audio snare_detector_cost -- --nocapture`.
#[test]
fn snare_detector_cost() {
    let name = &golden::clips("snare")[0];
    let pcm = golden::audio_f32(&golden::load("snare", name));
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
        "snare detector: {:.2} ms CPU per second of audio ({:.1} candidates/s, {} fires) on {name}",
        per_s * 1e3,
        det.decided() as f64 / secs,
        fires.len(),
    );
}
