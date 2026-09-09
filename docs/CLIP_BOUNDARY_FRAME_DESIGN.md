# Clip Boundary Frame — exact half-open boundary rule

**Status:** SHIPPED — D1/D3 superseded 2026-09-09 by D5 (exact half-open everywhere) · k3 (lead)  
**Prerequisites:** none  
Lifecycle: contract — cited by `PlaybackEngine` sync and the export frame loop.  
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.  

Superseded 2026-09-09: the half-frame boundary epsilon (D1/D3, shipped 2026-09-03) let an incoming clip render up to half a frame early and an outgoing clip linger half a frame past its end — during playback and export, not just when parked. Peter: "both clips or the wrong clip edge showing." Replaced by the Premiere/Resolve rule (D5): a clip is active where `start <= beat < end`, exactly, in every mode — stopped, scrubbing, playing, exporting. The start edge belongs to the clip; the end edge belongs to whatever follows (the next clip, lower layers, or black).

---

## 1. Audit — the seams (rule as of 2026-09-09)

| Concern | Where | What it does |
|---|---|---|
| Clip activity test | `crates/manifold-core/src/clip.rs` `is_active_at_beat` | `beat >= start_beat && beat < end_beat()` — half-open, the one rule. |
| Timeline active-clip query | `crates/manifold-core/src/layer.rs` | `collect_active_clips_at_beat` — exact half-open, no epsilon. |
| Timeline query caller | `crates/manifold-playback/src/engine.rs` | `query_active_timeline_clips` calls `get_active_clips_at_beat_ref(beat)`. |
| Video/media time | `crates/manifold-playback/src/engine.rs` | `compute_video_time` samples `in_point + source_elapsed` at `current_beat`. |
| Audio clip lookup | `crates/manifold-core/src/layer.rs` | `is_active_at_beat` — exact. |
| Selection range | `crates/manifold-core/src/selection.rs` | Same half-open test. |
| Export frame loop | `crates/manifold-app/src/content_export.rs` | Frame k samples at `export_start + k * dt`; frame 0's tick does not advance. |
| Min-remaining warm-up guard | `crates/manifold-playback/src/engine.rs` (`sync_clips_to_time`) | 20ms guard zeroed when stopped/paused or exporting; live playback only. |

---

## 2. Decisions

### D5 (2026-09-09, supersedes D1/D3): exact half-open activity, everywhere
A clip renders when `start <= beat < end`, in every mode. No tolerance.

- **Start edge.** The playhead on `start` shows the clip's first frame.
- **End edge.** The playhead on a lone `end` shows the gap (lower layers or black).
- **Adjacent boundary.** At A-end == B-start, B renders. Just before it, A renders.

**Rationale:** the output must agree with the timing the timeline shows. Premiere and Resolve draw the boundary the same way: the frame at the playhead line is the frame that begins there. The epsilon put the compositor's answer half a frame away from the transport's answer, which read on stage as the wrong clip at every cut.

**Superseded:** D1 (a clip owns both boundaries within half a frame) and D3 (half-frame epsilon, absorbed f32 round-trip). The epsilon also hid the real export defect: frame k rendered at `start + (k+1) * dt`, so frame 0 never sampled the export start. f32 round-trip is fixed at the source instead (D6: f64 export beats).

### D2: keep logical time untouched
`current_beat` stays the authority for transport, triggers, audio, OSC, timecode, and the sync start/stop diff. With no epsilon there is nothing left to scope.

### D4: do not change `Selection::contains_beat`
Selection is a beat interval for operations, not a visual sample. Its half-open semantics stay as-is.

### D6 (2026-09-09): export samples frame k at `export_start + k * dt`
The export loop's first tick does not advance the clock, so frame 0 renders the exact export start and the export spans `[start, end)` like the live compositor. `ExportConfig` beat fields are f64 end-to-end so an f32 round-trip cannot land the start a hair outside a clip edge.

---

## 3. Design body

### 3.1 The rule
```
active(clip, beat) = clip.start_beat <= beat && beat < clip.end_beat()
```
One rule in `Layer::collect_active_clips_at_beat`, used by every consumer. Non-overlap (a write-time invariant on `Layer`) guarantees at most one active clip per layer, so no tie-breaking exists.

### 3.2 Video time at the boundary
`compute_video_time` uses `current_beat`. A parked playhead one frame before `end` samples the last media frame; at `end` exactly the clip is inactive.

### 3.3 Audio unchanged
`Layer::active_audio_clip_at` keeps exact `is_active_at_beat`.

### 3.4 Min-remaining guard scope
`sync_clips_to_time` zeroes the 20ms warm-up guard when the engine is stopped/paused or in export mode, so inspecting or encoding a clip's final frame always starts it. Live playback keeps the guard: entering a clip's tail mid-show only happens via a hiccup or a seek, and sparing the decode warm-up is worth at most one sub-frame gap there.

### 3.5 Sync and trigger invariants
- `sync_clips_to_time` uses exact `current_beat` for start/stop diff.
- Clip edge triggers fire at exact boundaries.
- OSC/timecode use exact `current_beat`.

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| I1 | One activity rule, no epsilon parameters. | `rg 'boundary_epsilon'` in `crates/manifold-core/src/{layer,timeline}.rs` is empty. |
| I2 | Audio and selection stay exact. | `Layer::active_audio_clip_at` and `Selection::contains_beat` keep their signatures. |
| I3 | At an adjacent boundary the incoming clip renders; just before it, the outgoing one does. | `exact_boundary_adjacent_join_shows_incoming_clip` (layer.rs). |
| I4 | A lone end edge shows the gap; the start edge shows the clip. | `exact_boundary_end_edge_is_inactive`, `exact_boundary_start_edge_is_active` (layer.rs); `stopped_engine_shows_gap_at_lone_end_boundary`, `stopped_engine_keeps_clip_active_at_start_boundary` (engine_tick.rs). |
| I5 | Stopped/export starts a clip with sub-frame remaining lifetime. | `stopped_engine_starts_clip_with_sub_frame_remaining` (engine_tick.rs). |

---

## 5. Phasing

Landed as one change (2026-09-09): exact query restored, epsilon plumbing removed (`visual_boundary_epsilon`, `is_boundary_owned`, scheduler bypass), warm-up guard narrowed to live playback, export frame-0 sampling + f64 export beats, regression tests rewritten to the new rule.

## 6. Decided — do not reopen

- D5: exact half-open activity everywhere; the end edge belongs to what follows.
- D2: logical `current_beat` unchanged for triggers/audio/transport.
- D4: `Selection::contains_beat` unchanged.
- D6: export frame k samples at `export_start + k * dt`; export beats are f64.

## 7. Deferred

- Playhead snap / timeline ruler precision: if the UI parks the playhead a full frame past a boundary rather than on the edge, the picture will truthfully show that position — which will read as a snap bug, not a render bug. Trigger: user reports the picture disagreeing with the intended snap target after this lands.
