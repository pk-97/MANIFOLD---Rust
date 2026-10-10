//! The 54 extra tree features, in the order of `kick_goal_featsets.py` `build(g, "f69")`:
//! tail (3), templates (3), rise profile (32), low filter bank (16). Every coefficient
//! comes from the `extra.*` entries written by `tools/audio_analysis/kick_export/extra.py`.

mod dsp;
mod lowbank;
mod profile;
mod spec;
mod tail;
mod templates;

use crate::kick::container::{Container, ContainerError};

pub const EXTRA_DIMS: usize = 54;
const TAIL: std::ops::Range<usize> = 0..3;
const TMPL: std::ops::Range<usize> = 3..6;
const PROF: std::ops::Range<usize> = 6..38;
const LOW: std::ops::Range<usize> = 38..54;

/// 1 ms grid history: covers the 60 ms pre-onset reach, the evidence span, and about
/// 3.5 s of lateness between `ready_sample` and the `features` call.
const HIST_MS: usize = 4096;
/// Spectral frame history (one per hop, about 5.4 s at 48 kHz / 256).
const HIST_FRAMES: usize = 1024;

/// Stream constants shared by the pieces.
struct Consts {
    sr: f64,
    hop: u64,
    ms: f64,
}

impl Consts {
    /// `(hop_index + 1) * hop / sr`, the reference's `onset_s` / `emit_s`.
    fn hop_end_s(&self, h: u64) -> f64 {
        ((h + 1) * self.hop) as f64 / self.sr
    }
}

fn scalar(c: &Container, name: &str) -> Result<f64, ContainerError> {
    Ok(c.f64s(name, &[])?[0])
}

fn positive(c: &Container, name: &str) -> Result<u64, ContainerError> {
    match c.i64s(name, &[])?[0] {
        v if v > 0 => Ok(v as u64),
        _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "positive i64" }),
    }
}

/// Streams 48 kHz mono audio and answers the 54 extra features for any candidate whose
/// data has arrived. No allocation after `new`.
pub struct ExtraFeatures {
    k: Consts,
    samples: u64,
    tail: tail::Tail,
    spec: spec::BandSpec,
    templates: templates::Templates,
    low: lowbank::LowBank,
}

impl ExtraFeatures {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let k = Consts {
            sr: positive(c, "extra.sample_rate")? as f64,
            hop: positive(c, "extra.hop")?,
            ms: scalar(c, "extra.ms")?,
        };
        Ok(Self {
            tail: tail::Tail::new(c, &k)?,
            spec: spec::BandSpec::new(c, k.hop)?,
            templates: templates::Templates::new(c)?,
            low: lowbank::LowBank::new(c, &k)?,
            samples: 0,
            k,
        })
    }

    pub fn push(&mut self, samples: &[f64]) {
        for &x in samples {
            let s = self.samples;
            self.tail.push(s, x);
            self.spec.push(s, x);
            self.low.push(s, x);
            self.samples += 1;
        }
    }

    /// Samples pushed so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// The first stream sample count at which `features` is valid: the latest of the tail
    /// and low-bank envelopes at the deadline's 1 ms grid point, the profile's deadline
    /// frame, and the template patch's last frame (`cand + span - 1`, which can pass the
    /// deadline).
    pub fn ready_sample(&self, cand_hop: u64, avail_hop: u64) -> u64 {
        let frame_end = |f: u64| (f + 1) * self.k.hop;
        self.tail
            .ready_sample(&self.k, avail_hop)
            .max(self.low.ready_sample(&self.k, avail_hop))
            .max(frame_end(avail_hop))
            .max(frame_end(self.templates.last_frame(cand_hop)))
    }

    /// Writes `[tail3, tmpl3, prof32, low16]`. Call between `ready_sample` and about
    /// 3.5 s later; outside that window it panics rather than read missing history.
    pub fn features(&mut self, cand_hop: u64, avail_hop: u64, out: &mut [f64; EXTRA_DIMS]) {
        assert!(
            self.samples >= self.ready_sample(cand_hop, avail_hop),
            "kick extra: candidate {cand_hop} asked before its data arrived"
        );
        self.tail.features(&self.k, cand_hop, avail_hop, &mut out[TAIL]);
        self.templates.features(&self.spec, cand_hop, &mut out[TMPL]);
        profile::features(&self.spec, cand_hop, avail_hop, &mut out[PROF]);
        self.low.features(&self.k, cand_hop, avail_hop, &mut out[LOW]);
    }
}

#[cfg(test)]
mod parity {
    use super::*;
    use crate::kick::golden;

