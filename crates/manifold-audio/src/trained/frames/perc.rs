//! Per-band percussive share (perc) on the net's 2 ms grid, read from the net's own band envelopes: held
//! tones (pads, bass, voice, tails) score low, broadband hits score near 0 dB.
//! Reference: `kick_goal_nn.py` `percussive`, in float32 like it: sustained = each band's median over the
//! last `sustain` frames (frames before the stream repeat frame 0), percussive = the median across
//! `neighbours` bands of the frame (edge bands repeated), share = 10 log10(P^2 / (P^2 + S^2 + 1e-18) + 1e-6).

use crate::trained::container::{Container, ContainerError};
use crate::trained::net::{BANDS, HISTORY, Spectrum};

/// The longest median window a model may ask for.
const MAX_WINDOW: usize = 64;

pub struct Percussive {
    sustain: usize,
    neighbours: usize,
    /// Frames computed so far.
    frames: u64,
    /// Each frame's linear band levels, for the sustained median.
    lin: Vec<f32>,
    history: Vec<f32>,
    /// Each band's last `sustain` linear levels in ascending order: the sustained median slides along with the
    /// frames instead of being re-selected.
    sustained: Vec<f32>,
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
            sustained: vec![0.0; BANDS * MAX_WINDOW],
        })
    }

    /// Computes every frame the spectrum has that this channel does not yet.
    pub fn update(&mut self, spec: &Spectrum) {
        while let Some(row) = spec.frame(self.frames) {
            let at = (self.frames % HISTORY as u64) as usize * BANDS;
            for (l, &db) in self.lin[at..at + BANDS].iter_mut().zip(row) {
                *l = 10f32.powf(db / 20.0);
            }
            // The median across neighbouring bands slides from band to band.
            let half = self.neighbours / 2;
            let level = |band: usize, offset: isize| {
                self.lin[at + (band as isize + offset).clamp(0, BANDS as isize - 1) as usize]
            };
            let mut window = [0.0f32; MAX_WINDOW];
            let window = &mut window[..self.neighbours];
            for (j, v) in window.iter_mut().enumerate() {
                *v = level(0, j as isize - half as isize);
            }
            window.sort_unstable_by(f32::total_cmp);
            let mut per = [0.0f32; BANDS];
            for (b, p) in per.iter_mut().enumerate() {
                if b > 0 {
                    slide(
                        window,
                        level(b, -1 - half as isize),
                        level(b, half as isize),
                    );
                }
                *p = window[half];
            }
            // The sustained median slides from frame to frame: frame f leaves the window that frame
            // f + sustain enters, and frames before the stream are frame 0.
            let leaving = self.frames.saturating_sub(self.sustain as u64);
            let old_at = (leaving % HISTORY as u64) as usize * BANDS;
            for (b, &p) in per.iter().enumerate() {
                let new = self.lin[at + b];
                let w = &mut self.sustained[b * MAX_WINDOW..][..self.sustain];
                if self.frames == 0 {
                    w.fill(new);
                } else {
                    slide(w, self.lin[old_at + b], new);
                }
                let s = w[self.sustain / 2];
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

/// Replaces one `old` in the ascending `window` with `new`, keeping it ascending.
fn slide(window: &mut [f32], old: f32, new: f32) {
    let i = window.partition_point(|v| v.total_cmp(&old).is_lt());
    debug_assert!(
        window[i].total_cmp(&old).is_eq(),
        "the leaving value is in the window"
    );
    let j = window.partition_point(|v| v.total_cmp(&new).is_lt());
    if j > i {
        window.copy_within(i + 1..j, i);
        window[j - 1] = new;
    } else {
        window.copy_within(j..i, j + 1);
        window[j] = new;
    }
}
