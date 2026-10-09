//! Bounded paired stereo handoff. A rejected frame loses both channels.
//! Absolute sample positions let the consumer detect gaps instead of joining
//! unrelated audio across overflow, resets, or a closed editor.
use crossbeam_queue::ArrayQueue;

#[derive(Clone, Copy, Debug)]
pub struct StereoSample {
    pub left: f32,
    pub right: f32,
    pub index: u64,
    pub generation: u32,
    pub sample_rate: f32,
    pub beat: f64,
    pub bpm: f64,
}

pub struct SampleRing {
    samples: ArrayQueue<StereoSample>,
}
impl SampleRing {
    pub fn new(capacity: usize) -> Self {
        Self { samples: ArrayQueue::new(capacity.max(1)) }
    }
    pub fn push(&self, sample: StereoSample) -> bool {
        self.samples.push(sample).is_ok()
    }
    pub fn drain_into(&self, dst: &mut Vec<StereoSample>) {
        // Bound each drain even when the producer remains active.
        for _ in 0..self.samples.len() {
            if let Some(sample) = self.samples.pop() { dst.push(sample); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(index: u64) -> StereoSample {
        StereoSample { left: index as f32, right: -(index as f32), index,
            generation: 0, sample_rate: 48000.0, beat: 0.0, bpm: 120.0 }
    }
    #[test]
    fn overflow_keeps_pairs_and_exposes_gap() {
        let ring = SampleRing::new(2);
        assert!(ring.push(sample(0))); assert!(ring.push(sample(1)));
        assert!(!ring.push(sample(2)));
        let mut out = Vec::new(); ring.drain_into(&mut out);
        assert!(ring.push(sample(3))); ring.drain_into(&mut out);
        assert_eq!(out.iter().map(|s| s.index).collect::<Vec<_>>(), [0,1,3]);
        for s in out { assert_eq!(s.left, -s.right); }
    }
    #[test]
    fn concurrent_stereo_never_skews() {
        let ring = std::sync::Arc::new(SampleRing::new(32));
        let producer = ring.clone();
        let thread = std::thread::spawn(move || { for i in 0..10000 { producer.push(sample(i)); } });
        let mut out = Vec::new();
        while !thread.is_finished() { ring.drain_into(&mut out); }
        thread.join().unwrap(); ring.drain_into(&mut out);
        assert!(!out.is_empty());
        for pair in out.windows(2) { assert!(pair[1].index > pair[0].index); }
        for s in out { assert_eq!(s.left, -s.right); }
    }
}
