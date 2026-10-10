//! HistGradientBoosting inference for the base trees and the stage (reference: sklearn
//! `HistGradientBoostingClassifier.predict_proba`, `_predictor.pyx` `_predict_one_from_raw_data`).
//! Export layout: `tools/audio_analysis/kick_export/stage.py` `forest`.

use super::container::{Container, ContainerError};

#[derive(Debug, Clone, Copy)]
struct Node {
    threshold: f64,
    value: f64,
    feature: u32,
    left: u32,
    right: u32,
    missing_left: bool,
    is_leaf: bool,
}

/// A binary gradient-boosted forest on numeric features.
#[derive(Debug, Clone)]
pub struct Forest {
    nodes: Vec<Node>,
    roots: Vec<u32>,
    baseline: f64,
    n_features: usize,
}

fn bad(name: String, want: &'static str) -> ContainerError {
    ContainerError::WrongType { name, want }
}

impl Forest {
    /// Loads `<prefix>value`, `threshold`, `feature`, `left`, `right`, `missing_left`, `is_leaf`, `offsets`,
    /// `baseline` and `n_features`. Every child must sit after its parent inside its own tree, so a traversal
    /// always reaches a leaf.
    pub fn from_container(c: &Container, prefix: &str) -> Result<Self, ContainerError> {
        let key = |s: &str| format!("{prefix}{s}");
        let n = *c.shape(&key("value"))?.first().ok_or_else(|| bad(key("value"), "1-d node array"))?;
        let value = c.f64s(&key("value"), &[n])?;
        let threshold = c.f64s(&key("threshold"), &[n])?;
        let feature = c.i32s(&key("feature"), &[n])?;
        let left = c.i32s(&key("left"), &[n])?;
        let right = c.i32s(&key("right"), &[n])?;
        let missing_left = c.i32s(&key("missing_left"), &[n])?;
        let is_leaf = c.i32s(&key("is_leaf"), &[n])?;
        let t = *c.shape(&key("offsets"))?.first().ok_or_else(|| bad(key("offsets"), "1-d offsets"))?;
        let offsets = c.i64s(&key("offsets"), &[t])?;
        let baseline = c.f64s(&key("baseline"), &[])?[0];
        let n_features = c.i64s(&key("n_features"), &[])?[0];
        if n_features <= 0 {
            return Err(bad(key("n_features"), "positive feature count"));
        }
        let n_features = n_features as usize;
        if t < 2 || offsets[0] != 0 || offsets[t - 1] != n as i64 || offsets.windows(2).any(|w| w[1] <= w[0]) {
            return Err(bad(key("offsets"), "increasing tree offsets from 0 to the node count"));
        }
        let mut nodes = Vec::with_capacity(n);
        for w in offsets.windows(2) {
            let (start, end) = (w[0] as usize, w[1] as usize);
            for i in start..end {
                let leaf = match is_leaf[i] {
                    0 => false,
                    1 => true,
                    _ => return Err(bad(key("is_leaf"), "0 or 1")),
                };
                let miss = match missing_left[i] {
                    0 => false,
                    1 => true,
                    _ => return Err(bad(key("missing_left"), "0 or 1")),
                };
                let local = (i - start) as i64;
                let size = (end - start) as i64;
                let child = |v: i32, name: &str| {
                    let v = v as i64;
                    if leaf || (v > local && v < size) {
                        Ok(if leaf { 0 } else { (start as i64 + v) as u32 })
                    } else {
                        Err(bad(key(name), "child index after its parent, inside its tree"))
                    }
                };
                let (l, r) = (child(left[i], "left")?, child(right[i], "right")?);
                if !leaf && (feature[i] < 0 || feature[i] as usize >= n_features) {
                    return Err(bad(key("feature"), "feature index below n_features"));
                }
                nodes.push(Node {
                    threshold: threshold[i],
                    value: value[i],
                    feature: feature[i].max(0) as u32,
                    left: l,
                    right: r,
                    missing_left: miss,
                    is_leaf: leaf,
                });
            }
        }
        let roots = offsets[..t - 1].iter().map(|&o| o as u32).collect();
        Ok(Self { nodes, roots, baseline, n_features })
    }

    pub fn n_features(&self) -> usize {
        self.n_features
    }

    pub fn n_trees(&self) -> usize {
        self.roots.len()
    }

    /// Baseline plus every tree's leaf value, summed in tree order like sklearn's `_raw_predict`.
    pub fn raw(&self, x: &[f64]) -> f64 {
        assert_eq!(x.len(), self.n_features, "forest input width");
        let mut raw = self.baseline;
        for &root in &self.roots {
            raw += self.leaf(root as usize, x);
        }
        raw
    }

    /// The positive-class probability (`predict_proba(x)[:, 1]`).
    pub fn predict_proba(&self, x: &[f64]) -> f64 {
        expit(self.raw(x))
    }

    fn leaf(&self, mut i: usize, x: &[f64]) -> f64 {
        loop {
            let n = &self.nodes[i];
            if n.is_leaf {
                return n.value;
            }
            let v = x[n.feature as usize];
            let go_left = if v.is_nan() { n.missing_left } else { v <= n.threshold };
            i = if go_left { n.left } else { n.right } as usize;
        }
    }
}

