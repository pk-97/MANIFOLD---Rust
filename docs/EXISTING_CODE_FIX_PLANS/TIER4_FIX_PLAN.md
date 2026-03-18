# Tier 4 Fix Plan: `manifold-ui` + `manifold-app` Parity Remediation

**Status: COMPLETE** — Implemented 2026-03-18, commit `4bfb98e`

**Generated:** 2026-03-18 from line-by-line audit of all Unity UI/*.cs and WorkspaceController*.cs against Rust manifold-ui/src/*.rs and manifold-app/src/*.rs

**Methodology:** Every fix below references the exact Unity source file and line numbers. The implementing agent MUST read the Unity source — not this document — as the source of truth. This plan tells you WHAT to fix and WHERE to look, not HOW the code should read.

**Dependency:** Tier 0 (manifold-core) and Tier 1 (manifold-editing, manifold-playback) fixes should be completed first. Several fixes here depend on EffectContainer trait, ParamSource trait, and EditingService high-level operations.

---

# PART A: `manifold-ui`

---

## Phase 1: Color & Constant Value Corrections (CRITICAL — Visual Parity)

### 1A. Fix `GEN_TYPE_LABEL` color — WRONG COLOR

**File:** `crates/manifold-ui/src/color.rs` line 75
**Unity source:** `UIConstants.cs` — `GeneratorTypeLabel = Color(0.55, 0.70, 0.95, 1)`

**Bug:** Rust has `Color32(150, 200, 150, 255)` (GREEN). Unity has `Color32(140, 179, 242, 255)` (BLUE-PURPLE).

**Fix:** Change to `Color32(140, 179, 242, 255)`.

### 1B. Fix `TRACK_BG` color

**File:** `color.rs`
**Unity source:** `UIConstants.cs` — `TrackBackground = Color(0.14, 0.14, 0.145, 1)`

**Bug:** Rust `Color32(26, 26, 27, 255)`. Unity converts to `Color32(36, 36, 37, 255)`.

**Fix:** Change to `Color32(36, 36, 37, 255)`.

### 1C. Fix `TEXT_NORMAL` / `TEXT_PRIMARY_C32` blue channel

**File:** `color.rs`
**Unity source:** `UIConstants.cs` — `TextNormal = Color(0.88, 0.88, 0.90, 1)`

**Bug:** Rust `Color32(224, 224, 224, 255)`. Unity converts to `Color32(224, 224, 230, 255)` — blue channel 230, not 224.

**Fix:** Change to `Color32(224, 224, 230, 255)`.

### 1D. Fix `ABLETON_LINK_BLUE` color

**File:** `color.rs`
**Unity source:** `UIConstants.cs` — `AbletonLinkBlue = Color(0.22, 0.52, 0.70, 1)`

**Bug:** Rust `Color32(89, 173, 232, 255)`. Unity converts to `Color32(56, 133, 179, 255)`.

**Fix:** Change to `Color32(56, 133, 179, 255)`.

### 1E. Fix `BPM_RESET_ACTIVE` color

**File:** `color.rs`
**Unity source:** `UIConstants.cs` — `BpmResetActive = Color(0.20, 0.42, 0.24, 1)`

**Bug:** Rust green channel 163. Unity converts to green channel 107: `Color32(51, 107, 61, 255)`.

**Fix:** Change to `Color32(51, 107, 61, 255)`.

### 1F. Fix `DEFAULT_INSPECTOR_WIDTH`

**File:** `color.rs` line 373
**Unity source:** `UIConstants.cs` — `DefaultInspectorWidth = MaxInspectorWidth = 500f`

**Bug:** Rust uses `280.0`. Unity defaults to `500.0`.

**Fix:** Change to `500.0`.

### 1G. Fix `SCROLL_SPEED`

**File:** `scroll_container.rs`
**Unity source:** `BitmapScrollContainer.cs` — `const float SCROLL_SPEED = 30f`

**Bug:** Rust `SCROLL_SPEED = 12.5`. Unity uses `30.0`. Scrolling is 2.4x slower.

**Fix:** Change to `30.0`. Also verify scroll delta normalization — Unity divides raw scroll delta by 120 before multiplying by speed. If winit already normalizes differently, the speed constant may need adjustment. The key is that the PERCEIVED scroll speed should match.

---

## Phase 2: Missing Layout Constants

### 2A. Port `WidgetLayout` constants

**Unity source:** `WidgetLayout.cs` (~30+ constants)
**Rust file:** New section in `color.rs` or new file `widget_layout.rs`

Key constants needed:
- `Scale = 1.25f` — global UI scale multiplier
- Slider dimensions (height, track height, thumb size)
- Driver config dimensions
- Dropdown dimensions
- Scrollbar thickness
- Effect card structure (height, padding, button sizes)
- Font sizes for each context (label, value, header, etc.)

Read the FULL Unity file and port ALL constants.

### 2B. Port `InspectorLayout` constants

**Unity source:** `InspectorLayout.cs` (~15+ constants)
**Rust file:** New section in `color.rs` or new file `inspector_layout.rs`

Key constants:
- Section header height
- Padding values
- Spacing between elements
- Label widths
- Value field widths

These are REQUIRED for correct inspector panel rendering.

---

## Phase 3: InteractionOverlay Fixes (HIGH — Behavioral Bugs)

### 3A. Fix `ctrl_held()` — always returns false

**File:** `interaction_overlay.rs` lines 1031-1033
**Unity source:** `InteractionOverlay.cs` line 790-793

**Bug:** `ctrl_held()` is a placeholder that always returns `false`. This means Ctrl+drag region selection (additive selection that preserves existing selection) is completely broken.

**Fix:** Wire `ctrl_held()` to actual keyboard modifier state. The overlay receives `Modifiers` in its event methods — store the latest modifier state and check `ctrl` (or `cmd` on macOS).

### 3B. Fix `finalize_move_snap` — uses wrong start beat

**File:** `interaction_overlay.rs` line 897-899
**Unity source:** `InteractionOverlay.cs` line 760

**Bug:** Rust looks up `anchor_start` from snapshots (the ORIGINAL position before drag began). Unity uses `dragAnchorClip.StartBeat` which is the clip's CURRENT position (after being moved during drag). The finalize snap should use the current position to calculate the final snapped position.

**Fix:** Change to use the clip's current start beat from `host.find_clip_by_id()`, not the snapshot's original start beat.

### 3C. Verify `magnetic_snap` comparison space

**File:** `snap.rs` — magnetic_snap_beat implementation
**Unity source:** `InteractionOverlay.cs` lines 903-953

**Issue:** Unity's `MagneticSnapBeat` works in PIXEL distance (converts beat distance to pixels for threshold comparison). Rust's `magnetic_snap_beat` works in BEAT distance (converts pixel threshold to beats).

**Verify:** Both approaches should produce equivalent results IF the beat-to-pixel conversion is correct. Test with various zoom levels to confirm snap behavior matches.

### 3D. Add `preDragSplitCommands` tracking

**File:** `interaction_overlay.rs`
**Unity source:** `InteractionOverlay.cs` lines 69, 430-433

**Bug:** Rust does not track `pre_drag_split_commands`. In Unity, when a region drag starts and clips are split at region boundaries, the split commands are prepended to the undo composite so that undoing the move also undoes the splits.

**Fix:** Add `pre_drag_split_commands: Vec<Box<dyn Command>>` field. When `host.split_clips_for_region_move()` is called, capture the returned split commands. When the move is committed, prepend them to the composite command.

---

## Phase 4: Missing UI Components

### 4A. Port `TruncateWithEllipsis`

**File:** `text.rs`
**Unity source:** `BitmapText.cs` — `TruncateWithEllipsis` method

**Bug:** Entirely missing. Any text that exceeds its container width is not truncated, potentially overlapping other elements.

**Fix:** Port the progressive trim algorithm — measure text width, if exceeding max width, remove characters from end and append "..." until it fits.

### 4B. Add `Texture` field to UINode

**File:** `node.rs`
**Unity source:** `UINode.cs` — `Texture Texture` field

**Bug:** Missing field. Prevents creation of texture-bearing nodes (images, thumbnails).

**Fix:** Add `pub texture: Option<TextureHandle>` field (or equivalent) to `UINode`. Add corresponding `add_image` method to `UITree` matching Unity's `AddNode` overload that accepts a `Texture`.

### 4C. Fix right-click event on empty areas

**File:** `input.rs` — `process_right_click`
**Unity source:** `UIInputSystem.cs`

**Bug:** Unity only fires right-click event if `hitId >= 0`. Rust ALWAYS fires the event (using `u32::MAX` for miss). This means right-click handlers in Rust receive spurious events when clicking empty areas.

**Fix:** Only fire `UIEvent::RightClick` when a valid node is hit (matching Unity's guard).

---

## Phase 5: ScrollContainer Alignment

### 5A. Add `ScrollToReveal` method

**File:** `scroll_container.rs`
**Unity source:** `BitmapScrollContainer.cs` — `ScrollToReveal()` method

Port the auto-scroll logic that ensures a given item remains visible.

### 5B. Add Section-based architecture

**File:** `scroll_container.rs`
**Unity source:** `BitmapScrollContainer.cs` — `Section` inner class

Unity's scroll container has sections with `GetHeight`, `Build`, `Update`, `HandleClick`, and `Visible` properties. Consider adding equivalent functionality or document that sections are managed externally in Rust.

### 5C. Verify scroll delta normalization

**Unity source:** `BitmapScrollContainer.cs` — `PollScroll()` divides scroll delta by 120 (per-notch)
**Rust concern:** winit may provide differently-scaled scroll deltas

Verify that `apply_scroll_delta` produces the same perceived scroll speed as Unity after fixing the speed constant in Phase 1G.

---

# PART B: `manifold-app`

---

## Phase 6: Input Handler Fixes

### 6A. Add Numpad0 for mute toggle

**File:** `input_handler.rs` line 293
**Unity source:** `InputHandler.cs` line 420

**Bug:** Only matches `"0"` character. Unity also handles `Key.Numpad0`.

**Fix:** Add numpad zero handling. Verify winit's key representation for numpad keys.

### 6B. Add percussion import shortcuts (when subsystem is ported)

**File:** `input_handler.rs`
**Unity source:** `InputHandler.cs` lines 262-286

**Missing shortcuts:**
- `Cmd+Shift+I` — Open percussion import
- `Cmd+Shift+M` — Mark percussion beat
- `Cmd+Shift+[` — Nudge percussion left
- `Cmd+Shift+]` — Nudge percussion right
- `Cmd+Shift+R` — Reset percussion alignment

**Note:** These depend on the `PercussionImportOrchestrator` system which is not yet ported. Add the shortcuts when the subsystem is available. For now, document as known gap.

---

## Phase 7: Input Host / Editing Host Fixes

### 7A. Fix `on_undo_redo` missing post-undo refresh

**File:** `input_host.rs` line 58
**Unity source:** `WorkspaceController.cs` lines 378-386

**Bug:** Rust only calls `mark_compositor_dirty()` and sets flags after undo/redo. Unity also calls:
- `RefreshAllInspectors()`
- `ApplyProjectResolutionFromFooter()` — re-applies resolution to render pipeline
- `ApplyProjectFpsFromFooter()` — re-applies FPS

**Fix:** After undo/redo, if the project's resolution or FPS changed, re-apply those settings to the render pipeline. At minimum, set a flag that triggers resolution/FPS re-application in the next tick.

### 7B. Fix `beat_to_time` to use tempo map

**File:** `editing_host.rs` line 143-151
**Unity source:** `WorkspaceController.cs` line 665-670

**Bug:** Rust uses simple `beat * 60.0 / bpm`. Unity delegates to `playbackController.TimelineBeatToTime()` which goes through the full tempo map.

**Fix:** Use `TempoMapConverter::beat_to_seconds()` instead of simple BPM division. The `&mut self` constraint on `TempoMap` methods can be worked around by calling `ensure_sorted()` before the editing host needs the tempo map, or by using the `_immut` variant if the map is known to be sorted.

### 7C. Wire `get_max_duration_beats` to video library

**File:** `editing_host.rs` line 472-476
**Unity source:** `WorkspaceController.cs` — returns max duration from video library metadata

**Bug:** Returns `0.0` always. This means clip trim operations have no upper bound from video source length.

**Fix:** Wire to video library when available. For now, document as known gap requiring video library integration.

---

## Phase 8: Context Menu Completeness

### 8A. Add missing layer context menu items

**File:** `ui_root.rs` lines 416-427
**Unity source:** `InputHandler.cs` lines 704-787

**Missing items:**
- Paste (clips from clipboard)
- Import MIDI File
- Group Selected Layers
- Ungroup

**Fix:** Add these menu items and wire them to the corresponding EditingService methods.

### 8B. Add Import MIDI File to track context menu

**File:** `ui_root.rs` — `TrackRightClicked` handler
**Unity source:** `InputHandler.cs` lines 838-858

**Missing:** Import MIDI File option in empty track area right-click.

---

## Phase 9: File Drop Handling

### 9A. Improve file drop routing

**File:** `app.rs` lines 1949-1975
**Unity source:** `WorkspaceController.cs` (ProjectIO partial) lines 37-47

**Current state:** Rust handles project file drops (loads project). Video/audio/MIDI file drops are stubbed.

**Fix (when subsystems are available):**
1. Route video files through video library import
2. Route MIDI files through MIDI import
3. Route audio files through percussion import
4. Resolve drop placement (beat + layer from cursor position at drop time)

### 9B. Add file drop preview

**File:** `app.rs` line 1976-1978
**Unity source:** `WorkspaceController.cs` (ProjectIO partial) lines 49-117

**Current state:** `HoveredFile` only logs.

**Fix:** Show outline preview during file drag-over, matching Unity's DaVinci-style preview. This requires coordinate conversion from screen position to beat+layer during hover.

---

## Phase 10: Missing LateUpdate Subsystems (Deferred)

These depend on unported subsystems. Document as known gaps:

1. **Tempo lane editor** — requires tempo lane UI
2. **Grid overlay** — requires grid rendering infrastructure
3. **Overview strip** (mini timeline) — requires overview strip panel
4. **Imported audio waveform lane** — requires audio analysis
5. **Stem lane group** — requires stem import system

---

## Verification Checklist

After implementing all phases:

- [ ] `GEN_TYPE_LABEL` color is blue-purple `(140, 179, 242, 255)`, not green
- [ ] `TRACK_BG` color is `(36, 36, 37, 255)`, not `(26, 26, 27, 255)`
- [ ] `TEXT_NORMAL` blue channel is 230, not 224
- [ ] `ABLETON_LINK_BLUE` is `(56, 133, 179, 255)`
- [ ] `BPM_RESET_ACTIVE` green channel is 107, not 163
- [ ] `DEFAULT_INSPECTOR_WIDTH` is 500.0, not 280.0
- [ ] `SCROLL_SPEED` is 30.0, not 12.5
- [ ] `ctrl_held()` returns actual modifier state
- [ ] `finalize_move_snap` uses clip's current start beat, not snapshot original
- [ ] `TruncateWithEllipsis` is implemented
- [ ] Right-click only fires on valid node hits
- [ ] `on_undo_redo` re-applies resolution/FPS if changed
- [ ] `beat_to_time` uses tempo map, not simple BPM division
- [ ] Layer context menu has Paste, Import MIDI, Group, Ungroup
- [ ] `cargo build` succeeds for manifold-ui and manifold-app
- [ ] `cargo test` passes for both crates

---

## Priority Order

**P0 — Visual/behavioral bugs:**
1. Phase 1A: GEN_TYPE_LABEL wrong color (green instead of blue)
2. Phase 3B: finalize_move_snap wrong start beat
3. Phase 3A: ctrl_held() always false
4. Phase 1B-1F: Color/constant value corrections

**P1 — Missing critical UI functionality:**
5. Phase 4A: TruncateWithEllipsis
6. Phase 7A: on_undo_redo missing refresh
7. Phase 7B: beat_to_time uses wrong conversion
8. Phase 1G: SCROLL_SPEED 2.4x too slow

**P2 — Missing constants/layout:**
9. Phase 2A: WidgetLayout constants
10. Phase 2B: InspectorLayout constants

**P3 — Completeness:**
11. Phase 8: Context menu items
12. Phase 4B: UINode texture field
13. Phase 4C: Right-click empty area filter
14. Phase 9: File drop handling

**P4 — Deferred (subsystem dependencies):**
15. Phase 6B: Percussion shortcuts
16. Phase 9B: File drop preview
17. Phase 10: LateUpdate subsystems

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `manifold-ui/src/color.rs` | Fix 6 color values, fix DEFAULT_INSPECTOR_WIDTH |
| `manifold-ui/src/interaction_overlay.rs` | Fix ctrl_held, fix finalize_move_snap, add preDragSplitCommands |
| `manifold-ui/src/text.rs` | Add TruncateWithEllipsis |
| `manifold-ui/src/node.rs` | Add texture field |
| `manifold-ui/src/input.rs` | Fix right-click empty area guard |
| `manifold-ui/src/scroll_container.rs` | Fix SCROLL_SPEED, add ScrollToReveal |
| (new) `manifold-ui/src/widget_layout.rs` | Port WidgetLayout constants |
| (new) `manifold-ui/src/inspector_layout.rs` | Port InspectorLayout constants |
| `manifold-app/src/input_handler.rs` | Add Numpad0, document percussion gaps |
| `manifold-app/src/input_host.rs` | Fix on_undo_redo refresh |
| `manifold-app/src/editing_host.rs` | Fix beat_to_time, wire get_max_duration_beats |
| `manifold-app/src/ui_root.rs` | Add missing context menu items |
| `manifold-app/src/app.rs` | Improve file drop routing |
