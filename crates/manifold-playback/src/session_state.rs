//! Session mode runtime state — Ableton-style scene/clip launch tracking.
//!
//! P2 of `docs/SESSION_MODE_DESIGN.md`. Runtime-only: never serialized, never
//! undo-wrapped (§4). Owned by `PlaybackEngine`, sibling of `LiveClipManager`.
//!
//! [`SessionRuntime`] is the THIRD reference source into
//! `PlaybackEngine::sync_clips_to_time` — an input to the sole playback
//! authority, never a second one (§9). It never touches a renderer directly:
//! it produces [`ActiveClipRef`]s for the scheduler's diff, and reports
//! wrap-restart evictions (`resolve_refs`'s `wrap_restarts` out-param) for the
//! *caller* to apply via the engine's own `stop_clip` — the same primitive
//! `sync_clips_to_time` already uses for every other stop.

use ahash::{AHashMap, AHashSet};
use manifold_core::clip::TimelineClip;
use manifold_core::session::SessionGrid;
use manifold_core::timeline::Timeline;
use manifold_core::{Beats, ClipId, LayerId, SceneId};

use crate::scheduler::ActiveClipRef;

/// One layer's currently-playing session slot.
#[derive(Debug, Clone)]
struct PlayingSlot {
    scene_id: SceneId,
    /// Global beat the slot started at (post-quantize). §4.
    launch_beat: f64,
    /// Loop iteration resolved on the previous `resolve_refs` call — detects
    /// the wrap boundary.
    last_iteration: i64,
    /// Inner clip id active on the previous `resolve_refs` call. Wrap only
    /// force-restarts when the active clip_id is unchanged across the
    /// boundary (§4) — a sequence with multiple clips already gets a natural
    /// stop+start from `compute_sync`'s ordinary clip_id diff.
    last_active_clip_id: Option<ClipId>,
}

/// What a queued launch does when its quantize boundary arrives. Named but
/// left undefined by §4's `PendingSlotLaunch` sketch ("or Stop — see
/// LaunchAction below"); this is the mechanical completion — a private
/// interior type, not a public API shape the doc pins down.
#[derive(Debug, Clone)]
enum LaunchAction {
    Launch(SceneId),
    Stop,
}

/// A launch or stop waiting for the next quantize boundary.
#[derive(Debug, Clone)]
struct PendingSlotLaunch {
    layer_id: LayerId,
    action: LaunchAction,
    target_beat: f64,
}

/// Default global launch quantize: 1 bar at 4/4 (§4).
const DEFAULT_QUANTIZE_BEATS: f64 = 4.0;

/// Runtime session-playback state. See module docs and
/// `docs/SESSION_MODE_DESIGN.md` §4.
pub struct SessionRuntime {
    playing: AHashMap<LayerId, PlayingSlot>,
    pending: Vec<PendingSlotLaunch>,
    session_override: AHashSet<LayerId>,
    quantize_beats: Beats,
}

impl SessionRuntime {
    pub fn new() -> Self {
        Self {
            playing: AHashMap::with_capacity(8),
            pending: Vec::with_capacity(4),
            session_override: AHashSet::with_capacity(8),
            quantize_beats: Beats(DEFAULT_QUANTIZE_BEATS),
        }
    }

    // ─── Accessors ───

    /// Whether `layer_id` is detached from the arrangement (§6) — suppresses
    /// `query_active_timeline_clips` for that layer.
    pub fn is_overridden(&self, layer_id: &LayerId) -> bool {
        self.session_override.contains(layer_id)
    }

    /// The scene currently playing on `layer_id`, if any.
    pub fn playing_scene(&self, layer_id: &LayerId) -> Option<&SceneId> {
        self.playing.get(layer_id).map(|p| &p.scene_id)
    }

