//! The blend, the 8 s song-relative stage, cutoff and refractory. References:
//! `run_kick_goal_fast.py` (`lg`, the 'blend' base, `stage_matrix`), `kick_goal_selfsim.py` (`self_features`),
//! `kick_goal_eval.py` (`fires`). Constants come from `tools/audio_analysis/kick_export/stage.py`.

use super::container::{Container, ContainerError};
use super::trees::{Forest, expit, logit};

/// Descriptor width: the 32-bin rise profile then the 16-band low bank (`hstack(prof32, low16)`).
pub const DESCRIPTOR_LEN: usize = 48;
/// Stage inputs, in order: lp, sim, lp_rel, lvl_rel, evidence (`kick_goal_selfsim.SELF_NAMES`).
pub const STAGE_INPUTS: usize = 5;
/// Candidates per second the history holds without dropping any. Candidates are at most one per hop
/// (187.5 hops/s at 48 kHz, 256-sample hop); the margin covers candidates whose emission lags their onset.
pub const MAX_CANDIDATE_RATE: f64 = 192.0;

/// One candidate's outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub blend_p: f64,
    pub stage_x: [f64; STAGE_INPUTS],
    pub stage_p: f64,
    pub fire: bool,
}

#[derive(Debug, Clone, Copy)]
struct Past {
    emit_s: f64,
    w: f64,
    lp: f64,
    level: f64,
    d: [f64; DESCRIPTOR_LEN],
}

const EMPTY: Past = Past { emit_s: 0.0, w: 0.0, lp: 0.0, level: 0.0, d: [0.0; DESCRIPTOR_LEN] };

/// The causal decision stage. Call [`Stage::decide`] once per candidate in emission order.
#[derive(Debug, Clone)]
pub struct Stage {
    forest: Forest,
    cutoff: f64,
    refractory_s: f64,
    window_s: f64,
    power: f64,
    blend_clip: f64,
    lp_clip: f64,
    blend_weights: [f64; 2],
    sample_rate: i64,
    hop: i64,
    ring: Vec<Past>,
    head: usize,
    len: usize,
    last_fire: Option<u64>,
    dropped: u64,
}

fn bad(name: &str, want: &'static str) -> ContainerError {
    ContainerError::WrongType { name: name.to_owned(), want }
}

