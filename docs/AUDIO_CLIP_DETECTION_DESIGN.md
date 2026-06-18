<!-- index: Moves percussion detection from a global, fire-once import wizard to a per-audio-clip property. Select an audio clip on an audio layer → the inspector shows its detection settings (separation, quantize, per-instrument sensitivity + target layer) and a Detect button. Each audio clip owns its own detection state and the trigger clips it generated; re-detecting one clip only touches its own triggers. The detection backend (htdemucs+ADTOF Python pipeline, orchestrator, parser/planner/import-service) is reused as-is — the work is the ownership model, clip-anchored planning (warp-aware), the inspector UI, and deleting the global singleton path. Decisions locked 2026-06-18: existing backend now (DrumSep later), manual Detect (no auto-on-drop), one sensitivity slider per instrument, no migration of old global percussion state (start fresh). -->

# Audio Clip Detection — Design Doc

Status: **design only.** Branch: TBD (off current HEAD).

Detection becomes a property of the audio clip. Drop an audio file → it's an audio clip on an audio layer (already true). Select it → the inspector shows how it's heard and where its triggers go. Tweak, hit **Detect**, the trigger clips appear on their target layers. This is the studio half of the percussion pipeline: the clip owns its analysis, the way an Ableton clip owns its warp/quantize.

## 0. What already exists (reused as-is)

The detection engine is built and stays. None of it is rewritten — it's re-pointed from a global singleton to a per-clip owner.

