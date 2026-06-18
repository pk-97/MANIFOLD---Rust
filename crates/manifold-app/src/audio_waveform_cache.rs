//! Per-clip waveform peaks for the timeline (Phase 1 of the Audio Layer feature
//! — see `docs/AUDIO_LAYER_DESIGN.md`).
//!
//! Audio clips draw their waveform on the lane. Decoding a file is expensive, so
//! it runs on a background thread (mirroring `spawn_background_audio_load`): each
//! audio clip's file is decoded once into a fixed-size array of normalized peak
//! amplitudes, cached by `ClipId`. The peaks (a small `Arc<Vec<f32>>`) are
//! attached to each `ViewportClip` every sync — a cheap refcount bump — and the
//! bitmap renderer paints them inside the clip rect. The cache is lazy
//! (requested on first appearance), failure-tolerant (a bad decode logs and is
//! not retried), and self-evicting (clips that leave the project are dropped).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use ahash::AHashMap;
use manifold_core::id::ClipId;

/// Peak buckets across the whole file. Far more than any clip's on-screen pixel
/// width, so the renderer always down-samples (never up-samples) when drawing.
const WAVEFORM_BUCKETS: usize = 2048;

/// Background-decoded per-clip waveform peaks, owned by `UIRoot`.
pub struct AudioWaveformCache {
    ready: AHashMap<ClipId, Arc<Vec<f32>>>,
    /// Clips whose decode has been requested (in flight or finished/failed), so
    /// we never spawn a second decode for the same clip.
    requested: HashSet<ClipId>,
    tx: Sender<(ClipId, Vec<f32>)>,
    rx: Receiver<(ClipId, Vec<f32>)>,
}

impl Default for AudioWaveformCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { ready: AHashMap::new(), requested: HashSet::new(), tx, rx }
    }
}

impl AudioWaveformCache {
    /// The decoded peaks for a clip, if ready. Cheap `Arc` clone; attach to a
    /// `ViewportClip` each sync.
    pub fn peaks(&self, clip_id: &ClipId) -> Option<Arc<Vec<f32>>> {
        self.ready.get(clip_id).cloned()
    }

    /// Drive the cache: drain finished decodes, request decodes for any audio
    /// clip not yet requested, and evict clips no longer present. Call once per
    /// frame with `(clip_id, file_path)` for every audio clip in the project.
    pub fn poll_and_request(&mut self, audio_clips: &[(ClipId, String)]) {
        while let Ok((id, peaks)) = self.rx.try_recv() {
            self.ready.insert(id, Arc::new(peaks));
        }
        for (id, path) in audio_clips {
            if path.is_empty() || self.requested.contains(id) {
                continue;
            }
            self.requested.insert(id.clone());
            self.spawn_decode(id.clone(), path.clone());
        }
        // Evict clips that vanished from the project (keeps both maps bounded).
        let live: HashSet<&ClipId> = audio_clips.iter().map(|(id, _)| id).collect();
        self.ready.retain(|id, _| live.contains(id));
        self.requested.retain(|id| live.contains(id));
    }

    fn spawn_decode(&self, id: ClipId, path: String) {
        let tx = self.tx.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("audio-waveform".into())
            .spawn(move || match manifold_playback::audio_decoder::decode_audio_to_pcm(&path) {
                Ok(d) => {
                    let peaks = compute_peaks(&d.samples, d.channels, WAVEFORM_BUCKETS);
                    let _ = tx.send((id, peaks));
                }
                Err(e) => log::warn!("[AudioWaveform] decode failed for '{path}': {e}"),
            })
        {
            log::warn!("[AudioWaveform] failed to spawn decode thread: {e}");
        }
    }
}

/// Reduce interleaved PCM to `buckets` normalized peak amplitudes (max |sample|
/// per bucket, across channels), one bar per bucket. Empty input → empty.
fn compute_peaks(samples: &[f32], channels: usize, buckets: usize) -> Vec<f32> {
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    if frames == 0 || buckets == 0 {
        return Vec::new();
    }
    let mut peaks = vec![0.0f32; buckets];
    for (b, peak) in peaks.iter_mut().enumerate() {
        let start = b * frames / buckets;
        let end = (((b + 1) * frames / buckets).max(start + 1)).min(frames);
        let mut p = 0.0f32;
        for f in start..end {
            let base = f * ch;
            for c in 0..ch {
                let s = samples[base + c].abs();
                if s > p {
                    p = s;
                }
            }
        }
        *peak = p.min(1.0);
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_peaks_buckets_and_normalizes() {
        // Mono ramp 0..1; peaks should be non-decreasing and within 0..1.
        let n = 8000;
        let samples: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let peaks = compute_peaks(&samples, 1, 64);
        assert_eq!(peaks.len(), 64);
        assert!(peaks.iter().all(|&p| (0.0..=1.0).contains(&p)));
        assert!(peaks[63] >= peaks[0], "later buckets are louder for a ramp");
    }

    #[test]
    fn compute_peaks_stereo_takes_channel_max() {
        // Frame 0: L=0.2 R=0.9 → peak 0.9.
        let samples = [0.2, 0.9, 0.1, 0.1];
        let peaks = compute_peaks(&samples, 2, 1);
        assert_eq!(peaks.len(), 1);
        assert!((peaks[0] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn empty_input_yields_empty_peaks() {
        assert!(compute_peaks(&[], 2, 64).is_empty());
        assert!(compute_peaks(&[0.5], 1, 0).is_empty());
    }

    #[test]
    fn poll_requests_once_and_evicts_absent() {
        let mut cache = AudioWaveformCache::default();
        let id = ClipId::new("c1");
        // A bogus path is requested once (decode will fail in the background,
        // but the request is recorded so it is not re-spawned every frame).
        cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]);
        assert!(cache.requested.contains(&id));
        cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]);
        assert_eq!(cache.requested.len(), 1);
        // Clip vanishes → evicted from the requested set.
        cache.poll_and_request(&[]);
        assert!(cache.requested.is_empty());
        assert!(cache.peaks(&id).is_none());
    }
}
