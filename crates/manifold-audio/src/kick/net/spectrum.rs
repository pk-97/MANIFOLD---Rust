//! The net's 64 causal band envelopes on the 2 ms grid, in dB.
//! Reference: `kick_goal_nn.py` `spectrum()`.

use crate::kick::container::{Container, ContainerError};

pub const BANDS: usize = 64;
const SECTIONS: usize = 2;
/// Frames of history kept: the oldest frame a candidate reads is its pre-onset baseline,
/// so this bounds how late after `ready_sample` a candidate may still be scored (about 4 s).
pub const HISTORY: usize = 2048;

/// Streaming `spectrum()`: per band an order-4 Butterworth band-pass (scipy `sosfilt`,
/// direct form II transposed), rectified, averaged over the last `k` samples ending at
/// the grid sample itself, then `20 log10(mean + 1e-9)`.
pub struct Spectrum {
    // Structure of arrays over bands so the per-sample filter loop runs across bands.
    b: [[[f64; BANDS]; 3]; SECTIONS],
    a: [[[f64; BANDS]; 2]; SECTIONS],
    z: [[[f64; BANDS]; 2]; SECTIONS],
    k: [usize; BANDS],
    ring_start: [usize; BANDS],
    ring_pos: [usize; BANDS],
    /// Each band's last `k` rectified samples; zeros stand in for the time before the stream.
    rings: Vec<f64>,
    frame_hop: u64,
    samples: u64,
    frames: u64,
    history: Vec<f32>,
}

impl Spectrum {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let sos = c.f64s("net.sos", &[BANDS, SECTIONS, 6])?;
        let ks = c.i64s("net.k", &[BANDS])?;
        let frame_hop = c.i64s("net.frame_hop", &[])?[0];
        if frame_hop <= 0 || ks.iter().any(|&k| k <= 0) {
            return Err(ContainerError::WrongType { name: "net.k/net.frame_hop".into(), want: "positive lengths" });
        }
        let mut s = Self {
            b: [[[0.0; BANDS]; 3]; SECTIONS],
            a: [[[0.0; BANDS]; 2]; SECTIONS],
            z: [[[0.0; BANDS]; 2]; SECTIONS],
            k: [0; BANDS],
            ring_start: [0; BANDS],
            ring_pos: [0; BANDS],
            rings: Vec::new(),
            frame_hop: frame_hop as u64,
            samples: 0,
            frames: 0,
            history: vec![0.0; HISTORY * BANDS],
        };
        let mut start = 0;
        for band in 0..BANDS {
            for sec in 0..SECTIONS {
                let row = &sos[(band * SECTIONS + sec) * 6..][..6];
                s.b[sec][0][band] = row[0];
                s.b[sec][1][band] = row[1];
                s.b[sec][2][band] = row[2];
                s.a[sec][0][band] = row[4];
                s.a[sec][1][band] = row[5];
            }
            s.k[band] = ks[band] as usize;
            s.ring_start[band] = start;
            start += s.k[band];
        }
        s.rings = vec![0.0; start];
        Ok(s)
    }

    #[cfg(test)]
    /// Frames computed so far; frame `f` exists once sample `f * frame_hop` has been pushed.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn frame_hop(&self) -> u64 {
        self.frame_hop
    }

    /// Frame `f`'s 64 band levels in dB; `None` once it has left the history or before it exists.
    pub fn frame(&self, f: u64) -> Option<&[f32]> {
        if f >= self.frames || self.frames - f > HISTORY as u64 {
            return None;
        }
        let at = (f % HISTORY as u64) as usize * BANDS;
        Some(&self.history[at..at + BANDS])
    }

    pub fn push(&mut self, mut x: &[f64]) {
        while !x.is_empty() {
            // Run up to and including the next grid sample, where a frame is read.
            let grid = self.frames * self.frame_hop;
            let n = ((grid + 1 - self.samples) as usize).min(x.len());
            for &v in &x[..n] {
                self.step(v);
            }
            self.samples += n as u64;
            x = &x[n..];
            if self.samples == grid + 1 {
                self.emit_frame();
            }
        }
    }

    #[inline]
    fn step(&mut self, v: f64) {
        let mut cur = [v; BANDS];
        for sec in 0..SECTIONS {
            let [b0, b1, b2] = &self.b[sec];
            let [a1, a2] = &self.a[sec];
            let [z0, z1] = &mut self.z[sec];
            for i in 0..BANDS {
                let xc = cur[i];
                let y = b0[i] * xc + z0[i];
                z0[i] = b1[i] * xc - a1[i] * y + z1[i];
                z1[i] = b2[i] * xc - a2[i] * y;
                cur[i] = y;
            }
        }
        let slots = self.ring_pos.iter_mut().zip(&self.ring_start).zip(&self.k);
        for (y, ((pos, start), k)) in cur.iter().zip(slots) {
            self.rings[start + *pos] = y.abs();
            *pos = if *pos + 1 == *k { 0 } else { *pos + 1 };
        }
    }

    fn emit_frame(&mut self) {
        let at = (self.frames % HISTORY as u64) as usize * BANDS;
        for band in 0..BANDS {
            let ring = &self.rings[self.ring_start[band]..][..self.k[band]];
            let mean = ring_sum(ring) / self.k[band] as f64;
            self.history[at + band] = (20.0 * (mean + 1e-9).log10()) as f32;
        }
        self.frames += 1;
    }
}

/// The ring holds exactly the window, so its order does not matter; four lanes for speed.
fn ring_sum(r: &[f64]) -> f64 {
    let mut acc = [0.0f64; 4];
    let chunks = r.chunks_exact(4);
    let tail: f64 = chunks.remainder().iter().sum();
    for c in chunks {
        for j in 0..4 {
            acc[j] += c[j];
        }
    }
    (acc[0] + acc[1]) + (acc[2] + acc[3]) + tail
}