    pub fn is_playing_layer(&self, layer_id: &LayerId) -> bool {
        self.playing.contains_key(layer_id)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn playing_count(&self) -> usize {
        self.playing.len()
    }

    pub fn quantize_beats(&self) -> Beats {
        self.quantize_beats
    }

    pub fn set_quantize(&mut self, beats: Beats) {
        self.quantize_beats = beats.max(Beats::ZERO);
    }

    /// Full reset — new project load. Runtime state never survives a project
    /// swap (it is never serialized, so there is nothing to restore).
    pub fn reset(&mut self) {
        self.playing.clear();
        self.pending.clear();
        self.session_override.clear();
    }

    // ─── Quantize math (pure) ───

    /// Next quantize boundary at/after `beat`. `quantize <= 0` means launch
    /// now (§5 "Quantize 0 = launch immediately").
    fn ceil_to_boundary(beat: f64, quantize: f64) -> f64 {
        if quantize <= 0.0 {
            beat
        } else {
            (beat / quantize).ceil() * quantize
        }
    }

    fn next_boundary(&self, current_beat: f64) -> f64 {
        Self::ceil_to_boundary(current_beat, self.quantize_beats.0)
    }

    // ─── Launch semantics (§5) ───

    fn replace_pending_for_layer(&mut self, layer_id: &LayerId) {
        self.pending.retain(|p| &p.layer_id != layer_id);
    }

    /// Enqueue a slot launch at the next quantize boundary (or immediately if
    /// `immediate` — set when the launch is also starting the transport from
    /// stopped, §4). Replaces any pending launch for the layer.
    pub fn launch_slot(&mut self, layer_id: LayerId, scene_id: SceneId, current_beat: f64, immediate: bool) {
        self.replace_pending_for_layer(&layer_id);
        let target = if immediate {
            current_beat
        } else {
            self.next_boundary(current_beat)
        };
        self.pending.push(PendingSlotLaunch {
            layer_id,
            action: LaunchAction::Launch(scene_id),
            target_beat: target,
        });
    }

    /// Enqueue a quantized stop for one layer's session slot.
    /// `session_override` is untouched (§5/§12: the layer goes black, it does
    /// not fall back to the arrangement).
    pub fn stop_slot(&mut self, layer_id: LayerId, current_beat: f64, immediate: bool) {
        self.replace_pending_for_layer(&layer_id);
        let target = if immediate {
            current_beat
        } else {
            self.next_boundary(current_beat)
        };
        self.pending.push(PendingSlotLaunch {
            layer_id,
            action: LaunchAction::Stop,
            target_beat: target,
        });
    }

    /// Launch a scene: every layer with a slot in `scene_id` launches; every
    /// layer currently playing a session slot with NO slot in `scene_id` gets
    /// a quantized stop (Ableton "stop other tracks" default, §5). Layers
    /// never session-launched are untouched.
    pub fn launch_scene(&mut self, scene_id: &SceneId, grid: &SessionGrid, current_beat: f64, immediate: bool) {
        for slot in grid.slots.iter().filter(|s| &s.scene_id == scene_id) {
            self.launch_slot(slot.layer_id.clone(), scene_id.clone(), current_beat, immediate);
        }
        let playing_layers: Vec<LayerId> = self.playing.keys().cloned().collect();
        for layer_id in playing_layers {
            if grid.get_slot(&layer_id, scene_id).is_none() {
                self.stop_slot(layer_id, current_beat, immediate);
            }
        }
    }

    /// Quantized stop of every currently-playing (or about-to-play) session
    /// slot — the "stop all clips" gesture, distinct from a full transport
    /// stop (`on_transport_stop`, which is immediate and Ableton-standard).
    /// `session_override` is untouched, exactly like a single `stop_slot`
    /// (§12: session_override persists "including after a slot stops").
    pub fn stop_all(&mut self, current_beat: f64) {
        let layer_ids: AHashSet<LayerId> = self
            .playing
            .keys()
            .cloned()
            .chain(self.pending.iter().map(|p| p.layer_id.clone()))
            .collect();
        for layer_id in layer_ids {
            self.stop_slot(layer_id, current_beat, false);
        }
    }

    /// Back to arrangement: immediate (not quantized). Clears
    /// `session_override` and any playing/pending session state for the
    /// given layer, or every layer if `None`. Timeline clips resume via
    /// normal `sync_clips_to_time` on the next tick (§5).
    pub fn back_to_arrangement(&mut self, layer_id: Option<&LayerId>) {
        match layer_id {
            Some(id) => {
                self.session_override.remove(id);
                self.playing.remove(id);
                self.replace_pending_for_layer(id);
            }
            None => {
                self.session_override.clear();
                self.playing.clear();
                self.pending.clear();
            }
        }
    }

    /// Transport stop: stops all session playback and clears pending
    /// launches (Ableton behavior, §4). `session_override` is NOT cleared —
    /// layers stay detached until an explicit Back to Arrangement.
    pub fn on_transport_stop(&mut self) {
        self.playing.clear();
        self.pending.clear();
    }

    /// Seek: session slots are beat-anchored and stateless, so a seek never
    /// stops them (§4) — `elapsed` in `resolve_refs` just changes. Pending
    /// launches DO retarget to the next quantize boundary after the new
    /// position, so a jump backward/forward can't fire them off-grid or (if
    /// the new position is already past the old target) instantly.
    pub fn on_seek(&mut self, new_beat: f64) {
        let q = self.quantize_beats.0;
        for p in &mut self.pending {
            p.target_beat = Self::ceil_to_boundary(new_beat, q);
        }
    }

    // ─── Per-tick resolution (§4) ───

    /// Promote any pending launch/stop whose `target_beat` has arrived.
    /// Idempotent: promoted entries are drained from `pending`, so a repeat
    /// call at an unchanged `current_beat` is a no-op — consistent with the
    /// statelessness rule (§4): resolution derives from `current_beat` alone,
    /// this is the one genuine state transition, and it can't double-apply.
    fn activate_due_pending(&mut self, current_beat: f64) {
        if self.pending.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].target_beat <= current_beat + 1e-9 {
                let due = self.pending.remove(i);
                match due.action {
                    LaunchAction::Launch(scene_id) => {
                        self.session_override.insert(due.layer_id.clone());
                        self.playing.insert(
                            due.layer_id,
                            PlayingSlot {
                                scene_id,
                                launch_beat: due.target_beat,
                                last_iteration: 0,
                                last_active_clip_id: None,
                            },
                        );
                    }
                    LaunchAction::Stop => {
                        self.playing.remove(&due.layer_id);
                        // session_override persists (§12).
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    /// Resolve every currently-playing slot into `ActiveClipRef`s (the third
    /// `sync_clips_to_time` input) and detect the wrap-restart case (§4): when
    /// a loop iteration increments but the active inner clip is unchanged,
    /// its id is pushed into `wrap_restarts` so the caller can evict it via
    /// its own `stop_clip` — the ordinary `compute_sync` diff then restarts
    /// it (`to_start`) from `in_point` on the very next step of the same
    /// `sync_clips_to_time` call. `out` and `wrap_restarts` are caller-owned
    /// scratch buffers (hot-path / no-per-tick-allocation rule, §4/§9); both
    /// are cleared by the caller before this call, not here.
    pub fn resolve_refs(
        &mut self,
        current_beat: f64,
        grid: &SessionGrid,
        timeline: &Timeline,
        out: &mut Vec<ActiveClipRef>,
        wrap_restarts: &mut Vec<ClipId>,
    ) {
        self.activate_due_pending(current_beat);
        if self.playing.is_empty() {
            return;
        }

        for (layer_id, slot) in self.playing.iter_mut() {
            let Some(session_slot) = grid.get_slot(layer_id, &slot.scene_id) else {
                continue;
            };
            let length = session_slot.sequence.length_beats.0;
            if length <= 0.0 {
                continue;
            }
            let Some(layer_index) = timeline.layer_index_for_id(layer_id) else {
                continue;
            };

            let elapsed = current_beat - slot.launch_beat;
            let iteration_f = (elapsed / length).floor();
            let local = elapsed - iteration_f * length;
            let iteration = iteration_f as i64;

            let active_clip = session_slot
                .sequence
                .clips
                .iter()
                .find(|c| c.is_active_at_beat(Beats(local)));
            let active_clip_id = active_clip.map(|c| c.id.clone());

            if iteration != slot.last_iteration
                && active_clip_id.is_some()
                && active_clip_id == slot.last_active_clip_id
            {
                // Same clip spans the wrap boundary: compute_sync diffs by
                // clip_id and would treat this as "already active" and never
                // restart it. Force the restart explicitly (§4/§12 — sequence
                // wrap hard-restarts from in_point).
                wrap_restarts.push(active_clip_id.clone().expect("checked Some above"));
            }
            slot.last_iteration = iteration;
            slot.last_active_clip_id = active_clip_id;

            if let Some(clip) = active_clip {
                let global_start = slot.launch_beat + iteration as f64 * length + clip.start_beat.0;
                out.push(ActiveClipRef {
                    clip_id: clip.id.clone(),
                    layer_index: layer_index as i32,
                    clip_index: ActiveClipRef::SESSION_SLOT,
                    start_beat: Beats(global_start),
                    duration_beats: clip.duration_beats,
                    is_looping: clip.is_looping,
                    is_video: !clip.video_clip_id.is_empty(),
                });
            }
        }
    }

    /// Resolve the full `TimelineClip` for a session-slot `ActiveClipRef` at
    /// `start_clip` time (rare, per-event — mirrors the live-slot / timeline
    /// arms at `sync_clips_to_time`'s start loop). Returns a clone with
    /// `start_beat` rebased to `global_start_beat` (the value already
    /// computed by `resolve_refs`, in GLOBAL beats) — the stored inner clip's
    /// `start_beat` is relative to the sequence and must never leak into
    /// downstream video-time / loop math, which assumes a global beat.
    pub fn resolve_clip_for_start(
        &self,
        grid: &SessionGrid,
        layer_id: &LayerId,
        clip_id: &ClipId,
        global_start_beat: Beats,
    ) -> Option<TimelineClip> {
        let scene_id = self.playing_scene(layer_id)?;
        let slot = grid.get_slot(layer_id, scene_id)?;
        let inner = slot.sequence.clips.iter().find(|c| &c.id == clip_id)?;
        let mut resolved = inner.clone();
        resolved.start_beat = global_start_beat;
        Some(resolved)
    }
}

impl Default for SessionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::layer::Layer;
    use manifold_core::session::{ClipSequence, Scene, SessionSlot};
    use manifold_core::types::LayerType;

    fn layer_id(s: &str) -> LayerId {
        LayerId::new(s)
    }
    fn scene_id(s: &str) -> SceneId {
        SceneId::new(s)
    }

    fn timeline_with_layers(ids: &[&str]) -> Timeline {
        let mut timeline = Timeline::default();
        for (i, id) in ids.iter().enumerate() {
            let mut layer = Layer::new(format!("L{i}"), LayerType::Video, i as i32);
            layer.layer_id = layer_id(id);
            timeline.insert_layer(i, layer);
        }
        timeline
    }

    fn one_clip_sequence(length_beats: f64, clip_start: f64, clip_dur: f64, clip_id: &str) -> ClipSequence {
        let mut clip = TimelineClip::new_generator(Beats(clip_start), Beats(clip_dur));
        clip.id = manifold_core::ClipId::new(clip_id);
        ClipSequence {
            length_beats: Beats(length_beats),
            clips: vec![clip],
        }
    }

    fn grid_with_slot(layer: &LayerId, scene: &SceneId, seq: ClipSequence) -> SessionGrid {
        // `SessionGrid` has private cache fields, so it can't be built with a
        // struct literal + `..Default::default()` from outside its crate —
        // build via `default()` then push into the public `scenes`/`slots`
        // Vecs, same as any other external consumer would.
        let mut grid = SessionGrid::default();
        grid.scenes.push(Scene { id: scene.clone(), name: "Scene".into(), color: None });
        grid.slots.push(SessionSlot {
            layer_id: layer.clone(),
            scene_id: scene.clone(),
            sequence: seq,
            name: "Slot".into(),
            color: None,
        });
        grid
    }

    // ─── Quantize targeting (§5) ───

    #[test]
    fn launch_slot_targets_next_boundary() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 1.5, false);
        assert_eq!(rt.pending_count(), 1);
        // ceil(1.5 / 4) * 4 == 4.0
        assert!((rt.pending[0].target_beat - 4.0).abs() < 1e-9);
    }

    #[test]
    fn launch_slot_exactly_on_boundary_launches_now() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 8.0, false);
        assert!((rt.pending[0].target_beat - 8.0).abs() < 1e-9);
    }

    #[test]
    fn zero_quantize_launches_immediately() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(0.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 3.7, false);
        assert!((rt.pending[0].target_beat - 3.7).abs() < 1e-9);
    }

    #[test]
    fn immediate_flag_bypasses_quantize() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 1.5, true);
        assert!((rt.pending[0].target_beat - 1.5).abs() < 1e-9);
    }

    #[test]
    fn second_launch_replaces_pending_for_same_layer() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("a"), 0.0, false);
        rt.launch_slot(layer_id("l1"), scene_id("b"), 1.0, false);
        assert_eq!(rt.pending_count(), 1);
        assert!(matches!(&rt.pending[0].action, LaunchAction::Launch(s) if *s == scene_id("b")));
    }

    // ─── Activation / resolution math (local/iteration/wrap) ───

    #[test]
    fn activate_due_pending_promotes_launch() {
        let mut rt = SessionRuntime::new();
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 0.0, true); // target = 0.0
        rt.activate_due_pending(0.0);
        assert_eq!(rt.pending_count(), 0);
        assert!(rt.is_playing_layer(&layer_id("l1")));
        assert!(rt.is_overridden(&layer_id("l1")));
        assert_eq!(rt.playing_scene(&layer_id("l1")), Some(&scene_id("s1")));
    }

    #[test]
    fn activate_due_pending_not_yet_due_stays_pending() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 0.5, false); // target = 4.0
        rt.activate_due_pending(1.0);
        assert_eq!(rt.pending_count(), 1);
        assert!(!rt.is_playing_layer(&layer_id("l1")));
    }

    #[test]
    fn resolve_refs_local_and_iteration_within_first_loop() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let timeline = timeline_with_layers(&["l1"]);
        let grid = grid_with_slot(&l1, &s1, one_clip_sequence(4.0, 0.0, 4.0, "c1"));

        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 10.0, true); // launch_beat = 10.0

        let mut out = Vec::new();
        let mut wraps = Vec::new();
        // current_beat = 12.0 -> elapsed = 2.0, still inside iteration 0
        rt.resolve_refs(12.0, &grid, &timeline, &mut out, &mut wraps);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].clip_id, "c1");
        assert!(out[0].is_session_slot());
        // global_start = launch_beat(10) + iteration(0)*length(4) + clip.start_beat(0) = 10.0
        assert!((out[0].start_beat.0 - 10.0).abs() < 1e-9);
        assert!(wraps.is_empty(), "no wrap on the first iteration");
    }

    #[test]
    fn resolve_refs_wrap_restarts_same_clip_across_boundary() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let timeline = timeline_with_layers(&["l1"]);
        // Sequence length == clip duration: the single clip spans the whole
        // loop, so every wrap keeps the SAME clip_id active — the mandatory
        // wrap-restart case (§4/§12).
        let grid = grid_with_slot(&l1, &s1, one_clip_sequence(4.0, 0.0, 4.0, "c1"));

        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true); // launch_beat = 0.0

        let mut out = Vec::new();
        let mut wraps = Vec::new();

        // Tick 1: still in iteration 0 (elapsed 2.0 < length 4.0).
        rt.resolve_refs(2.0, &grid, &timeline, &mut out, &mut wraps);
        assert!(wraps.is_empty());
        assert!((out[0].start_beat.0 - 0.0).abs() < 1e-9);

        // Tick 2: elapsed 5.0 -> iteration 1, same clip_id "c1" as before.
        out.clear();
        rt.resolve_refs(5.0, &grid, &timeline, &mut out, &mut wraps);
        assert_eq!(wraps.len(), 1, "wrap boundary with unchanged clip_id must force a restart");
        assert_eq!(wraps[0], "c1");
        // global_start rebased to the new iteration: 0 + 1*4 + 0 = 4.0
        assert!((out[0].start_beat.0 - 4.0).abs() < 1e-9);

        // Tick 3: still iteration 1 -> no further wrap emitted.
        wraps.clear();
        out.clear();
        rt.resolve_refs(6.0, &grid, &timeline, &mut out, &mut wraps);
        assert!(wraps.is_empty(), "no repeat wrap within the same iteration");
    }

    #[test]
    fn resolve_refs_no_wrap_restart_when_clip_changes_across_boundary() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let timeline = timeline_with_layers(&["l1"]);
        // Two clips filling the 4-beat loop: [0,2) = "a", [2,4) = "b". The
        // wrap from iteration 0 -> 1 re-enters clip "a", a DIFFERENT clip_id
        // than whatever was active just before the boundary ("b") — that's
        // an ordinary compute_sync stop+start, not the same-clip wrap case.
        let mut clip_a = TimelineClip::new_generator(Beats(0.0), Beats(2.0));
        clip_a.id = manifold_core::ClipId::new("a");
        let mut clip_b = TimelineClip::new_generator(Beats(2.0), Beats(2.0));
        clip_b.id = manifold_core::ClipId::new("b");
        let grid = grid_with_slot(
            &l1,
            &s1,
            ClipSequence { length_beats: Beats(4.0), clips: vec![clip_a, clip_b] },
        );

        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);

        let mut out = Vec::new();
        let mut wraps = Vec::new();
        rt.resolve_refs(3.0, &grid, &timeline, &mut out, &mut wraps); // iteration 0, clip "b"
        assert_eq!(out[0].clip_id, "b");

        out.clear();
        rt.resolve_refs(4.5, &grid, &timeline, &mut out, &mut wraps); // iteration 1, clip "a"
        assert_eq!(out[0].clip_id, "a");
        assert!(wraps.is_empty(), "clip_id changed across the boundary — ordinary diff handles it");
    }

    // ─── Scene launch/stop matrix (§5) ───

    #[test]
    fn launch_scene_launches_every_slot_in_scene() {
        let l1 = layer_id("l1");
        let l2 = layer_id("l2");
        let scene_a = scene_id("a");
        let mut grid = grid_with_slot(&l1, &scene_a, one_clip_sequence(4.0, 0.0, 4.0, "c1"));
        grid.slots.push(SessionSlot {
            layer_id: l2.clone(),
            scene_id: scene_a.clone(),
            sequence: one_clip_sequence(4.0, 0.0, 4.0, "c2"),
            name: "Slot2".into(),
            color: None,
        });

        let mut rt = SessionRuntime::new();
        rt.launch_scene(&scene_a, &grid, 0.0, true);
        assert_eq!(rt.pending_count(), 2);
        for p in &rt.pending {
            assert!(matches!(&p.action, LaunchAction::Launch(s) if *s == scene_a));
        }
    }

    #[test]
    fn launch_scene_stops_layers_not_in_target_scene() {
        let l1 = layer_id("l1");
        let scene_a = scene_id("a");
        let scene_b = scene_id("b");
        // l1 has a slot in scene_a only; scene_b has nothing for l1.
        let grid = grid_with_slot(&l1, &scene_a, one_clip_sequence(4.0, 0.0, 4.0, "c1"));

        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), scene_a.clone(), 0.0, true);
        rt.activate_due_pending(0.0);
        assert!(rt.is_playing_layer(&l1));

        // Launching scene_b: l1 is currently playing but has no slot in b -> quantized stop.
        rt.launch_scene(&scene_b, &grid, 0.0, true);
        assert_eq!(rt.pending_count(), 1);
        assert!(matches!(&rt.pending[0].action, LaunchAction::Stop));
        assert_eq!(rt.pending[0].layer_id, l1);
    }

    #[test]
    fn stop_all_stops_every_playing_layer_keeps_override() {
        let l1 = layer_id("l1");
        let l2 = layer_id("l2");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.launch_slot(l2.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);
        assert_eq!(rt.playing_count(), 2);

        rt.stop_all(0.0);
        rt.activate_due_pending(0.0);
        assert_eq!(rt.playing_count(), 0);
        // session_override persists (§12) even though nothing is playing.
        assert!(rt.is_overridden(&l1));
        assert!(rt.is_overridden(&l2));
    }

    #[test]
    fn stop_slot_keeps_session_override() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);
        assert!(rt.is_overridden(&l1));

        rt.stop_slot(l1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);
        assert!(!rt.is_playing_layer(&l1));
        assert!(rt.is_overridden(&l1), "layer goes black, does not fall back to arrangement");
    }

    // ─── Back to arrangement ───

    #[test]
    fn back_to_arrangement_one_layer_clears_only_that_layer() {
        let l1 = layer_id("l1");
        let l2 = layer_id("l2");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.launch_slot(l2.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);

        rt.back_to_arrangement(Some(&l1));
        assert!(!rt.is_overridden(&l1));
        assert!(!rt.is_playing_layer(&l1));
        assert!(rt.is_overridden(&l2));
        assert!(rt.is_playing_layer(&l2));
    }

    #[test]
    fn back_to_arrangement_all_clears_everything() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);

        rt.back_to_arrangement(None);
        assert!(!rt.is_overridden(&l1));
        assert!(!rt.is_playing_layer(&l1));
    }

    // ─── Transport stop/seek behavior ───

    #[test]
    fn transport_stop_clears_playing_and_pending_keeps_override() {
        let l1 = layer_id("l1");
        let l2 = layer_id("l2");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);
        rt.launch_slot(l2.clone(), s1.clone(), 1.0, false); // still pending

        rt.on_transport_stop();
        assert_eq!(rt.playing_count(), 0);
        assert_eq!(rt.pending_count(), 0);
        assert!(rt.is_overridden(&l1), "session_override is NOT cleared by transport stop");
    }

    #[test]
    fn seek_does_not_stop_playing_slots() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);

        rt.on_seek(100.0);
        assert!(rt.is_playing_layer(&l1), "seek must not stop session playback (§4)");
    }

    #[test]
    fn seek_retargets_pending_launch_to_next_boundary_after_new_position() {
        let mut rt = SessionRuntime::new();
        rt.set_quantize(Beats(4.0));
        rt.launch_slot(layer_id("l1"), scene_id("s1"), 0.5, false); // target = 4.0
        assert!((rt.pending[0].target_beat - 4.0).abs() < 1e-9);

        rt.on_seek(50.0); // jump far past the old target
        // New target must be recomputed from the new position, not left stale.
        assert!((rt.pending[0].target_beat - 52.0).abs() < 1e-9); // ceil(50/4)*4 = 52
    }

    // ─── resolve_clip_for_start (global-beat rebasing) ───

    #[test]
    fn resolve_clip_for_start_rebases_to_global_beat() {
        let l1 = layer_id("l1");
        let s1 = scene_id("s1");
        // Inner clip's stored start_beat (2.0) is relative to the sequence —
        // must never leak into the resolved clip.
        let grid = grid_with_slot(&l1, &s1, one_clip_sequence(4.0, 2.0, 2.0, "c1"));

        let mut rt = SessionRuntime::new();
        rt.launch_slot(l1.clone(), s1.clone(), 0.0, true);
        rt.activate_due_pending(0.0);

        let resolved = rt
            .resolve_clip_for_start(&grid, &l1, &manifold_core::ClipId::new("c1"), Beats(42.0))
            .expect("clip resolves");
        assert!((resolved.start_beat.0 - 42.0).abs() < 1e-9);
    }
}
