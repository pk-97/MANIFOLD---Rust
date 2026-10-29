//! A song's tempo and bar 1, for Detect on an audio clip. Beat This! (Foscarin, Schlüter and Widmer, ISMIR 2024; MIT
//! code and weights) gives a beat and a downbeat activation every 20 ms. One straight line through the picked beats
//! gives the beat period and phase, which is exact for music on a fixed grid; the downbeat activation summed per beat
//! of the bar then picks which beat is bar 1.

mod mel;
mod model;
mod resample;
#[cfg(test)]
mod tests;

use mel::{BANDS, FPS, RATE};

/// Frames the network sees at once (30 s), its training length.
const CHUNK: usize = 1500;
/// Frames dropped at each chunk edge, which the network was not trained to get right. Neighbouring chunks overlap
/// by twice this.
const BORDER: usize = 6;
/// Chunks run on this many threads at most, two cores fewer than the machine has so the content and UI threads keep
/// theirs; each worker holds about 50 MB of activations.
const MAX_WORKERS: usize = 8;
const MIN_BEATS: usize = 16;
/// A picked beat within this of the fitted line is on the grid.
const ON_GRID_S: f64 = 0.04;
/// Share of picked beats that must sit on one grid for the song to have a steady tempo. Intros and breakdowns
/// without drums give scattered beats, and scattered beats land on a grid only by chance (about one in five).
const STEADY_SHARE: f64 = 0.6;
/// The fit starts from the run of this many beats whose gaps vary least.
const START_BEATS: usize = 16;
/// Beat This!'s peak window: a beat is the highest activation within ±70 ms.
const PEAK_RADIUS: usize = 3;

/// Beat and downbeat logits per frame.
type Logits = (Vec<f32>, Vec<f32>);

/// A steady beat grid in source seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Seconds per beat.
    pub period_s: f64,
    /// One downbeat; the others are whole bars away.
    pub downbeat_s: f64,
    pub beats_per_bar: u32,
}

impl Grid {
    pub fn bpm(&self) -> f64 {
        60.0 / self.period_s
    }

    /// The first downbeat at or after `t` seconds.
    pub fn downbeat_at_or_after(&self, t: f64) -> f64 {
        let bar = self.period_s * f64::from(self.beats_per_bar);
        self.downbeat_s + ((t - self.downbeat_s) / bar).ceil() * bar
    }
}

/// Why a song has no grid.
#[derive(Debug, Clone, PartialEq)]
pub enum NoGrid {
    TooShort,
    FewBeats(usize),
    Unsteady { on_grid: f64 },
}

impl std::fmt::Display for NoGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "too short to find beats"),
            Self::FewBeats(n) => write!(f, "only {n} beats found"),
            Self::Unsteady { on_grid } => {
                write!(
                    f,
                    "no steady tempo ({:.0}% of beats on one grid)",
                    on_grid * 100.0
                )
            }
        }
    }
}

/// The steady grid of `mono` (`rate` Hz), with bars of `beats_per_bar` beats.
pub fn song_grid(mono: &[f32], rate: u32, beats_per_bar: u32) -> Result<Grid, NoGrid> {
    let signal = resample::resample(mono, rate, RATE);
    let mel = mel::log_mel(&signal);
    if mel.len() / BANDS <= 2 * BORDER {
        return Err(NoGrid::TooShort);
    }
    let (beat, downbeat) = activations(&mel);
    fit(&beat, &downbeat, beats_per_bar.max(1))
}

/// Chunk starts as Beat This!'s `split_piece` makes them: the first starts BORDER frames before the song, each
/// later one CHUNK - 2·BORDER on, and the last ends at the song's end.
fn chunk_starts(frames: usize) -> Vec<isize> {
    let (step, border) = (CHUNK - 2 * BORDER, BORDER as isize);
    let mut starts: Vec<isize> = (0..)
        .map(|i| (i * step) as isize - border)
        .take_while(|&s| s < frames as isize - border)
        .collect();
    if frames > step {
        *starts.last_mut().expect("a song has a chunk") =
            frames as isize - (CHUNK - BORDER) as isize;
    }
    starts
}

