//! Per-clip waveform data for the timeline (Phase 1 of the Audio Layer feature
//! — see `docs/AUDIO_LAYER_DESIGN.md`).
//!
//! Audio clips draw their waveform on the lane using the same engine as the
//! audio-import / perc lanes: [`WaveformRenderer`] builds a zoom-aware MIP chain
//! with spectral coloring from raw PCM. Decoding + analysis is expensive, so it
//! runs on a background thread (mirroring `spawn_background_audio_load`): each
//! audio clip's file is decoded once and turned into a `WaveformRenderer`, cached
//! by `ClipId`. The renderer (a small `Arc`) is attached to each `ViewportClip`
//! every sync — a cheap refcount bump — and the bitmap renderer selects the right
//! MIP level for the current zoom and paints it inside the clip rect. The cache is
//! lazy (requested on first appearance), failure-tolerant (a bad decode logs and
//! is not retried), and self-evicting (clips that leave the project are dropped).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use ahash::AHashMap;
use manifold_core::id::ClipId;
use manifold_ui::waveform_renderer::WaveformRenderer;

/// Background-decoded per-clip waveform renderers, owned by `UIRoot`.
pub struct AudioWaveformCache {
    ready: AHashMap<ClipId, Arc<WaveformRenderer>>,
    /// Clips whose decode has been requested (in flight or finished/failed), so
    /// we never spawn a second decode for the same clip.
    requested: HashSet<ClipId>,
    tx: Sender<(ClipId, WaveformRenderer)>,
    rx: Receiver<(ClipId, WaveformRenderer)>,
}

impl Default for AudioWaveformCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { ready: AHashMap::new(), requested: HashSet::new(), tx, rx }
    }
}

impl AudioWaveformCache {
    /// The waveform renderer for a clip, if ready. Cheap `Arc` clone; attach to a
    /// `ViewportClip` each sync.
    pub fn renderer(&self, clip_id: &ClipId) -> Option<Arc<WaveformRenderer>> {
        self.ready.get(clip_id).cloned()
    }

    /// Drive the cache: drain finished decodes, request decodes for any audio
    /// clip not yet requested, and evict clips no longer present. Call once per
    /// frame with `(clip_id, file_path)` for every audio clip in the project.
    pub fn poll_and_request(&mut self, audio_clips: &[(ClipId, String)]) {
        while let Ok((id, renderer)) = self.rx.try_recv() {
            self.ready.insert(id, Arc::new(renderer));
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
                    let mut renderer = WaveformRenderer::new();
                    renderer.set_audio_data(&d.samples, d.channels, d.sample_rate);
                    if renderer.is_ready() {
                        let _ = tx.send((id, renderer));
                    } else {
                        log::warn!("[AudioWaveform] empty/unbuildable waveform for '{path}'");
                    }
                }
                Err(e) => log::warn!("[AudioWaveform] decode failed for '{path}': {e}"),
            })
        {
            log::warn!("[AudioWaveform] failed to spawn decode thread: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(cache.renderer(&id).is_none());
    }
}
