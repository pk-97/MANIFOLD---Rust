//! Parity of the candidate finder and the flatness frames against the goldens
//! written by `tools/audio_analysis/snare_release.py`.

use super::cands::SnareCands;
use super::flat::{Flatness, BANDS};
use super::golden;
use crate::kick::container::Container;

fn model() -> Container {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/snare_model.mkick");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Container::parse(&bytes).unwrap()
}

fn golden_names() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snare");
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
        let hops = g.shape("cand_db").unwrap()[0];
        let want_db = g.f64s("cand_db", &[hops]).unwrap();
        let k = g.shape("cand_hop").unwrap()[0];
        let want_cand = g.i64s("cand_hop", &[k]).unwrap();
        let want_avail = g.i64s("avail_hop", &[k]).unwrap();
        let frames = g.shape("flat").unwrap()[0];
        let want_flat = g.f32s("flat", &[frames, BANDS]).unwrap();

        let mut cands = SnareCands::new(&model).unwrap();
        let mut flat = Flatness::new(&model).unwrap();
        let mut got: Vec<(u64, u64)> = Vec::with_capacity(k + 8);
        let (mut db_err, mut flat_err) = (0.0f64, 0.0f64);
        let mut chunks = Chunks(0x9E37_79B9_7F4A_7C15 ^ ci as u64);
        let mut at = 0;
        while at < n {
            let len = chunks.next().min(n - at);
            let before = (cands.hops(), flat.frames());
            cands.push(&audio[at..at + len], &mut got);
            flat.push(&audio[at..at + len]);
            at += len;
            for h in before.0..cands.hops() {
                if (h as usize) < hops {
                    db_err = db_err.max((cands.db(h).unwrap() - want_db[h as usize]).abs());
                }
            }
            for f in before.1..flat.frames() {
                if (f as usize) < frames {
                    let row = &want_flat[f as usize * BANDS..][..BANDS];
                    for (a, b) in flat.frame(f).unwrap().iter().zip(row) {
                        flat_err = flat_err.max((a - b).abs() as f64);
                    }
                }
            }
        }
        assert!(cands.hops() as usize >= hops && flat.frames() as usize >= frames, "{name}: stream ended short");
        let want: Vec<(u64, u64)> = want_cand.iter().zip(want_avail).map(|(&c, &a)| (c as u64, a as u64)).collect();
        let mismatches = got.iter().zip(&want).filter(|(a, b)| a != b).count() + got.len().abs_diff(want.len());
        println!("{name}: {hops} hops cand_db max abs {db_err:.3e}; {k} candidates, {mismatches} mismatches; {frames} frames flat max abs {flat_err:.3e}");
        assert!(db_err <= 1e-6, "{name}: cand_db error {db_err}");
        assert_eq!(got, want, "{name}: candidates differ");
        assert!(flat_err <= 1e-3, "{name}: flat error {flat_err}");
    }
}

#[test]
fn push_cost_per_audio_second() {
    let model = model();
    let mut cands = SnareCands::new(&model).unwrap();
    let mut flat = Flatness::new(&model).unwrap();
    let mut seed = 0x1234_5678_9abc_def0u64;
    let audio: Vec<f64> = (0..48_000 * 5)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 20_000) as f64 / 20_000.0 - 0.5
        })
        .collect();
    let mut got: Vec<(u64, u64)> = Vec::with_capacity(4096);
    let t = std::time::Instant::now();
    for c in audio.chunks(512) {
        cands.push(c, &mut got);
    }
    let tc = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    for c in audio.chunks(512) {
        flat.push(c);
    }
    let tf = t.elapsed().as_secs_f64();
    println!("cands {:.2} ms/audio-s, flat {:.2} ms/audio-s", tc * 200.0, tf * 200.0);
}
