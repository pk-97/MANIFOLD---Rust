//! The snare v1 net against its goldens (`tools/audio_analysis/snare_release.py`). Upstream inputs
//! (candidates, noisiness frames) come from the golden, never from another Rust piece.

use super::*;
use crate::trained::golden::{self, Chunks};

/// `Song.__init__` clips the slice end to `[SLICE + JITTER, n - 1 - JITTER]` even at predict time;
/// candidates it clipped have no live equivalent, so their golden p stands in for the fire rule.
const PY_JITTER: i64 = 1;

fn model() -> Container {
    golden::snare_v2()
}

fn golden_names() -> Vec<String> {
    golden::clips("snare")
}

/// The golden's noisiness frames, held only once the stream has computed them.
struct Frames<'a> {
    data: &'a [f32],
    held: u64,
}

impl FrameSource for Frames<'_> {
    fn channels(&self) -> usize {
        1
    }

    fn frame(&self, _: usize, f: u64) -> Option<&[f32]> {
        (f < self.held).then(|| &self.data[f as usize * BANDS..][..BANDS])
    }
}

#[test]
fn parity_every_golden_clip() {
    let model = model();
    let train_id = model.text("train_id").unwrap().to_owned();
    let cutoff = model.f64s("cutoff", &[]).unwrap()[0];
    let refractory_s = model.f64s("refractory_s", &[]).unwrap()[0];
    let hop = model.i64s("net.hop", &[]).unwrap()[0] as f64;
    let sr = model.i64s("net.sample_rate", &[]).unwrap()[0] as f64;
    let slice = model.i64s("net.slice", &[]).unwrap()[0];
    let frame_s = model.f64s("net.frame_s", &[]).unwrap()[0];
    let ahead_s = model.f64s("net.ahead_s", &[]).unwrap()[0];
    for (ci, name) in golden_names().iter().enumerate() {
        let g = golden::load("snare", name);
        assert_eq!(g.text("train_id").unwrap(), train_id, "{name}: golden from another model");
        let n = g.shape("audio_i16").unwrap()[0];
        let audio: Vec<f64> = g.i16s("audio_i16", &[n]).unwrap().iter().map(|&s| s as f64 / 32768.0).collect();
        let k = g.shape("cand_hop").unwrap()[0];
        let cand = g.i64s("cand_hop", &[k]).unwrap();
        let avail = g.i64s("avail_hop", &[k]).unwrap();
        let frames = g.shape("net_spec").unwrap()[0];
        let spec = g.f32s("net_spec", &[frames, BANDS]).unwrap();
        let flat = g.f32s("flat", &[frames, BANDS]).unwrap();
        let want_logit = g.f64s("net_logit", &[k]).unwrap();
        let want_p = g.f64s("net_p", &[k]).unwrap();
        let m = g.shape("fires").unwrap()[0];
        let want_fires = g.i64s("fires", &[m]).unwrap();

        let mut net = Net::new(&model, 1).unwrap();
        let mut p = want_p.to_vec();
        let mut todo: Vec<usize> = Vec::new();
        let mut clipped = 0;
        for (i, &a) in avail.iter().enumerate() {
            let emit_s = (a + 1) as f64 * hop / sr;
            let end = ((emit_s + ahead_s) / frame_s) as i64;
            if end < slice + PY_JITTER || end > frames as i64 - 1 - PY_JITTER {
                clipped += 1;
            } else {
                todo.push(i);
            }
        }
        todo.sort_by_key(|&i| net.ready_sample(avail[i] as u64));

        let mut rng = Chunks(0x5EED_5A4E_0000_0001 ^ (ci as u64 + 1));
        let (mut pos, mut next_frame, mut next_cand) = (0, 0u64, 0);
        let (mut spec_err, mut logit_err, mut p_err, mut bad) = (0.0f64, 0.0f64, 0.0f64, 0);
        let mut net_time = std::time::Duration::ZERO;
        while pos < n {
            let c = rng.next().min(n - pos);
            net.push(&audio[pos..pos + c]);
            pos += c;
            let held = net.spectrum().frames().min(frames as u64);
            while next_frame < held {
                let got = net.spectrum().frame(next_frame).unwrap();
                let want = &spec[next_frame as usize * BANDS..][..BANDS];
                for (a, b) in got.iter().zip(want) {
                    spec_err = spec_err.max((a - b).abs() as f64);
                }
                next_frame += 1;
            }
            let src = Frames { data: flat, held };
            while next_cand < todo.len() && net.ready_sample(avail[todo[next_cand]] as u64) <= pos as u64 {
                let i = todo[next_cand];
                let t0 = std::time::Instant::now();
                let mut zs = [0.0f32];
                net.logits(cand[i] as u64, avail[i] as u64, &src, &mut zs);
                let (z, pi) = (zs[0] as f64, average(&zs, 1e-6));
                net_time += t0.elapsed();
                let (el, ep) = ((z - want_logit[i]).abs(), (pi - want_p[i]).abs());
                logit_err = logit_err.max(el);
                p_err = p_err.max(ep);
                if el.is_nan() || ep.is_nan() || el > 1e-3 || ep > 1e-4 {
                    bad += 1;
                    eprintln!("{name}: candidate {i} logit {z} want {} p {pi} want {}", want_logit[i], want_p[i]);
                }
                p[i] = pi;
                next_cand += 1;
            }
        }
        assert_eq!(next_frame, frames as u64, "{name}: spectrum frames");
        assert_eq!(next_cand, todo.len(), "{name}: candidates never became ready");

        // `snare_net.fires`: p >= cutoff, and the emission at least refractory_s after the last fire's.
        let mut fires = Vec::new();
        let mut last = -1e9f64;
        for (&pi, &a) in p.iter().zip(avail) {
            if pi >= cutoff && (a as f64 - last) * hop / sr >= refractory_s - 1e-12 {
                fires.push(a);
                last = a as f64;
            }
        }
        let mismatched: Vec<i64> = fires
            .iter()
            .filter(|f| !want_fires.contains(f))
            .chain(want_fires.iter().filter(|f| !fires.contains(f)))
            .copied()
            .collect();
        let near_cutoff = |h: i64| (0..k).any(|i| avail[i] == h && (want_p[i] - cutoff).abs() < 1e-3);
        eprintln!(
            "{name}: {} of {k} candidates scored ({clipped} clipped by Python), spec max |err| {spec_err:.3e} dB, \
             logit max |err| {logit_err:.3e}, p max |err| {p_err:.3e}, {bad} over tolerance; fires {} want {}, {} mismatched; {:.1} us per candidate ",
            todo.len(),
            fires.len(),
            want_fires.len(),
            mismatched.len(),
            net_time.as_secs_f64() * 1e6 / todo.len().max(1) as f64,
        );
        assert!(spec_err <= 1e-3, "{name}: spectrum max |err| {spec_err} dB");
        assert_eq!(bad, 0, "{name}: {bad} candidates over tolerance (logit {logit_err}, p {p_err})");
        assert!(
            mismatched.is_empty() || (mismatched.len() == 1 && near_cutoff(mismatched[0])),
            "{name}: fires mismatch {mismatched:?}"
        );
    }
}

/// Per-candidate cost; only a release run (`--release --no-capture`) says anything about the show.
#[test]
fn candidate_cost() {
    let mut net = Net::new(&model(), 1).unwrap();
    let audio: Vec<f64> = (0..48_000 * 2).map(|i| ((i as f64) * 0.013).sin() * 0.3).collect();
    net.push(&audio);
    let held = net.spectrum().frames();
    let data = vec![1.0f32; held as usize * BANDS];
    let src = Frames { data: &data, held };
    let reps = 20;
    let t0 = std::time::Instant::now();
    let mut acc = 0.0;
    for r in 0..reps {
        let mut zs = [0.0f32];
        net.logits(200 + r % 50, 202 + r % 50, &src, &mut zs);
        acc += average(&zs, 1e-6);
    }
    let per = t0.elapsed() / reps as u32;
    assert!(acc.is_finite());
    eprintln!("snare net: {:.1} us per candidate", per.as_secs_f64() * 1e6);
}