- **Python pipeline** — demucs stems → ADTOF drums + basic_pitch bass/synth + madmom beats → JSON. ([tools/audio_analysis/](../tools/audio_analysis/))
- **Orchestrator** — subprocess driving, non-blocking state machine on the content thread. ([percussion_orchestrator.rs](../crates/manifold-playback/src/percussion_orchestrator.rs))
- **Parse → plan → apply** — [percussion_parser.rs](../crates/manifold-playback/src/percussion_parser.rs), [percussion_planner.rs](../crates/manifold-playback/src/percussion_planner.rs), [percussion_import.rs](../crates/manifold-playback/src/percussion_import.rs) (the `EditingService` mutation gateway).
- **Audio clips** — `TimelineClip.audio_file_path`, `LayerType::Audio`, drag-drop, waveform, warp. ([clip.rs](../crates/manifold-core/src/clip.rs), [AUDIO_LAYER_DESIGN.md](AUDIO_LAYER_DESIGN.md))
- **Audio clip inspector section** — `build_audio_section` (Source / Warp / Clip BPM). Detection UI joins here. ([clip_chrome.rs:386](../crates/manifold-ui/src/panels/clip_chrome.rs#L386))

## 1. Decisions (locked 2026-06-18)

| Decision | Choice |
|---|---|
| Detection backend for this build | **Existing htdemucs + ADTOF.** DrumSep / model-choice is a clean follow-up. |
| Detect on drop? | **Manual.** Drop is silent; you press **Detect**. No surprise demucs run. |
| Per-instrument control | **One sensitivity slider** (→ confidence threshold) + on/off + target layer. |
| Old global percussion data | **Dropped.** No migration. `Project.percussion_import` is deleted, not ported. |

Engineering calls (not forks): detection is **warp-aware** (triggers follow the clip's warp), and the cutover is **atomic** (no two-path coexistence).

## 2. The core change — ownership moves to the clip

Today: `Project.percussion_import: Option<PercussionImportState>` — **one** analyzed track per project ([project.rs:65](../crates/manifold-core/src/project.rs#L65)). The whole model assumes a single source.

New: each audio clip carries its own detection state, and each generated trigger clip points back to the audio clip that made it.

### 2.1 On the source audio clip

```rust
// TimelineClip, only Some for audio clips
pub audio_detection: Option<AudioClipDetection>,

pub struct AudioClipDetection {
    pub config: DetectionConfig,                  // the inspector's knobs
    pub analysis: Option<PercussionAnalysisData>, // cached events from the last Detect
}

pub struct DetectionConfig {
    pub quantize_on: bool,
    pub quantize_step_beats: Beats,
    pub onset_compensation: Seconds,
    pub instruments: Vec<InstrumentDetect>,       // Kick, Snare, Hat, Perc, Bass, Synth, Pad, Vocal
    // separation_model: future (DrumSep)
}

pub struct InstrumentDetect {
    pub trigger_type: PercussionTriggerType,
    pub enabled: bool,
    pub sensitivity: f32,        // 0..1 → maps to min_confidence (inverted: high sens = low threshold)
    pub target_layer: Option<LayerId>,  // where its triggers land; None = auto-create by name
}
```

`PercussionAnalysisData` is already serializable. Caching it is the key UX lever — see §3.

### 2.2 On each generated trigger clip

```rust
// TimelineClip, only Some for clips created by detection
pub detection_source: Option<ClipId>,   // the audio clip that generated this trigger
```

This is what makes "each clip owns the triggers it made" hold. Re-detecting clip A clears **only** clips where `detection_source == A`, never B's. Replaces the current "clear every clip on the target layer" behavior in `PercussionImportService::apply_placement_plan`.

### 2.3 Deleted

- `Project.percussion_import` + its lazy-init helpers ([project.rs:1113–1143](../crates/manifold-core/src/project.rs#L1113)).
- `ContentCommand::PercussionImport(String)` (the file-dialog wizard entry) — replaced by clip-scoped commands (§5).
- The orchestrator's writes into the global state; alignment/nudge/calibrate re-homed per-clip (§6).

## 3. Two actions, one slow — the cached-analysis win

Most inspector knobs do **not** need a Python re-run, because they act at plan/apply time, not detection time:

- **Sensitivity** → the planner's `min_confidence` filter. Plan-time.
- **Quantize / onset comp** → plan-time.
- **Target layer** → apply-time.

So the clip caches its `PercussionAnalysisData` (the detected events). That gives two distinct actions:

- **Detect** (slow, runs Python) — first time, or when the detection model/profile changes. Caches the analysis.
- **Re-plan** (instant, no Python) — any sensitivity / quantize / routing change re-plans from the cached events and re-applies. The sliders feel live.

Without this, every slider nudge would kick off a demucs pass. With it, only "Detect" is slow.

## 4. The one real technical risk — clip-anchored, warp-aware planning

Today the planner maps event-seconds → timeline-beats through the **project** tempo map ([percussion_planner.rs:84](../crates/manifold-playback/src/percussion_planner.rs#L84)), assuming the audio starts at a global `audio_start_beat`. Per-clip, that's wrong twice over:

1. **The clip sits somewhere.** An event at source-second `t` must land relative to the clip's `start_beat` and `in_point`, not a global anchor.
2. **The clip may be warped.** The clip plays stretched by `warp_ratio = project_bpm / recorded_bpm` ([clip.rs](../crates/manifold-core/src/clip.rs)). A trigger at source-second `t` is heard at a different timeline position. The planner must map through the same ratio so triggers land on what the audience hears.

So `PercussionTimelinePlanner` needs a **clip-anchored converter**: source-seconds → (clip in_point offset) → (warp ratio) → timeline beats. The reprojection infra for tempo changes already exists ([percussion_analysis.rs `PercussionClipReprojectionPlanner`](../crates/manifold-core/src/percussion_analysis.rs)) — extend it to fold in the clip anchor + warp ratio rather than the global start beat. This is the part to prototype first; everything else is plumbing.

## 5. Commands (new, follow existing `Command` pattern)

Template: [commands/clip.rs](../crates/manifold-editing/src/commands/clip.rs), [commands/layer.rs](../crates/manifold-editing/src/commands/layer.rs).

- `SetClipDetectionConfigCommand { clip_id, config }` — inspector edits (undoable). Triggers a re-plan if analysis is cached.
- The Detect / Re-plan / Clear actions route as `ContentCommand`s to the orchestrator (they're multi-step + async, not single mutations): `DetectClip(ClipId)`, `ReplanClip(ClipId)`, `ClearClipTriggers(ClipId)`.

## 6. Re-homing the live affordances

`calibrate_downbeat`, `nudge_alignment`, `reset_alignment` operate on the global state today. They're genuinely useful live (slide the map onto a bar). Re-home them to act on the **selected audio clip's** triggers (clear-and-replace only that clip's `detection_source` clips). Same math, clip-scoped target.

## 7. Inspector UI

Joins the audio clip's `build_audio_section` ([clip_chrome.rs:386](../crates/manifold-ui/src/panels/clip_chrome.rs#L386)), below the existing Source / Warp / Clip-BPM rows.

```
┌─ SOURCE ─────────────────────────────┐  (exists)
│ track.wav   ·   Warp ON   ·  128 BPM │
├─ DETECTION ──────────────────────────┤  (new)
│ Status: detected ✓        [Detect]   │
│ Quantize:  [1/16 ▾]  ⊙ on            │
│ Onset comp: [−12 ms]                 │
├─ INSTRUMENTS ────────────────────────┤  (new)
│ ☑ Kick   ▓▓▓▓░   → Kick layer ▾      │
│ ☑ Snare  ▓▓▓░░   → Snare ▾           │
│ ☑ Hat    ▓▓░░░   → Hat ▾             │
│ ☐ Bass   ▓▓▓▓░   → Bass ▾           │
│  …                                   │
├──────────────────────────────────────┤
│ [Clear triggers]          [Detect]   │
└──────────────────────────────────────┘
```

- **Detect** runs Python (slow, progress shown via the orchestrator's existing status surface).
- Slider / quantize / routing edits → instant re-plan (§3).
- Each instrument row: on/off · one sensitivity slider · target-layer dropdown.
- Status line reuses the orchestrator's `status_message` / `status_progress01`.

Machine config (demucs device/model, backend paths, BPM min/max) stays in global settings — not in the clip inspector.

## 8. Phased plan

Each phase compiles and is testable. Branch off current HEAD.

- **P0 — Model.** `AudioClipDetection` + `DetectionConfig` on the audio clip; `detection_source` on trigger clips; delete `Project.percussion_import` + helpers. `SetClipDetectionConfigCommand`. Serialize/roundtrip + undo tests. *(core, editing, io)*
- **P1 — Orchestrator rework.** Per-clip `detect(clip)` writing cached analysis onto the clip; provenance tagging on created triggers; clear-only-`detection_source`. Delete global-state writes. *(playback)*
- **P2 — Clip-anchored warp-aware planning** (§4). The real risk; prototype first. Extend the planner/reprojection to anchor on clip `start_beat`/`in_point` + `warp_ratio`. *(core, playback)*
- **P3 — Cached re-plan path** (§3). `ReplanClip` from cached events, no Python. Sliders feel live. *(playback, app)*
- **P4 — Inspector UI** (§7). *(ui, app)*
- **P5 — Re-home nudge/calibrate per-clip** (§6); delete the dead global command surface. *(playback, app)*

**Checkpoint:** end of P3 = detection works per-clip with instant slider feedback (no UI yet). End of P4 = the feature.

## 9. Open / watch

- **Concurrency** — the orchestrator runs one detection at a time. Keep that: a Detect on clip B while A is running queues or rejects (status: "detection busy"). No parallel demucs.
- **Target-layer auto-create vs explicit** — when `target_layer == None`, fall back to the current by-name auto-create (Kick → "Kick" layer). Explicit routing overrides.
- **Hash/stem cache** — `compute_audio_hash` reads the whole file on the content thread (pre-existing). Per-clip detection calls it more often; consider hashing off-thread or by path+mtime. Pre-existing issue, flagged not fixed.