/// scipy `expit` (xsf `log_exp.h`): bit-identical to scipy with the platform libm.
pub fn expit(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// scipy `logit` (xsf `log_exp.h`): the log1p form near 0.5 keeps precision there. Bit-identical to scipy
/// with the platform libm; the plain `ln(x / (1 - x))` differs in about a third of inputs.
pub fn logit(x: f64) -> f64 {
    if !(0.3..=0.65).contains(&x) {
        (x / (1.0 - x)).ln()
    } else {
        let s = 2.0 * (x - 0.5);
        s.ln_1p() - (-s).ln_1p()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A container from (name, dtype code, shape, raw bytes) entries, for hand-built models.
    pub(crate) fn container(entries: &[(String, u8, Vec<u64>, Vec<u8>)]) -> Container {
        let mut b = b"MKICK001".to_vec();
        b.extend((entries.len() as u32).to_le_bytes());
        for (name, code, shape, raw) in entries {
            b.extend((name.len() as u16).to_le_bytes());
            b.extend(name.as_bytes());
            b.extend([*code, shape.len() as u8]);
            for d in shape {
                b.extend(d.to_le_bytes());
            }
            b.extend(raw);
        }
        Container::parse(&b).unwrap()
    }

    pub(crate) fn f64e(name: &str, v: &[f64], scalar: bool) -> (String, u8, Vec<u64>, Vec<u8>) {
        let shape = if scalar { vec![] } else { vec![v.len() as u64] };
        (name.into(), 0, shape, v.iter().flat_map(|x| x.to_le_bytes()).collect())
    }

    pub(crate) fn i32e(name: &str, v: &[i32]) -> (String, u8, Vec<u64>, Vec<u8>) {
        (name.into(), 2, vec![v.len() as u64], v.iter().flat_map(|x| x.to_le_bytes()).collect())
    }

    pub(crate) fn i64e(name: &str, v: &[i64], scalar: bool) -> (String, u8, Vec<u64>, Vec<u8>) {
        let shape = if scalar { vec![] } else { vec![v.len() as u64] };
        (name.into(), 4, shape, v.iter().flat_map(|x| x.to_le_bytes()).collect())
    }

    /// Two trees on 2 features: tree 0 splits feature 0 at 0.5 (NaN right), tree 1 splits feature 1 at -1
    /// (NaN left), then feature 0 at 2.
    pub(crate) fn two_trees(prefix: &str, baseline: f64) -> Vec<(String, u8, Vec<u64>, Vec<u8>)> {
        let p = |s: &str| format!("{prefix}{s}");
        vec![
            f64e(&p("value"), &[0.0, -1.0, 1.0, 0.0, 0.25, 0.0, 0.5, 2.0], false),
            f64e(&p("threshold"), &[0.5, 0.0, 0.0, -1.0, 0.0, 2.0, 0.0, 0.0], false),
            i32e(&p("feature"), &[0, 0, 0, 1, 0, 0, 0, 0]),
            i32e(&p("left"), &[1, 0, 0, 1, 0, 3, 0, 0]),
            i32e(&p("right"), &[2, 0, 0, 2, 0, 4, 0, 0]),
            i32e(&p("missing_left"), &[0, 0, 0, 1, 0, 0, 0, 0]),
            i32e(&p("is_leaf"), &[0, 1, 1, 0, 1, 0, 1, 1]),
            i64e(&p("offsets"), &[0, 3, 8], false),
            f64e(&p("baseline"), &[baseline], true),
            i64e(&p("n_features"), &[2], true),
        ]
    }

    #[test]
    fn routes_threshold_ties_left_and_nan_by_flag() {
        let f = Forest::from_container(&container(&two_trees("t.", 0.125)), "t.").unwrap();
        assert_eq!((f.n_trees(), f.n_features()), (2, 2));
        // Tree 0: 0.5 <= 0.5 goes left (-1). Tree 1: 0 > -1 goes right, then 0.5 <= 2 left (0.5).
        assert_eq!(f.raw(&[0.5, 0.0]), 0.125 - 1.0 + 0.5);
        // NaN in feature 0 goes right in tree 0 (+1); NaN in feature 1 goes left in tree 1 (0.25).
        assert_eq!(f.raw(&[f64::NAN, f64::NAN]), 0.125 + 1.0 + 0.25);
        assert_eq!(f.raw(&[3.0, 5.0]), 0.125 + 1.0 + 2.0);
        assert_eq!(f.predict_proba(&[3.0, 5.0]), expit(3.125));
    }

    #[test]
    fn rejects_a_child_before_its_parent() {
        let mut e = two_trees("t.", 0.0);
        e[3] = i32e("t.left", &[1, 0, 0, 0, 0, 3, 0, 0]);
        assert!(Forest::from_container(&container(&e), "t.").is_err());
        let mut e = two_trees("t.", 0.0);
        e[2] = i32e("t.feature", &[0, 0, 0, 2, 0, 0, 0, 0]);
        assert!(Forest::from_container(&container(&e), "t.").is_err());
    }

    /// Values printed by scipy 1.17.1 `logit`/`expit`, both branches of the logit and both edges.
    #[test]
    fn logit_and_expit_match_scipy() {
        for (x, want) in [
            (0.1f64, -2.197224577336219),
            (0.3, -0.8472978603872037),
            (0.4, -0.4054651081081643),
            (0.65, 0.6190392084062235),
            (0.9, 2.1972245773362196),
            (0.5000001, 3.9999999978946295e-07),
        ] {
            assert_eq!(logit(x), want, "logit({x})");
        }
        assert_eq!(expit(1.0), 0.7310585786300049);
        assert_eq!(expit(-3.125), 0.042087727915618836);
    }
}
