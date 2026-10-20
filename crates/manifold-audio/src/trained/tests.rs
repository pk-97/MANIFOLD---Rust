//! Parity of the generic runtime against every golden set (docs/DETECTOR_RUNTIME_DESIGN.md section 6), written
//! by `tools/audio_analysis/lab/release.py export`. Part parity feeds each part the golden's own inputs; end to
//! end feeds audio only, in seeded random chunks.

use super::container::Container;
use super::detector::{FireRule, combine};
use super::frames::FrameSource;
use super::golden::{self, Chunks};
use super::history::History;
use super::net::average;
use super::spec::{MODEL_HOP, MODEL_RATE, Measure, Spec};
use super::trees::{Forest, expit};
use super::*;
use crate::detectors::{KickFed, Side};

/// Every golden set on disk: a count match, so no set is silently skipped.
const SETS: [&str; 4] = ["cover_kick", "cover_snare", "kick", "snare"];
/// The tail reference's past-4 s max reflects at the clip start, reading ahead for 1 ms grid indices under
/// this; the causal port is compared on pre_rel_db from here on only.
const TAIL_CAUSAL_FROM_MS: i64 = 1999 + 5;

fn dir(set: &str) -> String {
    format!("detectors/{set}")
}

fn model(set: &str) -> Container {
    golden::load(&dir(set), "model.mkick")
}

fn clips(set: &str) -> Vec<String> {
    golden::clips(&dir(set))
}

/// Rows of the named measures, hstacked, from a golden's `m.*` entries.
fn rows(g: &Container, measures: &[Measure], k: usize) -> Vec<f64> {
    let width: usize = measures.iter().map(|m| m.width()).sum();
    let mut out = vec![0.0; k * width];
    let mut at = 0;
    for m in measures {
        let src = g.f64s(&format!("m.{}", m.name()), &[k, m.width()]).unwrap();
        for i in 0..k {
            out[i * width + at..][..m.width()].copy_from_slice(&src[i * m.width()..][..m.width()]);
        }
        at += m.width();
    }
    out
}

fn opt(g: &Container, name: &str, k: usize) -> Option<Vec<f64>> {
    g.contains(name).then(|| g.f64s(name, &[k]).unwrap().to_vec())
}

fn max_err(a: impl IntoIterator<Item = f64>, b: impl IntoIterator<Item = f64>) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| if x == y { 0.0 } else { (x - y).abs() }).fold(0.0, |m, e| if e.is_nan() { f64::INFINITY } else { m.max(e) })
}