/// Beat and downbeat logits per frame over the whole song. Each chunk's edges are dropped, and where chunks overlap
/// the earlier one wins.
fn activations(mel: &[f32]) -> Logits {
    let frames = mel.len() / BANDS;
    let model = model::Model::load().expect("the shipped Beat This! weights parse");
    let starts = chunk_starts(frames);
    let run = |s: isize| {
        let (from, to) = (
            s.max(0) as usize,
            ((s + CHUNK as isize) as usize).min(frames),
        );
        let left = (-s).max(0) as usize;
        let right = (s + CHUNK as isize - frames as isize).clamp(0, BORDER as isize) as usize;
        let mut chunk = vec![0.0; (left + (to - from) + right) * BANDS];
        chunk[left * BANDS..][..(to - from) * BANDS]
            .copy_from_slice(&mel[from * BANDS..to * BANDS]);
        model.logits(&chunk)
    };
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get().saturating_sub(2).max(1))
        .min(MAX_WORKERS)
        .min(starts.len());
    let mut outs: Vec<(usize, Logits)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let (starts, run) = (&starts, &run);
                scope.spawn(move || {
                    (w..starts.len())
                        .step_by(workers)
                        .map(|i| (i, run(starts[i])))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a Beat This! chunk worker panicked"))
            .collect()
    });
    outs.sort_by_key(|(i, _)| *i);

    let mut beat = vec![f32::NAN; frames];
    let mut downbeat = vec![f32::NAN; frames];
    for (i, (b, d)) in outs.iter().rev() {
        let at = starts[*i] + BORDER as isize;
        for (j, (b, d)) in b[BORDER..b.len() - BORDER]
            .iter()
            .zip(&d[BORDER..d.len() - BORDER])
            .enumerate()
        {
            let t = at + j as isize;
            if (0..frames as isize).contains(&t) {
                beat[t as usize] = *b;
                downbeat[t as usize] = *d;
            }
        }
    }
    (beat, downbeat)
}

/// Beat times: frames whose logit is positive and the highest within ±PEAK_RADIUS (the first of equal highs), placed
/// between frames by a parabola through the peak and its neighbours.
fn pick(logits: &[f32]) -> Vec<f64> {
    let n = logits.len();
    (0..n)
        .filter(|&i| {
            let l = logits[i];
            l > 0.0
                && (i.saturating_sub(PEAK_RADIUS)..i).all(|j| logits[j] < l)
                && (i + 1..(i + PEAK_RADIUS + 1).min(n)).all(|j| logits[j] <= l)
        })
        .map(|i| {
            let shift = if i > 0 && i + 1 < n {
                let (a, b, c) = (logits[i - 1] as f64, logits[i] as f64, logits[i + 1] as f64);
                let curve = a - 2.0 * b + c;
                if curve < 0.0 {
                    (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
                } else {
                    0.0
                }
            } else {
                0.0
            };
            (i as f64 + shift) / FPS
        })
        .collect()
}

/// Least-squares line t = a + p·k through the beats `keep` marks.
fn line(times: &[f64], ks: &[i64], keep: &[bool]) -> (f64, f64) {
    let pts = || {
        times
            .iter()
            .zip(ks)
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|((t, k), _)| (*k as f64, *t))
    };
    let n = pts().count() as f64;
    let (mk, mt) = pts().fold((0.0, 0.0), |(a, b), (k, t)| (a + k / n, b + t / n));
    let (cov, var) = pts().fold((0.0, 0.0), |(c, v), (k, t)| {
        (c + (k - mk) * (t - mt), v + (k - mk) * (k - mk))
    });
    let p = cov / var;
    (mt - p * mk, p)
}

