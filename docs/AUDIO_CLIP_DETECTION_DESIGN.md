<!-- index: Moves percussion detection from a global, fire-once import wizard to a per-audio-clip property. Select an audio clip on an audio layer → the inspector shows its detection settings (separation, quantize, per-instrument sensitivity + target layer) and a Detect button. Each audio clip owns its own detection state and the trigger clips it generated; re-detecting one clip only touches its own triggers. The detection backend (htdemucs+ADTOF Python pipeline, orchestrator, parser/planner/import-service) is reused as-is — the work is the ownership model, clip-anchored planning (warp-aware), the inspector UI, and deleting the global singleton path. Decisions locked 2026-06-18: existing backend now (DrumSep later), manual Detect (no auto-on-drop), one sensitivity slider per instrument, no migration of old global percussion state (start fresh). §8 (UX locked 2026-06-19): Detect grows into "Detect and Group" — consumes the demucs stems into analysis-only audio lanes, wraps source + stems + triggers in a named group (expanded, contained), and auto-creates one per-stem send (reused by source). The set is keyed to the source lane, not the clip: re-detecting other clips on the same lane reuses its lanes/sends. -->

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

Once §8 ships, the button reads **Detect and Group** and produces the full set; the mock's `[Detect]` is the pre-grouping label.

## 8. Detect and Group — stems become lanes, the lane becomes a set

**UX/UI locked with Peter 2026-06-19.** Infra in flux — see §8.6. The plain per-clip **Detect** (§7) grows into **Detect and Group**: one press turns an analyzed audio clip into a self-contained *set* on the timeline — the source, its stems, its triggers, and the modulation sends that listen to each stem. The mental model: drop a song → hit Detect → get a song folder, the way dragging a multitrack into Ableton gives you a track group.

The demucs pass already produces 4 stems (drums / bass / vocals / other); the per-clip path **discarded** them ([percussion_orchestrator.rs](../crates/manifold-playback/src/percussion_orchestrator.rs) caches `stem_paths` only on the legacy wizard path). Detect and Group **consumes** them.

### 8.1 What one press produces

- **4 stem audio lanes**, one per stem file, each in the new **analysis-only** output state — silent to master, still feeding its send. See [LAYER_CONTROLS_DESIGN §5](LAYER_CONTROLS_DESIGN.md).
- **Trigger lanes** with the detected hits placed (Kick / Snare / …), as today.
- **One send per stem** in Audio Setup, source = that stem lane (`AudioSend.source = Layer(id)` via `SetAudioSendSourceCommand` — the exact mechanism LAYER_CONTROLS §5 already ships). **Reused by source**, so re-detect never piles up duplicates.
- **A group** wrapping source + stems + triggers, named after the song.

```
▼ ▦ Midnight ──────────────────[set]─ ⊘all ◎all ⠿drag ─┐  ← group header
│  ♪ Midnight        🔊 LIVE   [▓▓▓▓ source clip ▓▓▓▓] │  audible
│  〰 Midnight·Drums  👂 ANLYS  [░░ dimmed stem ░░] →send│  silent, listening
│  〰 Midnight·Bass   👂 ANLYS  [░░ dimmed stem ░░] →send│
│  〰 Midnight·Vocals 👂 ANLYS  [░░ dimmed stem ░░] →send│
│  〰 Midnight·Other  👂 ANLYS  [░░ dimmed stem ░░] →send│
│  ◆ Kick            trig       │▌  ▌ ▌▌   ▌  ▌▌  ▌      │  placed hits
│  ◆ Snare           trig       │   ▌  ▌▌    ▌    ▌      │
└───────────────────────────────────────────────────────┘
```

### 8.2 The group reveals expanded and stays open

Decided: **expanded, stays open** — not collapsed, not auto-collapse. The cost (a group is ~9 lanes) is paid by making the group **read as one object even when open**:

- lanes **indented under a colored group header**;
- the header carries **collapse · mute-all · solo-all · drag-the-whole-set**.

Without containment, three detected songs = ~27 loose lanes and the timeline is a junk drawer. Expanded is only acceptable *because* the group is visually contained. This is a hard requirement of the choice, not a nicety.

### 8.3 The set belongs to the lane, not the clip (lane-keyed reuse)

The "intelligent, just-works" rule. The set is keyed to the **source audio lane**:

- **First Detect** on a lane builds the set (stem lanes, trigger lanes, sends).
- **Detect another clip on the same lane** → **reuses** the set. New stem clips and trigger clips drop onto the *existing* lanes at the new clip's position. No second folder, no duplicate lanes, no duplicate sends.
- **Re-detect one clip** → replaces only that clip's own contributions (its `detection_source` clips); the rest is untouched.
- **Two different songs on one lane** → they **share one set** (the lane is always the unit). A "Drums" stem lane then holds drums clips from both songs. Accepted edge case — the drop affordance (AUDIO_LAYER §6) nudges toward one song per lane anyway.