/// Section 6 part parity: trees on golden m.*, D17 on golden net.logit, combine on golden p.net / p.trees,
/// history on golden p.combined and m.*, the fire rule on golden p.final.
fn part_parity(set: &str, m: &Container, spec: &Spec, name: &str, g: &Container) {
    let k = g.shape("cand_hop").unwrap()[0];
    let avail = g.i64s("avail_hop", &[k]).unwrap();
    assert!(avail.windows(2).all(|w| w[1] >= w[0]), "{set}/{name}: emission order is index order");
    let p_combined = g.f64s("p.combined", &[k]).unwrap();
    let p_final = g.f64s("p.final", &[k]).unwrap();
    let mut report = Vec::new();
    let p_trees = opt(g, "p.trees", k);
    if let Some(ms) = &spec.trees {
        let forest = Forest::from_container(m, "trees.").unwrap();
        let x = rows(g, ms, k);
        let width = forest.n_features();
        let want = p_trees.as_ref().expect("p.trees with trees");
        let mut bad = 0;
        for i in 0..k {
            let raw = forest.raw(&x[i * width..][..width]);
            let close = (expit(raw) - want[i]).abs() <= 1e-12 || (raw - super::trees::logit(want[i])).abs() <= 1e-6;
            bad += !close as usize;
        }
        report.push(format!("trees {bad} over"));
        assert_eq!(bad, 0, "{set}/{name}: trees");
    }
    let p_net = opt(g, "p.net", k);
    if let Some(n) = &spec.net {
        let z = g.f32s("net.logit", &[k, n.count]).unwrap();
        let want = p_net.as_ref().expect("p.net with a net");
        let e = max_err((0..k).map(|i| average(&z[i * n.count..][..n.count], spec.combine_clip)), want.iter().copied());
        report.push(format!("p.net {e:.1e}"));
        assert!(e <= 1e-6, "{set}/{name}: p.net from golden logits {e}");
    }
    let nan = vec![f64::NAN; k];
    let (pn, pt) = (p_net.as_ref().unwrap_or(&nan), p_trees.as_ref().unwrap_or(&nan));
    let e = max_err((0..k).map(|i| combine(spec.combine, pn[i], pt[i], spec.combine_clip)), p_combined.iter().copied());
    report.push(format!("p.combined {e:.1e}"));
    assert!(e <= 1e-12, "{set}/{name}: p.combined {e}");
    match &spec.stage {
        Some(st) => {
            let mut h = History::new(m, st, MODEL_HOP, MODEL_RATE).unwrap();
            let shape = rows(g, &st.shape, k);
            let width = st.shape_width();
            let level = st.level.map(|(lm, col)| {
                let src = g.f64s(&format!("m.{}", lm.name()), &[k, lm.width()]).unwrap();
                (0..k).map(|i| src[i * lm.width() + col]).collect::<Vec<_>>()
            });
            let n_in = st.inputs();
            let (want_x, want_q) = (g.f64s("stage.x", &[k, n_in]).unwrap(), g.f64s("p.stage", &[k]).unwrap());
            let mut err = [0.0f64; 3];
            for i in 0..k {
                let lv = level.as_ref().map_or(0.0, |l| l[i]);
                let (x, q, f) = h.decide(p_combined[i], &shape[i * width..][..width], lv, avail[i] as u64 + 1);
                err[0] = err[0].max(max_err(x.iter().copied(), want_x[i * n_in..][..n_in].iter().copied()));
                err[1] = err[1].max(max_err([q], [want_q[i]]));
                err[2] = err[2].max(max_err([f], [p_final[i]]));
            }
            report.push(format!("stage.x {:.1e} p.stage {:.1e} p.final {:.1e}", err[0], err[1], err[2]));
            // The reference's window sums are differences of whole-clip prefix sums divided by the window weight
            // (kick_goal_selfsim.self_features), so its inputs carry eps x the clip's running total / that weight
            // (1.4e-11 on the 2026-10-13 snare's level column); the history here sums each window directly. The
            // stage's outputs and the fires stay exact.
            assert!(err[0] <= 1e-9, "{set}/{name}: history inputs {err:?}");
            assert!(err[1] <= 1e-12 && err[2] <= 1e-12, "{set}/{name}: history {err:?}");
            assert_eq!(h.dropped(), 0);
        }
        None => assert_eq!(p_final, p_combined, "{set}/{name}: no stage, p.final is p.combined"),
    }
    let mut rule = FireRule::new(spec.cutoff, spec.refractory_s);
    let fires: Vec<i64> = (0..k).filter(|&i| rule.decide(p_final[i], avail[i] as u64)).map(|i| i as i64).collect();
    let nf = g.shape("fires").unwrap()[0];
    assert_eq!(fires, g.i64s("fires", &[nf]).unwrap(), "{set}/{name}: fire rule on golden p.final");
    println!("{set}/{name} part: {k} candidates, {}; fires {nf} exact", report.join(", "));
}

/// The model alone, or fed by the kick set's model when it reads kick verdicts: the production pairing, with
/// the emission time of every kick decision.
enum Run {
    Alone(Box<TrainedDetector>),
    Fed(Box<KickFed>, Vec<f64>),
}

impl Run {
    fn new(m: &Container) -> Self {
        let det = TrainedDetector::from_container(m, 48_000).unwrap();
        if det.wants_kick().is_none() {
            return Self::Alone(Box::new(det));
        }
        let kick = TrainedDetector::from_container(&model("kick"), 48_000).unwrap();
        Self::Fed(Box::new(KickFed::new(kick, det).expect("the kick set is the kick this model reads")), Vec::new())
    }

