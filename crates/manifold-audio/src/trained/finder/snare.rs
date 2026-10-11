//! The snare finder (`snare:<dB>`): rises in the 1-8 kHz band energy. Reference: `snare_cands.py`
//! `band_hops` and `candidates`.

use super::Candidate;
use crate::trained::container::{Container, ContainerError};
use crate::trained::measure::dsp::Sos;

/// Per-hop dB values kept for inspection (about 5.4 s at 48 kHz / 256).
const DB_HISTORY: usize = 1024;

pub struct SnareCands {
    sos: Sos,
    /// Running `np.cumsum` of the squared band signal, `cum[i]` at `i % (window + 1)`;
    /// kept as a running sum because the reference's cancellation error is part of its output.
    cum: Vec<f64>,
    sum: f64,
    samples: u64,
    hop: u64,
    window: u64,
    rise_db: f64,
    lookback: u64,
    refractory: i64,
    deadline: u64,
    db: Vec<f64>,
    hops: u64,
    last: i64,
    armed: bool,
}

fn positive(c: &Container, name: &str) -> Result<u64, ContainerError> {
    let v = c.i64s(name, &[])?[0];
    if v <= 0 {
        return Err(ContainerError::WrongType { name: name.into(), want: "positive integer" });
    }
    Ok(v as u64)
}

impl SnareCands {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let n = c.shape("cand.sos")?.first().copied().unwrap_or(0);
        let sos = c.f64s("cand.sos", &[n, 6])?;
        let hop = positive(c, "cand.hop")?;
        let window = positive(c, "cand.window")?;
        let lookback = positive(c, "cand.lookback")?;
        if window > hop || lookback as usize >= DB_HISTORY {
            return Err(ContainerError::WrongType { name: "cand.window/cand.lookback".into(), want: "window <= hop, short lookback" });
        }
        Ok(Self {
            sos: Sos::new(sos),
            cum: vec![0.0; window as usize + 1],
            sum: 0.0,
            samples: 0,
            hop,
            window,
            rise_db: c.f64s("cand.rise_db", &[])?[0],
            lookback,
            refractory: positive(c, "cand.refractory")? as i64,
            deadline: positive(c, "cand.deadline")?,
            db: vec![0.0; DB_HISTORY],
            hops: 0,
            last: -100,
            armed: true,
        })
    }

    /// Hops whose dB value is known so far.
    pub fn hops(&self) -> u64 {
        self.hops
    }

    /// Hop `h`'s band energy in dB (`band_hops`), while it is in the history.
    pub fn db(&self, h: u64) -> Option<f64> {
        (h < self.hops && self.hops - h <= DB_HISTORY as u64).then(|| self.db[(h % DB_HISTORY as u64) as usize])
    }

    /// The most candidates one push of `samples` samples can append: one per completed hop.
    pub fn max_candidates(&self, samples: usize) -> usize {
        samples / self.hop as usize + 1
    }

    /// Hops from a candidate to its emission.
    pub fn deadline(&self) -> u64 {
        self.deadline
    }

    /// Feeds audio; each candidate is appended at the end of hop `cand_hop`, the moment the reference could
    /// first know it. `avail_hop = cand_hop + deadline`. The caller reserves [`Self::max_candidates`] first.
    pub fn push(&mut self, x: &[f64], out: &mut Vec<Candidate>) {
        let ring = self.window + 1;
        for &v in x {
            let y = self.sos.step(v);
            self.sum += y * y;
            self.samples += 1;
            self.cum[(self.samples % ring) as usize] = self.sum;
            if self.samples.is_multiple_of(self.hop) {
                let old = self.cum[((self.samples - self.window) % ring) as usize];
                let e = 10.0 * ((self.sum - old) / self.window as f64 + 1e-12).log10();
                if let Some((cand_hop, avail_hop)) = self.hop_done(e) {
                    out.push(Candidate { cand_hop, avail_hop, f15: [0.0; super::F15] });
                }
            }
        }
    }

    fn hop_done(&mut self, e: f64) -> Option<(u64, u64)> {
        let h = self.hops;
        self.db[(h % DB_HISTORY as u64) as usize] = e;
        self.hops += 1;
        if h < self.lookback {
            return None;
        }
        let min = (h - self.lookback..h).map(|i| self.db[(i % DB_HISTORY as u64) as usize]).fold(f64::INFINITY, f64::min);
        let r = e - min;
        if r < self.rise_db / 2.0 {
            self.armed = true;
        }
        if self.armed && r >= self.rise_db && h as i64 - self.last >= self.refractory {
            self.last = h as i64;
            self.armed = false;
            return Some((h, h + self.deadline));
        }
        None
    }
}
