//! Parity against the exporter's goldens (`tools/audio_analysis/kick_release.py`).

use super::*;
use crate::kick::golden;

/// `Song.__init__` clips the slice end to `[SLICE + JITTER, n - 1 - JITTER]` with the training
/// jitter even at predict time; candidates it clipped have no live equivalent and are skipped.
const PY_JITTER: i64 = 1;

fn model() -> Container {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kick_model.mkick");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Container::parse(&bytes).unwrap()
}

fn golden_names() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kick");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no goldens in {}", dir.display());
    names
}

/// Seeded chunk sizes in 256..=1024 (xorshift; the crate has no rand dependency).
struct Chunks(u64);

impl Chunks {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        256 + (self.0 % 769) as usize
    }
}

#[test]
fn parity_every_golden_clip() {
    let model = model();
    let train_id = model.text("train_id").unwrap().to_owned();
    for (ci, name) in golden_names().iter().enumerate() {
        let g = golden::load(name);
        assert_eq!(g.text("train_id").unwrap(), train_id, "{name}: golden from another model");
        let n = g.shape("audio_i16").unwrap()[0];
        let audio: Vec<f64> = g.i16s("audio_i16", &[n]).unwrap().iter().map(|&s| s as f64 / 32768.0).collect();
        let k = g.shape("cand_hop").unwrap()[0];
        let cand = g.i64s("cand_hop", &[k]).unwrap();
        let avail = g.i64s("avail_hop", &[k]).unwrap();
        let onset_s = g.f64s("onset_s", &[k]).unwrap();
        let emit_s = g.f64s("emit_s", &[k]).unwrap();
        let frames = g.shape("net_spec").unwrap()[0];
        let spec = g.f32s("net_spec", &[frames, BANDS]).unwrap();
        let want_p = g.f64s("net_p", &[k]).unwrap();

        let mut net = KickNet::new(&model).unwrap();
        // Candidates Python's slice end was not clipped for, in the order they become ready.
        let mut todo: Vec<usize> = Vec::new();
        let (mut clipped_low, mut clipped_high) = (0, 0);
        for i in 0..k {
            assert_eq!(net.hop_s(cand[i] as u64).to_bits(), onset_s[i].to_bits(), "{name}: onset_s[{i}]");
            assert_eq!(net.hop_s(avail[i] as u64).to_bits(), emit_s[i].to_bits(), "{name}: emit_s[{i}]");
            let end = ((emit_s[i] + net.ahead_s) / net.frame_s) as i64;
            if end < net.slice as i64 + PY_JITTER {
                clipped_low += 1;
            } else if end > frames as i64 - 1 - PY_JITTER {
                clipped_high += 1;
            } else {
                todo.push(i);
            }
        }
        todo.sort_by_key(|&i| net.ready_sample(avail[i] as u64));

        let mut rng = Chunks(0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
        let (mut pos, mut next_frame, mut next_cand) = (0, 0u64, 0);
        let (mut spec_err, mut p_err, mut bad) = (0.0f64, 0.0f64, 0);
        let (mut push_time, mut net_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        let mut scored = 0;
        while pos < n {
            let m = rng.next().min(n - pos);
            let t0 = std::time::Instant::now();
            net.push(&audio[pos..pos + m]);
            push_time += t0.elapsed();
            pos += m;
            while next_frame < net.spec.frames().min(frames as u64) {
                let got = net.spec.frame(next_frame).unwrap();
                let want = &spec[next_frame as usize * BANDS..][..BANDS];
                for (a, b) in got.iter().zip(want) {
                    spec_err = spec_err.max((a - b).abs() as f64);
                }
                next_frame += 1;
            }
            while next_cand < todo.len() && net.ready_sample(avail[todo[next_cand]] as u64) <= pos as u64 {
                let i = todo[next_cand];
                let t0 = std::time::Instant::now();
                let p = net.probability(cand[i] as u64, avail[i] as u64);
                net_time += t0.elapsed();
                scored += 1;
                let e = (p - want_p[i]).abs();
                p_err = p_err.max(e);
                if e.is_nan() || e > 1e-4 {
                    bad += 1;
                    eprintln!("{name}: candidate {i} p {p} want {}", want_p[i]);
                }
                next_cand += 1;
            }
        }
        assert_eq!(next_frame, frames as u64, "{name}: spectrum frames");
        assert_eq!(scored, todo.len(), "{name}: candidates never became ready");
        eprintln!(
            "{name}: {frames} frames, spec max |err| {spec_err:.3e} dB; {} of {k} candidates scored ({clipped_low} at the clip start and {clipped_high} at its end clipped by Python), \
             p max |err| {p_err:.3e}, {bad} over 1e-4; push {:.2} ms per audio second, {:.1} us per candidate",
            todo.len(),
            push_time.as_secs_f64() * 1e3 / (n as f64 / 48_000.0),
            net_time.as_secs_f64() * 1e6 / scored.max(1) as f64,
        );
        assert!(spec_err <= 1e-3, "{name}: spectrum max |err| {spec_err} dB");
        assert_eq!(bad, 0, "{name}: {bad} probabilities over 1e-4 (max {p_err})");
    }
}

/// Per-candidate cost; only a release run (`--release --no-capture`) says anything about the show.
#[test]
fn candidate_cost() {
    let mut net = KickNet::new(&model()).unwrap();
    let audio: Vec<f64> = (0..48_000 * 2).map(|i| ((i as f64) * 0.013).sin() * 0.3).collect();
    let t0 = std::time::Instant::now();
    net.push(&audio);
    let push = t0.elapsed();
    let reps = 20;
    let t0 = std::time::Instant::now();
    let mut acc = 0.0;
    for r in 0..reps {
        acc += net.probability(200 + r % 50, 202 + r % 50);
    }
    let per = t0.elapsed() / reps as u32;
    assert!(acc.is_finite());
    eprintln!("kick net: {:.1} us per candidate; push {:.2} ms per second of audio", per.as_secs_f64() * 1e6, push.as_secs_f64() * 1e3 / 2.0);
}
