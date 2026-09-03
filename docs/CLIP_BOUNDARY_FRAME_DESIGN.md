# Clip Boundary Frame — boundary ownership rule

**Status:** SHIPPED — P1–P3 on main · 2026-09-03 · k3 (lead)  
**Prerequisites:** none  
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.  

Shipped 2026-09-03: boundary ownership implemented via `visual_boundary_epsilon` in `PlaybackEngine` and `is_boundary_owned` in `ActiveClipRef`. The scheduler's min-remaining guard is bypassed only for boundary-owned clips, so generators, scenes, and images render at the edge while video clips keep their warm-up guard everywhere except exact boundaries.
Peter: "playhead sitting on the same frame as the edge of a clip goes black, and so does export frame 0 at a clip boundary." The engine samples at a dimensionless point in beat time. A clip is active only where `start <= beat < end`. Park or export the playhead exactly on `end` and the active-clip query returns nothing, so the compositor clears to black. Peter's call: a clip should own its boundaries — the playhead on a clip's right edge should still show that clip's last frame, and the playhead on its left edge should show its first frame.

---

## 1. Audit — what exists today (verified 2026-09-03)

| Concern | Where | What it does |
|---|---|---|
| Clip activity test | `crates/manifold-core/src/clip.rs:175` | `beat >= start_beat && beat < end_beat()` — half-open, the root seam. |
| Timeline active-clip query | `crates/manifold-core/src/layer.rs:464` | `collect_active_clips_at_beat` finds point-in-interval clips per layer. |
| Timeline query caller | `crates/manifold-playback/src/engine.rs:1085` | `query_active_timeline_clips` calls `get_active_clips_at_beat_ref(beat)`. |
| Video/media time | `crates/manifold-playback/src/engine.rs:2140` | `compute_video_time` samples `in_point + source_elapsed` at `current_beat`. |
| Audio clip lookup | `crates/manifold-core/src/layer.rs:320` | `is_active_at_beat` — stays half-open; audio must remain exact. |
| Selection range | `crates/manifold-core/src/selection.rs:53` | Same half-open test — stays unchanged. |
| Export frame loop | `crates/manifold-app/src/content_export.rs:533` | Tick advances by `dt` before sampling; frame k renders at `start + (k+1) * dt`. |
| Parked/scrub render | `crates/manifold-playback/src/engine.rs:972` | Non-playing tick does not advance time; renders at the parked beat. |
| f32 seams | `docs/CORE_ENGINE_MAP.md` item 6 | UI and external clocks round-trip beats through f32, so a "snapped" boundary can land a hair inside or outside the clip. |

Findings: every clip type shares the same half-open point-sampling path, so all of them can black out at a boundary. No probe is needed; the logic is decisive.

---

## 2. Decisions

### D1: a clip owns its boundaries in the visual timeline query
Change the timeline active-clip query so a clip is active if the playhead is inside the clip or within half a frame of its start or end. On overlap at an adjacent boundary, the later-starting clip wins. This is the boundary ownership rule.

- **Start edge.** The playhead lands exactly on `start`. The clip is active and shows its first frame.
- **End edge.** The playhead lands exactly on `end`. The clip is active and shows its last frame.
- **Adjacent boundary.** Clip A ends at `end`, clip B starts at `end`. The later-starting clip (B) wins the exact boundary; A still wins just before the boundary.

**Rationale:** matches Peter's expectation that the clip block owns its edges, and it also absorbs the f32 round-trip errors that push a "snapped" value a hair past a boundary.

**Rejected alternative — mid-frame sampling.** Shifts the sample point by half a frame. At a lone end boundary this still produces black, because the half-frame offset points outside the clip. Does not match Peter's right-edge expectation.

**Rejected alternative — global activity rule.** Changing `is_active_at_beat` everywhere would alter audio clip selection and session launch behavior. Keep the change scoped to the visual timeline query.

### D2: keep logical time untouched
`current_beat` stays the authority for transport, triggers, audio, OSC, timecode, and the sync start/stop diff. Only the visual active-clip query gets the boundary tolerance.

**Rationale:** preserves exact edge semantics for everything that is not the compositor's "which clip do I draw now" decision.

