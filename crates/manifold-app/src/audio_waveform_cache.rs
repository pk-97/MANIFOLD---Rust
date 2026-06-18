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

/// Consecutive polls a clip must be absent before its renderer is evicted.
/// The clip list is built from the UI-thread project copy while the clips that
/// are *displayed* come from the content-thread snapshot; the two can disagree
/// for a frame or two around edits. Evicting on the first miss made the
/// attached renderer toggle `Some → None → Some`, which blanked and re-drew the
/// waveform (the flicker). A short grace (~2 s at 60 fps) absorbs that churn
/// while still bounding memory when a clip is genuinely gone.
const EVICT_GRACE_POLLS: u32 = 120;

/// Background-decoded per-clip waveform renderers, owned by `UIRoot`.
pub struct AudioWaveformCache {
    ready: AHashMap<ClipId, Arc<WaveformRenderer>>,
    /// Clips whose decode has been requested (in flight or finished/failed), so
    /// we never spawn a second decode for the same clip.
    requested: HashSet<ClipId>,
    /// Consecutive polls each tracked clip has been absent from the live list.
    /// Reset to 0 whenever the clip reappears; eviction only at the grace limit.
    absent_polls: AHashMap<ClipId, u32>,
    tx: Sender<(ClipId, WaveformRenderer)>,
    rx: Receiver<(ClipId, WaveformRenderer)>,
}

impl Default for AudioWaveformCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            ready: AHashMap::new(),
            requested: HashSet::new(),
            absent_polls: AHashMap::new(),
            tx,
            rx,
        }
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
    ///
    /// Returns `true` when a decode finished on this call. The viewport's clip
    /// snapshot is only rebuilt on drag / structural change, so a renderer that
    /// lands between those would never get attached (the waveform would stay
    /// blank until the next unrelated edit). The caller forces a clip re-sync on
    /// `true` so the new renderer attaches the moment it's ready.
    #[must_use]
    pub fn poll_and_request(&mut self, audio_clips: &[(ClipId, String)]) -> bool {
        let mut newly_ready = false;
        while let Ok((id, renderer)) = self.rx.try_recv() {
            self.ready.insert(id, Arc::new(renderer));
            newly_ready = true;
        }
        for (id, path) in audio_clips {
            if path.is_empty() || self.requested.contains(id) {
                continue;
            }
            self.requested.insert(id.clone());
            self.spawn_decode(id.clone(), path.clone());
        }
        // Evict clips absent for the full grace window — not on the first miss,
        // so a one-frame snapshot disagreement doesn't drop a live renderer and
        // flicker the waveform. Present clips reset their absence counter.
        let live: HashSet<&ClipId> = audio_clips.iter().map(|(id, _)| id).collect();
        let tracked: Vec<ClipId> =
            self.ready.keys().chain(self.requested.iter()).cloned().collect();
        for id in tracked {
            if live.contains(&id) {
                self.absent_polls.remove(&id);
            } else {
                *self.absent_polls.entry(id).or_insert(0) += 1;
            }
        }
        let evicted: HashSet<ClipId> = self
            .absent_polls
            .iter()
            .filter(|&(_, &n)| n >= EVICT_GRACE_POLLS)
            .map(|(id, _)| id.clone())
            .collect();
        if !evicted.is_empty() {
            log::info!(
                "[AudioWaveform] evicting {} clip(s) absent >{} polls: {:?}",
                evicted.len(),
                EVICT_GRACE_POLLS,
                evicted
            );
            self.ready.retain(|id, _| !evicted.contains(id));
            self.requested.retain(|id| !evicted.contains(id));
            self.absent_polls.retain(|id, _| !evicted.contains(id));
        }
        newly_ready
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
        // Nothing decodes synchronously, so the first poll reports no new ready.
        assert!(!cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]));
        assert!(cache.requested.contains(&id));
        let _ = cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]);
        assert_eq!(cache.requested.len(), 1);
        // Clip vanishes for ONE poll → still tracked (grace window absorbs a
        // transient snapshot disagreement; this is what kills the flicker).
        let _ = cache.poll_and_request(&[]);
        assert_eq!(cache.requested.len(), 1, "one miss must not evict");
        // Absent for the full grace window → finally evicted.
        for _ in 0..EVICT_GRACE_POLLS {
            let _ = cache.poll_and_request(&[]);
        }
        assert!(cache.requested.is_empty());
        assert!(cache.renderer(&id).is_none());
    }

    #[test]
    fn reappearing_clip_resets_grace() {
        let mut cache = AudioWaveformCache::default();
        let id = ClipId::new("c1");
        let _ = cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]);
        // Absent for almost the whole window, then it comes back…
        for _ in 0..EVICT_GRACE_POLLS - 1 {
            let _ = cache.poll_and_request(&[]);
        }
        let _ = cache.poll_and_request(&[(id.clone(), "/no/such.wav".into())]);
        // …the counter reset, so another near-full window still doesn't evict.
        for _ in 0..EVICT_GRACE_POLLS - 1 {
            let _ = cache.poll_and_request(&[]);
        }
        assert!(cache.requested.contains(&id), "reappearance must reset grace");
    }
}
