//! The content reaction pass (docs/UI_INVALIDATION_DESIGN.md D4, section 4).
//!
//! Commands only edit; the work an edit implies runs here, once per drain
//! pass, keyed on the part stamps that moved since the last pass. Nothing
//! else on the content side rebuilds routes, listeners, overlays or renderer
//! caches after an edit (INV-2; the project-load path is the one exception).

use manifold_core::change::{ChangeSet, Part, PartStamps};

use manifold_core::tempo::TempoMap;
use manifold_core::units::Bpm;

use crate::content_thread::ContentThread;

/// Parts whose change rebuilds OSC routes and Ableton listeners.
const ROUTE_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Mappings]);
/// Parts whose change makes clip-control bindings stale (BUG-6ompp (content tick re-hashes every clip)).
const BINDING_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Clips, Part::Effects, Part::Session]);

#[derive(Default)]
pub(crate) struct ReactionState {
    /// The stamps the last pass reacted to.
    pub reacted: PartStamps,
    /// Tempo map and BPM as they stood before the first Settings edit since
    /// the last pass; the Settings row reconciles the playhead against it.
    pub tempo_retained: Option<(TempoMap, Bpm)>,
    /// A content-originated `mutate` moved a part outside the live scalars,
    /// so the next `send_state` publishes a snapshot (D23).
    pub publish_pending: bool,
    /// Ableton discovery finished: the Live-side ranges the write targets
    /// cache changed, so the listeners rebuild once even if no part moved.
    pub session_discovered: bool,
    /// D15: moves only when a captured live value changed.
    pub values_seq: u64,
    /// D16: one sequence per runtime input the UI builds structure from.
    pub runtime: crate::content_state::RuntimeSeqs,
    /// What `runtime` last saw, compared each publish.
    runtime_seen: RuntimeSeen,
    /// The failed chain builds last published; replaced only when they change.
    pub chain_failures: crate::content_state::ChainFailures,
}

/// The last-published value behind each runtime sequence.
#[derive(Default, PartialEq)]
struct RuntimeSeen {
    latches_len: usize,
    latches_key: u64,
    led_present: bool,
    sdr_preview: bool,
    chain_failures_key: (usize, u64),
}

impl ContentThread {
    /// Bump each runtime sequence whose value changed since the last publish.
    /// The latch key is an order-free sum of entry hashes: no allocation.
    pub(crate) fn track_runtime_seqs(&mut self) -> crate::content_state::RuntimeSeqs {
        let latches = self.engine.automation_latches();
        let hasher = ahash::RandomState::with_seeds(1, 2, 3, 4);
        let latches_key = latches.keys().map(|k| hasher.hash_one(k)).fold(0u64, u64::wrapping_add);
        let mut chain_failures_key = (0usize, 0u64);
        self.content_pipeline.visit_chain_failures(&mut |owner, message| {
            chain_failures_key.0 += 1;
            chain_failures_key.1 = chain_failures_key.1.wrapping_add(hasher.hash_one((owner, &**message)));
        });
        let seen = RuntimeSeen {
            latches_len: latches.len(),
            latches_key,
            led_present: !matches!(self.led_preview, crate::content_state::LedPreview::None),
            sdr_preview: self.content_pipeline.sdr_preview(),
            chain_failures_key,
        };
        let last = &self.reactions.runtime_seen;
        let runtime = &mut self.reactions.runtime;
        if (seen.latches_len, seen.latches_key) != (last.latches_len, last.latches_key) {
            runtime.automation_latches += 1;
        }
        if seen.led_present != last.led_present {
            runtime.led_preview_presence += 1;
        }
        if seen.sdr_preview != last.sdr_preview {
            runtime.sdr_preview += 1;
        }
        runtime.layer_notices = self.content_pipeline.layer_notices.notices().version;
        if seen.chain_failures_key != last.chain_failures_key {
            runtime.chain_failures += 1;
            let mut failures = Vec::with_capacity(seen.chain_failures_key.0);
            self.content_pipeline.visit_chain_failures(&mut |owner, message| {
                failures.push((owner.cloned(), message.clone()));
            });
            self.reactions.chain_failures = failures.into();
        }
        self.reactions.runtime_seen = seen;
        self.reactions.runtime
    }

    /// Keep the tempo map as it stands before a Settings edit, once per pass.
    pub(crate) fn retain_tempo_for_reaction(&mut self) {
        if self.reactions.tempo_retained.is_none() {
            self.reactions.tempo_retained =
                self.engine.project().map(|p| (p.tempo_map.clone(), p.settings.bpm));
        }
    }

    /// Run every reaction whose part moved since the last pass.
    pub(crate) fn run_reactions(&mut self) {
        let retained = self.reactions.tempo_retained.take();
        let stamps = self.editing_service.stamps();
        let moved = stamps.moved_since(&self.reactions.reacted);
        let discovered = std::mem::take(&mut self.reactions.session_discovered);
        if moved.is_empty() && !discovered {
            return;
        }
        self.reactions.reacted = stamps;

        if let Some(p) = self.engine.project() {
            if moved.intersects(ROUTE_PARTS) {
                self.osc_param_router.rebuild(p, &mut self.osc_receiver);
            }
            if moved.intersects(ROUTE_PARTS) || discovered {
                self.ableton_bridge.rebuild_listeners(p);
            } else if moved.contains(Part::MappingRanges) {
                self.ableton_bridge.refresh_ranges(p);
            }
            if moved.contains(Part::EmbeddedPresets) {
                crate::project_io::install_project_preset_overlay(p);
            }
            if moved.contains(Part::Shapes) {
                self.audio_mod_runtime.sync_send_gains(&p.audio_setup);
            }
        }
        if moved.intersects(BINDING_PARTS) {
            self.engine.mark_clip_bindings_stale();
        }
        if moved.contains(Part::Settings)
            && let Some((map, bpm)) = retained
        {
            self.engine.reconcile_tempo_edit(&map, bpm);
        }
        if moved.contains(Part::Media) {
            let (renderers, project) = self.engine.split_renderer_project();
            if let Some(p) = project {
                for renderer in renderers.iter_mut() {
                    manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::RendererReload);
                    renderer.on_project_loaded(p);
                }
            }
        }
        // Restore any layers stopped by runaway GPU frames so an edit retries.
        for renderer in self.engine.renderers_mut() {
            if let Some(gen_renderer) = renderer.as_any_mut().downcast_mut::<manifold_compositor::generator_renderer::GeneratorRenderer>() {
                gen_renderer.restore_stopped_layers();
            }
        }
        // Refresh the compositor even while paused: a blend-mode change,
        // effect edit or value write must show without playback advancing.
        self.engine.mark_compositor_dirty_now();
    }
}