impl Stage {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let forest = Forest::from_container(c, "stage.")?;
        if forest.n_features() != STAGE_INPUTS {
            return Err(bad("stage.n_features", "5 stage inputs"));
        }
        let scalar = |name: &str| c.f64s(name, &[]).map(|v| v[0]);
        let window_s = scalar("stage.window_s")?;
        if !(window_s > 0.0 && window_s <= 60.0) {
            return Err(bad("stage.window_s", "a window between 0 and 60 s"));
        }
        let w = c.f64s("stage.blend_weights", &[2])?;
        let sample_rate = c.i64s("stage.sample_rate", &[])?[0];
        let hop = c.i64s("stage.hop", &[])?[0];
        if sample_rate <= 0 || hop <= 0 {
            return Err(bad("stage.hop", "positive hop and sample rate"));
        }
        let capacity = (window_s * MAX_CANDIDATE_RATE).ceil() as usize + 1;
        Ok(Self {
            forest,
            cutoff: scalar("stage.cutoff")?,
            refractory_s: scalar("stage.refractory_s")?,
            window_s,
            power: scalar("stage.power")?,
            blend_clip: scalar("stage.blend_clip")?,
            lp_clip: scalar("stage.lp_clip")?,
            blend_weights: [w[0], w[1]],
            sample_rate,
            hop,
            ring: vec![EMPTY; capacity],
            head: 0,
            len: 0,
            last_fire: None,
            dropped: 0,
        })
    }

    /// Forgets the history and the last fire (a new stream).
    pub fn reset(&mut self) {
        self.head = 0;
        self.len = 0;
        self.last_fire = None;
    }

    pub fn cutoff(&self) -> f64 {
        self.cutoff
    }

    /// Candidates dropped from a full history (the memory was shorter than the window for them). Zero
    /// unless candidates arrive faster than [`MAX_CANDIDATE_RATE`] over a whole window.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Blend, stage inputs, stage probability and the fire decision for one candidate. Inputs: the base
    /// trees' and the net's probabilities, the raw descriptor `hstack(prof32, low16)` (clipped and
    /// normalised here), the level `f15[0]`, the emission hop (emission time = `emit_hop * hop / sr`,
    /// i.e. `avail_hop + 1`) and the availability hop the refractory counts in. Calls must come in
    /// non-decreasing emission order. Stage reads no audio: it is callable as soon as its inputs exist.
    pub fn decide(
        &mut self,
        tree_p: f64,
        net_p: f64,
        descriptor: &[f64; DESCRIPTOR_LEN],
        level: f64,
        emit_hop: u64,
        avail_hop: u64,
    ) -> Decision {
        let blend_p = self.blend(tree_p, net_p);
        let emit_s = (emit_hop as i64 * self.hop) as f64 / self.sample_rate as f64;
        let d = descriptor_unit(descriptor);
        let lp = logit(blend_p.clamp(self.lp_clip, 1.0 - self.lp_clip));

        // self_features: the window is every earlier candidate with emit >= now - window_s.
        let from = emit_s - self.window_s;
        while self.len > 0 && self.ring[self.head].emit_s < from {
            self.pop();
        }
        // Summed over the ring each call, oldest first: no running sums, so no drift over a long set.
        let (mut sw, mut sl, mut sv) = (0.0, 0.0, 0.0);
        let mut tmpl = [0.0; DESCRIPTOR_LEN];
        for k in 0..self.len {
            let p = &self.ring[(self.head + k) % self.ring.len()];
            sw += p.w;
            for (t, x) in tmpl.iter_mut().zip(&p.d) {
                *t += p.w * x;
            }
            sl += p.w * p.lp;
            sv += p.w * p.level;
        }
        let mut x = [lp, 0.0, 0.0, 0.0, sw.ln_1p()];
        if sw > 1e-9 {
            let safe = sw.max(1e-12);
            let dot: f64 = d.iter().zip(&tmpl).map(|(a, b)| a * b).sum();
            let norm = tmpl.iter().map(|t| t * t).sum::<f64>().sqrt();
            x[1] = dot / (norm + 1e-12);
            x[2] = lp - sl / safe;
            x[3] = level - sv / safe;
        }
        let stage_p = self.forest.predict_proba(&x);
        let fire = stage_p >= self.cutoff && self.clear_of_refractory(avail_hop);
        if fire {
            self.last_fire = Some(avail_hop);
        }
        self.push(Past { emit_s, w: blend_p.clamp(0.0, 1.0).powf(self.power), lp, level, d });
        Decision { blend_p, stage_x: x, stage_p, fire }
    }

    /// `expit(mean logit)` of trees and net (`run_kick_goal_fast.py` 'blend'); weights 0.5/0.5 give
    /// exactly `(a + b) / 2`.
    fn blend(&self, tree_p: f64, net_p: f64) -> f64 {
        let lg = |p: f64| logit(p.clamp(self.blend_clip, 1.0 - self.blend_clip));
        expit(self.blend_weights[0] * lg(tree_p) + self.blend_weights[1] * lg(net_p))
    }

    /// `kick_goal_eval.fires`: hop distance to the last fire, in seconds, against the refractory.
    fn clear_of_refractory(&self, avail_hop: u64) -> bool {
        match self.last_fire {
            None => true,
            Some(last) => {
                let gap = (avail_hop as i64 - last as i64) * self.hop;
                gap as f64 / self.sample_rate as f64 >= self.refractory_s - 1e-12
            }
        }
    }

    fn pop(&mut self) {
        self.head = (self.head + 1) % self.ring.len();
        self.len -= 1;
    }

    fn push(&mut self, p: Past) {
        if self.len == self.ring.len() {
            self.pop();
            self.dropped += 1;
        }
        let i = (self.head + self.len) % self.ring.len();
        self.ring[i] = p;
        self.len += 1;
    }
}

