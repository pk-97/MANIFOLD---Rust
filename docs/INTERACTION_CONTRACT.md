# MANIFOLD Interaction Contract — Complete Behavioral Specification

> **Date:** 2026-03-16
> **Extracted from:** Unity MANIFOLD source code (canonical implementation)
> **Purpose:** Machine-testable behavioral specification for every user interaction. Each behavior has explicit triggers, state transitions, thresholds, and visual feedback documented. AI agents implementing the Rust port MUST match these behaviors exactly.

---

## Table of Contents

1. [Input Architecture](#1-input-architecture)
2. [Constants & Thresholds](#2-constants--thresholds)
3. [Bitmap UI State Machine](#3-bitmap-ui-state-machine)
4. [Clip Move Drag](#4-clip-move-drag)
5. [Clip Trim Drag](#5-clip-trim-drag)
6. [Region Selection Drag](#6-region-selection-drag)
7. [Playhead / Ruler Scrub](#7-playhead--ruler-scrub)
8. [Insert Cursor](#8-insert-cursor)
9. [Click Behaviors (Non-Drag)](#9-click-behaviors-non-drag)
10. [Hover Behavior](#10-hover-behavior)
11. [Scroll & Zoom](#11-scroll--zoom)
12. [Magnetic Snap System](#12-magnetic-snap-system)
13. [Slider Interactions](#13-slider-interactions)
14. [Effect Card Drag Reorder](#14-effect-card-drag-reorder)
15. [Effect Card Selection](#15-effect-card-selection)
16. [Driver Inline Editing](#16-driver-inline-editing)
17. [Envelope Inline Editing](#17-envelope-inline-editing)
18. [Rack Header Interactions](#18-rack-header-interactions)
19. [Layer Header Interactions](#19-layer-header-interactions)
20. [Transport Bar Interactions](#20-transport-bar-interactions)
21. [Context Menus](#21-context-menus)
22. [Dropdown Panel](#22-dropdown-panel)
23. [Effect Browser Popup](#23-effect-browser-popup)
24. [Text Input](#24-text-input)
25. [Cursor System](#25-cursor-system)
26. [Selection Model](#26-selection-model)
27. [Undo Commit Patterns](#27-undo-commit-patterns)
28. [Keyboard Shortcuts (Complete State Machine)](#28-keyboard-shortcuts)
29. [Key Invariants for Rust Port](#29-key-invariants)
30. [Rust Port Gaps vs This Contract](#30-rust-port-gaps)

---

## 1. Input Architecture

The Unity version has two parallel input systems. The Rust port unifies these into a single `UIInputSystem`.

**System A — UGUI EventSystem:** Used by `InteractionOverlay` (timeline viewport clips), `RulerScrubHandler`, `PanelResizeHandle`. Implements Unity's `IPointerClickHandler`, `IDragHandler`, etc.

**System B — Bitmap UI:** Custom retained-mode UI tree rendered to RenderTextures. `UIBitmapRoot.Update()` reads mouse/keyboard each frame and routes to per-panel `UIInputSystem` instances. Used by all inspector panels, transport bar, layer headers, dropdown, browser popup, effect cards.

**Keyboard dispatch:** `InputHandler.HandleKeyboardInput()` reads keyboard directly every frame. Suppressed when a text input field is focused.

**Key design rule:** Pointer events during a drag are ALWAYS routed to the panel that received the PointerDown, even if the cursor leaves that panel's bounds.

---

## 2. Constants & Thresholds

### Interaction Thresholds

| Constant | Value | Used By |
|----------|-------|---------|
| `DRAG_THRESHOLD` | 4px (bitmap UI) / 10px (UGUI default) | Drag initiation |
| `DOUBLE_CLICK_TIME` | 0.3 seconds | Double-click detection |
| `SNAP_THRESHOLD_PX` | 12px | Magnetic snap to grid + clip edges |
| `MAX_SNAP_BEATS` | 0.5 beats | Cap snap range at extreme zoom-out |
| `TRIM_HANDLE_WIDTH_PX` | 8px per edge | Clip trim handle hit area |
| `MIN_TRIM_HANDLES_WIDTH` | 16px (2 x 8px) | Below this, no trim handles shown |
| `MIN_CLIP_DURATION` | 0.25 beats (1/16 note) | Minimum after trim/resize |
| `DRAG_EDGE_SCROLL_ZONE` | 72px from viewport edge | Auto-scroll trigger zone during drag |
| `DRAG_EDGE_SCROLL_SPEED` | 900 px/sec (at max intensity) | Auto-scroll speed |
| `SHIFT_SCROLL_SPEED` | 0.67 pixels per scroll unit | Horizontal scroll rate |
| `INSPECTOR_SCROLL_SPEED` | 12.5 px per scroll unit | Inspector panel scroll |
| `PLAYHEAD_AUTO_SCROLL_TRIGGER` | 50px from right viewport edge | Playback auto-scroll |
| `PLAYHEAD_AUTO_SCROLL_TARGET` | 25% from left edge | Where playhead lands after scroll |

### Layout Constants

| Constant | Value |
|----------|-------|
| `CLIP_VERTICAL_PADDING` | 12px within track |
| `TRACK_HEIGHT` | 140px |
| `INSERT_CURSOR_WIDTH` | 2px |
| `PLAYHEAD_WIDTH` | 2px |
| `RULER_HEIGHT` | 24px |

### Zoom Levels (pixels per beat)

```
[1, 2, 5, 10, 20, 40, 80, 120, 200, 400]
Default: index 7 = 120 ppb
```

---

## 3. Bitmap UI State Machine

### State Variables
- `hoveredId` (int, -1 = none) — node under cursor
- `pressedId` (int, -1 = none) — node that received mouse-down
- `focusedId` (int, -1 = none) — keyboard focus target
- `isDragging` (bool)
- `pressOrigin` (Vec2) — screen position at mouse-down
- `lastDragPos` (Vec2) — for computing deltas
- `lastClickTime` / `lastClickId` — double-click detection

### State Transitions

```
PointerDown:
  pressedId = hitId
  store pressOrigin, lastDragPos
  isDragging = false
  fire OnPointerDown(hitId)
  set focusedId = hitId

PointerMove (while pressed):
  if !isDragging and distance(pos, pressOrigin) >= DRAG_THRESHOLD:
    isDragging = true
    fire OnDragBegin(pressedId, pos, pressOrigin)
  if isDragging:
    fire OnDrag(pressedId, pos, delta)

PointerUp:
  if isDragging:
    fire OnDragEnd(pressedId, pos)
  else if hitId == pressedId:
    fire OnClick(hitId, pos)
    if same hitId within DOUBLE_CLICK_TIME:
      fire OnDoubleClick(hitId, pos)
  clear pressedId, isDragging

ApplicationFocusLost:
  synthesize PointerUp (cancel in-progress drags)
```

### Right-Click
Hit-tests at position, fires `OnRightClick(hitId, pos)` if a node is hit.

---

## 4. Clip Move Drag

### Trigger
`OnBeginDrag` when hit test at `pressPosition` returns `HitRegion.Body` on an unlocked clip.

### Anchor Offset
```
DragOffsetBeats = mouseBeat - clip.StartBeat
```
Captured once at drag start. During drag: `targetBeat = mouseBeat - DragOffsetBeats`.

### Per-Frame During Drag
1. `AutoScrollTimelineForDrag(screenPos)` — edge auto-scroll (72px zone, 900px/s max, linear ramp)
2. Compute target layer from screen Y position
3. Layer delta clamped to keep all selected clips within valid range
4. **Type compatibility check:** If any clip would land on incompatible layer (generator vs video), `layerDelta = 0`, cursor = `Cursors.SetBlocked()` (prohibition circle)
5. Move clips to new layers if delta != 0
6. Compute beat position via `MagneticSnapBeat()` (see section 12)
7. Floor clamp: no clip goes below beat 0 (`beatDelta = max(beatDelta, -minStartBeat)`)
8. Apply to all selected clips uniformly
9. Invalidate all layer bitmaps

### Multi-Clip Move
All selected clips move as a group, maintaining relative positions. Anchor clip = the one directly clicked.

### Visual Feedback
- Clips follow cursor directly (no ghost/shadow)
- Cursor: `SetMove()` (4-direction cross) normally, `SetBlocked()` (prohibition) when type-incompatible

### On Drag End
1. `MoveClipCommand` for each clip that moved (position or layer differs from snapshot)
2. Overlap enforcement (DaVinci-style, see section 4.1)
3. Bundle into `CompositeCommand` for single undo step
4. Reset cursor to default

### 4.1 DaVinci-Style Overlap Enforcement

Applied to every clip that was placed during drag/trim. Four cases:

| Case | Condition | Action |
|------|-----------|--------|
| Full cover | Placed clip fully covers existing | Delete existing clip |
| Cover start | Placed clip covers start of existing | Trim existing start to `placed.EndBeat`, advance InPoint |
| Cover end | Placed clip covers end of existing | Trim existing to `placed.StartBeat - existing.StartBeat` |
| Split | Placed clip sits inside existing | Trim existing to left portion, create tail clip |

### 4.2 Region-Partial Move
When a region selection is active and the user clicks a clip inside the region to drag:
1. Split straddling clips at region boundaries before dragging
2. Pre-drag split commands stored for composite undo
3. Only interior segments become the drag set

---

## 5. Clip Trim Drag

### Trigger
`OnBeginDrag` when hit test returns `HitRegion.TrimLeft` or `HitRegion.TrimRight`.

### Trim Handle Visibility
- Each handle is 8px wide from the clip edge
- Only active when clip width > 16px (2 x 8)
- Below 16px: entire clip = Body (move only)

### Left Trim Behavior
```
newStartBeat = MagneticSnapBeat(mouseBeat)
if video_clip:
  newStartBeat = max(originalStartBeat, newStartBeat)  // can't extend left
if generator_clip:
  // can extend left freely
newStartBeat = min(newStartBeat, originalEndBeat - 0.25)  // min duration
inPoint = max(0, originalInPoint + (newStartBeat - originalStartBeat) * spb)
duration = originalEndBeat - newStartBeat
```

### Right Trim Behavior
```
newEndBeat = MagneticSnapBeat(mouseBeat)
newEndBeat = max(clip.StartBeat + 0.25, newEndBeat)  // min duration
if video_clip and !looping:
  maxDuration = (videoLength - clip.InPoint) / spb
  newEndBeat = min(clip.StartBeat + maxDuration, newEndBeat)
if generator_clip or looping_video:
  // no max limit
duration = newEndBeat - clip.StartBeat
```

### Cursor Feedback
Trim handle hover or active trim: `Cursors.SetResizeHorizontal()` (horizontal double-arrow).

### Undo Semantics
- Snapshot at drag start: `originalStartBeat`, `originalDurationBeats`, `originalInPoint`
- On drag end: `TrimClipCommand(original values, new values)`
- Overlap enforcement applied after trim

### Duration Tooltip
**Not implemented** in Unity. No tooltip UI during trim drag.

---

## 6. Region Selection Drag

### Trigger
`OnBeginDrag` when no clip is hit at `pressPosition`.

### Behavior
- Anchor is at `pressPosition` (where user clicked, not where drag threshold tripped)
- Both edges grid-snapped via `SnapBeatToGrid()`
- Live update: `SetRegion()` called every frame during drag
- Region rendered as translucent blue overlay spanning full track height

### Clip Selection
**Partial overlap**: any clip where `clip.EndBeat > region.MinBeat && clip.StartBeat < region.MaxBeat` is selected.

### Modifier Keys
- **Ctrl/Cmd held at drag start:** preserves existing selection (additive)
- **Without Ctrl:** `ClearSelection()` before starting region

---

## 7. Playhead / Ruler Scrub

### Click on Ruler
Instant seek. `ScrubToPosition()` converts screen X to beat, grid-snaps, converts to time, calls `Seek(time)`.

### Drag on Ruler
Continuous scrub. `ScrubToPosition()` called every frame while pressed.

### Alt+Click
Free scrub (no grid snap). Also auto-enabled at maximum zoom level.

### During Playback
Works — calls `Seek()` unconditionally (play state doesn't change).

### Cursor
System default (no custom cursor during scrub).

---

## 8. Insert Cursor

### Setting the Cursor
Single click on empty timeline space (no clip hit, no Shift held). Beat is grid-snapped.

### What It Clears
Setting insert cursor clears: clip selection, layer selection, region selection.

### Arrow Key Navigation (when no clips selected)
- **Left/Right:** moves by grid step (Shift = 1/16 beat)
- **Up/Down:** moves to adjacent layer, skipping zero-height (collapsed) layers
- **Auto-select:** if a clip exists at the new position, it is selected instead of placing cursor
- Beat clamped >= 0, layer clamped to valid range

### Relationship to Play
Pressing Space when insert cursor is set: seeks to cursor beat, then plays.

### Relationship to Paste
Insert cursor position (`beat + layer`) is used as paste target.

### Visual Appearance
2px vertical line, color `InsertCursorBlue = rgba(0.35, 0.58, 0.95, 0.9)`, spans full track height. Only drawn on the active layer.

**Note:** No triangle indicator in current Unity implementation despite the USER_GUIDE spec mentioning one.

---

## 9. Click Behaviors (Non-Drag)

These fire when the mouse is released without exceeding the drag threshold.

| Action | Behavior |
|--------|----------|
| Click clip | Select single clip, clear region/cursor, show ClipInspector |
| Cmd+Click clip | Toggle clip in/out of multi-selection |
| Shift+Click clip | Extend region from anchor to clip bounds |
| Click empty space | Grid-snap beat, set insert cursor, clear selection |
| Shift+Click empty | Extend region from anchor to clicked position |
| Double-click empty | Create clip at grid-snapped position, select it |
| Right-click clip | Select if not selected, show clip context menu |
| Right-click empty/layer | Show layer context menu |
| Click locked clip | Ignored entirely |

---

## 10. Hover Behavior

### Timeline Viewport
- Updates `HoveredClipId` continuously on pointer move
- Triggers bitmap repaint on affected layers when hover changes
- Hovered clips render with brighter color variant

### Cursor Changes (when not dragging)
| Hit Region | Cursor |
|------------|--------|
| TrimLeft / TrimRight | `SetResizeHorizontal()` (double-arrow) |
| Body | `SetMove()` (4-direction cross) |
| No hit | `SetDefault()` (system arrow) |

### Pointer Exit
Clears hovered clip, resets cursor to default.

---

## 11. Scroll & Zoom

### Mouse Wheel (no modifier)
Vertical scroll through layers (scroll rect default).

### Shift+Scroll
Horizontal pan. Speed: 0.67 pixels per scroll unit, normalized to content width.

### Alt+Scroll
Zoom in/out. **Anchored to playhead**: captures playhead's viewport X position before zoom, repositions scroll after zoom to keep playhead at same screen position.

### Zoom to Fit (F key)
1. Find extent of all clips (min start, max end)
2. Add 10% padding (minimum 1 beat per side)
3. `idealPpb = viewportWidth / fitBeats`
4. Clamp to [1, max zoom level]
5. Can hit intermediate values (not restricted to the 10 discrete levels)
6. Scroll to center on clip extent

### Auto-Scroll During Playback
- Trigger: playhead within 50px of right viewport edge
- Target: scroll so playhead is at 25% from left edge
- Also triggers if playhead within 20px of left edge

### Auto-Scroll During Drag
- Edge zone: 72px from each viewport edge
- Speed: linear ramp, 0-900 px/sec (closer to edge = faster)
- Polled every frame while `IsDragging` is true (continues even with stationary mouse)

### Scroll Limits
Cannot scroll past start or end of content. Content width dynamically extends to include playhead + one viewport width.

---

## 12. Magnetic Snap System

Used by clip move, clip trim, and insert cursor positioning.

### Algorithm
```
effective_threshold = min(SNAP_THRESHOLD_PX, MAX_SNAP_BEATS * ppb)

candidates = []
candidates.push(nearest_grid_line)           // Round(beat / gridInterval) * gridInterval
candidates.push(neighbor_clip_start_edges)   // All clips on same layer (excluding self)
candidates.push(neighbor_clip_end_edges)     // All clips on same layer (excluding self)

closest = candidate with minimum distance in pixels
if distance(closest) <= effective_threshold:
  return closest  // snap
else:
  return raw_beat  // no snap
```

### Grid Interval (zoom-dependent)
| ppb | Grid Interval |
|-----|---------------|
| >= 16 | 0.25 beats (16th notes) |
| >= 12 | 0.5 beats (8th notes) |
| >= 6 | 1.0 beat (quarter notes) |
| < 6 | beatsPerBar (full bars) |

### What Uses Snap
- Clip move drag
- Clip trim drag (left and right)
- Insert cursor click placement
- Region selection edges

---

## 13. Slider Interactions

### Architecture
`BitmapSlider` is a **stateless static helper**. It builds 5 nodes:
1. **Label** (optional, fixed width) — text
2. **Track** (interactive) — drag target
3. **Fill** (non-interactive, child of Track) — from left to value position
4. **Thumb** (non-interactive, child of Track) — 8px bar at value position
5. **ValueText** (interactive) — click for direct numeric entry

The owning panel manages ALL state, events, and undo.

### Drag Flow
```
PointerDown on Track:
  → onSnapshot(paramIndex)          // capture pre-drag value for undo
  → ApplyDragValue(paramIndex, x)   // immediate value update on click
  → pause driver if active          // driver.isPausedByUser = true

During Drag:
  → ApplyDragValue(paramIndex, x)   // continuous update
  → update Fill width, Thumb position, ValueText string

DragEnd:
  → onCommit(paramIndex)            // compare to snapshot, record undo if changed
  → unpause driver
```

### ApplyDragValue Math
```
normalized = clamp((localX - trackX - FILL_INSET) / usableWidth, 0, 1)
if wholeNumbers: value = round(normalized * (max - min) + min); normalized = (value - min) / (max - min)
else: value = normalized * (max - min) + min
```

### Right-Click Reset
Right-click on slider track:
1. `onSnapshot()` — capture for undo
2. Set value to `paramDefault` (from EffectDefinitionRegistry)
3. Mark compositor dirty
4. Update visual
5. `onCommit()` — record undo

### Value Text Click
Click on the ValueText button opens `BitmapTextInput`:
1. Computes screen rect from node bounds
2. Opens native text field with current value
3. On commit: parse float, round if `wholeNumbers`, clamp to [min,max], set param, commit undo

### Modifier Keys on Sliders
**No special Cmd+Click or Shift+Click behavior on sliders.** These modifiers only apply to card-level clicks (when no slider was hit).

### Visual Colors
| Element | Normal | Hover | Pressed |
|---------|--------|-------|---------|
| Track | (40, 40, 42) | (48, 48, 52) | (36, 36, 38) |
| Fill | (50, 70, 100, 120) | — | — |
| Thumb | (180, 200, 230) | — | — |

### Slider Variants

| Location | Label Width | Range | Special |
|----------|------------|-------|---------|
| Effect param | 80px | paramMin..paramMax | wholeNumbers flag per param |
| ADSR (A,D,R) | 17px | 0..8 beats | amber colors |
| ADSR (S) | 17px | 0..1 | amber colors |
| Rack Mix | 24px | 0..1 | — |
| Master Opacity | 50px | 0..1 | — |
| Clip Slip | 52px | 0..maxSlip (seconds) | value formatted as `"{val:.2}s"` |
| Clip Loop Len | 52px | 0..maxLoopBeats | quarter-note snapping: `round(beats * 4) / 4` |
| Gen param | 80px | paramMin..paramMax | wholeNumbers flag per param |

---

## 14. Effect Card Drag Reorder

### Drag Initiation
**Drag handle only** — drag begins only on the hamburger icon node (`IsDragHandle`), not anywhere on the card. If drag begins on a non-handle node, it routes to slider drag instead.

### Ghost Element
- Width: `min(panelWidth - 24, 160)` for cards, `min(panelWidth - 24, 180)` for racks
- Height: 24px
- Background: `(60, 80, 120, 200)` — semi-transparent blue
- Text: `(220, 220, 230, 255)` — effect type name
- Corner radius: 4px
- Follows pointer (clamped within content area)

### Drop Indicator
- 2px tall line, `panelWidth - 8` wide, x=4
- Color: `AccentBlue (89, 148, 235, 255)`
- Positioned at midpoint between entry boundaries

### Source Dimming
- Card border opacity reduced from 255 to 100
- For rack drag: all member cards also dimmed

### Cross-Inspector
**No cross-inspector dragging.** Cards can only be reordered within the same effects list.

### Drop Completion
1. Restore dimming
2. Compute target index from pointer Y
3. Skip if same position (no-op)
4. Fire `ReorderEffectCommand` via EditingService
5. Rebuild cards

### Cancel
No explicit Escape cancel. Application focus loss synthesizes PointerUp.

---

## 15. Effect Card Selection

### Click Behavior
Only fires when no slider/trim/target was hit at the pointer-down position.

| Modifier | Action |
|----------|--------|
| Plain click | Select single, clear others |
| Cmd+Click | Toggle in/out of selection |
| Shift+Click | Range select from anchor to clicked index |

### Visual
- Selected: blue border `(89, 148, 235, 255)`
- Unselected: gray border `(46, 46, 49, 255)`

### Operations on Multi-Selected Cards
Require `inspectorHasFocus = true`:

| Shortcut | Operation |
|----------|-----------|
| Cmd+C | Copy to EffectClipboard |
| Cmd+X | Copy + delete (reverse order) |
| Cmd+V | Paste after last selected, or append |
| Delete | Delete (reverse order, CompositeCommand) |
| Cmd+G | Group (requires 2+), name: "Effect Rack {N}" |
| Cmd+Shift+G | Ungroup (dissolve rack of first selected) |
| Escape | Clear selection + focus |

### Selection Persistence
Before rebuild: save selected EffectInstance references. After: restore by reference match.

---

## 16. Driver Inline Editing

### Opening Driver Config
Click D button (20x20, right of slider row):
- If no driver exists: create `ParameterDriver(paramIndex, BeatDivision.Quarter, Waveform.Sine)`, record `AddDriverCommand`
- If driver exists: toggle `driver.enabled`, record `ToggleDriverEnabledCommand`
- Card height changes — `DRIVER_CONFIG_HEIGHT = 52px` when expanded

### Beat Division Buttons
11 buttons in a row: `1/32, 1/16, 1/8, 1/4, 1/2, 1/1, 2/1, 4/1, 8/1, 16/1, 32/1`
- Click selects division, records `ChangeDriverBeatDivCommand`
- Active button gets teal `(20, 166, 191)`, inactive `(44, 44, 48)`
- Button width = `(availWidth - 10) / 11`, spacing = 1px

### Dot/Triplet Modifiers
Two buttons below beat divisions:
- **Dot "."**: toggles dotted variant. Mutually exclusive with triplet.
- **Triplet "T"**: toggles triplet variant. Mutually exclusive with dot.
- Both record `ChangeDriverBeatDivCommand`

### Waveform Buttons
5 buttons: Sine, Triangle, Sawtooth, Square, Random
- Width: 30px, spacing: 2px
- Click selects, records `ChangeDriverWaveformCommand`
- Active: teal, inactive: gray

### Reverse Toggle
Button "Rev", 32px wide, right-aligned in second row.
- Click toggles `driver.reversed`, records `ToggleDriverReversedCommand`

### Trim Handle Drag
Two draggable bars on the slider track when driver is expanded:
- Min bar (left) and Max bar (right), each 4px wide
- Fill between: `(20, 166, 191, 38)` semi-transparent teal
- Drag: constrain min <= max and max >= min
- Undo: snapshot `(trimMin, trimMax)` on start, `ChangeTrimCommand` on end

### D Button Colors
| State | Color |
|-------|-------|
| Active | (20, 166, 191) teal |
| Active hover | (40, 186, 211) |
| Active pressed | (10, 146, 171) |
| Inactive | (72, 72, 78) |

---

## 17. Envelope Inline Editing

### Opening Envelope Config
Click E button (20x20, right of D button):
- If no envelope: create `ParamEnvelope`, record `AddEnvelopeCommand`
- If exists: toggle `enabled`, record `ToggleEnvelopeEnabledCommand`
- Card height changes — `ENV_CONFIG_HEIGHT = 55px` when expanded

### ADSR Layout
55px container, 2 rows of 2 sliders:
- Row 1: Attack (A) + Decay (D), each half-width
- Row 2: Sustain (S) + Release (R)
- Label width: 17px, font: 8pt
- Ranges: A/D/R = [0, 8] beats, S = [0, 1]
- Amber colors: track `(44,44,48)`, fill `(100,70,30,120)`, thumb `(230,180,100)`

### Target Bar Drag
Amber bar on slider track (6px wide, extends 2px above/below track):
- Color: `(191, 115, 20)`, hover: `(211, 135, 40)`
- Drag: normalizes X to [0,1], updates `env.targetNormalized`
- Undo: snapshot on start, commit on end

### E Button Colors
| State | Color |
|-------|-------|
| Active | (191, 115, 20) amber |
| Active hover | (211, 135, 40) |
| Active pressed | (171, 95, 10) |
| Inactive | (72, 72, 78) |

---

## 18. Rack Header Interactions

| Control | Behavior |
|---------|----------|
| Toggle (ON/OFF) | Toggle group enable |
| Chevron | Collapse/expand rack |
| Ungroup (X) | Dissolve group |
| Name single click | Select all effects in group |
| Name double click | Open rename text input |
| Mix slider drag | Wet/dry [0,1], snapshot/commit pattern |

### Rack Header Layout
- Height: 44px (22px header row + 18px mix row + padding)
- Border: 1px

---

## 19. Layer Header Interactions

### Click Handlers Per Layer
| Control | Action |
|---------|--------|
| Background | Focus layer, show LayerInspector |
| Chevron | Collapse/expand |
| Name label (click) | Focus layer |
| Name label (double-click) | Rename (text input) |
| Mute (M) | Toggle mute |
| Solo (S) | Toggle solo |
| Blend mode | Open dropdown |
| Folder button | Open folder dialog (video layers) |
| +Clip button | Create video clip |
| +Gen button | Create generator clip |
| MIDI input | Open MIDI learn |
| Channel dropdown | Open channel picker |

### Layer Selection
| Modifier | Behavior |
|----------|----------|
| Normal click | Clear all, select one layer |
| Cmd+Click | Toggle layer in/out of selection |
| Shift+Click | Range select from anchor to target |

### Layer Drag Reorder
- Trigger: drag begins on drag handle node
- Visual: source layer dimmed to `(22, 22, 24)`, blue insertion line (2px, `(100, 180, 255)`) at target boundary
- End: fire reorder command if source != target
- Cancel: restore dimming on focus loss

---

## 20. Transport Bar Interactions

All transport buttons are bitmap UI buttons with standard hover/pressed color feedback. No drag behavior. Each button fires a single action on click.

### Save Button Special Behavior
Text contains `*` when dirty -> background changes to `SAVE_DIRTY_BG = (82, 68, 48)` (warm amber).

### BPM Field
Click opens text input. Valid range: 20-300 BPM.

---

## 21. Context Menus

### Clip Context Menu
- Trigger: right-click on clip
- Items: Split at Playhead, Delete, Duplicate

### Layer Context Menu
- Trigger: right-click on layer header OR empty track area
- Items (conditional):
  - Paste (if clipboard has content)
  - Insert Video Layer (always)
  - Insert Generator (always)
  - Import MIDI File (always)
  - Group Selected Layers (if 2+ layers selected)
  - Ungroup (if clicked layer is a group)
  - Delete Layer (if >1 layer exists)

### Dismiss
- Click outside (consumed, not passed through)
- Escape key

---

## 22. Dropdown Panel

- Open: positioned below anchor, edge-clamped. If extends below screen, opens upward.
- Click on item: select, fire callback, close.
- Click outside: auto-dismissed by UIBitmapRoot (click consumed).
- No keyboard navigation in dropdown (items respond to click only).
- Renders as topmost PanelSlot for Z-order.

---

## 23. Effect Browser Popup

- Search bar: text input, case-insensitive filtering
- Category chips: "All" + per-category (effect mode only)
- Grid: scrollable cells, click selects and closes
- Paste button: shown when EffectClipboard has content
- Scroll: mouse wheel, 12.5 px/unit
- Escape: closes
- Click outside: auto-dismissed

---

## 24. Text Input

- `BitmapTextInput.BeginEdit(rect, currentValue, fontSize, onCommit)`: shows native text field
- Commit: Enter key or focus loss
- Cancel: Escape key
- While active: `UIBitmapRoot.SuppressInput = true` (prevents bitmap UI from capturing events)
- Only Escape and NumpadEnter keyboard events pass through to `InputHandler` while text input is active

---

## 25. Cursor System

Four cursor states, procedurally generated at 24x24 logical pixels:

| Cursor | Appearance | Used When |
|--------|-----------|-----------|
| Default | System arrow | No clip hover, idle |
| ResizeHorizontal | Double-arrow (left-right) | Trim handle hover/drag |
| Move | 4-direction cross | Clip body hover/drag |
| Blocked | Prohibition circle (red) | Incompatible cross-layer drag |

---

## 26. Selection Model

### Selection Types (Mutually Exclusive)
1. **Clip selection** — `HashSet<clip_id>`, primary + layer
2. **Layer selection** — `HashSet<layer_id>`, primary
3. **Region selection** — `{startBeat, endBeat, startLayer, endLayer, isActive}`
4. **Insert cursor** — `{beat, layerIndex}`

### Clearing Rules
| Setting | Clears |
|---------|--------|
| SelectClip | Region, insert cursor, layer selection |
| SetInsertCursor | Region, clips, layers |
| SetRegion | Clips, layers, insert cursor |
| ClearSelection | All four |

### Version Counter
`SelectionVersion` incremented on every change — used for dirty-checking by renderers.

### Layer Active Check
A layer is "active" if:
- Explicitly selected via layer header
- A clip on this layer is selected
- Insert cursor is on this layer
- Region selection spans this layer

---

## 27. Undo Commit Patterns

### Snapshot/Commit Pattern (Sliders, Drag)
```
PointerDown → snapshot(current_value)    // capture for undo
Drag        → apply(new_value)           // live visual update, NO undo yet
DragEnd     → commit()                   // compare snapshot vs current, record Command if different
```

### Direct Pattern (Toggles, Clicks)
```
Click → execute(Command)                 // immediate execute + undo recording
```

### Composite Pattern (Multi-Clip Move, Multi-Effect Delete)
```
DragEnd → collect all individual commands
       → wrap in CompositeCommand
       → record as single undo step
```

### What Gets Snapshot
| Operation | Snapshot Fields |
|-----------|----------------|
| Clip move | StartBeat, LayerIndex per clip |
| Clip trim | StartBeat, DurationBeats, InPoint |
| Effect param | paramValues[idx] |
| Opacity | opacity float |
| Trim handles | trimMin, trimMax |
| Envelope target | targetNormalized |
| ADSR | attackBeats, decayBeats, sustainLevel, releaseBeats |

---

## 28. Keyboard Shortcuts

### Modifier Matching Rules
- **Exact match**: bare `S` won't fire if Ctrl is also held
- `Ctrl` maps to both Ctrl AND Cmd keys (macOS)

### Suppression
- When text input is active: only Escape and NumpadEnter pass through

### Priority Chain (Escape)
1. If context menu/dropdown open -> dismiss
2. If monitor output active -> no-op
3. If inspector has focus -> clear effect selection, clear focus
4. Otherwise -> clear all selection + insert cursor

### Context-Sensitive Shortcuts
These check `inspectorHasFocus` to decide effect vs clip scope:

| Shortcut | Inspector Focused | Timeline Focused |
|----------|-------------------|------------------|
| Cmd+C | Copy effects | Copy clips |
| Cmd+X | Cut effects | Cut clips |
| Cmd+V | Paste effects | Paste clips at cursor |
| Delete | Delete effects | Delete clips (or layer if no clips selected) |
| Cmd+G | Group effects | Group layers |
| Cmd+Shift+G | Ungroup effects | Ungroup layers |

### Arrow Key Behavior
- **If clips selected:** nudge by grid step (Left/Right), 1/16 with Shift
- **If no clips selected:** navigate insert cursor (Ableton-style)
  - Auto-selects clip if one exists at new position
  - Sets insert cursor if position is empty
  - Up/Down skips zero-height layers

---

## 29. Key Invariants

1. **Drag threshold: 4px** (bitmap) / 10px (UGUI) before drag begins
2. **Double-click: 0.3s** window
3. **Min clip duration: 0.25 beats** (1/16 note)
4. **Trim handles: 8px** per edge, only when clip > 16px wide
5. **Magnetic snap: 12px** threshold, capped at 0.5 beats
6. **Snap candidates:** grid lines + ALL clip edges on same layer (excluding self)
7. **Undo: snapshot before, commit after** — composite for multi-clip ops
8. **Selection is mutually exclusive:** clip vs layer vs region vs insert cursor
9. **Context-sensitive shortcuts:** inspector focus routes Cmd+C/X/V/Del/G to effects
10. **Modifier matching is exact:** bare S won't fire if Ctrl held
11. **Focus loss synthesizes PointerUp** to cancel drags
12. **Locked clips excluded** from all interactions
13. **Cross-layer: type compatibility enforced** with blocked cursor
14. **Region-partial move splits straddling clips** at boundaries
15. **Video left-trim can't extend past original start** (generators can)
16. **Pointer during drag always routes to pressed panel** even if cursor leaves bounds
17. **Driver paused during manual slider drag** (prevents LFO fighting)

---

## 30. Rust Port Gaps vs This Contract

### Missing Interaction Behaviors (Critical)

| # | Behavior | Status in Rust |
|---|----------|----------------|
| 1 | Neighbor clip edge snap (12px magnetic) | Only grid snap exists |
| 2 | Alt+Click free scrub on ruler | Not implemented |
| 3 | Shift+Click range select on clips | Not implemented |
| 4 | Drag-and-drop from OS | No handler |
| 5 | Double-click to rename layers | Logged as not implemented |
| 6 | F key zoom-to-fit | Not wired |
| 7 | Home/End seek shortcuts | Not wired |
| 8 | Shift+Arrow fine nudge (1/16 beat) | Not wired |
| 9 | Up/Down arrow layer navigation with auto-select | Not wired |
| 10 | I/O export marker shortcuts | Not wired |
| 11 | Backtick Performance HUD toggle | Not wired |
| 12 | Cmd+D duplicate shortcut | Not wired |
| 13 | Cmd+G / Cmd+Shift+G group/ungroup shortcuts | Not wired |
| 14 | Effect browser with search + category chips | Flat dropdown only |
| 15 | Multi-select inspector (common values, `*` placeholder) | Shows single clip |
| 16 | Region operations (copy/cut/delete at boundaries) | Not implemented |
| 17 | Context-sensitive Cmd+C/X/V (inspector vs timeline) | Effects clipboard not wired |
| 18 | Driver pausing during manual slider drag | Not implemented |
| 19 | Cursor changes on clip hover (move/resize/blocked) | Not implemented |
| 20 | Auto-scroll during playback (50px trigger, 25% target) | Not implemented |
| 21 | Application focus loss -> cancel drags | Not implemented |
| 22 | Layer drag reorder (visual: dimming + insertion line) | UI exists, dispatch no-op |
| 23 | Effect card drag reorder (ghost + indicator + dimming) | Partial |
| 24 | Rack header interactions (rename, mix slider, ungroup X) | Partial |
| 25 | Text input fields (BPM, FPS, layer name, effect value) | Not implemented |
| 26 | Quarter-note snap on loop duration slider | Not verified |
| 27 | Driver/envelope runtime evaluation | Not implemented |
| 28 | Overlap enforcement on clip paste | Not verified |

### Threshold Mismatches

| Constant | Unity | Rust | Issue |
|----------|-------|------|-------|
| Drag threshold | 4px (bitmap) / 10px (UGUI) | 4px | Match for bitmap, but UGUI interactions use 10px |
| Ruler height | 24px | 40px | **WRONG** |
| Clip vertical padding | 12px | 4px | **WRONG** (should be 12px) |
| Inspector scroll speed | 12.5 px/unit | 12.5 px/unit | Match |
| Snap threshold | 12px | N/A (no neighbor snap) | **MISSING** |

### State Machine Gaps

| State Machine | Unity | Rust | Gap |
|---------------|-------|------|-----|
| DragMode enum | None/Move/TrimLeft/TrimRight/RegionSelect | None/Move/TrimLeft/TrimRight/RegionSelect | Match |
| Selection exclusivity | Clip vs Layer vs Region vs Cursor | Clip + InsertCursor (no Layer/Region exclusivity) | **PARTIAL** |
| Escape priority chain | 4-level (menu->monitor->inspector->selection) | 2-level (selection->stop) | **INCOMPLETE** |
| Inspector focus tracking | `inspectorHasFocus` flag | Not tracked | **MISSING** |
| Keyboard suppression during text input | Only Escape/Enter pass through | No text input system | **MISSING** |

---

## Appendix: Recommended Testing Strategy

For each interaction behavior, create a test that:
1. Sets up initial state (project with clips, selection, etc.)
2. Simulates the input sequence (pointer down at X,Y -> move to X2,Y2 -> up)
3. Asserts the expected state change (clip position, selection, undo stack)

### Example Test Cases

```
test_clip_move_preserves_offset:
  Given: clip at beat 4, click at beat 5 (offset = 1)
  When: drag to beat 10
  Then: clip.start_beat == 9 (10 - 1)

test_clip_trim_minimum_duration:
  Given: clip at beat 4, duration 1 beat
  When: trim right edge to beat 4.1
  Then: clip.duration == 0.25 (clamped to minimum)

test_magnetic_snap_to_neighbor:
  Given: clip A ends at beat 8, dragging clip B
  When: clip B start near beat 7.95 (within 12px threshold)
  Then: clip B snaps to beat 8

test_video_left_trim_clamp:
  Given: video clip at beat 4 (original start)
  When: trim left to beat 3
  Then: clip.start_beat == 4 (clamped to original)

test_generator_left_trim_extend:
  Given: generator clip at beat 4
  When: trim left to beat 3
  Then: clip.start_beat == 3 (extended freely)

test_cross_layer_type_block:
  Given: video clip on video layer, generator layer exists
  When: drag clip to generator layer
  Then: layerDelta == 0, cursor == Blocked

test_escape_priority_chain:
  Given: context menu open
  When: press Escape
  Then: menu dismissed, selection unchanged

test_cmd_c_context_sensitive:
  Given: inspector focused with 2 effects selected
  When: press Cmd+C
  Then: EffectClipboard has 2 entries (NOT clip clipboard)

test_slider_right_click_reset:
  Given: effect param at 0.7, default 0.5
  When: right-click slider track
  Then: param == 0.5, undo stack has command

test_insert_cursor_arrow_autoselect:
  Given: insert cursor at beat 4, layer 0. Clip exists at beat 5, layer 0
  When: press Right arrow
  Then: clip at beat 5 is selected, insert cursor cleared
```
