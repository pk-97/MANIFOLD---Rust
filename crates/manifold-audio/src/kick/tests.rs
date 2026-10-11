//! The shipped kick end to end against its v1 goldens (`tests/fixtures/kick/`, written by
//! `tools/audio_analysis/kick_release.py`), through the generic runtime.

use super::*;
use crate::analysis::LinearResampler;
use crate::trained::golden::{self, Chunks};

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
    assert_eq!(m.i32s("recipe_version", &[]).unwrap(), &[v1::RECIPE_VERSION]);
    for name in golden::clips("kick") {
        assert_eq!(golden::load("kick", &name).text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}");
    }
}

/// Fires from audio alone, fed in random chunks, against the golden `fires` (candidate indices, mapped
/// through `cand_hop`/`avail_hop`). Candidates still waiting at the clip end have no decision yet and are
/// left out.
#[test]
fn fires_match_every_golden_clip_end_to_end() {
    for (ci, name) in golden::clips("kick").iter().enumerate() {
        let g = golden::load("kick", name);
        let k = g.shape("cand_hop").unwrap()[0];
        let cand = g.i64s("cand_hop", &[k]).unwrap();
        let avail = g.i64s("avail_hop", &[k]).unwrap();
        let nf = g.shape("fires").unwrap()[0];
        let want_idx = g.i64s("fires", &[nf]).unwrap();
        let pcm = golden::audio_f32(&g);
        let mut det = KickDetector::new(48_000).unwrap();
        let fires = run(&mut det, &pcm, 0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
        let decided = det.decided() as usize;
        let want: Vec<(u64, u64)> = want_idx
            .iter()
            .filter(|&&i| (i as usize) < decided)
            .map(|&i| (cand[i as usize] as u64, avail[i as usize] as u64))
            .collect();
        let got: Vec<(u64, u64)> = fires.iter().map(|f| (f.cand_hop, f.avail_hop)).collect();
        let lag_s = det.0.decision_lag_s();
        let lag_ms: Vec<f64> = fires.iter().map(|f| (f.sample as f64 - ((f.avail_hop + 1) * 256) as f64) / 48.0).collect();
        let (lo, hi) = lag_ms.iter().fold((f64::INFINITY, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
        println!(
            "{name}: {k} candidates, {decided} decided, fires {}/{} (golden total {nf}), decision lag after emission {lo:.1}..{hi:.1} ms (bound {:.1} ms)",
            got.len(),
            want.len(),
            lag_s * 1e3
        );
        assert_eq!(det.dropped(), 0, "{name}: pending queue overflowed");
        assert!(k - decided <= 64, "{name}: only {decided} of {k} decided");
        assert_eq!(got, want, "{name}: fired candidates");
        for f in &fires {
            let emission = (f.avail_hop + 1) * 256;
            assert!(f.sample >= emission, "{name}: fire stamped before its emission");
            // The bound holds once the net has a full slice of stream behind it (v1 clips start on music).
            if emission >= 24_000 {
                assert!((f.sample - emission) as f64 <= lag_s * 48_000.0, "{name}: fire later than decision_lag_s");
            }
        }
    }
}

/// Any input rate works and stamps fires on the input grid: a 44.1 kHz stream of the dense clip fires
/// near where the 48 kHz stream does (resampling moves features slightly, so this checks the clock, not
/// exact parity).
#[test]
fn resampled_input_stamps_on_the_input_grid() {
    let g = golden::load("kick", &golden::clips("kick")[0]);
    let pcm48 = golden::audio_f32(&g);
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
    let mut c = golden::asset("kick");
    c.insert("recipe_version", container::Array::i32_scalar(2));
    assert_eq!(KickDetector::from_container(&c, 48_000).err(), Some(KickError::V1(V1Error::Recipe(2))));
}

/// CPU per second of dense audio. Only a release run means anything:
/// `cargo test --release -p manifold-audio kick_detector_cost -- --nocapture`.
#[test]
fn kick_detector_cost() {
    let name = &golden::clips("kick")[0];
    let pcm = golden::audio_f32(&golden::load("kick", name));
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
        "kick detector: {:.2} ms CPU per second of audio ({:.1} candidates/s, {} fires) on {name}",
        per_s * 1e3,
        det.decided() as f64 / secs,
        fires.len(),
    );
}