### D3: tolerance equals half a frame in beats
Use `boundary_epsilon = 0.5 * frame_beat_delta`, computed from the BPM and the tick's `dt_seconds` (or `export_fixed_dt`). This is wide enough to absorb f32 round-trip and playhead-line-on-edge cases, but narrow enough that a full frame past the boundary correctly leaves the clip inactive.

### D4: do not change `Selection::contains_beat`
Selection is a beat interval for operations, not a visual sample. Its half-open semantics stay as-is.

---

## 3. Design body

### 3.1 Computing the tolerance
```
frame_beat_delta = (bpm / 60.0) * dt_seconds
boundary_epsilon = 0.5 * frame_beat_delta
```

Use f64. The BPM comes from the project settings already synced for `current_beat`.

### 3.2 Timeline query change
`Timeline::get_active_clips_at_beat_ref` and `Layer::collect_active_clips_at_beat` accept a `boundary_epsilon: Beats` parameter. A clip is a candidate if:

```
beat + epsilon >= clip.start_beat && beat - epsilon < clip.end_beat()
```

If a layer returns more than one candidate (only possible at a boundary between two adjacent clips), select the one with the later `start_beat`. If still ambiguous, prefer the shorter clip. This preserves non-overlap semantics everywhere except the boundary window.

Call sites:
- `engine.rs:1085` `query_active_timeline_clips` — pass the computed epsilon and use the same epsilon in `filter_ready_clips`.
- All other callers pass `Beats::ZERO` to keep exact semantics.

### 3.3 Video time at the boundary
`compute_video_time` continues to use `current_beat`. With the boundary tolerance, the clip stays active at `current_beat == end`, so `source_elapsed` reaches the media duration. The video renderer clamps to the last decoded frame; if a specific decoder returns black at exact EOF, that is a decoder clamp issue, not this seam.

### 3.4 Audio unchanged
`Layer::active_audio_clip_at` keeps using exact `is_active_at_beat`. Audio should not hold a boundary note beyond its end.

### 3.5 Sync and trigger invariants
- `sync_clips_to_time` uses exact `current_beat` for start/stop diff.
- Clip edge triggers fire at exact boundaries.
- OSC/timecode use exact `current_beat`.

The only visual change is which clips appear in `timeline_active_scratch` (and therefore `filter_ready_clips`).

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| I1 | `is_active_at_beat` is unchanged. | `rg 'is_active_at_beat'` shows the same half-open implementation. |
| I2 | Only visual timeline query uses epsilon. | `rg 'get_active_clips_at_beat_ref\|collect_active_clips_at_beat'` — only the engine call passes a non-zero epsilon. |
| I3 | Audio and selection stay exact. | `Layer::active_audio_clip_at` and `Selection::contains_beat` keep their current signatures. |
| I4 | At an adjacent boundary, the later clip wins. | Regression test with A `[0,8)` and B `[8,16)` parked at exactly `8.0` asserts B is active. |
| I5 | A lone clip still owns its end boundary. | Regression test with A `[0,8)` parked at exactly `8.0` asserts A is active. |

---

## 5. Phasing

### P1 — visual boundary tolerance
Add `boundary_epsilon` parameter to the timeline query, plumb the engine's half-frame epsilon, and resolve per-layer boundary ties. Regression tests: parked playhead on start edge, end edge, and adjacent boundary.

Gate: tests pass; no change to audio/session behavior.

### P2 — export first frame and live playback
Pass the same epsilon through the export and playing tick paths. Regression test: export a generator clip whose in-point is at a boundary; assert frame 0 is not black.

Gate: test passes; existing export e2e tests still pass.

### P3 — documentation and bead closure
Update this design doc status to SHIPPED. Close BUG-h2o8 (MANIFOLD social export renders black first frame at clip boundary).

Gate: bead closed; design status updated; supersession sweep.

---

## 6. Decided — do not reopen

- D1: boundary ownership rule for the visual timeline query.
- D2: logical `current_beat` unchanged for triggers/audio/transport.
- D4: `Selection::contains_beat` unchanged.

## 7. Deferred

- Extend boundary ownership to the playhead snap / timeline ruler if P1 shows the underlying UI is parking the playhead a full frame past the boundary rather than on the edge. Trigger: user still reports black after P1 lands.
