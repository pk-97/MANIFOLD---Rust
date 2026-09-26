//! Background cache for audio clip waveform data.
//!
//! Audio clips draw their waveform on the lane using the same engine as the
//! waveform painter: [`WaveformRenderer`] builds a zoom-aware pyramid of
//! low/mid/high peak and RMS data from raw PCM. Decoding and analysis run in
//! background jobs. The decoded renderer is shared by exact
//! source-path identity, then attached to each `ViewportClip` through the
//! clip-to-source association. The cache is lazy (requested on first
//! appearance), failure-tolerant (a bad decode logs and is not retried), and
//! self-evicting (sources and clip associations that leave the project are
//! dropped after a short grace window).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use ahash::AHashMap;
use manifold_core::id::ClipId;
use manifold_ui::waveform_renderer::WaveformRenderer;

/// Consecutive polls a clip or source must be absent before it is evicted.
/// The clip list is built from the UI-thread project copy while the clips that
/// are displayed come from the content-thread snapshot; the two can disagree
/// for a frame or two around edits. Evicting on the first miss made the
/// attached renderer toggle `Some → None → Some`, which blanked and re-drew
/// the waveform (the flicker). A short grace (~2 s at 60 fps) absorbs that
/// churn while still bounding memory when a clip or source is genuinely gone.
const EVICT_GRACE_POLLS: u32 = 120;

struct ClipState {
    source: Arc<str>,
    absent_polls: u32,
}

struct SourceState {
    renderer: Option<Arc<WaveformRenderer>>,
    request_id: u64,
    absent_polls: u32,
}

struct DecodeCompletion {
    source: Arc<str>,
    request_id: u64,
    renderer: WaveformRenderer,
}

/// Background-decoded waveform renderers, owned by `UIRoot`.
pub struct AudioWaveformCache {
    clips: AHashMap<ClipId, ClipState>,
    sources: AHashMap<Arc<str>, SourceState>,
    next_request_id: u64,
    tx: Sender<DecodeCompletion>,
    rx: Receiver<DecodeCompletion>,
}

impl Default for AudioWaveformCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            clips: AHashMap::new(),
            sources: AHashMap::new(),
            next_request_id: 0,
            tx,
            rx,
        }
    }
}

impl AudioWaveformCache {
    /// The waveform renderer for a clip, if its source is ready. Cheap `Arc`
    /// clone; attach to a `ViewportClip` each sync.
    pub fn renderer(&self, clip_id: &ClipId) -> Option<Arc<WaveformRenderer>> {
        let source = self.clips.get(clip_id)?.source.as_ref();
        self.sources.get(source)?.renderer.clone()
    }

    /// Drive the cache using the production background decoder.
    #[must_use]
    pub fn poll_and_request(&mut self, audio_clips: &[(ClipId, String)]) -> bool {
        self.poll_with_request(audio_clips, |tx, source, request_id| {
            Self::spawn_decode(tx, source, request_id)
        })
    }

    /// Drive the cache: drain finished decodes, request decodes for any audio
    /// clip whose exact source path is not yet requested, and evict clips and
    /// sources no longer present. The request callback is kept private so
    /// deterministic tests can record requests without starting decode
    /// threads; production callers use [`Self::poll_and_request`].
    #[must_use]
    fn poll_with_request<F>(
        &mut self,
        audio_clips: &[(ClipId, String)],
        mut request_decode: F,
    ) -> bool
    where
        F: FnMut(&Sender<DecodeCompletion>, Arc<str>, u64),
    {
        let mut changed = false;
        while let Ok(completion) = self.rx.try_recv() {
            if let Some(source) = self.sources.get_mut(completion.source.as_ref())
                && source.request_id == completion.request_id
            {
                source.renderer = Some(Arc::new(completion.renderer));
                changed = true;
            }
        }

        // Increment first, then reset entries seen in this poll. This keeps
        // the steady path allocation-free and avoids temporary live-ID sets.
        for clip in self.clips.values_mut() {
            clip.absent_polls = clip.absent_polls.saturating_add(1);
        }
        for source in self.sources.values_mut() {
            source.absent_polls = source.absent_polls.saturating_add(1);
        }

        for (id, path) in audio_clips {
            if path.is_empty() {
                if self.clips.remove(id).is_some() {
                    // An explicitly cleared path is a mapping change. The
                    // source itself remains in its normal grace window.
                    changed = true;
                }
                continue;
            }

            if self
                .clips
                .get(id)
                .is_some_and(|clip| clip.source.as_ref() == path.as_str())
            {
                // Borrow only the clip and source entries on the stable path;
                // no source Arc or String is cloned per frame.
                self.clips
                    .get_mut(id)
                    .expect("clip was just found")
                    .absent_polls = 0;
                if let Some(source) = self.sources.get_mut(path.as_str()) {
                    source.absent_polls = 0;
                }
                continue;
            }

            let source = Arc::<str>::from(path.as_str());
            if let Some(clip) = self.clips.get_mut(id) {
                clip.source = source.clone();
                clip.absent_polls = 0;
            } else {
                self.clips.insert(
                    id.clone(),
                    ClipState {
                        source: source.clone(),
                        absent_polls: 0,
                    },
                );
            }

            self.attach_source(source, &mut request_decode);
            // Any new or changed association invalidates the current clip
            // projection immediately, even while its source is decoding.
            changed = true;
        }

        self.clips
            .retain(|_, clip| clip.absent_polls < EVICT_GRACE_POLLS);
        self.sources
            .retain(|_, source| source.absent_polls < EVICT_GRACE_POLLS);

        changed
    }