    /// Pieces of the golden in contract order: (name, width, range in the 54).
    const PIECES: [(&str, usize, std::ops::Range<usize>); 4] =
        [("tail3", 3, TAIL), ("tmpl3", 3, TMPL), ("prof32", 32, PROF), ("low16", 16, LOW)];
    /// The reference's past-4 s max reads ahead for 1 ms grid indices under this
    /// (scipy reflect boundary); the causal port is compared from here on only.
    const TAIL_CAUSAL_FROM_MS: i64 = 1999 + 5;

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
        assert!(!names.is_empty(), "no kick goldens in {}", dir.display());
        names
    }

    #[test]
    fn extra_features_match_every_golden_clip() {
        let model = model();
        let train_id = model.text("train_id").unwrap().to_owned();
        let mut failures = Vec::new();
        for name in clip_names() {
            let clip = golden::load(&name);
            assert_eq!(clip.text("train_id").unwrap(), train_id, "{name}: golden from another model");
            let audio: Vec<f64> = {
                let n = clip.shape("audio_i16").unwrap()[0];
                clip.i16s("audio_i16", &[n]).unwrap().iter().map(|&v| v as f64 / 32768.0).collect()
            };
            let k = clip.shape("cand_hop").unwrap()[0];
            let cand = clip.i64s("cand_hop", &[k]).unwrap();
            let avail = clip.i64s("avail_hop", &[k]).unwrap();
            let onset = clip.f64s("onset_s", &[k]).unwrap();
            let want: Vec<&[f64]> = PIECES.iter().map(|(p, w, _)| clip.f64s(p, &[k, *w]).unwrap()).collect();

            let mut ex = ExtraFeatures::new(&model).unwrap();
            for i in 0..k {
                assert_eq!(ex.k.hop_end_s(cand[i] as u64), onset[i], "{name}: onset time of candidate {i}");
            }
            let ready: Vec<u64> = (0..k).map(|i| ex.ready_sample(cand[i] as u64, avail[i] as u64)).collect();
            // The reference truncates or zeroes windows that run past the clip end; a stream
            // never ends, so candidates within a hop of the end are not comparable.
            let comparable = |i: usize| ready[i] + 256 <= audio.len() as u64;
            let mut got = vec![[f64::NAN; EXTRA_DIMS]; k];
            let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ name.len() as u64;
            let (mut at, mut next) = (0usize, 0usize);
            let mut order: Vec<usize> = (0..k).collect();
            order.sort_by_key(|&i| ready[i]);
            while at < audio.len() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let chunk = 256 + ((seed >> 33) % 769) as usize;
                let end = (at + chunk).min(audio.len());
                ex.push(&audio[at..end]);
                at = end;
                while next < k && ready[order[next]] <= ex.samples() {
                    let i = order[next];
                    ex.features(cand[i] as u64, avail[i] as u64, &mut got[i]);
                    next += 1;
                }
            }

            let (mut tail_excluded, mut tail_differ, mut tail_worst) = (0, 0, 0.0f64);
            let mut skipped_end = 0;
            for (p, (piece, w, range)) in PIECES.iter().enumerate() {
                let mut max_err = 0.0f64;
                let mut bad = 0;
                for i in 0..k {
                    if !comparable(i) {
                        skipped_end += (p == 0) as usize;
                        continue;
                    }
                    let onset_ms = (onset[i] / ex.k.ms).round_ties_even() as i64;
                    for j in 0..*w {
                        let (g, r) = (got[i][range.start + j], want[p][i * w + j]);
                        let err = (g - r).abs();
                        if *piece == "tail3" && j == 1 && (60..TAIL_CAUSAL_FROM_MS).contains(&onset_ms) {
                            tail_excluded += 1;
                            tail_differ += (err > 1e-6) as usize;
                            tail_worst = tail_worst.max(err);
                            continue;
                        }
                        if err.is_nan() || err > 1e-6 {
                            bad += 1;
                            if bad <= 3 {
                                failures.push(format!("{name} {piece}[{i}][{j}] cand {}: rust {g} ref {r}", cand[i]));
                            }
                        }
                        if !err.is_nan() {
                            max_err = max_err.max(err);
                        }
                    }
                }
                println!("{name} {piece}: max abs err {max_err:.3e}, {bad} over 1e-6");
                if bad > 0 {
                    failures.push(format!("{name} {piece}: {bad} values over 1e-6"));
                }
            }
            println!("{name}: {k} candidates, {skipped_end} skipped at clip end, {tail_excluded} early pre_rel_db rows not compared ({tail_differ} differ, worst {tail_worst:.3} dB)");
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
