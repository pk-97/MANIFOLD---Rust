//! The kick CNN's forward pass in f32. Reference: `kick_goal_nn.py` `Net`.
//!
//! Layout is PyTorch's (channel, band, frame). Every conv input lives in a buffer with a
//! one-cell zero border, so a 3x3 tap is a plain shifted row and the inner loop runs
//! contiguously over output frames (the compiler vectorises it).

use super::spectrum::BANDS;
use crate::kick::container::{Container, ContainerError};

const IN_CH: usize = 2;
const C1: usize = 16;
const C2: usize = 32;
const C3: usize = 32;
const HIDDEN: usize = 32;
const POOLED: usize = C3 * (BANDS / 4);

struct Conv {
    cin: usize,
    cout: usize,
    w: Vec<f32>,
    b: Vec<f32>,
}

impl Conv {
    fn new(c: &Container, name: &str, cin: usize, cout: usize) -> Result<Self, ContainerError> {
        Ok(Self {
            cin,
            cout,
            w: c.f32s(&format!("net.{name}.weight"), &[cout, cin, 3, 3])?.to_vec(),
            b: c.f32s(&format!("net.{name}.bias"), &[cout])?.to_vec(),
        })
    }

    /// `Conv2d(3, padding=1)` then ReLU: `inp` is (cin, h + 2, w + 2) with a zero border,
    /// `out` is (cout, h, w).
    fn forward(&self, inp: &[f32], h: usize, w: usize, out: &mut [f32]) {
        let (pw, ph) = (w + 2, h + 2);
        for oc in 0..self.cout {
            let plane = &mut out[oc * h * w..][..h * w];
            plane.fill(self.b[oc]);
            for ic in 0..self.cin {
                let k = &self.w[(oc * self.cin + ic) * 9..][..9];
                let src = &inp[ic * ph * pw..][..ph * pw];
                for y in 0..h {
                    let acc = &mut plane[y * w..][..w];
                    let r0 = &src[y * pw..][..pw];
                    let r1 = &src[(y + 1) * pw..][..pw];
                    let r2 = &src[(y + 2) * pw..][..pw];
                    tap_row(acc, r0, r1, r2, k);
                }
            }
            for v in plane.iter_mut() {
                *v = v.max(0.0);
            }
        }
    }
}

#[inline]
fn tap_row(acc: &mut [f32], r0: &[f32], r1: &[f32], r2: &[f32], k: &[f32]) {
    let w = acc.len();
    let (r00, r01, r02) = (&r0[..w], &r0[1..w + 1], &r0[2..w + 2]);
    let (r10, r11, r12) = (&r1[..w], &r1[1..w + 1], &r1[2..w + 2]);
    let (r20, r21, r22) = (&r2[..w], &r2[1..w + 1], &r2[2..w + 2]);
    for x in 0..w {
        let mut a = acc[x];
        a = k[0].mul_add(r00[x], a);
        a = k[1].mul_add(r01[x], a);
        a = k[2].mul_add(r02[x], a);
        a = k[3].mul_add(r10[x], a);
        a = k[4].mul_add(r11[x], a);
        a = k[5].mul_add(r12[x], a);
        a = k[6].mul_add(r20[x], a);
        a = k[7].mul_add(r21[x], a);
        acc[x] = k[8].mul_add(r22[x], a);
    }
}

/// `MaxPool2d(2)` (floor) from (c, h, w) into the interior of a zero-bordered (c, h/2 + 2, w/2 + 2).
fn pool_into(src: &[f32], c: usize, h: usize, w: usize, dst: &mut [f32]) {
    let (oh, ow) = (h / 2, w / 2);
    let pw = ow + 2;
    for ch in 0..c {
        let s = &src[ch * h * w..][..h * w];
        let d = &mut dst[ch * (oh + 2) * pw..][..(oh + 2) * pw];
        for y in 0..oh {
            let a = &s[2 * y * w..][..w];
            let b = &s[(2 * y + 1) * w..][..w];
            let row = &mut d[(y + 1) * pw + 1..][..ow];
            for (x, o) in row.iter_mut().enumerate() {
                *o = a[2 * x].max(a[2 * x + 1]).max(b[2 * x]).max(b[2 * x + 1]);
            }
        }
    }
}

