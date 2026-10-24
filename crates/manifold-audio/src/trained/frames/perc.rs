//! Per-band percussive share (perc) on the net's 2 ms grid, read from the net's own band envelopes: held
//! tones (pads, bass, voice, tails) score low, broadband hits score near 0 dB.
//! Reference: `kick_goal_nn.py` `percussive`, in float32 like it: sustained = each band's median over the
//! last `sustain` frames (frames before the stream repeat frame 0), percussive = the median across
//! `neighbours` bands of the frame (edge bands repeated), share = 10 log10(P^2 / (P^2 + S^2 + 1e-18) + 1e-6).

use crate::trained::container::{Container, ContainerError};
use crate::trained::net::{BANDS, HISTORY, Spectrum};

/// Medians are taken on the stack.
const MAX_WINDOW: usize = 64;

pub struct Percussive {
    sustain: usize,
    neighbours: usize,
    /// Frames computed so far.
    frames: u64,
    /// Each frame's linear band levels, for the sustained median.
    lin: Vec<f32>,
    history: Vec<f32>,
}

impl Percussive {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let get = |name: &str| -> Result<usize, ContainerError> {
            match c.i64s(name, &[])?[0] {
                v if v > 0 && v as usize <= MAX_WINDOW && v % 2 == 1 => Ok(v as usize),
                _ => Err(ContainerError::WrongType {
                    name: name.into(),
                    want: "an odd window of at most 64",
                }),
            }
        };
        Ok(Self {
            sustain: get("perc.sustain")?,
            neighbours: get("perc.neighbours")?,
            frames: 0,
            lin: vec![0.0; HISTORY * BANDS],
            history: vec![0.0; HISTORY * BANDS],
        })
    }

    /// Computes every frame the spectrum has that this channel does not yet.
    pub fn update(&mut self, spec: &Spectrum) {
        while let Some(row) = spec.frame(self.frames) {
            let at = (self.frames % HISTORY as u64) as usize * BANDS;
            for (l, &db) in self.lin[at..at + BANDS].iter_mut().zip(row) {
                *l = 10f32.powf(db / 20.0);
            }
            let mut med = [0.0f32; MAX_WINDOW];
            let mut per = [0.0f32; BANDS];
            let half = self.neighbours / 2;
            for (b, p) in per.iter_mut().enumerate() {
                let w = &mut med[..self.neighbours];
                for (j, m) in w.iter_mut().enumerate() {
                    let nb = (b + j).saturating_sub(half).min(BANDS - 1);
                    *m = self.lin[at + nb];
                }
                *p = *w.select_nth_unstable_by(half, f32::total_cmp).1;
            }
            for (b, &p) in per.iter().enumerate() {
                let w = &mut med[..self.sustain];
                for (j, m) in w.iter_mut().enumerate() {
                    let f = (self.frames + j as u64).saturating_sub(self.sustain as u64 - 1);
                    *m = self.lin[(f % HISTORY as u64) as usize * BANDS + b];
                }
                let s = *w.select_nth_unstable_by(self.sustain / 2, f32::total_cmp).1;
                let pp = p * p;
                self.history[at + b] = 10.0 * (pp / (pp + s * s + 1e-18) + 1e-6).log10();
            }
            self.frames += 1;
        }
    }

    /// Frame `f`'s 64 values; `None` once it has left the history or before it exists.
    pub fn frame(&self, f: u64) -> Option<&[f32]> {
        if f >= self.frames || self.frames - f > HISTORY as u64 {
            return None;
        }
        let at = (f % HISTORY as u64) as usize * BANDS;
        Some(&self.history[at..at + BANDS])
    }
}