    fn det(&self) -> &TrainedDetector {
        match self {
            Self::Alone(d) => d,
            Self::Fed(f, _) => f.reader(),
        }
    }

    fn push_traced(&mut self, samples: &[f32], mut on_decision: impl FnMut(&Decision<'_>, Option<Fire>)) {
        match self {
            Self::Alone(d) => d.push_traced(samples, on_decision),
            Self::Fed(f, emits) => f.push_traced(samples, |side, d, fire| match side {
                Side::Kick => emits.push(((d.avail_hop + 1) * MODEL_HOP) as f64 / MODEL_RATE as f64),
                Side::Reader => on_decision(d, fire),
            }),
        }
    }
}

/// A model's stream-start edge (see `end_to_end`): which emissions have a net slice that would start before the
/// clip, and the longest stage window, over which such a decision taints later stage inputs.
struct NetEdge {
    ahead: f64,
    frame_s: f64,
    slice: i64,
    max_w: f64,
}

impl NetEdge {
    fn of(m: &Container, spec: &Spec) -> Option<Self> {
        spec.net.as_ref()?;
        Some(Self {
            ahead: m.f64s("net.ahead_s", &[]).unwrap()[0],
            frame_s: m.f64s("net.frame_s", &[]).unwrap()[0],
            slice: m.i64s("net.slice", &[]).unwrap()[0],
            max_w: spec.stage.as_ref().map_or(0.0, |s| s.windows_s.iter().copied().fold(0.0, f64::max)),
        })
    }

