# Tier 0 Fix Plan: `manifold-core` Parity Remediation

**Generated:** 2026-03-18 from line-by-line audit of all Unity Data/*.cs against Rust manifold-core/src/*.rs

**Methodology:** Every fix below references the exact Unity source file and line numbers. The implementing agent MUST read the Unity source — not this document — as the source of truth. This plan tells you WHAT to fix and WHERE to look, not HOW the code should read.

**Dependency order:** Phases must be executed in sequence. Within a phase, files can be done in parallel.

---

## Phase 1: Foundation — Traits & ParamDef (do first, everything else depends on these)

### 1A. Restructure ParamDef to include all metadata

**File:** `effects.rs` lines 9-25 (current Rust ParamDef struct)
**Unity source:** `ParamDef.cs` lines 8-35

The Rust `ParamDef` struct already has all 9 fields (`name`, `min`, `max`, `default_value`, `whole_numbers`, `is_toggle`, `value_labels`, `format_string`, `osc_suffix`). This is correct. **No structural change needed.**

However, the param_defs() methods on `EffectType` and `GeneratorType` in `types.rs` return `&[(&str, f32, f32, f32, bool)]` tuples instead of `&[ParamDef]`. This means the metadata fields (`value_labels`, `format_string`, `osc_suffix`, `is_toggle`) are never populated.

**Fix:**
1. Change `EffectType::param_defs()` (types.rs ~line 185) to return `Vec<ParamDef>` (or `&'static [ParamDef]` via `lazy_static`/`once_cell`)
2. Change `GeneratorType::param_defs()` (types.rs ~line 552) similarly
3. Populate ALL metadata from Unity's `EffectDefinitionRegistry.BuildDefinitions()` and `GeneratorDefinitionRegistry.BuildDefinitions()`
4. Every param that has `valueLabels`, `oscSuffix`, `formatString`, or `isToggle` in Unity must have them in the Rust ParamDef

**Unity references for exact metadata per effect:**
- `EffectDefinitionRegistry.cs` lines 115-492 (BuildDefinitions) — every ParamDef constructor call has the full metadata
- `GeneratorDefinitionRegistry.cs` lines 137-491 (BuildDefinitions) — same

**Also add `oscPrefix` field to a new `EffectDef` / `GeneratorDef` struct** (see 1D below).

### 1B. Add `IEffectContainer` trait

**File:** New code in `effects.rs`
**Unity source:** `IEffectContainer.cs` lines 10-19

Create trait:
```rust
pub trait EffectContainer {
    fn effects(&self) -> &[EffectInstance];
    fn effects_mut(&mut self) -> &mut Vec<EffectInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: EffectType) -> Option<&EffectInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
    fn envelopes(&self) -> &[ParamEnvelope];
    fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope>;
    fn has_envelopes(&self) -> bool;
}
```

Then implement for: `TimelineClip` (clip.rs), `Layer` (layer.rs), `ProjectSettings` (settings.rs).

### 1C. Add `IParamSource` trait

**File:** New code in `effects.rs` or `generator.rs`
**Unity source:** `IParamSource.cs` lines 10-25

Create trait:
```rust
pub trait ParamSource {
    fn display_name(&self) -> &str;
    fn param_count(&self) -> usize;
    fn get_param_def(&self, index: usize) -> ParamDef;
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
    fn get_base_param(&self, index: usize) -> f32;
    fn set_base_param(&mut self, index: usize, value: f32);
    fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_index: i32) -> &ParameterDriver;
    fn remove_driver(&mut self, param_index: i32);
}
```

Then implement for: `EffectInstance` (effects.rs), `GeneratorParamSource` (new, see Phase 2).

### 1D. Add `EffectDef` and `GeneratorDef` structs

**Unity source:** `EffectDefinitionRegistry.cs` lines 14-20, `GeneratorDefinitionRegistry.cs` lines 27-34

These are the per-type metadata containers. The Rust code currently inlines this as tuples on the enum methods. Add proper structs:

```rust
pub struct EffectDef {
    pub display_name: &'static str,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub osc_prefix: Option<&'static str>,
}

pub struct GeneratorDef {
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub osc_prefix: Option<&'static str>,
}
```

### 1E. Add `ISelectionRegionTarget` trait

**File:** `selection.rs`
**Unity source:** `SelectionRegion.cs` lines 22-26

```rust
pub trait SelectionRegionTarget {
    fn set_region(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32);
    fn clear_region(&mut self);
}
```

---

## Phase 2: Registry Methods (depends on Phase 1 structs)

### 2A. Add `EffectDefinitionRegistry` methods

**File:** New module or add to `effects.rs`
**Unity source:** `EffectDefinitionRegistry.cs` lines 22-112

Add these as associated functions on a new `EffectDefinitionRegistry` struct or as methods on `EffectType`:

1. `get(effect_type) -> &EffectDef` — line 22-25
2. `try_get(effect_type) -> Option<&EffectDef>` — line 27-30
3. `create_default(effect_type) -> EffectInstance` — lines 35-42
4. `format_value(effect_type, param_index, value) -> String` — lines 69-80
   - Named labels take priority, then wholeNumbers round, then F2 format
5. `get_osc_address(effect_type, param_index) -> Option<String>` — lines 85-94
6. `get_osc_address_for_layer(effect_type, layer_id, param_index) -> Option<String>` — lines 101-112
7. `get_all_effect_types() -> Vec<EffectType>` — lines 48-53
8. `get_all_effect_types_sorted() -> Vec<EffectType>` — lines 58-63

### 2B. Add `EffectCategoryRegistry`

**File:** New module or add to `effects.rs`
**Unity source:** `EffectCategoryRegistry.cs` lines 1-120

1. Constants: `SPATIAL`, `POST_PROCESS`, `FILMIC`, `SURVEILLANCE`, `GENERATORS` — lines 14-18
2. `ALL_CATEGORIES` array — line 20-23
3. `get_category(EffectType) -> &str` — lines 27-30 (with full mapping from lines 62-104)
4. `get_category_for_generator(GeneratorType) -> &str` — lines 32-35 (always returns GENERATORS)
5. `get_effects_in_category(category) -> Vec<EffectType>` — lines 40-48
6. `get_generators() -> Vec<GeneratorType>` — lines 53-58

Category mappings (from lines 62-104):
- **Spatial:** Transform, InvertColors
- **Post-Process:** Feedback, PixelSort, Bloom, InfiniteZoom, Kaleidoscope, EdgeStretch, VoronoiPrism, QuadMirror, Dither, Strobe, StylizedFeedback, Mirror, BlobTracking, CRT, FluidDistortion, EdgeGlow, Datamosh, SlitScan, ColorGrade, WireframeDepth
- **Filmic:** ChromaticAberration, GradientMap, Glitch, FilmGrain, Halation, Microscope
- **Surveillance:** Corruption, Infrared, Surveillance, Redaction

### 2C. Add `GeneratorDefinitionRegistry` methods

**File:** New module or add to `generator.rs`
**Unity source:** `GeneratorDefinitionRegistry.cs` lines 17-520

1. `get(gen_type) -> &GeneratorDef` — line 38
2. `try_get(gen_type) -> Option<&GeneratorDef>` — line 43
3. `is_line_based(gen_type) -> bool` — line 48
4. `get_param_def(gen_type, index) -> ParamDef` — line 53
5. `get_defaults(gen_type) -> Vec<f32>` — line 60
6. `format_gen_value(gen_type, index, value) -> String` — lines 70-86
7. `get_osc_address(gen_type, index) -> Option<String>` — line 88
8. `get_osc_address_for_layer(gen_type, layer_id, index) -> Option<String>` — line 98
9. `try_get_gen_param_range(gen_type, index) -> Option<(f32, f32)>` — line 109
10. `clamp_param(gen_type, index, value) -> f32` — line 124
11. `max_param_count() -> usize` — lines 17-24

### 2D. Add `GeneratorParamSource` adapter

**File:** `generator.rs`
**Unity source:** `GeneratorParamSource.cs` lines 11-52

This is a thin wrapper around Layer that implements `ParamSource` for generator params. It delegates to the layer's `gen_params` for actual storage. The struct holds a reference (or in Rust, methods take `&Layer`/`&mut Layer`).

Port all 11 methods from the Unity source.

---

## Phase 3: Missing Methods on Existing Structs

### 3A. `EffectInstance` — add missing methods

**File:** `effects.rs`, impl block starting at line 58
**Unity source:** `EffectInstance.cs`

Add these methods (translating line-by-line from Unity):

| Method | Unity Lines | Notes |
|--------|------------|-------|
| `new(effect_type)` constructor | 79-83 | Creates with type + enabled=true |
| `has_drivers(&self) -> bool` | line 28 | `drivers.is_some_and(\|d\| !d.is_empty())` |
| `display_name(&self) -> &str` | line 55 | Looks up in EffectDefinitionRegistry |
| `get_param_def(&self, index) -> ParamDef` | lines 58-62 | Looks up in registry |
| `param_count(&self) -> usize` | line 84 | `param_values.len()` |
| `get_param(&self, index) -> f32` | lines 86-91 | Bounds-checked read |
| `set_param(&mut self, index, value)` | lines 93-101 | Resize + write |
| `get_base_param(&self, index) -> f32` | lines 104-110 | Falls through to effective if no base |
| `find_driver(&self, param_index) -> Option<&ParameterDriver>` | lines 44-50 | Linear search |
| `get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>` | line 64 | Returns raw field |
| `create_driver(&mut self, param_index) -> &ParameterDriver` | lines 66-71 | Creates + adds |
| `remove_driver(&mut self, param_index)` | line 73 | Removes from list |
| `ensure_param_capacity(&mut self, count)` | lines 152-158 | Resize to at least count |
| `impl ParamSource for EffectInstance` | — | Wire up to above methods |

### 3B. `GeneratorParamState` — add missing methods

**File:** `generator.rs`, impl block starting at line 25
**Unity source:** `GeneratorParamState.cs`

| Method | Unity Lines | Notes |
|--------|------------|-------|
| `get_param(&self, index) -> f32` | lines 43-48 | Bounds-checked read from param_values |
| `set_param(&mut self, index, value)` | lines 51-61 | Resize + clamp to registry range |
| `get_param_base(&self, index) -> f32` | lines 64-69 | Read from base, fall through to effective |
| `set_param_base(&mut self, index, value)` | lines 75-88 | Write to both base + effective |
| `find_driver(&self, param_index) -> Option<&ParameterDriver>` | lines 34-40 | Linear search |
| `find_envelope(&self, param_index) -> Option<&ParamEnvelope>` | lines 121-127 | Linear search |
| `has_envelopes(&self) -> bool` | line 130 | Check non-null + non-empty |
| `drivers_mut(&mut self) -> &mut Vec<ParameterDriver>` | lines 24-31 | Auto-create on access |
| `envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope>` | lines 133-140 | Auto-create on access |
| `snapshot_params(&self) -> Vec<f32>` | lines 186-190 | Clone base_param_values |
| `snapshot_drivers(&self) -> Option<Vec<ParameterDriver>>` | lines 193-200 | Deep clone |
| `snapshot_envelopes(&self) -> Option<Vec<ParamEnvelope>>` | lines 203-210 | Deep clone |
| `restore(&mut self, type, params, drivers, envelopes)` | lines 168-183 | Full state restore (undo) |

**Also fix `init_defaults()`** (line 69): Must accept `gen_type: GeneratorType` parameter and set `self.generator_type = gen_type` before reading param_defs, matching Unity line 145.

### 3C. `ParameterDriver` — fix Random hash + add missing bits

**File:** `effects.rs` lines 209-213
**Unity source:** `ParameterDriver.cs` lines 224-236 (`HashToFloat`)

**Fix the hash function** — current Rust uses wrong algorithm. Replace with:
```rust
DriverWaveform::Random => {
    let cycle = (current_beat / period).floor() as i32;
    let mut h = cycle as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    (h & 0x7FFFFF) as f32 / 0x7FFFFF as f32
}
```

**Add missing field:** `is_paused_by_user: bool` (non-serialized runtime state) — Unity line 59.
Add `#[serde(skip)] pub is_paused_by_user: bool` to the struct.

**Add missing constructor:** `new(param_index, division, waveform)` — Unity lines 63-69.

**Add missing method:** `BeatDivision::to_label()` — Unity `BeatDivisionHelper.ToLabel()` lines 122-148. Returns display strings like "1/4", "1/8.", "1/4T".

**Add missing constants:** `STRAIGHT_COUNT=11`, `DOTTED_COUNT=5`, `TRIPLET_COUNT=4`, `TOTAL_COUNT=20` — Unity lines 150-153.

**Add missing methods on BeatDivision:**
- `decompose() -> (usize, BeatModifier)` — Unity lines 158-164
- `try_compose(base_index, modifier) -> Option<BeatDivision>` — Unity lines 170-184

### 3D. `ParamEnvelope` — add constructors

**File:** `effects.rs`
**Unity source:** `ParamEnvelope.cs` lines 42-52

Add:
- `new_for_gen(param_index: i32) -> Self` — Unity lines 42-45
- `new_for_effect(effect_type: EffectType, param_index: i32) -> Self` — Unity lines 48-52

---

## Phase 4: Data Model Method Gaps

### 4A. `TimelineClip` (clip.rs)

**Unity source:** `TimelineClip.cs`

| Fix | Unity Lines | Description |
|-----|------------|-------------|
| Change `video_clip_id` type | line 9 | `String` → `Option<String>` — C# allows null for generator clips |
| Add `find_effect(&self, EffectType)` | line 230 | Search effects by type |
| Add `find_effect_group(&self, &str)` | line 249 | Search groups by ID |
| Add `set_scale(&mut self, v: f32)` | line 179 | Clamp to min 0.01: `self.scale = v.max(0.01)` |
| Add `set_loop_duration_beats(&mut self, v: f32)` | line 201 | Clamp non-negative: `v.max(0.0)` |
| `impl EffectContainer for TimelineClip` | — | Wire up to existing fields/methods |
| Add effects getter (immutable) | line 217-224 | Return `&[EffectInstance]` (return `&self.effects`) |

### 4B. `Layer` (layer.rs)

**Unity source:** `Layer.cs`

| Fix | Unity Lines | Description |
|-----|------------|-------------|
| `duration_mode` type | line 41 | `Option<ClipDurationMode>` → `ClipDurationMode` with default `NoteOff` |
| Add `set_opacity(&mut self, v)` | line 140 | `self.opacity = v.clamp(0.0, 1.0)` |
| Add `set_midi_note(&mut self, v)` | line 264-265 | `if v < 0 { -1 } else { v.clamp(0, 127) }` |
| Add `set_midi_channel(&mut self, v)` | line 271 | `if v < 0 { -1 } else { v.clamp(0, 15) }` |
| Add `clear_clips(&mut self)` | line 445 | Clear clips vec + mark unsorted |
| Add `get_duration_beats(&self) -> f32` | line 530 | Max end_beat across all clips |
| Add `collect_active_clips_at_time()` | line 436 | Convert time→beat then delegate |
| Add `update_clip_generator_types()` | line 569 | When layer gen type changes, update all gen clips |
| `impl EffectContainer for Layer` | — | Wire up effects/groups/envelopes |

### 4C. `Timeline` (timeline.rs)

**Unity source:** `Timeline.cs`

Read the full Unity file before implementing. Key missing methods:

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `find_layer_by_id(&self, &str)` | lines 225-234 | Search layers by ID |
| `move_layer(&mut self, from, to)` | lines 250-266 | Move layer from index to index |
| `get_duration_seconds(spb)` | lines 105-108 | duration_beats * spb |
| `get_content_range(spb)` | lines 114-121 | Returns (start_seconds, end_seconds) |
| `get_active_clips_at_time(time, spb)` | lines 397-401 | Time→beat conversion + delegate |
| `clear_all_clips(&mut self)` | lines 439-445 | Clear clips on all layers |
| `insert_existing_layer(&mut self, index, layer)` | lines 190-196 | Insert pre-built layer at index |

**Also consider:** The `find_clip_by_id` requiring `&mut self` is an architectural issue. If the lookup cache needs mutation, consider using `Cell`/`RefCell` for the cache to allow `&self` lookups, matching Unity's semantics.

### 4D. `ProjectSettings` (settings.rs)

**Unity source:** `ProjectSettings.cs`

This has the most missing methods. Read the FULL Unity file. Key additions:

**Clamped setters (CRITICAL — prevent invalid state):**

| Setter | Unity Lines | Clamp |
|--------|------------|-------|
| `set_bpm(v)` | line 155 | `v.clamp(20.0, 300.0)` |
| `set_output_width(v)` | line 99 | `v.max(1)` |
| `set_output_height(v)` | line 106 | `v.max(1)` |
| `set_frame_rate(v)` | line 113 | `v.max(1.0)` |
| `set_time_sig_numerator(v)` | line 162 | `v.clamp(1, 16)` |
| `set_time_sig_denominator(v)` | line 169 | `v.clamp(1, 16)` |
| `set_master_opacity(v)` | line 196 | `v.clamp(0.0, 1.0)` |
| `set_video_player_pool_size(v)` | line 134 | `v.max(1)` |
| `set_max_layers(v)` | line 141 | `v.max(1)` |
| `set_default_recording_layer(v)` | line 148 | `v.max(0)` |
| `set_osc_send_port(v)` | line 284 | `v.clamp(1024, 65535)` |

**Computed properties:**

| Property | Unity Lines | Formula |
|----------|------------|---------|
| `seconds_per_beat()` | line 373 | `60.0 / self.bpm` |
| `seconds_per_bar()` | line 376 | `seconds_per_beat() * time_sig_numerator as f32` |
| `get_frame_duration()` | line 495-498 | `1.0 / self.frame_rate` |
| `time_to_frame(seconds)` | line 503-506 | `(seconds * self.frame_rate).floor() as i32` |
| `frame_to_time(frame)` | line 511-514 | `frame as f32 / self.frame_rate` |
| `has_any_master_effect()` | lines 200-213 | Check opacity + any enabled effect |

**Effect lookup:**

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `find_master_effect(type)` | lines 230-239 | Search master_effects by type |
| `find_master_effect_group(id)` | lines 252-258 | Search master_effect_groups by ID |
| `impl EffectContainer` | lines 260-268 | Wire up master effects |

**Video library paths:**

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `add_video_library_path(path)` | lines 457-466 | Add unique path |
| `remove_video_library_path(path)` | lines 471-474 | Remove by value |
| `clear_video_library_paths()` | lines 479-482 | Clear all |

### 4E. `Project` (project.rs)

**Unity source:** `Project.cs`

| Fix | Description |
|-----|-------------|
| `percussion_import` type | Change from `Option<PercussionImportState>` to `PercussionImportState` (non-optional, always allocated) |
| Complete `validate()` | Match Unity's full validation (lines 245-286) |
| Add `get_statistics()` | Unity lines 363-382 — formatted project stats string |
| Review `on_after_deserialize()` | Lines 79-84 add layer index sync not in Unity — verify this is actually needed or remove |

---

## Phase 5: Bug Fixes & Value-Level Corrections

### 5A. `math.rs` — quantize_time_seconds sentinel preservation

**File:** `math.rs` line 18
**Unity source:** `BeatQuantizer.cs` lines 30-36

Add guard for negative values:
```rust
pub fn quantize_time_seconds(seconds: f32) -> f32 {
    if seconds < 0.0 { return seconds; }  // preserve negative sentinels
    (seconds / Self::TIME_SECONDS_STEP).round() * Self::TIME_SECONDS_STEP
}
```

### 5B. `tempo.rs` — get_bpm_at_beat initial value

**File:** `tempo.rs` line 52
**Unity source:** `TempoMap.cs` lines 198-214

Unity initializes BPM from `points[0].Bpm`, not from `fallback`. Fix:
```rust
pub fn get_bpm_at_beat(&mut self, beat: f32, fallback: f32) -> f32 {
    self.ensure_sorted();
    if self.points.is_empty() {
        return fallback.clamp(20.0, 300.0);
    }
    let mut bpm = self.points[0].bpm;  // ← was: fallback
    for point in &self.points {
        if point.beat <= beat {
            bpm = point.bpm;
        } else {
            break;
        }
    }
    bpm.clamp(20.0, 300.0)
}
```

### 5C. `tempo.rs` — negative beat/seconds handling

**Unity source:** `TempoMapConverter.cs` lines 23-24, 70-71

Add negative value handling at the start of `beat_to_seconds()` and `seconds_to_beat()`:
- `beat_to_seconds`: if `beat <= 0.0`, use beat-zero BPM for the conversion
- `seconds_to_beat`: if `seconds <= 0.0`, use beat-zero BPM for the conversion

Read the Unity source carefully — it has specific logic for this.

### 5D. `generator.rs` — init_defaults signature

**File:** `generator.rs` line 69
**Unity source:** `GeneratorParamState.cs` lines 143-155

Unity's `InitDefaults(GeneratorType genType)` takes a type parameter and:
1. Validates via `TryGet`
2. Sets `self.generator_type = genType`
3. Creates both arrays from definition defaults

Current Rust `init_defaults()` takes no parameter and assumes type is already set. Fix to match Unity.

---

## Phase 6: Missing Utility Classes

### 6A. `MidiNoteParser`

**File:** New code in `midi.rs`
**Unity source:** `MidiNoteParser.cs` lines 1-106

Port the entire static class:
1. `NOTE_NAMES` array: `["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]`
2. `ENHARMONICS` table: 9 entries — `("CB", 11, -1)`, `("DB", 1, 0)`, etc. (lines 17-27)
3. `note_to_name(note: i32) -> String` — lines 34-39 (returns "--" for out of range)
4. `name_to_note(input: &str) -> i32` — lines 47-99 (returns -1 on failure, handles sharps/flats/enharmonics/raw numbers)

### 6B. `RecordingProvenance` methods

**File:** `recording.rs`
**Unity source:** `RecordingProvenance.cs` lines 142-349

Port all 13 missing methods. Key ones:
1. `ensure_valid()` — lines 142-166 (validates all lists, removes nulls, re-quantizes)
2. `clear()` — lines 168-174
3. `add_recorded_clip(clip)` — lines 176-181
4. `add_tempo_change(change)` — lines 183-188
5. `try_get_recorded_tempo_lane()` — lines 190-201
6. `set_recorded_tempo_lane()` — lines 203-225
7. `capture_recorded_tempo_lane()` — lines 227-240
8. `try_restore_recorded_tempo_lane()` — lines 242-265
9. `get_source_at_beat()` — lines 267-286
10. `is_recorded_tempo_lane_equivalent()` — lines 288-316
11. `try_get_recorded_project_bpm()` — lines 318-329
12. `set_recorded_project_bpm()` — lines 331-342
13. `clear_recorded_project_bpm()` — lines 344-349

### 6C. `PercussionImportState.ensure_valid()`

**File:** `percussion.rs`
**Unity source:** `PercussionImportState.cs` lines 109-160

Port the full validation method.

### 6D. `VideoLibrary` methods

**File:** `video.rs`
**Unity source:** `VideoLibrary.cs`

Port missing methods:
1. `add_clip()` — lines 56-69
2. `remove_clip()` — lines 74-87
3. `clear()` — lines 126-130
4. `find_clip_by_path()` — lines 259-269
5. `remove_missing_clips()` — lines 292-308
6. `SUPPORTED_EXTENSIONS` constant — line 25

`ScanDirectory` and `ProcessVideoFile` can be deferred — they require filesystem access that may work differently in Rust.

---

## Phase 7: Update lib.rs exports

After all phases, update `lib.rs` to export new public items:
- `EffectContainer` trait
- `ParamSource` trait
- `SelectionRegionTarget` trait
- `EffectDefinitionRegistry` / `GeneratorDefinitionRegistry` (if standalone structs)
- `EffectCategoryRegistry`
- `GeneratorParamSource`
- `MidiNoteParser`

---

## Verification Checklist

After implementing all phases, verify:

- [ ] `cargo build` succeeds for `manifold-core`
- [ ] `cargo test` passes for `manifold-core`
- [ ] Every `EffectType` variant has full `ParamDef` metadata (valueLabels, oscSuffix, etc.)
- [ ] Every `GeneratorType` variant has full `ParamDef` metadata
- [ ] `EffectContainer` trait is implemented by TimelineClip, Layer, ProjectSettings
- [ ] `ParamSource` trait is implemented by EffectInstance
- [ ] `ParameterDriver::evaluate()` Random waveform matches Unity's hash exactly
- [ ] `BeatQuantizer::quantize_time_seconds()` preserves negative sentinels
- [ ] `TempoMap::get_bpm_at_beat()` initializes from first point, not fallback
- [ ] All clamped setters enforce Unity's exact ranges
- [ ] `MidiNoteParser::note_to_name()` and `name_to_note()` match Unity output for all 128 notes + enharmonics
- [ ] Existing tests still pass
- [ ] No new `pub` items that should be `pub(crate)`

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `effects.rs` | Add traits, fix hash, add methods, add registries |
| `generator.rs` | Add methods, add GeneratorParamSource, add registry methods |
| `types.rs` | Change param_defs() return type to ParamDef, add metadata |
| `clip.rs` | Add methods, impl EffectContainer, change video_clip_id type |
| `layer.rs` | Add methods, impl EffectContainer, fix duration_mode type |
| `timeline.rs` | Add missing methods |
| `settings.rs` | Add clamped setters, computed properties, impl EffectContainer |
| `project.rs` | Fix percussion_import type, complete validate() |
| `selection.rs` | Add SelectionRegionTarget trait |
| `math.rs` | Fix quantize_time_seconds sentinel |
| `tempo.rs` | Fix get_bpm_at_beat init, add negative handling |
| `midi.rs` | Add MidiNoteParser |
| `recording.rs` | Add 13 methods |
| `percussion.rs` | Add ensure_valid() |
| `video.rs` | Add library management methods |
| `lib.rs` | Update exports |
