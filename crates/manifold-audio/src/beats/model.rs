//! The Beat This! network in f32: a convolutional stem, three frontend blocks that attend across frequency and then
//! across time before halving the bands, six transformer layers over time, and a head giving a beat and a downbeat
//! logit per frame. Reference: `beat_this/model/beat_tracker.py` and `roformer.py`; the weights come from
//! `tools/audio_analysis/beat_this_export.py`.
//!
//! Activations are row-major with channels last. Frontend tensors are (band, frame, channel), so attention across
//! time reads contiguous sequences, and attention across frequency runs on the (frame, band, channel) transpose.

use super::mel::BANDS;
use crate::trained::container::{Container, ContainerError, Data};

const HEAD_DIM: usize = 32;
const STEM_DIM: usize = 32;
const DIM: usize = 512;
const LAYERS: usize = 6;
const FRONTEND_BLOCKS: usize = 3;
const FF_MULT: usize = 4;
const BN_EPS: f32 = 1e-5;
/// Feed-forward rows processed at once, which bounds the hidden buffer.
const FF_TILE: usize = 4096;

/// c = a·b + beta·c, each operand given with its (row stride, column stride).
#[allow(clippy::too_many_arguments)]
fn gemm(
    m: usize,
    k: usize,
    n: usize,
    a: &[f32],
    sa: (usize, usize),
    b: &[f32],
    sb: (usize, usize),
    beta: f32,
    c: &mut [f32],
    sc: (usize, usize),
) {
    if m == 0 || n == 0 {
        return;
    }
    let last = |rows: usize, cols: usize, s: (usize, usize)| (rows - 1) * s.0 + (cols - 1) * s.1;
    assert!(k == 0 || (last(m, k, sa) < a.len() && last(k, n, sb) < b.len()));
    assert!(last(m, n, sc) < c.len());
    // SAFETY: the asserts keep every strided element inside its slice, and `c` is a unique borrow, so it aliases
    // neither input.
    unsafe {
        matrixmultiply::sgemm(
            m,
            k,
            n,
            1.0,
            a.as_ptr(),
            sa.0 as isize,
            sa.1 as isize,
            b.as_ptr(),
            sb.0 as isize,
            sb.1 as isize,
            beta,
            c.as_mut_ptr(),
            sc.0 as isize,
            sc.1 as isize,
        );
    }
}

fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + libm::erff(x * std::f32::consts::FRAC_1_SQRT_2))
}