    fn clipped(&self, emit_s: f64) -> bool {
        (((emit_s + self.ahead) / self.frame_s) as i64) < self.slice + 1
    }
}

#[derive(Default)]
struct Got {
    cand: u64,
    avail: u64,
    measures: Vec<f64>,
    logits: Vec<f32>,
    p_net: f64,
    p_trees: f64,
    p_combined: f64,
    stage_x: Vec<f64>,
    p_stage: f64,
    p_final: f64,
    fire: bool,
}

/// Section 6 end to end: audio only, seeded chunks of 256..=1024, output capacity reserved and unchanged.
fn end_to_end(set: &str, m: &Container, spec: &Spec, name: &str, g: &Container, seed: u64) {
    let k = g.shape("cand_hop").unwrap()[0];
    let (cand, avail) = (g.i64s("cand_hop", &[k]).unwrap(), g.i64s("avail_hop", &[k]).unwrap());
    let onset_s = g.f64s("onset_s", &[k]).unwrap();
    let audio = golden::audio_f32(g);
    let mut run = Run::new(m);
    let lag = run.det().decision_lag_s();
    let mut got: Vec<Got> = Vec::with_capacity(k + 64);
    let mut fires: Vec<Fire> = Vec::new();
    let mut chunks = Chunks(seed);
    let (mut at, mut spec_f, mut spec_err) = (0, 0u64, 0.0f64);
    let gspec = g.contains("frames.spec").then(|| (g.shape("frames.spec").unwrap()[0], g.f32s("frames.spec", &[g.shape("frames.spec").unwrap()[0], 64]).unwrap()));
    // Each extra channel in the net's order: (name, golden frames, values, frames compared, max error).
    let mut gch: Vec<(&str, usize, &[f32], u64, f64)> = spec.net.as_ref().map_or(Vec::new(), |n| {
        n.channels
            .iter()
            .map(|ch| {
                let key = format!("frames.{}", ch.name());
                let t = g.shape(&key).unwrap()[0];
                (ch.name(), t, g.f32s(&key, &[t, 64]).unwrap(), 0, 0.0)
            })
            .collect()
    });
    while at < audio.len() {
        let n = chunks.next().min(audio.len() - at);
        fires.reserve(run.det().max_fires(n));
        let cap = fires.capacity();
        run.push_traced(&audio[at..at + n], |d, f| {
            got.push(Got {
                cand: d.cand_hop,
                avail: d.avail_hop,
                measures: d.measures.to_vec(),
                logits: d.net_logits.to_vec(),
                p_net: d.p_net,
                p_trees: d.p_trees,
                p_combined: d.p_combined,
                stage_x: d.stage_x.to_vec(),
                p_stage: d.p_stage,
                p_final: d.p_final,
                fire: d.fire,
            });
            fires.extend(f);
        });
        assert_eq!(fires.capacity(), cap, "{set}/{name}: push grew the output");
        at += n;
        let det = run.det();
        if let (Some(net), Some((t, want))) = (det.net(), &gspec) {
            while (spec_f as usize) < *t && let Some(row) = net.spectrum().frame(spec_f) {
                spec_err = spec_err.max(max_err(row.iter().map(|&v| v as f64), want[spec_f as usize * 64..][..64].iter().map(|&v| v as f64)));
                spec_f += 1;
            }
        }
        for (i, (_, t, want, f, err)) in gch.iter_mut().enumerate() {
            while (*f as usize) < *t && let Some(row) = det.frames().frame(i, *f) {
                *err = err.max(max_err(row.iter().map(|&v| v as f64), want[*f as usize * 64..][..64].iter().map(|&v| v as f64)));
                *f += 1;
            }
        }
    }
    assert_eq!(run.det().dropped(), 0, "{set}/{name}: dropped candidates");
    let d = got.len();
    assert!(d <= k && k - d <= 64, "{set}/{name}: {d} of {k} decided");
    for (i, x) in got.iter().enumerate() {
        assert_eq!((x.cand as i64, x.avail as i64), (cand[i], avail[i]), "{set}/{name}: candidate {i}");
    }
    if let Some((t, _)) = &gspec {
        assert!(spec_f as usize + 64 >= *t, "{set}/{name}: only {spec_f} of {t} spectrum frames compared");
    }
    let frames = gch.iter().map(|(n, t, _, f, e)| format!(" {n} {e:.1e} ({f} of {t})")).collect::<String>();
    assert!(spec_err <= 1e-3 && gch.iter().all(|c| c.4 <= 1e-3), "{set}/{name}: frames spec {spec_err}{frames} dB");

    // Measures, abs 1e-6; the kick verdict (kick2) carries the kick's probability, abs 1e-4 like the kick's own
    // p.final, and is not comparable where the kick's window opens inside the kick's stream-start edge.
    let used = spec.measures();
    let want_m = rows(g, &used, k);
    let width: usize = used.iter().map(|m| m.width()).sum();
    let col_of = |want: Measure| used.iter().position(|&m| m == want).map(|p| used[..p].iter().map(|m| m.width()).sum::<usize>());
    let tail_col = col_of(Measure::Tail3).map(|c| c + 1);
    let kick_cols = col_of(Measure::Kick2).map(|c| c..c + Measure::Kick2.width());
    // The last clipped kick decision taints the kick's p.final over its longest stage window.
    let kick_edge_s = match (&run, &kick_cols) {
        (Run::Fed(_, emits), Some(_)) => {
            let km = model("kick");
            let edge = NetEdge::of(&km, &Spec::parse(&km).unwrap());
            edge.and_then(|e| emits.iter().rev().find(|&&t| e.clipped(t)).map(|t| t + e.max_w + m.f64s("kick2.window_s", &[2]).unwrap()[0]))
        }
        _ => None,
    };
    let kick_edge = |i: usize| kick_edge_s.is_some_and(|e| onset_s[i] <= e);
    let (mut m_err, mut k_err, mut tail_skipped) = (0.0f64, 0.0f64, 0);
    for (i, x) in got.iter().enumerate() {
        let onset_ms = (onset_s[i] / 0.001).round_ties_even() as i64;
        for c in 0..width {
            if Some(c) == tail_col && (60..TAIL_CAUSAL_FROM_MS).contains(&onset_ms) {
                tail_skipped += 1;
                continue;
            }
            let e = max_err([x.measures[c]], [want_m[i * width + c]]);
            match &kick_cols {
                Some(r) if r.contains(&c) => k_err = if kick_edge(i) { k_err } else { k_err.max(e) },
                _ => m_err = m_err.max(e),
            }
        }
    }
    // Net logits 1e-3, every probability 1e-4. `Song.__init__` clips a slice that would start before the clip
    // (with the training jitter, even at predict time); a stream has no such clip, so those candidates' net
    // outputs are not comparable, nor is any stage input whose memory window holds one. Only clips that start on
    // music have any (D10).
    let emit_s = g.f64s("emit_s", &[k]).unwrap();
    // A kick2 reader's candidates in the kick's edge are left out with them.
    let edge = NetEdge::of(m, spec);
    let clipped: Vec<bool> = (0..d).map(|i| edge.as_ref().is_some_and(|e| e.clipped(emit_s[i])) || kick_edge(i)).collect();
    let max_w = spec.stage.as_ref().map_or(0.0, |s| s.windows_s.iter().copied().fold(0.0, f64::max));
    let last_clipped = (0..d).rev().find(|&i| clipped[i]);
    let tainted: Vec<bool> = (0..d).map(|i| clipped[i] || last_clipped.is_some_and(|c| c <= i && emit_s[i] - emit_s[c] <= max_w)).collect();
    let count = spec.net.as_ref().map_or(0, |n| n.count);
    let z = (count > 0).then(|| g.f32s("net.logit", &[k, count]).unwrap());
    let keep = |i: &usize, stage: bool| !(if stage { tainted[*i] } else { clipped[*i] });
    let z_err = z.map_or(0.0, |z| {
        let ix: Vec<usize> = (0..d).filter(|i| keep(i, false)).collect();
        max_err(ix.iter().flat_map(|&i| got[i].logits.iter().map(|&v| v as f64)), ix.iter().flat_map(|&i| z[i * count..][..count].iter().map(|&v| v as f64)))
    });
    let p_err = |name: &str, stage: bool, f: &dyn Fn(&Got) -> f64| {
        opt(g, name, k).map_or(0.0, |w| {
            let ix: Vec<usize> = (0..d).filter(|i| keep(i, stage)).collect();
            max_err(ix.iter().map(|&i| f(&got[i])), ix.iter().map(|&i| w[i]))
        })
    };
    let staged = spec.stage.is_some();
    let errs = [
        ("p.net", p_err("p.net", false, &|x| x.p_net)),
        ("p.trees", p_err("p.trees", false, &|x| x.p_trees)),
        ("p.combined", p_err("p.combined", false, &|x| x.p_combined)),
        ("p.stage", p_err("p.stage", true, &|x| x.p_stage)),
        ("p.final", p_err("p.final", staged, &|x| x.p_final)),
    ];
    let x_err = spec.stage.as_ref().map_or(0.0, |st| {
        let n_in = st.inputs();
        let w = g.f64s("stage.x", &[k, n_in]).unwrap();
        let ix: Vec<usize> = (0..d).filter(|i| keep(i, true)).collect();
        max_err(ix.iter().flat_map(|&i| got[i].stage_x.clone()), ix.iter().flat_map(|&i| w[i * n_in..][..n_in].iter().copied()))
    });
    let edge = (clipped.iter().filter(|&&c| c).count(), tainted.iter().filter(|&&c| c).count());
    // Fires exact, except at most one per clip within 1e-3 of the cutoff.
    let nf = g.shape("fires").unwrap()[0];
    let want_f: Vec<usize> = g.i64s("fires", &[nf]).unwrap().iter().map(|&i| i as usize).filter(|&i| i < d).collect();
    let got_f: Vec<usize> = (0..d).filter(|&i| got[i].fire).collect();
    let p_final = g.f64s("p.final", &[k]).unwrap();
    let mismatched: Vec<usize> =
        got_f.iter().filter(|i| !want_f.contains(i)).chain(want_f.iter().filter(|i| !got_f.contains(i))).copied().collect();
    let (lo, hi) = fires.iter().fold((f64::INFINITY, 0.0f64), |(a, b), f| {
        let e = (f.sample - (f.avail_hop + 1) * MODEL_HOP) as f64 / 48.0;
        (a.min(e), b.max(e))
    });
    println!(
        "{set}/{name} e2e: {d} of {k} decided; frames spec {spec_err:.1e}{frames} dB; measures {m_err:.1e} ({tail_skipped} early pre_rel_db not compared); \
         kick2 {k_err:.1e}; net.logit {z_err:.1e}; {}; stage.x {x_err:.1e} (stream-start edge: {} net, {} stage candidates not compared); fires {}/{} mismatched {mismatched:?}; lag {lo:.1}..{hi:.1} ms (bound {:.1})",
        errs.iter().map(|(n, e)| format!("{n} {e:.1e}")).collect::<Vec<_>>().join(" "),
        edge.0,
        edge.1,
        got_f.len(),
        want_f.len(),
        lag * 1e3
    );
    assert!(m_err <= 1e-6, "{set}/{name}: measures {m_err}");
    assert!(k_err <= 1e-4, "{set}/{name}: kick2 {k_err}");
    assert!(z_err <= 1e-3, "{set}/{name}: net.logit {z_err}");
    for (n, e) in errs {
        assert!(e <= 1e-4, "{set}/{name}: {n} {e}");
    }
    assert!(x_err <= 1e-4, "{set}/{name}: stage.x {x_err}");
    assert!(
        mismatched.is_empty() || (mismatched.len() == 1 && (p_final[mismatched[0]] - spec.cutoff).abs() < 1e-3),
        "{set}/{name}: fires mismatched {mismatched:?}"
    );
    for f in &fires {
        let emission = (f.avail_hop + 1) * MODEL_HOP;
        assert!(f.sample >= emission, "{set}/{name}: fire stamped before its emission");
        // The bound holds once the net has a full slice of stream behind it.
        if emission >= 24_000 {
            assert!((f.sample - emission) as f64 <= lag * MODEL_RATE as f64, "{set}/{name}: fire later than decision_lag_s");
        }
    }
}

fn parity(set: &str) {
    let m = model(set);
    let spec = Spec::parse(&m).unwrap_or_else(|e| panic!("{set}: model refused: {e}"));
    for (ci, name) in clips(set).iter().enumerate() {
        let g = golden::load(&dir(set), name);
        assert_eq!(g.text("train_id").unwrap(), spec.train_id, "{set}/{name}: golden from another model");
        assert_eq!(g.i32s("contract", &[]).unwrap(), &[spec::CONTRACT]);
        part_parity(set, &m, &spec, name, &g);
        end_to_end(set, &m, &spec, name, &g, 0x9E37_79B9_7F4A_7C15 ^ (ci as u64 + 1));
    }
}

#[test]
fn parity_kick() {
    parity("kick");
}

#[test]
fn parity_snare() {
    parity("snare");
}

#[test]
fn parity_cover_kick() {
    parity("cover_kick");
}

#[test]
fn parity_cover_snare() {
    parity("cover_snare");
}

#[test]
fn every_golden_set_runs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detectors");
    let mut found: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("model.mkick").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    found.sort();
    assert_eq!(found, SETS, "golden sets on disk differ from the parity tests");
}

#[test]
fn goldens_match_their_model() {
    for set in SETS {
        let id = model(set).text("train_id").unwrap().to_owned();
        for name in clips(set) {
            assert_eq!(golden::load(&dir(set), &name).text("train_id").unwrap(), id, "{set}/{name}");
        }
    }
}

/// The app runs the tested models: each embedded file is its family's golden set model, byte for byte.
#[test]
fn embedded_assets_are_the_shipped_sets() {
    for (kind, bytes) in crate::detectors::EMBEDDED {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}/model.mkick", dir(kind.name())));
        let set = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(bytes == set.as_slice(), "assets/{}_model.mkick differs from {}", kind.name(), path.display());
        assert_eq!(Container::parse(bytes).unwrap().text("family").unwrap(), kind.name());
    }
}
