# MANIFOLD Rust Port — Unity Parity Audit

> **Date:** 2026-03-16
> **Audited against:** `Assets/Docs/USER_GUIDE.md` in the Unity project (canonical spec)
> **Purpose:** Exhaustive gap analysis for AI agents continuing the Rust migration. Every deviation from the Unity version is documented here. The Rust port MUST be identical to the Unity version in all user-facing behavior, parameters, colors, layouts, and interactions.

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Section-by-Section Audit](#section-by-section-audit)
   - [1. Application Overview](#1-application-overview)
   - [2. Workspace Layout](#2-workspace-layout)
   - [3. Transport Bar](#3-transport-bar)
   - [4. Header Bar](#4-header-bar)
   - [5. Footer Bar](#5-footer-bar)
   - [6. Timeline Viewport](#6-timeline-viewport)
   - [7. Playhead & Insert Cursor](#7-playhead--insert-cursor)
   - [8. Ruler & Grid](#8-ruler--grid)
   - [9. Clips](#9-clips)
   - [10. Layers](#10-layers)
   - [11. Selection Model](#11-selection-model)
   - [12. Clipboard & Region Operations](#12-clipboard--region-operations)
   - [13. Inspector Panels](#13-inspector-panels)
   - [14. Effects System](#14-effects-system)
   - [15. Effect Groups (Racks)](#15-effect-groups-racks)
   - [16. Drivers (LFO Modulation)](#16-drivers-lfo-modulation)
   - [17. Envelopes (ADSR Modulation)](#17-envelopes-adsr-modulation)
   - [18. Generators](#18-generators)
   - [19. Video Library & Media](#19-video-library--media)
   - [20. Compositor & Output](#20-compositor--output)
   - [21. MIDI Input & Live Performance](#21-midi-input--live-performance)
   - [22. Sync Sources](#22-sync-sources)
   - [23. Export Pipeline](#23-export-pipeline)
   - [24. LED / ArtNet / DMX](#24-led--artnet--dmx)
   - [25. Percussion Analysis Pipeline](#25-percussion-analysis-pipeline)
   - [26. Project Files & I/O](#26-project-files--io)
   - [27. Undo / Redo](#27-undo--redo)
   - [28. Keyboard Shortcuts](#28-keyboard-shortcuts)
   - [29. Context Menus](#29-context-menus)
   - [30. Performance HUD](#30-performance-hud)
   - [31. OSC Remote Control](#31-osc-remote-control)
   - [32. Visual Style & Color Language](#32-visual-style--color-language)
3. [Critical Infrastructure Gaps](#critical-infrastructure-gaps)
4. [Detailed Discrepancy Lists](#detailed-discrepancy-lists)
   - [Dimension Mismatches](#dimension-mismatches)
   - [Missing Interaction Behaviors](#missing-interaction-behaviors)
   - [Color Deviations](#color-deviations)
   - [Effect Parameter Gaps](#effect-parameter-gaps)
   - [Generator Parameter Gaps](#generator-parameter-gaps)
5. [Recommendations (Priority Order)](#recommendations-priority-order)

---

## Executive Summary

The Rust port has solid foundational architecture (94 source files, 7 crates) with the domain model, editing service, undo system, and UI skeleton largely complete. However, the port is far from feature-complete and has significant deviations from the Unity specification across effects, generators, UI accuracy, and entire missing subsystems.

### By the Numbers

| Area | Implemented | Total | Percentage |
|------|-------------|-------|------------|
| Effects (GPU-rendered) | 5 | 40 | 12.5% |
| Effects (params defined) | 16 | 40 | 40% |
| Generators (GPU-rendered) | 1 | 19 | 5.3% |
| Generators (params defined) | 7 | 19 | 36.8% |
| Sync sources | 0 | 4 | 0% |
| Keyboard shortcuts | ~15 | ~35 | ~43% |
| Context menu items | 6 | ~15 | 40% |
| Blend modes (data) | 13 | 13 | 100% |
| Blend modes (GPU) | 0 | 13 | 0% |
| Commands (undo/redo) | All core | All core | ~95% |

### Subsystem Status

| Subsystem | Status |
|-----------|--------|
| Core data models | DONE |
| Editing service + undo | DONE |
| UI skeleton (panels, layout) | DONE |
| Transport bar UI | DONE (buttons exist, many are no-ops) |
| Inspector panels (master/layer/clip) | DONE |
| Effect card UI | DONE |
| Generator param UI | DONE |
| Dropdown/context menu system | DONE |
| Clip drag/move/trim | DONE |
| Region selection | DONE |
| Clipboard (copy/paste clips) | DONE |
| Video playback | STUB ONLY (no real video decode) |
| Sync sources (Link, CLK, OSC) | TRAIT STUB ONLY |
| MIDI input pipeline | LOGIC EXISTS, NOT WIRED |
| Project save | NOT IMPLEMENTED (load only) |
| File dialogs | NOT IMPLEMENTED |
| Text input fields | NOT IMPLEMENTED |
| Export pipeline | NOT STARTED |
| LED / ArtNet / DMX | NOT STARTED |
| Percussion pipeline | NOT STARTED |
| Performance HUD | NOT STARTED |
| OSC remote control | NOT STARTED |
| Driver/envelope runtime evaluation | NOT STARTED |

### Crate Architecture

```
manifold-core       — Data models, types, enums (13 files)
manifold-editing    — Commands, undo, clipboard, EditingService (15 files)
manifold-playback   — PlaybackEngine, ClipScheduler, SyncArbiter (8 files)
manifold-io         — Project loading, migration (2 files)
manifold-renderer   — wgpu GPU rendering, effects, generators (16+ files)
manifold-ui         — Custom bitmap UI system, all panels (20 files)
manifold-app        — winit event loop, Application, UIRoot, UIBridge (5 files)
```

---

## Section-by-Section Audit

### 1. Application Overview

| Spec Item | Status | Notes |
|-----------|--------|-------|
| Timeline editing | PARTIAL | Clip CRUD works, but no video playback |
| Live performance (MIDI) | NOT FUNCTIONAL | LiveClipManager exists but no MIDI input pipeline |
| Generative visuals (19 generators) | 1/19 | Only Plasma renders |
| Effect processing (40 effects) | 5/40 | InvertColors, ColorGrade, Mirror, Feedback, Bloom |
| Multi-output (monitor, LED, export) | NOT STARTED | Monitor toggle is a no-op; LED/export absent |

### 2. Workspace Layout

| Spec Item | Unity Spec | Rust | Status |
|-----------|-----------|------|--------|
| Transport bar height | 36px | 36px | MATCH |
| Header bar height | 40px | 40px | MATCH |
| Footer bar height | 29px | 29px | MATCH |
| Layer headers width | Fixed 200px | 200px | MATCH |
| Inspector width range | 196-500px | 196-500px | MATCH |
| Default inspector width | Not specified (reasonable ~280px) | 500px (MAX) | **WRONG** — starts at maximum width |
| Video/Timeline split | 15-70% timeline | 15-70% | MATCH (default 30%) |
| Timeline split persisted | Per-project | Not persisted | **MISSING** — no save |
| Startup: one empty video layer | Required | Generator layer + Plasma clip | **WRONG** — creates test project instead |
| Startup: MasterInspector shown | Required | Yes | MATCH |

**CRITICAL:** Startup behavior is wrong. Must create an empty project with one empty video layer, not a test project with a Plasma generator clip.

### 3. Transport Bar

#### 3.1 Left Group — Sync Controls

| Control | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Clock Authority button | Cycles INT -> LINK -> CLK -> OSC | Cycles INT -> Link -> MidiClock | **MISSING OSC** option |
| LINK button + dot + status | Toggle, orange dot (#BF7A14) | Button exists, toggle is no-op | NOT FUNCTIONAL |
| CLK button + dot + status | Toggle, purple dot (#944D94) | Button exists, toggle is no-op | NOT FUNCTIONAL |
| CLK Device button | Cycles MIDI input devices | Button exists, no-op | NOT FUNCTIONAL |
| SYNC button + dot + status | Toggle OSC output, teal dot (#389E85) | Button exists, no-op | NOT FUNCTIONAL |
| Dot colors | Orange/Purple/Teal when enabled, gray when disabled | Colors defined correctly | MATCH (colors only) |

#### 3.2 Center Group — Transport + BPM

| Control | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| PLAY button colors | Green #40B852 / Yellow #D1A626 | PLAY_ACTIVE(64,184,82) / PAUSED_YELLOW(209,166,38) | MATCH |
| STOP button color | Dark red #803333 | STOP_RED(128,51,51) | MATCH |
| REC button colors | Inactive #6B2626, active #D12E2E | RECORD_RED(107,38,38) / RECORD_ACTIVE(209,46,46) | MATCH |
| REC disabled when OSC authority | Required | Not implemented | **MISSING** |
| BPM field click -> text input | Required | "text input not yet implemented" | **MISSING** |
| BPM valid range 20-300 | Required | BeatQuantizer clamps but range not verified | NEEDS CHECK |
| BPM Reset (R) button | Green when recorded BPM differs | Button exists, "not yet implemented" | NOT FUNCTIONAL |
| BPM Clear (CLR) button | Dark red when >1 tempo point | Functional | MATCH |

#### 3.3 Right Group — File & Export

| Control | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| NEW | Creates blank project | Not wired | **MISSING** |
| OPEN | Opens native file dialog | "file picker not yet implemented" | **MISSING** |
| OPEN RECENT | Loads last opened project | Not wired | **MISSING** |
| SAVE | Saves to existing path, or Save As | Logged, not implemented | **MISSING** |
| SAVE AS | Opens Save As dialog | Not wired | **MISSING** |
| SAVE dirty indicator | "SAVE*" with warm tint | SAVE_DIRTY_BG(82,68,48) exists, asterisk detection works | PARTIAL (no save to make dirty) |
| EXPORT | Starts/cancels MP4 export | Not wired | **MISSING** |
| HDR | Toggles HDR10 export mode | Functional | MATCH |
| XML | Exports DaVinci Resolve FCPXML | Not wired | **MISSING** |
| PERC | Opens percussion import | "not yet implemented" | **MISSING** |
| Export range label | Blue text "IN: X OUT: Y" | Label node exists, no export range markers | **MISSING** |

### 4. Header Bar

| Control | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Project Name | Current project name or "My Project" | Shows "My Project" | MATCH |
| Import Status | Shows "Importing..." with progress bar | UI exists but no import pipeline | STUB |
| Time Display format | `MM:SS.FF \| BAR.BEAT.16TH` | Format exists in push_state | MATCH |
| Zoom Out/In buttons | Step zoom | Functional | MATCH |
| Zoom levels | 1, 2, 5, 10, 20, 40, 80, 120, 200, 400 px/beat | Same array | MATCH |
| Monitor button label | "OUT" | "Monitor" | **WRONG LABEL** |
| Monitor button behavior | Toggle external monitor, blue when active | "not yet implemented" | NOT FUNCTIONAL |

### 5. Footer Bar

| Control | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Selection Info | "N clips selected" or empty | Exists in push_state | MATCH |
| Quantize | Cycles Off / 1/4 / 1/8 / 1/16 | Functional | MATCH |
| Resolution | Dropdown of detected monitor resolutions | Button exists, dropdown functional | PARTIAL (no CoreGraphics detection) |
| FPS field | Editable text field | "text input not yet implemented" | **MISSING** |

### 6. Timeline Viewport

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Ruler height | 24px | **40px** | **WRONG** (40 vs 24) |
| Overview Strip | Miniature full-timeline preview (bitmap) | 16px allocated, no content rendered | STUB |
| Waveform Lane | Audio waveform display | Not present | **MISSING** |
| Stem Lanes | Demucs audio stems with re-analysis buttons | Not present | **MISSING** |
| Grid Overlay | Adaptive vertical lines | Implemented | MATCH |
| Alt+Scroll -> Zoom | Anchored to playhead | Alt+Scroll zooms with cursor anchor | MATCH |
| Shift+Scroll -> H-pan | Horizontal scroll | Functional | MATCH |
| F key -> Zoom to fit | With 10% padding | Not wired | **MISSING** |
| Auto-scroll during drag | Near viewport edges | check_auto_scroll exists | MATCH |
| Scroll behavior | Immediate, no momentum | Code-editor style | MATCH |

### 7. Playhead & Insert Cursor

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Playhead visual | Colored vertical line | PLAYHEAD_RED(217,64,56), 2px wide | MATCH |
| Insert cursor visual | Thin line (2px) + small triangle indicator | INSERT_CURSOR_BLUE(89,148,242), 1px wide | **MISSING triangle indicator** |
| Insert cursor set by click | Click empty timeline space | Functional | MATCH |
| Play starts from insert cursor | When cursor is set | Implemented in dispatch | MATCH |

### 8. Ruler & Grid

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Ruler click scrub | Grid-snapped | Functional (Seek action) | MATCH |
| Alt+Click -> Free scrub | Disables snap | Not implemented | **MISSING** |
| Export In Marker (I key) | Blue line + triangle | Not wired | **MISSING** |
| Export Out Marker (O key) | Blue line + triangle | Not wired | **MISSING** |
| Alt+I / Alt+O | Clear respective marker | Not wired | **MISSING** |
| Export range highlight | Shaded region between In/Out | UI constants exist, no key bindings | **MISSING** |

### 9. Clips

#### 9.1-9.3 Clip Types & Visual States

| State / Color | Unity Spec | Rust | Status |
|---------------|-----------|------|--------|
| Video normal | (0.68, 0.66, 0.64) = (173, 168, 163) | CLIP_NORMAL(173, 168, 163) | MATCH |
| Video hover | (0.74, 0.72, 0.70) = (189, 184, 179) | CLIP_HOVER(189, 184, 179) | MATCH |
| Video selected | (0.85, 0.82, 0.78) = (217, 209, 199) | CLIP_SELECTED(217, 209, 199) | MATCH |
| Generator normal | (0.396, 0.988, 1.0) = (101, 252, 255) | CLIP_GEN_NORMAL(101, 252, 255) | MATCH |
| Generator hover | (0.30, 0.38, 0.60) = (77, 97, 153) | CLIP_GEN_HOVER(77, 97, 153) | MATCH |
| Generator selected | (0.40, 0.55, 0.88) = (102, 140, 224) | CLIP_GEN_SELECTED(102, 140, 224) | MATCH |
| Phantom clip | Semi-transparent while MIDI note held | No phantom rendering | **MISSING** |
| Muted clip | Visual indicator (icon or color shift) | No visual mute indicator | **MISSING** |
| Locked clip | Dark gray at 50% alpha | CLIP_LOCKED(82,79,77,128) exists, no lock toggle in UI | **MISSING** (no interaction) |
| Error clip | Inline warning icon + red tint | Not implemented | **MISSING** |

#### 9.4 Clip Creation

| Method | Unity Spec | Rust | Status |
|--------|-----------|------|--------|
| Double-click empty space | Creates clip at grid-snapped position | Functional (TrackDoubleClicked) | MATCH |
| Drag video files from OS | Creates clips sequentially at drop position | No drag-drop handler | **MISSING** |
| MIDI NoteOn | Creates phantom clip at current beat | LiveClipManager exists, not wired | NOT FUNCTIONAL |
| Paste (Cmd+V) | At insert cursor position + layer | Functional | MATCH |
| Duplicate (Cmd+D) | Copies after originals with minimal gap | Functional | MATCH |

#### 9.5-9.6 Clip Editing

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Click+drag move | Offset from click point maintained | Functional | MATCH |
| Grid snap (8px threshold) | Magnetic snap to grid + neighboring clip edges | Grid snap exists, no neighbor snap | PARTIAL |
| Cross-layer move | Compatible layers only (video<->video, gen<->gen) | Functional in dispatch | MATCH |
| Overlap rule (DaVinci-style) | Dragged clip wins, overlapped trimmed/removed | enforce_non_overlap functional | MATCH |
| Arrow keys nudge | Left/Right by grid step | Functional | MATCH |
| Shift+Arrow fine nudge | 1/16 beat | Not wired | **MISSING** |
| Multi-clip move | Maintain relative positions | Functional | MATCH |
| Trim handles (8px) | Left/right resize with cursor change | Functional | MATCH |
| Minimum duration | 0.25 beats (1/16 note) | Enforced in trim | MATCH |
| Duration tooltip | Shows near cursor during resize | Not implemented | **MISSING** |
| Split (S key) | At playhead position | Functional | MATCH |

#### 9.8 Clip Properties (Data Model)

All properties from the Unity spec exist in the Rust `TimelineClip` struct: StartBeat, DurationBeats, InPoint, IsLooping, LoopDurationBeats, IsMuted, IsLocked, TranslateX/Y, Scale, Rotation, InvertColors, RecordedBpm, GeneratorType, Effects, EffectGroups. **MATCH.**

### 10. Layers

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Layer types (Video/Generator/Group) | 3 types | All 3 in enum | MATCH |
| Layer header: chevron | Collapse/expand | Functional | MATCH |
| Layer header: name | Editable text | Displays name, double-click rename NOT IMPLEMENTED | PARTIAL |
| Layer header: Mute (M) | Orange when active | MUTE_BTN_ACTIVE(199,102,56) | MATCH |
| Layer header: Solo (S) | Yellow when active | SOLO_BTN_ACTIVE(217,191,64) | MATCH |
| Layer header: Blend mode | Dropdown | Functional | MATCH |
| Add Video Layer | Right-click -> "Insert Video Layer" | Functional | MATCH |
| Add Generator Layer | Right-click, defaults to Plasma | Functional | MATCH |
| Delete Layer | Minimum 1 layer enforced | Functional | MATCH |
| Group Layers (Cmd+G) | Select 2+ layers | Command exists | PARTIAL |
| Ungroup | Right-click group | Command exists | PARTIAL |
| Import MIDI File | Right-click -> "Import MIDI File" | Not present | **MISSING** |
| Layer opacity slider | 0-1 | Functional with undo | MATCH |
| Layer drag reorder | Drag handle in header | UI exists, dispatch is no-op | NOT FUNCTIONAL |
| Click layer header | Focus layer, show LayerInspector | Functional | MATCH |

### 11. Selection Model

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Click clip -> select single | Deselect others, show ClipInspector | Functional | MATCH |
| Cmd+Click -> toggle multi | Toggle in/out of multi-selection | Functional | MATCH |
| Shift+Click -> range select | Range select across layers | Not implemented | **MISSING** |
| Click empty -> deselect all | Set insert cursor, show MasterInspector | Functional | MATCH |
| Cmd+A -> select all | All clips on timeline | Functional | MATCH |
| Escape -> clear | Show MasterInspector | Functional | MATCH |
| Region selection (click+drag) | Draw selection rectangle | Functional (RegionDrag) | MATCH |
| Multi-select inspector | Common values shown, differing = `*` | Shows single clip only | **MISSING** |

### 12. Clipboard & Region Operations

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Cmd+C/X/V (timeline) | Copy/cut/paste clips with beat offsets | Functional | MATCH |
| Cmd+D duplicate | After originals | Functional | MATCH |
| Delete/Backspace | Delete selected | Functional | MATCH |
| Cmd+C/X/V (inspector) | Copy/paste effects when inspector focused | EffectClipboard exists, not wired to Cmd+C | **MISSING** |
| Region copy | Clips trimmed at region boundaries | Not implemented | **MISSING** |
| Region cut | Split at boundaries, interior deleted | Not implemented | **MISSING** |
| Region delete | Split at boundaries, interior deleted | Not implemented | **MISSING** |
| Region paste | Pattern placed with gap preservation | Not implemented | **MISSING** |

### 13. Inspector Panels

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Master Inspector | Shown when nothing selected | Functional | MATCH |
| Layer Inspector | Shown when layer focused | Functional | MATCH |
| Clip Inspector | Shown when clip(s) selected | Functional | MATCH |
| Selection priority | Clip > Layer > Master | Implemented | MATCH |
| Inspector resize | Drag left edge, 196-500px | Functional | MATCH |
| Width persists across type switches | Required | Yes | MATCH |
| Width persists across save/load | Required | No save exists | **MISSING** |
| Collapsible sections | Within panels | Functional (chevron toggles) | MATCH |
| Content scrolls as single unit | No nested scrolling | Functional (12.5 px/tick) | MATCH |
| Version-gated updates | Only rebuild on data version change | data_version counter exists | MATCH |

### 14. Effects System

#### Effect Implementation Status (40 Unity Effects)

The Unity version has 40 effects. The Rust enum has 32 variants (IDs skip 2-9). Of those 32, only 5 have GPU shader implementations, and 16 have zero parameter definitions.

| # | Effect | Rust Enum | GPU Impl | Params Defined | Unity Param Count |
|---|--------|-----------|----------|----------------|-------------------|
| 1 | Transform | Transform (0) | Data-only | 4 | 4 |
| 2 | Invert Colors | InvertColors (1) | **YES** | 1 | 1 |
| — | IDs 2-9 | NOT IN ENUM | — | — | Unknown if Unity has effects here |
| 3 | Feedback | Feedback (10) | **YES** | 1 | 1 |
| 4 | Pixel Sort | PixelSort (11) | No | 2 | 2+ |
| 5 | Bloom | Bloom (12) | **YES** | 2 | 1+ |
| 6 | Infinite Zoom | InfiniteZoom (13) | No | **0** | 2+ |
| 7 | Kaleidoscope | Kaleidoscope (14) | No | 2 | 2+ |
| 8 | Edge Stretch | EdgeStretch (15) | No | 2 | 3+ |
| 9 | Voronoi Prism | VoronoiPrism (16) | No | **0** | 2+ |
| 10 | Quad Mirror | QuadMirror (17) | No | **0** | 1+ |
| 11 | Dither | Dither (18) | No | 1 | 2 (Amount + Algorithm enum) |
| 12 | Strobe | Strobe (19) | No | 2 | 3 (Amount + Rate + Mode enum) |
| 13 | Stylized Feedback | StylizedFeedback (20) | No | **0** | 4+ |
| 14 | Mirror | Mirror (21) | **YES** | 1 | 2 (Amount + Mode) |
| 15 | Blob Tracking | BlobTracking (22) | No | **0** | 4+ |
| 16 | CRT | CRT (23) | No | 1 | 5 |
| 17 | Fluid Distortion | FluidDistortion (24) | No | **0** | 4+ |
| 18 | Edge Glow | EdgeGlow (25) | No | **0** | 3+ |
| 19 | Datamosh | Datamosh (26) | No | **0** | 2 |
| 20 | Slit Scan | SlitScan (27) | No | **0** | 3 |
| 21 | Color Grade | ColorGrade (28) | **YES** | 4 | 9 |
| 22 | Wireframe Depth | WireframeDepth (29) | No | **0** | 14 |
| 23 | Chromatic Aberration | ChromaticAberration (30) | No | 1 | 4 |
| 24 | Gradient Map | GradientMap (31) | No | **0** | 7 |
| 25 | Glitch | Glitch (32) | No | 1 | 4 |
| 26 | Film Grain | FilmGrain (33) | No | 1 | 3 |
| 27 | Halation | Halation (34) | No | 2 | 4 |
| 28 | Microscope | Microscope (35) | No | **0** | 11 |
| 29 | Corruption | Corruption (36) | No | **0** | 6 |
| 30 | Infrared | Infrared (37) | No | **0** | 6 |
| 31 | Surveillance | Surveillance (38) | No | **0** | 7 |
| 32 | Redaction | Redaction (39) | No | **0** | 7 |

**Key:** Bold **0** = zero params defined in Rust (returns `&[]` from `param_defs()`).

#### Parameter Discrepancies on Implemented Effects

| Effect | Unity Params | Rust Params | Missing from Rust |
|--------|-------------|-------------|-------------------|
| ColorGrade | Gain, Saturation, Hue, Contrast, Colorize, Tint (9 total) | HueShift, Saturation, Gain, Contrast (4 total) | **5 params missing** (Colorize, Tint, etc.) |
| Bloom | Amount (1) | Threshold + Intensity (2) | Different param names/ranges |
| Mirror | Amount + Mode (2) | Mode only (1) | **Missing Amount (wet/dry blend)** |
| Feedback | Amount (1) | Amount (1) | MATCH |
| InvertColors | Amount (1) | Intensity (1) | Name differs ("Amount" vs "Intensity") |

**IMPORTANT:** Every implemented effect must have param names, ranges, and defaults identical to Unity. Refer to `Assets/Scripts/Data/EffectDefinitionRegistry.cs` in the Unity project as the canonical source.

#### Effect Infrastructure Issues

| Issue | Detail |
|-------|--------|
| `copy_texture_to_texture` is a STUB | Currently just clears the target instead of copying. Breaks effect chain input. |
| Wet/dry lerp is a TODO | "Phase C -- needs wet_dry_lerp.wgsl" comment in effect_chain.rs. Group wet/dry blend doesn't work. |
| Effect browser popup | Unity: 480px grid, search bar, category chips. Rust: flat dropdown list, no search, no categories. |

### 15. Effect Groups (Racks)

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Create group (Cmd+G) | Groups selected effects | Command exists | PARTIAL (no selection UI) |
| Ungroup (Cmd+Shift+G) | Removes membership | Command exists | PARTIAL |
| Group header UI | Collapse, enable, wet/dry slider | Commands exist | PARTIAL |
| Wet/dry GPU blend | `lerp(dry, wet, wetDry)` | TODO STUB | **NOT FUNCTIONAL** |
| Group visual UI | Rack with indented cards | No visual grouping rendered | **MISSING** |
| Effect selection within racks | Cmd+Click toggle, Shift+Click range | Not implemented | **MISSING** |

### 16. Drivers (LFO Modulation)

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| 5 waveforms | Sine, Triangle, Sawtooth, Square, Random | All 5 in enum | MATCH |
| 23 beat divisions | Straight (11) + Dotted (5) + Triplet (4) | 11 shown in UI, dot+triplet modifiers | PARTIAL |
| Phase control (0-1) | Adjustable | Field exists, no UI for editing | **MISSING UI** |
| Trim Min/Max | 0-1 range clamp | Fields exist + trim handle UI | MATCH |
| Reversed toggle | Invert output | Field exists + button | MATCH |
| Beat-synced evaluation | Modulates params per-frame | **No driver evaluation in render loop** | **NOT FUNCTIONAL** |
| Visual feedback on slider | Base value (gray) + modulated value (colored thumb) | No modulation visualization | **MISSING** |

### 17. Envelopes (ADSR Modulation)

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| ADSR parameters | Attack, Decay, Sustain, Release | All fields present | MATCH |
| Target normalized (0-1) | Destination value | Field exists + target bar UI | MATCH |
| Per-frame evaluation | Modulates params during playback | **No envelope evaluation at runtime** | **NOT FUNCTIONAL** |
| Add envelope button | Per effect card | Toggle button exists | MATCH |
| Mini waveform display | Shows ADSR shape | Not implemented | **MISSING** |
| ADSR slider UI | 4 editable values | 4 sliders exist (A, D, S, R) | MATCH |
| Undo for ADSR changes | Undoable | Live mutation only, no undo recording | **MISSING** |
| Multiple envelopes on same param | Additive composition | Not supported | **MISSING** |

### 18. Generators

#### Generator Implementation Status (19 Unity Generators)

| Generator | Rust Enum | GPU Impl | Params Defined | Unity Category |
|-----------|-----------|----------|----------------|----------------|
| Tesseract | Yes (4) | No | **0** | Line-Based |
| Duocylinder | Yes (3) | No | **0** | Line-Based |
| Lissajous | Yes (7) | No | 4 | Line-Based |
| Wireframe Zoo | Yes (10) | No | **0** | Line-Based |
| Oscilloscope XY | Yes (9) | No | **0** | Line-Based |
| Basic Shapes Snap | Yes (2) | No | **0** | Shader-Based |
| Concentric Tunnel | Yes (5) | No | 3 | Shader-Based |
| Plasma | Yes (6) | **YES** | 4 | Shader-Based |
| Fractal Zoom | Yes (8) | No | 3 | Shader-Based |
| Number Station | Yes (16) | No | **0** | Shader-Based |
| Parametric Surface | Yes (13) | No | **0** | Compute-Based |
| Strange Attractor | Yes (14) | No | **0** | Compute-Based |
| Strange Attractor (GPU) | Yes (18) | No | **0** | Compute-Based |
| Fluid Simulation 3D | Yes (19) | No | 4 | Compute-Based |
| Reaction-Diffusion | Yes (11) | No | 4 | Stateful |
| Flowfield | Yes (12) | No | 3 | Stateful |
| Fluid Simulation | Yes (15) | No | 4 | Stateful |
| Mycelium | Yes (17) | No | **0** | Stateful |

**Key:** Bold **0** = zero params defined in Rust (returns `&[]` from `param_defs()`).

#### Plasma Generator Parameter Mismatch

**CRITICAL:** The Plasma renderer uses indices 0-4 (Pattern, Complexity, Contrast, Speed, Scale) but `GeneratorType::param_defs()` in types.rs lists only 4 entries (Speed, Scale, Complexity, ColorShift) with different ordering.

- Renderer index 0 = "Pattern" -> **not in param_defs**
- Renderer index 1 = "Complexity" -> param_defs index 2
- Renderer index 2 = "Contrast" -> **not in param_defs**
- Renderer index 3 = "Speed" -> param_defs index 0
- Renderer index 4 = "Scale" -> param_defs index 1
- param_defs index 3 "ColorShift" -> **not used by renderer**

This mismatch means the UI sliders don't correspond to what the shader actually reads.

#### Shared Parameter Slot System

Unity uses 12 reusable parameter slots with per-generator remapping via `applicableParamIndices`. Rust uses per-type `param_defs()`. The architecture is different but functionally equivalent IF the params are correct -- which they currently are not for most generators.

**IMPORTANT:** Refer to `Assets/Scripts/Data/GeneratorDefinitionRegistry.cs` in the Unity project for canonical parameter definitions for all 19 generators.

### 19. Video Library & Media

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Video discovery (folder scan) | Per-layer VideoFolderPath | Not implemented | **MISSING** |
| Supported formats | .mp4, .mov, .webm, .avi | No video decode at all | **MISSING** |
| Thumbnail LRU cache (80 clips) | Horizontal atlas, 72px, async | Not implemented | **MISSING** |
| Drag-and-drop import from OS | Default 4 beats, async metadata | No drag-drop handler | **MISSING** |
| VideoPlayer pool (10 players) | Pre-allocated, no prepareCompleted | StubRenderer only | **MISSING** |
| Pending pauses (40ms) | Play briefly for decoder init | Stub handles it structurally | N/A (no real video) |
| Recently started exclusion (50ms) | Prevent white flash | Tracking map exists in engine | MATCH (structural) |
| Micro-clip skip (<50ms) | Skipped by scheduler | MIN_START_REMAINING_TIME = 0.02s | MATCH |

### 20. Compositor & Output

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Compositing pipeline (7 steps) | Clip -> layer -> master effects -> output | Compositor + LayerCompositor exist | PARTIAL |
| Per-clip effects | Applied in chain | EffectChain exists, copy_texture is a stub | PARTIAL |
| Blend modes (13) | Normal through Darken | All 13 in BlendMode enum | DATA MATCH |
| Blend mode GPU implementation | Shader-based per-mode | **Not implemented** — no blend shaders | **MISSING** |
| Layer compositing | Sorted by layer index | LayerCompositor renders generators only | PARTIAL |
| Master effects | Composition-wide | EffectChain exists | PARTIAL |
| ACES tonemapping | For HDR | Not implemented | **MISSING** |
| Monitor output (OUT button) | Routes to secondary display | Window creation exists, output not wired | NOT FUNCTIONAL |
| RenderTexture buffers | Ping-pong, per-layer lazy, per-clip lazy | RenderTarget struct exists | PARTIAL |

### 21. MIDI Input & Live Performance

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| MIDI note mapping (0-127) | Per-note config | MidiMappingConfig exists | DATA MATCH |
| Live clip lifecycle | NoteOn -> phantom -> NoteOff -> commit | LiveClipManager complete | LOGIC EXISTS |
| Quantized launch | Immediate / Beat / Bar / Sixteenth | Pure logic exists | LOGIC EXISTS |
| MIDI safety rules | Auto-commit, time guard, channel filter | All implemented in LiveClipManager | LOGIC EXISTS |
| Actual MIDI input | CoreMIDI / platform MIDI | **No MIDI library or input pipeline** | **MISSING** |
| Performance workflow | End-to-end live performance | Nothing wired | NOT FUNCTIONAL |

### 22. Sync Sources

| Source | Unity Spec | Rust | Status |
|--------|-----------|------|--------|
| Internal | Manual play/pause | PlaybackEngine advances time | FUNCTIONAL |
| Ableton Link | Slave to Link tempo | SyncSource trait stub, no implementation | **MISSING** |
| MIDI Clock | Clock ticks + SPP | SyncSource trait stub, no implementation | **MISSING** |
| OSC Timecode | LiveMTC (H:M:S:F) | **Not even in ClockAuthority enum** | **MISSING** |
| OSC Output | `/manifold/*` messages | Not present | **MISSING** |

### 23. Export Pipeline

**ENTIRELY ABSENT.** No crate, no code, no pipeline.

| Feature | Status |
|---------|--------|
| H.264 SDR export | NOT STARTED |
| HEVC HDR10 export | NOT STARTED |
| Metal GPU encoder | NOT STARTED |
| FFmpeg fallback | NOT STARTED |
| Frame pacing (real-time vs offline) | NOT STARTED |
| Export range markers | NOT STARTED |
| Audio muxing | NOT STARTED |
| Export progress UI | NOT STARTED |

### 24. LED / ArtNet / DMX

**ENTIRELY ABSENT.** No crate, no code, no configuration.

| Feature | Status |
|---------|--------|
| LedSettings configuration | NOT STARTED |
| ArtNet UDP protocol | NOT STARTED |
| Edge-extend shader | NOT STARTED |
| Async GPU readback | NOT STARTED |
| DMX universe packing | NOT STARTED |
| Energy gating (percussion) | NOT STARTED |
| LED exit point | NOT STARTED |

### 25. Percussion Analysis Pipeline

**ENTIRELY ABSENT.** Data model (`PercussionImportState`) exists for deserialization but no analysis pipeline.

| Feature | Status |
|---------|--------|
| Python analysis runner | NOT STARTED |
| Demucs stem separation | NOT STARTED |
| Onset detection (CNN) | NOT STARTED |
| Auto-clip placement | NOT STARTED |
| BPM detection | NOT STARTED |
| Per-instrument re-analysis | NOT STARTED |
| Waveform lane display | NOT STARTED |
| ~85 tuning parameters | NOT STARTED |

### 26. Project Files & I/O

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Load V2 (.manifold ZIP) | Required | Only JSON loading, no ZIP | PARTIAL |
| Load V1 (.json) | Auto-migrate | Migration v1.0.0 -> v1.1.0 exists | MATCH |
| Save | Required | **Not implemented** | **MISSING** |
| Save As | Required | **Not implemented** | **MISSING** |
| Dirty state tracking | SAVE* indicator | data_version counter exists | PARTIAL |
| Relative path resolution | Auto-resolve on open | Deserialized but not resolved | **MISSING** |
| File dialog (native) | Open/Save/Save As | Not implemented | **MISSING** |
| Drag-and-drop files | Video, .manifold, MIDI, percussion JSON | Not implemented | **MISSING** |

### 27. Undo / Redo

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Clip CRUD | Undoable | All commands exist and work | MATCH |
| Layer CRUD | Undoable | All commands exist and work | MATCH |
| Effect add/remove/toggle/reorder | Undoable | All commands exist and work | MATCH |
| Effect param changes | Undoable | ChangeEffectParamCommand exists | MATCH |
| Group changes | Undoable | All commands exist and work | MATCH |
| Driver changes | Undoable | All commands exist and work | MATCH |
| Envelope ADSR changes | Undoable | **Live mutation, NO undo recording** | **MISSING** |
| BPM changes | Undoable | ChangeBpmCommand exists | MATCH |
| Selection changes | NOT recorded | Correct — command is a no-op | MATCH |
| Cmd+Z / Cmd+Shift+Z / Cmd+Y | Standard shortcuts | All wired | MATCH |
| Max undo history | Unlimited (Unity) | 200 (Rust) | DIFFERENT |
| Undo/redo visual flash | Brief flash on affected elements | Not implemented | **MISSING** |

### 28. Keyboard Shortcuts

| Shortcut | Unity Action | Rust | Status |
|----------|-------------|------|--------|
| Space | Play / Pause | Functional | MATCH |
| Home | Seek to beat 0 | Not wired | **MISSING** |
| End | Seek to end of timeline | Not wired | **MISSING** |
| Delete / Backspace | Delete selected | Functional | MATCH |
| S | Split at playhead | Functional | MATCH |
| E | Extend by grid step | Functional | MATCH |
| Shift+E | Shrink by grid step | Functional | MATCH |
| 0 | Mute toggle | Functional | MATCH |
| Left Arrow | Nudge/cursor left | Functional | MATCH |
| Shift+Left Arrow | Fine nudge (1/16 beat) | Not wired | **MISSING** |
| Right Arrow | Nudge/cursor right | Functional | MATCH |
| Shift+Right Arrow | Fine nudge (1/16 beat) | Not wired | **MISSING** |
| Up Arrow | Navigate up / move to layer above | Not wired | **MISSING** |
| Down Arrow | Navigate down / move to layer below | Not wired | **MISSING** |
| Cmd+A | Select all | Functional | MATCH |
| Cmd+C | Copy | Functional (clips only, not inspector-aware) | PARTIAL |
| Cmd+X | Cut | Functional (clips only) | PARTIAL |
| Cmd+V | Paste | Functional (clips only) | PARTIAL |
| Cmd+D | Duplicate | Not wired as keyboard shortcut | **MISSING** |
| Cmd+G | Group effects/layers | Not wired as keyboard shortcut | **MISSING** |
| Cmd+Shift+G | Ungroup | Not wired as keyboard shortcut | **MISSING** |
| Escape | Dismiss menu -> exit monitor -> clear effects -> clear clips | Partial (clears selection or stops) | PARTIAL |
| Cmd+S | Save | Logged, not functional | **MISSING** |
| Cmd+Z | Undo | Functional | MATCH |
| Cmd+Shift+Z | Redo | Functional | MATCH |
| Cmd+Y | Redo (alternate) | Functional | MATCH |
| F | Zoom to fit | Not wired | **MISSING** |
| ` (backtick) | Toggle performance HUD | Not wired | **MISSING** |
| Alt+Scroll | Zoom in/out | Functional | MATCH |
| Shift+Scroll | Horizontal scroll | Functional | MATCH |
| I | Set export in-point | Not wired | **MISSING** |
| O | Set export out-point | Not wired | **MISSING** |
| Alt+I | Clear export in-point | Not wired | **MISSING** |
| Alt+O | Clear export out-point | Not wired | **MISSING** |
| Cmd+Shift+I | Import percussion map | Not wired | **MISSING** |
| Cmd+Shift+M | Mark percussion downbeat | Not wired | **MISSING** |
| Cmd+Shift+[ | Nudge percussion left | Not wired | **MISSING** |
| Cmd+Shift+] | Nudge percussion right | Not wired | **MISSING** |
| Cmd+Shift+R | Reset percussion alignment | Not wired | **MISSING** |

### 29. Context Menus

#### Right-Click on Clip

| Item | Unity Spec | Rust | Status |
|------|-----------|------|--------|
| Split at Playhead | Splits selected clip(s) | Functional | MATCH |
| Delete | Deletes selected clip(s) | Functional | MATCH |
| Duplicate | Duplicates selected clip(s) | Functional | MATCH |

#### Right-Click on Layer Header

| Item | Unity Spec | Rust | Status |
|------|-----------|------|--------|
| Paste | When clipboard has clips | Not in menu | **MISSING** |
| Insert Video Layer | Always | Functional | MATCH |
| Insert Generator | Always | Functional | MATCH |
| Import MIDI File | Always | Not in menu | **MISSING** |
| Group Selected Layers | When 2+ layers selected | Not in menu | **MISSING** |
| Ungroup | When right-clicked is a group | Not in menu | **MISSING** |
| Delete Layer | When >1 layer exists | Not in menu | **MISSING** |

#### Right-Click on Empty Timeline

| Item | Unity Spec | Rust | Status |
|------|-----------|------|--------|
| Shows layer context menu | For the layer under click | Functional | MATCH |

### 30. Performance HUD

**NOT IMPLEMENTED.** No backtick toggle, no FPS counter, no frame time, no memory usage, no beat/BPM display, no MIDI status, no compositor state, no diagnostic logging.

### 31. OSC Remote Control

**NOT IMPLEMENTED.** No OSC address space, no parameter routing, no transport messages.

### 32. Visual Style & Color Language

#### Elevation Hierarchy

| Level | Unity Gray Value | Unity Gray (0-255) | Rust Constant | Rust Value | Match? |
|-------|-----------------|--------------------|--------------|-----------| --------|
| Void (0.05) | 0.05 | ~13 | DARK_BG | (13, 13, 14) | MATCH |
| Deep (0.10) | 0.10 | ~26 | TRACK_BG | (36, 36, 37) | **TOO BRIGHT** (36 vs 26) |
| Base (0.14) | 0.14 | ~36 | PANEL_BG | (37, 37, 38) | CLOSE |
| Surface (0.19) | 0.19 | ~48 | INPUT_FIELD_BG | (49, 49, 51) | CLOSE |
| Raised (0.23) | 0.23 | ~59 | BUTTON_INACTIVE | (59, 59, 61) | MATCH |
| Elevated (0.28) | 0.28 | ~71 | BUTTON_DIM | (71, 71, 74) | MATCH |

**Issue:** Track background (Deep level) is 36 instead of 26 — tracks are visually lighter than Unity.

#### Accent Colors

| Element | Unity Hex | Rust Hex | Match? |
|---------|----------|----------|--------|
| Selection blue | #5994EB | #5994EB | MATCH |
| Play green | #40B852 | #40B852 | MATCH |
| Stop red | #803333 | #803333 | MATCH |
| Record inactive | #6B2626 | #6B2626 | MATCH |
| Record active | #D12E2E | #D12E2E | MATCH |
| Paused yellow | #D1A626 | #D1A626 | MATCH |
| Link orange | #BF7A14 | #BF7A14 | MATCH |
| CLK purple | #944D94 | #944D94 | MATCH |
| Sync teal | #389E85 | #389E85 | MATCH |
| Export range | #4D8DEB | #4D8CEB | **1 unit off** (140 vs 141 green) |

#### Text & Font

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Primary text color | Off-white (0.88 = 224, 224, 224) | TEXT_NORMAL(224, 224, 230) | **Blue tint** (230 vs 224) |
| Font family | Inter (bundled TTF) | glyphon system default | **WRONG FONT** |

**CRITICAL:** The Inter font is not being loaded. All text renders in the wrong typeface. Must load `Inter-Regular.ttf` from bundled resources.

#### Visual Feedback

| Feature | Unity Spec | Rust | Status |
|---------|-----------|------|--------|
| Clip hover brightness | Immediate, no delay | Hover colors exist | MATCH |
| Selection blue border | Blue border + interior tint | SELECTED_BORDER + CLIP_BORDER_WIDTH | MATCH |
| Drag: clip moves with cursor | No ghost | Functional | MATCH |
| Resize duration tooltip | Near cursor | Not implemented | **MISSING** |
| Undo/redo flash | Brief flash on affected elements | Not implemented | **MISSING** |
| Error: inline warning + red tint + toast | Persistent visual + one-time toast | Not implemented | **MISSING** |
| Toast notifications | <=3s, bottom of viewport, one at a time | Not implemented | **MISSING** |

---

## Critical Infrastructure Gaps

These are blocking gaps that prevent the Rust port from functioning as a usable application:

| Gap | Impact | Blocks |
|-----|--------|--------|
| **No video playback** | Core feature — timeline is useless without video | Everything video-related |
| **No project save** | Users lose all work every session | Usability |
| **No file dialogs** | Can't open/save/import anything | File I/O |
| **No text input fields** | Can't edit BPM, FPS, layer names, project name | Usability |
| **No MIDI input** | Live performance doesn't work | Performance workflow |
| **No sync sources** | Can't sync to DAW (Link, MIDI Clock, OSC) | Live use |
| **No export** | Can't produce video output | Output |
| **No LED/ArtNet** | No lighting output | Output |
| **No percussion analysis** | Missing workflow | Audio-visual sync |
| **Effect wet/dry blend broken** | Group system non-functional at GPU level | Effect groups |
| **copy_texture_to_texture is a stub** | Effect chain input copy doesn't work | All effects |
| **No driver/envelope evaluation** | LFO and ADSR don't modulate params at runtime | Modulation system |
| **No Inter font loaded** | All text renders in wrong typeface | Visual fidelity |
| **Wrong startup project** | Creates Plasma test project instead of empty video layer | First impression |

---

## Detailed Discrepancy Lists

### Dimension Mismatches

| Element | Unity | Rust | Fix Required |
|---------|-------|------|-------------|
| Ruler height | 24px | 40px | Change to 24px |
| Default inspector width | ~280px | 500px (MAX) | Change default to reasonable value |
| Monitor button label | "OUT" | "Monitor" | Change to "OUT" |
| Insert cursor width | 2px + triangle | 1px, no triangle | Add triangle indicator, change to 2px |
| Overview strip | Bitmap preview | Empty 16px | Implement content |

### Missing Interaction Behaviors

1. No Alt+Click free scrub on ruler
2. No Shift+Click range selection on clips
3. No drag-and-drop from OS (video files, MIDI files, project files)
4. No double-click to rename layers
5. No F key zoom-to-fit
6. No Home/End seek shortcuts
7. No fine nudge (Shift+Arrow = 1/16 beat)
8. No Up/Down arrow layer navigation
9. No I/O export marker shortcuts
10. No backtick Performance HUD toggle
11. No Cmd+D duplicate shortcut
12. No Cmd+G / Cmd+Shift+G group/ungroup shortcuts
13. No effect browser with search bar and category chips (just flat dropdown)
14. No multi-select inspector (common properties with `*` placeholder)
15. No neighbor clip edge snap (only grid snap exists)
16. No duration tooltip during resize
17. No undo/redo visual flash
18. No toast notification system
19. No context-sensitive Cmd+C/X/V (inspector effects vs timeline clips)
20. No region operations (region copy/cut/delete/paste)

### Color Deviations

| Element | Unity Value | Rust Value | Delta |
|---------|------------|------------|-------|
| Track background (Deep) | Gray 26 | Gray 36 | **+10** (too bright) |
| Primary text | (224, 224, 224) | (224, 224, 230) | Blue channel +6 |
| Export marker green | 141 | 140 | -1 |

### Effect Parameter Gaps

Effects with fewer params than Unity (implemented effects only):

| Effect | Rust Params | Unity Params | Missing |
|--------|-----------|-------------|---------|
| ColorGrade | 4 (HueShift, Saturation, Gain, Contrast) | 9 (+ Colorize, Tint, etc.) | 5 params |
| Mirror | 1 (Mode) | 2 (Amount + Mode) | Amount |
| Bloom | 2 (Threshold, Intensity) | 1 (Amount) | Different param model |
| InvertColors | 1 (Intensity) | 1 (Amount) | Name mismatch |

Effects with ZERO param definitions (16 total):
InfiniteZoom, VoronoiPrism, QuadMirror, StylizedFeedback, BlobTracking, FluidDistortion, EdgeGlow, Datamosh, SlitScan, WireframeDepth, GradientMap, Microscope, Corruption, Infrared, Surveillance, Redaction

### Generator Parameter Gaps

Generators with ZERO param definitions (12 total):
Tesseract, Duocylinder, WireframeZoo, OscilloscopeXY, BasicShapesSnap, NumberStation, ParametricSurface, StrangeAttractor, ComputeStrangeAttractor, Mycelium

Generators with param definition mismatches:
- **Plasma**: Renderer uses 5 indices (Pattern, Complexity, Contrast, Speed, Scale) but param_defs lists 4 (Speed, Scale, Complexity, ColorShift) with different ordering and names

---

## Recommendations (Priority Order)

### Tier 1: Correctness (must fix before any feature work)

1. **Fix startup** — create empty video layer, not test project with Plasma clip
2. **Load Inter font** — bundle Inter-Regular.ttf, load via glyphon
3. **Fix ruler height** — change from 40px to 24px
4. **Fix track background color** — change TRACK_BG from (36,36,37) to ~(26,26,27) to match Unity elevation
5. **Fix monitor button label** — change "Monitor" to "OUT"
6. **Fix insert cursor** — add triangle indicator, change width to 2px
7. **Fix primary text color** — change TEXT_NORMAL from (224,224,230) to (224,224,224)
8. **Fix Plasma param mismatch** — align param_defs ordering/names with renderer indices

### Tier 2: Core infrastructure (unblocks functional use)

9. **Implement copy_texture_to_texture** — real texture copy instead of clear
10. **Implement wet/dry lerp shader** — unblocks group system
11. **Complete ALL effect param definitions** — all 40 effects need full parameter lists matching Unity `EffectDefinitionRegistry`
12. **Complete ALL generator param definitions** — all 19 generators need full parameter slots matching Unity `GeneratorDefinitionRegistry`
13. **Add driver/envelope runtime evaluation** — evaluate LFO and ADSR per-frame on params
14. **Add text input fields** — for BPM, FPS, layer names
15. **Add project save** — JSON serialization + file dialog
16. **Add native file dialogs** — open/save via `rfd` or similar crate

### Tier 3: Missing keyboard shortcuts and interactions

17. Wire Home/End seek shortcuts
18. Wire F zoom-to-fit
19. Wire Shift+Arrow fine nudge (1/16 beat)
20. Wire Up/Down arrow layer navigation
21. Wire Cmd+D duplicate shortcut
22. Wire Cmd+G / Cmd+Shift+G group/ungroup shortcuts
23. Wire I/O export range marker shortcuts
24. Wire backtick Performance HUD toggle
25. Implement Alt+Click free scrub on ruler
26. Implement Shift+Click range selection on clips
27. Implement context-sensitive Cmd+C/X/V (inspector vs timeline)
28. Add missing context menu items (layer: Paste, Import MIDI, Group, Ungroup, Delete)

### Tier 4: Feature completion

29. Implement remaining 35 effect GPU shaders
30. Implement remaining 18 generator GPU shaders
31. Implement video decode and playback (gstreamer/ffmpeg)
32. Implement MIDI input (midir/coremidi)
33. Implement Ableton Link sync
34. Implement MIDI Clock sync
35. Implement OSC sync (input and output)
36. Implement export pipeline (H.264/HEVC)
37. Implement LED/ArtNet/DMX output
38. Implement percussion analysis pipeline
39. Implement Performance HUD overlay
40. Implement toast notification system
41. Implement effect browser popup (search + categories)
42. Implement region operations (region copy/cut/delete/paste)
43. Implement drag-and-drop from OS
44. Implement .manifold ZIP format (V2)

---

## File Location Reference

### Rust Crate Layout

```
crates/
  manifold-core/src/        — Data models: clip, layer, effects, generator, settings, midi, tempo, etc.
  manifold-editing/src/      — EditingService, UndoRedoManager, all Command structs
  manifold-playback/src/     — PlaybackEngine, ClipScheduler, SyncArbiter, LiveClipManager
  manifold-io/src/           — Project loading, V1->V1.1 migration
  manifold-renderer/src/     — GPU: compositor, effects, generators, UI renderer
  manifold-ui/src/           — UI tree, input system, color constants, all panel definitions
  manifold-app/src/          — Application entry, event loop, UIRoot, UIBridge dispatch
```

### Unity Reference Files (canonical source of truth)

```
Assets/Docs/USER_GUIDE.md                          — Complete feature specification
Assets/Scripts/Data/EffectDefinitionRegistry.cs     — All 40 effect param definitions
Assets/Scripts/Data/GeneratorDefinitionRegistry.cs  — All 19 generator param definitions
Assets/Scripts/UI/Timeline/Core/UIConstants.cs      — All color constants
Assets/Docs/VISUAL_INVARIANTS.md                    — Visual behavior rules
Assets/Docs/EFFECT_GUIDE.md                         — Effect implementation details
Assets/Docs/GENERATOR_GUIDE.md                      — Generator implementation details
Assets/Docs/COMPOSITING_GUIDE.md                    — Compositing pipeline details
```

---

> **For AI agents continuing work:** Always cross-reference changes against this audit and the Unity `USER_GUIDE.md`. The Rust port must be pixel-identical and behavior-identical to the Unity version. When implementing any effect or generator, read the Unity `EffectDefinitionRegistry.cs` or `GeneratorDefinitionRegistry.cs` first to get exact parameter names, ranges, and defaults.