pub struct Cnn {
    frames: usize,
    conv1: Conv,
    conv2: Conv,
    conv3: Conv,
    fc1_w: Vec<f32>,
    fc1_b: Vec<f32>,
    fc2_w: Vec<f32>,
    fc2_b: f32,
    /// The zero-bordered (IN_CH, BANDS + 2, frames + 2) input; callers fill the interior.
    pub input: Vec<f32>,
    out1: Vec<f32>,
    in2: Vec<f32>,
    out2: Vec<f32>,
    in3: Vec<f32>,
    out3: Vec<f32>,
    pooled: Vec<f32>,
    hidden: Vec<f32>,
}

impl Cnn {
    /// `frames` is the slice length; the head's width (one time bin) is checked by its weight shape.
    pub fn new(c: &Container, frames: usize) -> Result<Self, ContainerError> {
        let (w1, w2) = (frames / 2, frames / 4);
        Ok(Self {
            frames,
            conv1: Conv::new(c, "conv.0", IN_CH, C1)?,
            conv2: Conv::new(c, "conv.3", C1, C2)?,
            conv3: Conv::new(c, "conv.6", C2, C3)?,
            fc1_w: c.f32s("net.head.0.weight", &[HIDDEN, POOLED])?.to_vec(),
            fc1_b: c.f32s("net.head.0.bias", &[HIDDEN])?.to_vec(),
            fc2_w: c.f32s("net.head.2.weight", &[1, HIDDEN])?.to_vec(),
            fc2_b: c.f32s("net.head.2.bias", &[1])?[0],
            input: vec![0.0; IN_CH * (BANDS + 2) * (frames + 2)],
            out1: vec![0.0; C1 * BANDS * frames],
            in2: vec![0.0; C1 * (BANDS / 2 + 2) * (w1 + 2)],
            out2: vec![0.0; C2 * (BANDS / 2) * w1],
            in3: vec![0.0; C2 * (BANDS / 4 + 2) * (w2 + 2)],
            out3: vec![0.0; C3 * (BANDS / 4) * w2],
            pooled: vec![0.0; POOLED],
            hidden: vec![0.0; HIDDEN],
        })
    }

    /// Index of input cell (channel, band, frame) inside `input`.
    #[inline]
    pub fn input_at(&self, ch: usize, band: usize, frame: usize) -> usize {
        (ch * (BANDS + 2) + band + 1) * (self.frames + 2) + frame + 1
    }

    /// The net's logit for the filled `input`.
    pub fn logit(&mut self) -> f32 {
        let (h0, w0) = (BANDS, self.frames);
        self.conv1.forward(&self.input, h0, w0, &mut self.out1);
        pool_into(&self.out1, C1, h0, w0, &mut self.in2);
        let (h1, w1) = (h0 / 2, w0 / 2);
        self.conv2.forward(&self.in2, h1, w1, &mut self.out2);
        pool_into(&self.out2, C2, h1, w1, &mut self.in3);
        let (h2, w2) = (h1 / 2, w1 / 2);
        self.conv3.forward(&self.in3, h2, w2, &mut self.out3);
        // AdaptiveMaxPool2d((BANDS / 4, 1)): bands are already BANDS / 4, so each row's max over time.
        for (p, row) in self.pooled.iter_mut().zip(self.out3.chunks_exact(w2)) {
            *p = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        }
        for (j, hid) in self.hidden.iter_mut().enumerate() {
            let w = &self.fc1_w[j * POOLED..][..POOLED];
            let s: f32 = w.iter().zip(&self.pooled).map(|(a, b)| a * b).sum();
            *hid = (s + self.fc1_b[j]).max(0.0);
        }
        let s: f32 = self.fc2_w.iter().zip(&self.hidden).map(|(a, b)| a * b).sum();
        s + self.fc2_b
    }
}
