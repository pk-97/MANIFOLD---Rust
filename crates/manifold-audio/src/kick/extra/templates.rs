//! Template features (3): best cosine similarity of a candidate's onset patch to the kick
//! templates, over all bands, bands under 300 Hz, and bands over 1 kHz.
//! Port of `patches` and `template_features` in `tools/audio_analysis/eval/kick_goal_templates.py`.

use super::spec::{BandSpec, BANDS};
use crate::kick::container::{Container, ContainerError};

/// The reference zeroes candidates before this hop (its base needs frames `c-5..c-2`).
pub(super) const MIN_CAND: u64 = 6;
/// Base frames: the mean of `c-5`, `c-4`, `c-3`.
pub(super) const BASE_FROM: u64 = 5;
pub(super) const BASE_LEN: u64 = 3;

/// One band selection: its mask and the templates restricted to it, already unit length.
struct Selection {
    mask: [bool; BANDS],
    /// `(k, span * selected bands)`, frame-major like the reference's reshape.
    templates: Vec<f64>,
    width: usize,
}

pub(super) struct Templates {
    span: u64,
    k: usize,
    eps: f64,
    sel: [Selection; 3],
    scratch: Vec<f64>,
}

fn mask(c: &Container, name: &str) -> Result<[bool; BANDS], ContainerError> {
    let m = c.i64s(name, &[BANDS])?;
    let mut out = [false; BANDS];
    for (o, &v) in out.iter_mut().zip(m) {
        *o = v != 0;
    }
    Ok(out)
}

impl Templates {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let span = super::positive(c, "extra.patch_span")?;
        let k = match c.shape("extra.templates_all")? {
            [k, _] => *k,
            got => {
                return Err(ContainerError::WrongShape { name: "extra.templates_all".into(), want: vec![0, 0], got: got.to_vec() });
            }
        };
        let all = [true; BANDS];
        let low = mask(c, "extra.band_low")?;
        let high = mask(c, "extra.band_high")?;
        let selection = |name: &str, mask: [bool; BANDS]| -> Result<Selection, ContainerError> {
            let width = span as usize * mask.iter().filter(|&&m| m).count();
            Ok(Selection { mask, templates: c.f64s(name, &[k, width])?.to_vec(), width })
        };
        Ok(Self {
            span,
            k,
            eps: super::scalar(c, "extra.unit_eps")?,
            sel: [selection("extra.templates_all", all)?, selection("extra.templates_low", low)?, selection("extra.templates_high", high)?],
            scratch: vec![0.0; span as usize * BANDS],
        })
    }

    /// The patch reads frames `cand .. cand + span`, which can run past the evidence deadline.
    pub fn last_frame(&self, cand: u64) -> u64 {
        cand + self.span - 1
    }

    /// `[tmpl_sim, tmpl_sim_low, tmpl_sim_high]`.
    pub fn features(&mut self, spec: &BandSpec, cand: u64, out: &mut [f64]) {
        out.fill(0.0);
        if cand < MIN_CAND {
            return;
        }
        assert!(spec.holds(cand - BASE_FROM, self.last_frame(cand)), "kick templates: candidate {cand} read outside the stream history");
        let mut base = [0.0; BANDS];
        for (b, v) in base.iter_mut().enumerate() {
            let f = |d: u64| spec.frame_at(cand - BASE_FROM + d)[b];
            *v = (f(0) + f(1) + f(2)) / BASE_LEN as f64;
        }
        for (o, sel) in out.iter_mut().zip(&self.sel) {
            let p = &mut self.scratch[..sel.width];
            let mut n = 0;
            for f in 0..self.span {
                let frame = spec.frame_at(cand + f);
                for b in 0..BANDS {
                    if sel.mask[b] {
                        p[n] = (frame[b] - base[b]).max(0.0);
                        n += 1;
                    }
                }
            }
            let norm = p.iter().map(|v| v * v).sum::<f64>().sqrt() + self.eps;
            for v in p.iter_mut() {
                *v /= norm;
            }
            *o = sel
                .templates
                .chunks_exact(sel.width)
                .take(self.k)
                .map(|t| t.iter().zip(p.iter()).map(|(a, b)| a * b).sum::<f64>())
                .fold(f64::NEG_INFINITY, f64::max);
        }
    }
}