    fn attach_source<F>(&mut self, source: Arc<str>, request_decode: &mut F)
    where
        F: FnMut(&Sender<DecodeCompletion>, Arc<str>, u64),
    {
        if let Some(existing) = self.sources.get_mut(source.as_ref()) {
            existing.absent_polls = 0;
            return;
        }

        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.sources.insert(
            source.clone(),
            SourceState {
                renderer: None,
                request_id,
                absent_polls: 0,
            },
        );
        request_decode(&self.tx, source, request_id);
    }

    fn spawn_decode(tx: &Sender<DecodeCompletion>, source: Arc<str>, request_id: u64) {
        let tx = tx.clone();
        let path = source.to_string();
        if let Err(e) = std::thread::Builder::new()
            .name("audio-waveform".into())
            .spawn(
                move || match manifold_playback::audio_decoder::decode_audio_to_pcm(&path) {
                    Ok(d) => {
                        let mut renderer = WaveformRenderer::new();
                        renderer.set_audio_data(&d.samples, d.channels, d.sample_rate);
                        if renderer.is_ready() {
                            let _ = tx.send(DecodeCompletion {
                                source,
                                request_id,
                                renderer,
                            });
                        } else {
                            log::warn!("[AudioWaveform] empty/unbuildable waveform for '{path}'");
                        }
                    }
                    Err(e) => log::warn!("[AudioWaveform] decode failed for '{path}': {e}"),
                },
            )
        {
            log::warn!("[AudioWaveform] failed to spawn decode thread: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_renderer() -> WaveformRenderer {
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&[0.0; 1600], 1, 44_100);
        assert!(renderer.is_ready());
        renderer
    }

    fn poll(cache: &mut AudioWaveformCache, clips: &[(ClipId, String)]) -> bool {
        cache.poll_with_request(clips, |_, _, _| {})
    }

    fn poll_recording(cache: &mut AudioWaveformCache, clips: &[(ClipId, String)]) -> (bool, usize) {
        let mut requests = 0;
        let changed = cache.poll_with_request(clips, |_, _, _| requests += 1);
        (changed, requests)
    }

    fn request_id(cache: &AudioWaveformCache, source: &str) -> u64 {
        cache
            .sources
            .get(source)
            .expect("source request")
            .request_id
    }

    fn inject_completion(cache: &AudioWaveformCache, source: &str, request_id: u64) {
        cache
            .tx
            .send(DecodeCompletion {
                source: Arc::from(source),
                request_id,
                renderer: ready_renderer(),
            })
            .expect("test receiver is alive");
    }

    #[test]
    fn shares_one_request_and_renderer_for_duplicate_source_paths() {
        let mut cache = AudioWaveformCache::default();
        let first = ClipId::new("first");
        let second = ClipId::new("second");
        let clips = [
            (first.clone(), "/audio/exact.wav".to_owned()),
            (second.clone(), "/audio/exact.wav".to_owned()),
        ];

        assert!(poll(&mut cache, &clips));
        assert_eq!(cache.sources.len(), 1);
        let (changed, requests) = poll_recording(&mut cache, &clips);
        assert!(!changed);
        assert_eq!(requests, 0, "stable source must not request again");

        let request = request_id(&cache, "/audio/exact.wav");
        inject_completion(&cache, "/audio/exact.wav", request);
        assert!(poll(&mut cache, &clips));
        let first_renderer = cache.renderer(&first).expect("first renderer");
        let second_renderer = cache.renderer(&second).expect("second renderer");
        assert!(Arc::ptr_eq(&first_renderer, &second_renderer));
    }

    #[test]
    fn changed_clip_path_reassociates_to_pending_and_ready_sources() {
        let mut cache = AudioWaveformCache::default();
        let changed = ClipId::new("changed");
        let source_clip = ClipId::new("source");

        let first = [(changed.clone(), "/audio/first.wav".to_owned())];
        assert!(poll(&mut cache, &first));
        let first_request = request_id(&cache, "/audio/first.wav");
        inject_completion(&cache, "/audio/first.wav", first_request);
        assert!(poll(&mut cache, &first));

        let second = [(source_clip.clone(), "/audio/second.wav".to_owned())];
        assert!(poll(&mut cache, &second));
        let second_request = request_id(&cache, "/audio/second.wav");
        inject_completion(&cache, "/audio/second.wav", second_request);
        assert!(poll(&mut cache, &second));

        let pending = [(changed.clone(), "/audio/pending.wav".to_owned())];
        assert!(poll(&mut cache, &pending));
        assert!(cache.renderer(&changed).is_none());

        let pending_request = request_id(&cache, "/audio/pending.wav");
        inject_completion(&cache, "/audio/pending.wav", pending_request);
        assert!(poll(&mut cache, &pending));
        assert!(cache.renderer(&changed).is_some());

        let reassociated = [(changed.clone(), "/audio/second.wav".to_owned())];
        assert!(poll(&mut cache, &reassociated));
        assert!(cache.renderer(&changed).is_some());

        let cleared = [(changed.clone(), String::new())];
        assert!(poll(&mut cache, &cleared));
        assert!(cache.renderer(&changed).is_none());
    }

    #[test]
    fn reappearing_clip_resets_both_grace_counters() {
        let mut cache = AudioWaveformCache::default();
        let id = ClipId::new("reappears");
        let clips = [(id.clone(), "/audio/reappears.wav".to_owned())];
        let _ = poll(&mut cache, &clips);

        for _ in 0..EVICT_GRACE_POLLS - 1 {
            let _ = poll(&mut cache, &[]);
        }
        assert!(cache.clips.contains_key(&id));
        assert!(cache.sources.contains_key("/audio/reappears.wav"));

        assert!(!poll(&mut cache, &clips));
        assert_eq!(cache.clips.get(&id).unwrap().absent_polls, 0);
        assert_eq!(
            cache
                .sources
                .get("/audio/reappears.wav")
                .unwrap()
                .absent_polls,
            0
        );

        for _ in 0..EVICT_GRACE_POLLS - 1 {
            let _ = poll(&mut cache, &[]);
        }
        assert!(cache.clips.contains_key(&id));
        assert!(cache.sources.contains_key("/audio/reappears.wav"));
    }

    #[test]
    fn clip_and_source_share_one_grace_window() {
        let mut cache = AudioWaveformCache::default();
        let id = ClipId::new("grace");
        let clips = [(id.clone(), "/audio/grace.wav".to_owned())];
        let _ = poll(&mut cache, &clips);

        for _ in 0..EVICT_GRACE_POLLS - 1 {
            let _ = poll(&mut cache, &[]);
        }
        assert!(cache.clips.contains_key(&id));
        assert!(cache.sources.contains_key("/audio/grace.wav"));

        let _ = poll(&mut cache, &[]);
        assert!(!cache.clips.contains_key(&id));
        assert!(!cache.sources.contains_key("/audio/grace.wav"));
    }

    #[test]
    fn stale_completion_cannot_fill_a_re_requested_source() {
        let mut cache = AudioWaveformCache::default();
        let old_clip = ClipId::new("old");
        let source = "/audio/re-request.wav";
        let old_clips = [(old_clip, source.to_owned())];
        let _ = poll(&mut cache, &old_clips);
        let old_request = request_id(&cache, source);

        for _ in 0..EVICT_GRACE_POLLS {
            let _ = poll(&mut cache, &[]);
        }
        assert!(!cache.sources.contains_key(source));

        let new_clip = ClipId::new("new");
        let new_clips = [(new_clip.clone(), source.to_owned())];
        let _ = poll(&mut cache, &new_clips);
        let new_request = request_id(&cache, source);
        assert_ne!(old_request, new_request);

        inject_completion(&cache, source, old_request);
        assert!(!poll(&mut cache, &new_clips));
        assert!(cache.renderer(&new_clip).is_none());

        inject_completion(&cache, source, new_request);
        assert!(poll(&mut cache, &new_clips));
        assert!(cache.renderer(&new_clip).is_some());
    }
}