/// e^x for x <= 0 in a branch-free form the compiler vectorises: 2^n by exponent bits times a degree-6 polynomial
/// for e^r, |r| <= ln2/2, accurate to about one f32 ulp. Softmax spends most of its time here.
fn exp(x: f32) -> f32 {
    let x = x.max(-87.0);
    let n = (x * std::f32::consts::LOG2_E).round();
    let r = x - n * std::f32::consts::LN_2;
    let p = 1.0
        + r * (1.0
            + r * (0.5
                + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    p * f32::from_bits(((n as i32 + 127) as u32) << 23)
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// `x` (a, b, c) as (b, a, c).
fn swap_outer(x: &[f32], a: usize, b: usize, c: usize) -> Vec<f32> {
    let mut y = vec![0.0; x.len()];
    for i in 0..a {
        for j in 0..b {
            y[(j * a + i) * c..][..c].copy_from_slice(&x[(i * b + j) * c..][..c]);
        }
    }
    y
}

/// Moves an f32 entry out of the container after checking its shape.
fn take(c: &mut Container, name: &str, shape: &[usize]) -> Result<Vec<f32>, ContainerError> {
    c.f32s(name, shape)?;
    match c.remove(name).map(|a| a.data) {
        Some(Data::F32(v)) => Ok(v),
        _ => unreachable!("f32s checked the entry"),
    }
}

/// y = x·Wᵀ (+ b), with W in PyTorch's (out, in) layout.
struct Linear {
    w: Vec<f32>,
    b: Option<Vec<f32>>,
    inp: usize,
    out: usize,
}

impl Linear {
    fn load(
        c: &mut Container,
        name: &str,
        inp: usize,
        out: usize,
        bias: bool,
    ) -> Result<Self, ContainerError> {
        Ok(Self {
            w: take(c, &format!("{name}.weight"), &[out, inp])?,
            b: bias
                .then(|| take(c, &format!("{name}.bias"), &[out]))
                .transpose()?,
            inp,
            out,
        })
    }

    fn apply(&self, x: &[f32], rows: usize, y: &mut [f32]) {
        gemm(
            rows,
            self.inp,
            self.out,
            x,
            (self.inp, 1),
            &self.w,
            (1, self.inp),
            0.0,
            y,
            (self.out, 1),
        );
        if let Some(b) = &self.b {
            for row in y[..rows * self.out].chunks_exact_mut(self.out) {
                row.iter_mut().zip(b).for_each(|(v, b)| *v += b);
            }
        }
    }
}

/// RMSNorm: x / max(|x|, 1e-12) · sqrt(dim) · gamma, with sqrt(dim) folded into the gain.
struct Norm {
    g: Vec<f32>,
}

impl Norm {
    fn load(c: &mut Container, name: &str, dim: usize) -> Result<Self, ContainerError> {
        let scale = (dim as f32).sqrt();
        let mut g = take(c, &format!("{name}.gamma"), &[dim])?;
        g.iter_mut().for_each(|v| *v *= scale);
        Ok(Self { g })
    }

    fn apply(&self, x: &[f32], y: &mut [f32]) {
        let d = self.g.len();
        for (xr, yr) in x.chunks_exact(d).zip(y.chunks_exact_mut(d)) {
            let inv = 1.0 / xr.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
            for ((y, x), g) in yr.iter_mut().zip(xr).zip(&self.g) {
                *y = x * inv * g;
            }
        }
    }
}

/// Rotary position embedding over interleaved pairs (x[2i], x[2i+1]), shared by every attention layer.
struct Rope {
    freqs: Vec<f32>,
}

impl Rope {
    /// Rotates the query and key parts of each `qkv` row by its position within its sequence of `n`.
    fn rotate(&self, qkv: &mut [f32], n: usize, dim: usize) {
        let pairs = self.freqs.len();
        let mut table = Vec::with_capacity(n * pairs);
        for p in 0..n {
            for f in &self.freqs {
                let angle = p as f32 * f;
                table.push((angle.cos(), angle.sin()));
            }
        }
        for (r, row) in qkv.chunks_exact_mut(3 * dim).enumerate() {
            let rot = &table[(r % n) * pairs..][..pairs];
            for head in row[..2 * dim].chunks_exact_mut(HEAD_DIM) {
                for (pair, &(cos, sin)) in head.chunks_exact_mut(2).zip(rot) {
                    let (x0, x1) = (pair[0], pair[1]);
                    pair[0] = x0 * cos - x1 * sin;
                    pair[1] = x1 * cos + x0 * sin;
                }
            }
        }
    }
}

/// Gated multi-head self-attention with a pre-norm, added onto its input.
struct Attention {
    norm: Norm,
    qkv: Linear,
    gates: Linear,
    out: Linear,
    dim: usize,
    heads: usize,
}

impl Attention {
    fn load(c: &mut Container, name: &str, dim: usize) -> Result<Self, ContainerError> {
        let heads = dim / HEAD_DIM;
        Ok(Self {
            norm: Norm::load(c, &format!("{name}.norm"), dim)?,
            qkv: Linear::load(c, &format!("{name}.to_qkv"), dim, 3 * dim, false)?,
            gates: Linear::load(c, &format!("{name}.to_gates"), dim, heads, true)?,
            out: Linear::load(c, &format!("{name}.to_out.0"), dim, dim, false)?,
            dim,
            heads,
        })
    }

    /// x += attention(x), over `seqs` independent sequences of `n` rows each.
    fn add_to(&self, x: &mut [f32], seqs: usize, n: usize, rope: &Rope) {
        let (d, h) = (self.dim, self.heads);
        let rows = seqs * n;
        let mut xn = vec![0.0; rows * d];
        self.norm.apply(x, &mut xn);
        let mut qkv = vec![0.0; rows * 3 * d];
        self.qkv.apply(&xn, rows, &mut qkv);
        let mut gates = vec![0.0; rows * h];
        self.gates.apply(&xn, rows, &mut gates);
        rope.rotate(&mut qkv, n, d);

        let scale = (HEAD_DIM as f32).powf(-0.5);
        let mut att = vec![0.0; rows * d];
        let mut scores = vec![0.0; n * n];
        for s in 0..seqs {
            let row0 = s * n;
            let seq = &qkv[row0 * 3 * d..];
            for head in 0..h {
                let col = head * HEAD_DIM;
                gemm(
                    n,
                    HEAD_DIM,
                    n,
                    &seq[col..],
                    (3 * d, 1),
                    &seq[d + col..],
                    (1, 3 * d),
                    0.0,
                    &mut scores,
                    (n, 1),
                );
                for row in scores.chunks_exact_mut(n) {
                    let max = row.iter().fold(f32::NEG_INFINITY, |m, &v| m.max(v)) * scale;
                    row.iter_mut().for_each(|v| *v = exp(*v * scale - max));
                    let sum: f32 = row.iter().sum();
                    let inv = 1.0 / sum;
                    row.iter_mut().for_each(|v| *v *= inv);
                }
                let out = &mut att[row0 * d + col..];
                gemm(
                    n,
                    n,
                    HEAD_DIM,
                    &scores,
                    (n, 1),
                    &seq[2 * d + col..],
                    (3 * d, 1),
                    0.0,
                    out,
                    (d, 1),
                );
                for i in 0..n {
                    let g = sigmoid(gates[(row0 + i) * h + head]);
                    out[i * d..][..HEAD_DIM].iter_mut().for_each(|v| *v *= g);
                }
            }
        }
        self.out.apply(&att, rows, &mut xn);
        x.iter_mut().zip(&xn).for_each(|(x, y)| *x += y);
    }
}

/// Pre-norm feed-forward (dim → 4·dim → dim, GELU), added onto its input.
struct FeedForward {
    norm: Norm,
    up: Linear,
    down: Linear,
}

impl FeedForward {
    fn load(c: &mut Container, name: &str, dim: usize) -> Result<Self, ContainerError> {
        Ok(Self {
            norm: Norm::load(c, &format!("{name}.net.0"), dim)?,
            up: Linear::load(c, &format!("{name}.net.1"), dim, FF_MULT * dim, true)?,
            down: Linear::load(c, &format!("{name}.net.4"), FF_MULT * dim, dim, true)?,
        })
    }

    fn add_to(&self, x: &mut [f32], rows: usize) {
        let (d, inner) = (self.up.inp, self.up.out);
        let tile = FF_TILE.min(rows);
        let mut xn = vec![0.0; tile * d];
        let mut hidden = vec![0.0; tile * inner];
        let mut y = vec![0.0; tile * d];
        for start in (0..rows).step_by(tile) {
            let r = tile.min(rows - start);
            let xs = &mut x[start * d..(start + r) * d];
            self.norm.apply(xs, &mut xn[..r * d]);
            self.up.apply(&xn[..r * d], r, &mut hidden[..r * inner]);
            hidden[..r * inner].iter_mut().for_each(|v| *v = gelu(*v));
            self.down.apply(&hidden[..r * inner], r, &mut y[..r * d]);
            xs.iter_mut().zip(&y[..r * d]).for_each(|(x, y)| *x += y);
        }
    }
}

/// Per-channel BatchNorm in eval mode as y = x·scale + shift.
fn batch_norm(
    c: &mut Container,
    name: &str,
    ch: usize,
) -> Result<(Vec<f32>, Vec<f32>), ContainerError> {
    let w = take(c, &format!("{name}.weight"), &[ch])?;
    let b = take(c, &format!("{name}.bias"), &[ch])?;
    let mean = take(c, &format!("{name}.running_mean"), &[ch])?;
    let var = take(c, &format!("{name}.running_var"), &[ch])?;
    let scale: Vec<f32> = w
        .iter()
        .zip(&var)
        .map(|(w, v)| w / (v + BN_EPS).sqrt())
        .collect();
    let shift = b
        .iter()
        .zip(&mean)
        .zip(&scale)
        .map(|((b, m), s)| b - m * s)
        .collect();
    Ok((scale, shift))
}

/// Conv2d with kernel (kf, 3), stride (kf, 1), one frame of zero padding in time and no bias, then BatchNorm (folded
/// into the weights) and GELU. Input and output are (band, frame, channel).
struct Conv {
    kf: usize,
    cin: usize,
    cout: usize,
    /// [kf][kt][cin][cout]: each tap is a (cin, cout) matrix.
    w: Vec<f32>,
    shift: Vec<f32>,
}

impl Conv {
    fn load(
        c: &mut Container,
        conv: &str,
        norm: &str,
        kf: usize,
        cin: usize,
        cout: usize,
    ) -> Result<Self, ContainerError> {
        let raw = take(c, &format!("{conv}.weight"), &[cout, cin, kf, 3])?;
        let (scale, shift) = batch_norm(c, norm, cout)?;
        let mut w = vec![0.0; raw.len()];
        for co in 0..cout {
            for ci in 0..cin {
                for f in 0..kf {
                    for t in 0..3 {
                        w[((f * 3 + t) * cin + ci) * cout + co] =
                            raw[((co * cin + ci) * kf + f) * 3 + t] * scale[co];
                    }
                }
            }
        }
        Ok(Self {
            kf,
            cin,
            cout,
            w,
            shift,
        })
    }

    fn apply(&self, x: &[f32], bands: usize, frames: usize) -> Vec<f32> {
        let (cin, cout) = (self.cin, self.cout);
        let out_bands = bands / self.kf;
        let mut out: Vec<f32> = self
            .shift
            .iter()
            .copied()
            .cycle()
            .take(out_bands * frames * cout)
            .collect();
        for fo in 0..out_bands {
            for f in 0..self.kf {
                let fi = fo * self.kf + f;
                for kt in 0..3 {
                    // Tap kt reads frame t + kt - 1; the padded frame contributes nothing.
                    let (m, from, to) = match kt {
                        0 => (frames - 1, 0, 1),
                        1 => (frames, 0, 0),
                        _ => (frames - 1, 1, 0),
                    };
                    let w = &self.w[(f * 3 + kt) * cin * cout..][..cin * cout];
                    let a = &x[(fi * frames + from) * cin..];
                    let c = &mut out[(fo * frames + to) * cout..];
                    gemm(m, cin, cout, a, (cin, 1), w, (cout, 1), 1.0, c, (cout, 1));
                }
            }
        }
        out.iter_mut().for_each(|v| *v = gelu(*v));
        out
    }
}

/// Attention and feed-forward across frequency, then across time, then the band-halving conv.
struct FrontendBlock {
    attn_f: Attention,
    ff_f: FeedForward,
    attn_t: Attention,
    ff_t: FeedForward,
    conv: Conv,
}

pub(super) struct Model {
    bn_scale: Vec<f32>,
    bn_shift: Vec<f32>,
    stem: Conv,
    blocks: Vec<FrontendBlock>,
    /// Frontend output to model width, its input columns reordered from PyTorch's (channel, band) to (band, channel).
    linear: Linear,
    layers: Vec<(Attention, FeedForward)>,
    norm: Norm,
    head: Linear,
    rope: Rope,
}

impl Model {
    /// The shipped `final0` weights.
    pub(super) fn load() -> Result<Self, ContainerError> {
        let mut c = Container::parse(include_bytes!("../../assets/beat_this.mkick"))?;
        let c = &mut c;
        let (bn_scale, bn_shift) = batch_norm(c, "frontend.stem.bn1d", BANDS)?;
        let stem = Conv::load(
            c,
            "frontend.stem.conv2d",
            "frontend.stem.bn2d",
            4,
            1,
            STEM_DIM,
        )?;
        let mut blocks = Vec::with_capacity(FRONTEND_BLOCKS);
        let mut dim = STEM_DIM;
        for i in 0..FRONTEND_BLOCKS {
            let p = format!("frontend.blocks.{i}");
            blocks.push(FrontendBlock {
                attn_f: Attention::load(c, &format!("{p}.partial.attnF"), dim)?,
                ff_f: FeedForward::load(c, &format!("{p}.partial.ffF"), dim)?,
                attn_t: Attention::load(c, &format!("{p}.partial.attnT"), dim)?,
                ff_t: FeedForward::load(c, &format!("{p}.partial.ffT"), dim)?,
                conv: Conv::load(
                    c,
                    &format!("{p}.conv2d"),
                    &format!("{p}.norm"),
                    2,
                    dim,
                    2 * dim,
                )?,
            });
            dim *= 2;
        }
        let bands = (BANDS / 4) >> FRONTEND_BLOCKS;
        let mut linear = Linear::load(c, "frontend.linear", dim * bands, DIM, true)?;
        let torch_order = std::mem::take(&mut linear.w);
        linear.w = vec![0.0; torch_order.len()];
        for o in 0..DIM {
            for b in 0..bands {
                for ch in 0..dim {
                    linear.w[o * dim * bands + b * dim + ch] =
                        torch_order[o * dim * bands + ch * bands + b];
                }
            }
        }
        let mut layers = Vec::with_capacity(LAYERS);
        for l in 0..LAYERS {
            let p = format!("transformer_blocks.layers.{l}");
            layers.push((
                Attention::load(c, &format!("{p}.0"), DIM)?,
                FeedForward::load(c, &format!("{p}.1"), DIM)?,
            ));
        }
        Ok(Self {
            bn_scale,
            bn_shift,
            stem,
            blocks,
            linear,
            layers,
            norm: Norm::load(c, "transformer_blocks.norm", DIM)?,
            head: Linear::load(c, "task_heads.beat_downbeat_lin", DIM, 2, true)?,
            rope: Rope {
                freqs: take(c, "rotary.freqs", &[HEAD_DIM / 2])?,
            },
        })
    }

    /// Beat and downbeat logits for `mel` (frames × BANDS). The beat logit includes the downbeat one, as the
    /// network's sum head defines it.
    pub(super) fn logits(&self, mel: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let frames = mel.len() / BANDS;
        let mut x = vec![0.0; BANDS * frames];
        for t in 0..frames {
            for b in 0..BANDS {
                x[b * frames + t] = mel[t * BANDS + b] * self.bn_scale[b] + self.bn_shift[b];
            }
        }
        let mut x = self.stem.apply(&x, BANDS, frames);
        let (mut bands, mut ch) = (BANDS / 4, STEM_DIM);
        for block in &self.blocks {
            let mut by_frame = swap_outer(&x, bands, frames, ch);
            block
                .attn_f
                .add_to(&mut by_frame, frames, bands, &self.rope);
            block.ff_f.add_to(&mut by_frame, frames * bands);
            x = swap_outer(&by_frame, frames, bands, ch);
            block.attn_t.add_to(&mut x, bands, frames, &self.rope);
            block.ff_t.add_to(&mut x, bands * frames);
            x = block.conv.apply(&x, bands, frames);
            bands /= 2;
            ch *= 2;
        }
        let by_frame = swap_outer(&x, bands, frames, ch);
        let mut h = vec![0.0; frames * DIM];
        self.linear.apply(&by_frame, frames, &mut h);
        for (attn, ff) in &self.layers {
            attn.add_to(&mut h, 1, frames, &self.rope);
            ff.add_to(&mut h, frames);
        }
        let mut normed = vec![0.0; frames * DIM];
        self.norm.apply(&h, &mut normed);
        let mut out = vec![0.0; frames * 2];
        self.head.apply(&normed, frames, &mut out);
        out.chunks_exact(2).map(|o| (o[0] + o[1], o[1])).unzip()
    }
}