/// `kick_goal_selfsim.descriptor`: clipped at zero, then L2-normalised (NaN passes through like numpy).
fn descriptor_unit(raw: &[f64; DESCRIPTOR_LEN]) -> [f64; DESCRIPTOR_LEN] {
    let mut d = raw.map(|v| if v < 0.0 { 0.0 } else { v });
    let norm = d.iter().map(|v| v * v).sum::<f64>().sqrt() + 1e-9;
    for v in &mut d {
        *v /= norm;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kick::trees::tests::{container, f64e, i32e, i64e};

    /// Stage forest: one tree splitting lp at 1.0 into -2 / +3; cutoff 0.5.
    fn model() -> Container {
        container(&[
            f64e("stage.value", &[0.0, -2.0, 3.0], false),
            f64e("stage.threshold", &[1.0, 0.0, 0.0], false),
            i32e("stage.feature", &[0, 0, 0]),
            i32e("stage.left", &[1, 0, 0]),
            i32e("stage.right", &[2, 0, 0]),
            i32e("stage.missing_left", &[0, 0, 0]),
            i32e("stage.is_leaf", &[0, 1, 1]),
            i64e("stage.offsets", &[0, 3], false),
            f64e("stage.baseline", &[0.0], true),
            i64e("stage.n_features", &[5], true),
            f64e("stage.cutoff", &[0.5], true),
            f64e("stage.refractory_s", &[0.06], true),
            f64e("stage.window_s", &[8.0], true),
            f64e("stage.power", &[8.0], true),
            f64e("stage.blend_clip", &[1e-6], true),
            f64e("stage.lp_clip", &[1e-6], true),
            f64e("stage.blend_weights", &[0.5, 0.5], false),
            i64e("stage.sample_rate", &[48000], true),
            i64e("stage.hop", &[256], true),
        ])
    }

    /// Expected values from `self_features`, the blend and `fires` on the same stream (scratch script
    /// stage_vals.py, scipy 1.17.1 / numpy 2.4.2). The window drops the first two candidates at the fifth.
    #[test]
    fn matches_the_python_stage_on_a_hand_made_stream() {
        let tree = [0.9, 0.2, 0.95, 0.6, 0.99, 0.97, 0.4];
        let net = [0.8, 0.1, 0.9, 0.7, 0.999, 0.5, 0.45];
        let avail = [10u64, 12, 400, 1000, 1700, 1704, 1712];
        let level = [1.0, -2.0, 3.0, 0.5, 2.5, 4.0, -1.0];
        let blend = [
            0.8571428571428572,
            0.14285714285714285,
            0.928960606878694,
            0.6516685226452116,
            0.9968302801438442,
            0.8504391264975321,
            0.42480768092719207,
        ];
        let want_x = [
            [1.7917594692280556, 0.0, 0.0, 0.0, 0.0],
            [-1.791759469228055, 0.4992034811813095, -3.5835189384561104, -3.0, 0.2556937210025549],
            [2.5708317782513297, 0.43419068203529493, 0.779074442556656, 2.0000017861214787, 0.25569385533139927],
            [0.6263814842476834, 0.5654751113254876, -1.676127593927203, -1.8111750943225196, 0.6129969929811535],
            [5.750937314391587, 0.5061644557133708, 3.2878219310204937, -0.3615079089287918, 0.4619228524941818],
            [1.738049344917636, 0.5591452721733352, -2.7771003663117853, 1.3641206848569767, 0.940805627286397],
            [-0.30306790178515775, 0.6631057822110182, -4.404270946171403, -3.8392112571636527, 1.0422759115106217],
        ];
        let mut s = Stage::new(&model()).unwrap();
        let mut fired = vec![];
        for k in 0..7 {
            let mut shape = [0.0; DESCRIPTOR_LEN];
            for (j, v) in shape.iter_mut().enumerate() {
                *v = (0.3 * (k as f64 + 1.0) * (j as f64 + 1.0)).sin() + 0.1 * k as f64;
            }
            let d = s.decide(tree[k], net[k], &shape, level[k], avail[k] + 1, avail[k]);
            assert!((d.blend_p - blend[k]).abs() < 1e-15, "blend {k}");
            for (c, (a, b)) in d.stage_x.iter().zip(&want_x[k]).enumerate() {
                assert!((a - b).abs() < 1e-12, "x[{k}][{c}] {a} vs {b}");
            }
            assert_eq!(d.stage_p, expit(if d.stage_x[0] <= 1.0 { -2.0 } else { 3.0 }));
            if d.fire {
                fired.push(k);
            }
        }
        // Candidate 5 passes the cutoff 4 hops (21 ms) after the fire at 4: refractory.
        assert_eq!(fired, [0, 2, 4]);
        assert_eq!(s.dropped(), 0);
    }

    #[test]
    fn a_full_history_drops_its_oldest() {
        let mut s = Stage::new(&model()).unwrap();
        let cap = s.ring.len();
        let shape = [1.0; DESCRIPTOR_LEN];
        for k in 0..cap as u64 + 3 {
            s.decide(0.9, 0.9, &shape, 0.0, 1, 0);
            assert_eq!(s.len, cap.min(k as usize + 1));
        }
        assert_eq!(s.dropped(), 3);
    }
}

#[cfg(test)]
mod parity {
    use super::*;
    use crate::kick::golden;

    fn model() -> Container {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kick_model.mkick");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Container::parse(&bytes).unwrap()
    }

    fn clips() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kick");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("clip_") && n.ends_with(".mkick"))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no clip goldens in {}", dir.display());
        names
    }

    /// Trees, blend, stage inputs, stage probability and fires against every clip golden, upstream inputs
    /// (f69, net_p, prof32, low16, f15[:, 0], avail_hop) taken from the golden.
    #[test]
    fn stage_matches_every_clip_golden() {
        let m = model();
        let trees = Forest::from_container(&m, "trees.").unwrap();
        for name in clips() {
            let g = golden::load(&name);
            assert_eq!(g.text("train_id").unwrap(), m.text("train_id").unwrap(), "{name}: train_id");
            let k = g.shape("tree_p").unwrap()[0];
            let f69 = g.f64s("f69", &[k, 69]).unwrap();
            let f15 = g.f64s("f15", &[k, 15]).unwrap();
            let prof = g.f64s("prof32", &[k, 32]).unwrap();
            let low = g.f64s("low16", &[k, 16]).unwrap();
            let net_p = g.f64s("net_p", &[k]).unwrap();
            let avail = g.i64s("avail_hop", &[k]).unwrap();
            let emit_s = g.f64s("emit_s", &[k]).unwrap();
            let (sr, hop) = (g.i64s("sample_rate", &[]).unwrap()[0] as u64, g.i64s("hop", &[]).unwrap()[0] as u64);
            let (tree_p, blend_p) = (g.f64s("tree_p", &[k]).unwrap(), g.f64s("blend_p", &[k]).unwrap());
            let (stage_x, stage_p) = (g.f64s("stage_x", &[k, 5]).unwrap(), g.f64s("stage_p", &[k]).unwrap());
            let m_fires = g.shape("fires").unwrap()[0];
            let want_fires = g.i64s("fires", &[m_fires]).unwrap();
            assert!(avail.windows(2).all(|w| w[1] >= w[0]), "{name}: emission order is index order");

            let mut stage = Stage::new(&m).unwrap();
            let mut err = [0.0f64; 4];
            let mut fires = vec![];
            for i in 0..k {
                let tp = trees.predict_proba(&f69[i * 69..(i + 1) * 69]);
                err[0] = err[0].max((tp - tree_p[i]).abs());
                let mut desc = [0.0; DESCRIPTOR_LEN];
                desc[..32].copy_from_slice(&prof[i * 32..(i + 1) * 32]);
                desc[32..].copy_from_slice(&low[i * 16..(i + 1) * 16]);
                let a = avail[i] as u64;
                assert_eq!(((a + 1) * hop) as f64 / sr as f64, emit_s[i], "{name}: emit_s {i}");
                // Fed the golden tree_p: each comparison isolates one stage of the chain.
                let d = stage.decide(tree_p[i], net_p[i], &desc, f15[i * 15], a + 1, a);
                err[1] = err[1].max((d.blend_p - blend_p[i]).abs());
                for c in 0..5 {
                    err[2] = err[2].max((d.stage_x[c] - stage_x[i * 5 + c]).abs());
                }
                err[3] = err[3].max((d.stage_p - stage_p[i]).abs());
                if d.fire {
                    fires.push(i as i64);
                }
            }
            let near = |i: i64| (stage_p[i as usize] - stage.cutoff()).abs() < 1e-3;
            let mismatched: Vec<i64> = fires
                .iter()
                .filter(|i| !want_fires.contains(i))
                .chain(want_fires.iter().filter(|i| !fires.contains(i)))
                .copied()
                .collect();
            println!(
                "{name}: k {k} tree_p {:.2e} blend_p {:.2e} stage_x {:.2e} stage_p {:.2e} fires {}/{} mismatched {mismatched:?}",
                err[0],
                err[1],
                err[2],
                err[3],
                fires.len(),
                want_fires.len()
            );
            assert!(err[0] <= 1e-6 && err[1] <= 1e-6 && err[2] <= 1e-6 && err[3] <= 1e-4, "{name}: {err:?}");
            assert!(
                mismatched.is_empty() || (mismatched.len() == 1 && near(mismatched[0])),
                "{name}: fire mismatches {mismatched:?}"
            );
        }
    }
}