fn fit(beat: &[f32], downbeat: &[f32], beats_per_bar: u32) -> Result<Grid, NoGrid> {
    let times = pick(beat);
    if times.len() < MIN_BEATS {
        return Err(NoGrid::FewBeats(times.len()));
    }
    // Start inside the song's most regular run of beats, from its median gap and the beats' mean phase there, then
    // fit inside a window that doubles each round: the period gets exact enough to number beats a whole song away
    // before those are used. Each beat's number comes from the current line, so missed beats and strays never shift
    // the count.
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let spread = |g: &[f64]| {
        let mean = g.iter().sum::<f64>() / g.len() as f64;
        (g.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / g.len() as f64).sqrt() / mean
    };
    let run = START_BEATS - 1;
    let start = (0..=gaps.len() - run)
        .min_by(|&i, &j| spread(&gaps[i..i + run]).total_cmp(&spread(&gaps[j..j + run])))
        .expect("MIN_BEATS covers a run");
    let mut regular = gaps[start..start + run].to_vec();
    regular.sort_by(f64::total_cmp);
    let p0 = regular[run / 2];
    let mid = times[start + START_BEATS / 2];
    let near = || times.iter().filter(|t| (*t - mid).abs() <= 8.0 * p0);
    let (s, c) = near().fold((0.0, 0.0), |(s, c), t| {
        let angle = std::f64::consts::TAU * (t - mid) / p0;
        (s + angle.sin(), c + angle.cos())
    });
    let (mut a, mut p) = (mid + p0 * s.atan2(c) / std::f64::consts::TAU, p0);
    let span = (times[times.len() - 1] - mid).max(mid - times[0]);
    let mut ks = vec![0i64; times.len()];
    let mut on = vec![false; times.len()];
    let mut radius = 8.0 * p0;
    loop {
        for _ in 0..2 {
            for ((t, k), on) in times.iter().zip(&mut ks).zip(&mut on) {
                *k = ((t - a) / p).round() as i64;
                *on = (t - mid).abs() <= radius && (t - (a + p * *k as f64)).abs() <= ON_GRID_S;
            }
            if on.iter().filter(|o| **o).count() < 2 {
                return Err(NoGrid::Unsteady { on_grid: 0.0 });
            }
            (a, p) = line(&times, &ks, &on);
        }
        if radius >= span {
            break;
        }
        radius *= 2.0;
    }
    for ((t, k), on) in times.iter().zip(&mut ks).zip(&mut on) {
        *k = ((t - a) / p).round() as i64;
        *on = (t - (a + p * *k as f64)).abs() <= ON_GRID_S;
    }
    let on_grid = on.iter().filter(|o| **o).count() as f64 / times.len() as f64;
    if on_grid < STEADY_SHARE {
        return Err(NoGrid::Unsteady { on_grid });
    }

    // Bar 1 is the beat of the bar where the downbeat activation is strongest on average across the song.
    let bar = i64::from(beats_per_bar);
    let last_s = (downbeat.len() - 1) as f64 / FPS;
    let mut score = vec![(0.0f64, 0usize); beats_per_bar as usize];
    for k in ((0.0 - a) / p).ceil() as i64..=((last_s - a) / p).floor() as i64 {
        let frame = ((a + p * k as f64) * FPS).round() as usize;
        let act = downbeat[frame.saturating_sub(1)..(frame + 2).min(downbeat.len())]
            .iter()
            .fold(f32::NEG_INFINITY, |m, &v| m.max(v));
        let slot = &mut score[k.rem_euclid(bar) as usize];
        slot.0 += 1.0 / (1.0 + (-f64::from(act)).exp());
        slot.1 += 1;
    }
    let phase = (0..score.len())
        .max_by(|&i, &j| {
            let mean = |(s, n): (f64, usize)| s / n.max(1) as f64;
            mean(score[i]).total_cmp(&mean(score[j]))
        })
        .expect("a bar has a beat");
    Ok(Grid {
        period_s: p,
        downbeat_s: a + p * phase as f64,
        beats_per_bar,
    })
}