```
Detect clip ①          Detect clip ② (same lane)     Re-detect clip ①
builds the set         REUSES the set                replaces only ①
 +4 stem lanes          +clips at ②'s position        ① clips swapped
 +trigger lanes         no new lanes                  ②, stems, sends
 +4 sends               no new sends / folder         left untouched

 Source: [▓①]           [▓①]      [▓②]                [▓①']     [▓②]
 Drums : [░①]           [░①]      [░②]                [░①']     [░②]
 Kick  : ▌① ▌①          ▌① ▌①     ▌② ▌②               ▌①' ▌①'   ▌② ▌②
```

Implementation note: this extends §2.2's `detection_source` provenance from trigger clips to the generated **stem** clips too, so reuse/replace is the same clear-by-source logic on both. The stem/trigger **lanes** persist across detects (keyed by source lane); only the **clips** on them are added or replaced.

### 8.4 Where it lives & how it feels

- **In the clip inspector**, alongside the offline detection controls (§7). The §7 **Detect** button becomes **Detect and Group**.
- **Async, non-blocking.** Stem separation is slow. Progress shows **inline on the source clip** (phase label + bar), not only in a global status line. You keep working while it runs.

### 8.5 Sends — naming & legibility

- One send per stem, **named song + stem** ("Midnight · Drums"), so three songs don't collide on three lanes all called "Drums".
- The stem lane **shows it owns a send** (its Send dropdown reads it). The stem → send link must be visible, never silent — otherwise the auto-created sends look like they appeared from nowhere.

### 8.6 Dependency on the audio-infra rework

Send creation and analysis-only routing lean on audio/send infra being reworked concurrently (2026-06-19, separate effort). This design assumes two primitives survive that rework:

1. a lane that is **silent to master but hot to its own send** (the analysis-only state — AUDIO_LAYER §5, LAYER_CONTROLS §5);
2. a send **sourced from a lane** (`AudioSend.source = Layer(id)`, already in LAYER_CONTROLS §5).

If both hold, this design drops straight on top. If the send model changes, the stem → send wiring (§8.1, §8.5) is the only part that adjusts.

## 9. Phased plan

Each phase compiles and is testable. Branch off current HEAD.

- **P0 — Model.** `AudioClipDetection` + `DetectionConfig` on the audio clip; `detection_source` on trigger clips; delete `Project.percussion_import` + helpers. `SetClipDetectionConfigCommand`. Serialize/roundtrip + undo tests. *(core, editing, io)*
- **P1 — Orchestrator rework.** Per-clip `detect(clip)` writing cached analysis onto the clip; provenance tagging on created triggers; clear-only-`detection_source`. Delete global-state writes. *(playback)*
- **P2 — Clip-anchored warp-aware planning** (§4). The real risk; prototype first. Extend the planner/reprojection to anchor on clip `start_beat`/`in_point` + `warp_ratio`. *(core, playback)*
- **P3 — Cached re-plan path** (§3). `ReplanClip` from cached events, no Python. Sliders feel live. *(playback, app)*
- **P4 — Inspector UI** (§7). *(ui, app)*
- **P5 — Re-home nudge/calibrate per-clip** (§6); delete the dead global command surface. *(playback, app)*
- **P6 — Detect and Group** (§8). Consume the demucs stems into analysis-only lanes; build the lane-keyed set (group + stem lanes + trigger lanes + per-stem sends); extend `detection_source` to stem clips; rename the inspector action to **Detect and Group**; inline progress on the source clip. Depends on the audio-infra rework (§8.6) landing the analysis-only output state + layer-sourced sends. *(core, playback, ui, app)*

**Checkpoint:** end of P3 = detection works per-clip with instant slider feedback (no UI yet). End of P4 = per-clip detect is the feature. End of P6 = Detect and Group — the full set.

## 10. Open / watch

- **Concurrency** — the orchestrator runs one detection at a time. Keep that: a Detect on clip B while A is running queues or rejects (status: "detection busy"). No parallel demucs.
- **Target-layer auto-create vs explicit** — when `target_layer == None`, fall back to the current by-name auto-create (Kick → "Kick" layer). Explicit routing overrides.
- **Hash/stem cache** — `compute_audio_hash` reads the whole file on the content thread (pre-existing). Per-clip detection calls it more often; consider hashing off-thread or by path+mtime. Pre-existing issue, flagged not fixed.
- **Group container UI** (§8.2) — "expanded, stays open" is only viable if the layer group renders as a visually contained, collapsible, drag-as-one object with mute-all / solo-all on the header. If the group UI can't carry those, revisit the reveal decision. This is a dependency, not an afterthought.
- **Mute reversal** (§8.1) — the analysis-only / mute split reverses AUDIO_LAYER_DESIGN §5's "mute keeps the send" rationale (a plain mute now kills the send; analysis-only is the silent-but-listening state). Flagged in both layer docs; confirm with Peter it's the intended model before P6.
