# Stability Audit: Generator Files (14 Remaining)

**Auditor:** Claude Opus 4.6 (automated)
**Date:** 2026-03-28
**Scope:** 14 generator files not covered in initial stability audit
**Context:** Visual DAW for live performance; must survive 4+ hour shows at 60 FPS

---

## Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 1 |
| WARNING  | 10 |
| INFO     | 8 |
| VERIFIED SAFE | 17 |

---

## CRITICAL

### C-1: MRI Volume — synchronous file I/O on content thread hot path

**File:** `crates/manifold-renderer/src/generators/mri_volume.rs:195`

`load_tiff_slice()` performs synchronous `std::fs::File::open()` + TIFF decode + `Vec<u8>` allocation during `render()` on the content thread. This blocks the entire 60 FPS render loop. TIFF decode of medical imaging data can take 5-50ms depending on slice dimensions. During a 4-hour show, any parameter sweep over the `SLICE_POS` knob will cause repeated frame drops as each new slice index triggers a file read.

The dirty-check at line 189 (`need_load`) limits this to parameter changes only, but a performer continuously sweeping the slice position knob will hit this every frame.

Additionally, `load_tiff_slice` allocates a `Vec<u8>` per load call (`mri_volume_loader.rs:87`), and the U16-to-U8 and F32-to-U8 conversion paths at `mri_volume_loader.rs:105-110` each allocate via `.collect()`.

---

## WARNING

### W-1: `project_4d` — division by zero risk when `w == proj_dist`

**File:** `crates/manifold-renderer/src/generators/generator_math.rs:53`

```rust
let f = proj_dist / (proj_dist - w);
```

When `w == proj_dist`, this produces `Inf`. Since `w` comes from rotated 4D vertex coordinates (range [-1, 1] from unit hypercube/torus), and `proj_dist` defaults to 3.0, this cannot happen with the default `dist` parameter. However, if a user sets the `DIST` param close to 1.0, rotated w values could approach `proj_dist`, producing extremely large or infinite projected coordinates. These would propagate to line positions and GPU buffers.

Similarly at line 59: `let s = proj_dist / (proj_dist + p3z)` — if `p3z == -proj_dist`, another division by zero.

**Affected generators:** `tesseract.rs:97`, `duocylinder.rs:105`

### W-2: `time * speed` — f32 precision loss after extended playback

**File:** `crates/manifold-renderer/src/generators/parametric_surface.rs:167`

```rust
time_val: ctx.time * speed,
```

`ctx.time` is `f32` (cast from `f64` at the engine level). After 4 hours at 120 BPM, `time` reaches ~14,400 seconds. At this magnitude, f32 has ~1ms precision, which is acceptable for most visual purposes. However, when multiplied by `speed` (which can be user-controlled), values like `time * 4.0 = 57,600` reduce f32 precision to ~4ms, causing visible temporal quantization in smooth animations.

This applies to ALL generators that use `ctx.time`:
- `parametric_surface.rs:167` — `ctx.time * speed`
- `plasma.rs:91` — `ctx.time` as uniform
- `basic_shapes_snap.rs:52` — `ctx.time` as uniform
- `concentric_tunnel.rs:94` — `ctx.time` as uniform
- `tesseract.rs:89` — `ctx.time` used for rotation angles
- `duocylinder.rs:97-100` — `ctx.time` used for rotation angles
- `lissajous.rs:76,84,88,90` — `ctx.time` used for frequency/phase
- `oscilloscope_xy.rs:122` — `ctx.time * wave_speed * 0.3`

The engine tracks time internally as f64 (`current_time_double`) but casts to f32 before passing to generators (`engine.rs:243`). The precision loss is inherent in the f32 GeneratorContext field.

### W-3: wireframe_zoo — per-frame Vec allocation via `normalize_shape`

**File:** `crates/manifold-renderer/src/generators/wireframe_zoo.rs:177`

```rust
let verts_3d = normalize_shape(raw_verts);
```

`normalize_shape()` at line 117 returns `Vec<[f32; 3]>` — a heap allocation every frame. The maximum is 20 vertices (dodecahedron), so the allocation is small (~240 bytes), but it violates the "no per-frame allocations on hot paths" invariant. Should be pre-normalized at init time or use a scratch buffer.

### W-4: wireframe_zoo — per-frame edge_a/edge_b rebuild

**File:** `crates/manifold-renderer/src/generators/wireframe_zoo.rs:184-189`

```rust
self.helper.edge_a.clear();
self.helper.edge_b.clear();
for e in edges {
    self.helper.edge_a.push(e[0]);
    self.helper.edge_b.push(e[1]);
}
```

Edge connectivity is rebuilt every frame even though it only changes when `shape_idx` changes. The Vec has capacity from init (30 edges), so no allocation occurs, but the redundant work could be avoided with dirty-checking on `shape_idx`.

### W-5: `hash_beat` — potential index out of range from float-to-int conversion

**File:** `crates/manifold-renderer/src/generators/oscilloscope_xy.rs:91`

```rust
let ratio_idx1 = ((seed1 * RATIO_A.len() as f32) as usize) % RATIO_A.len();
```

`hash_beat` returns a value in `[0, 1)` via `.fract()`. If the fract result is exactly 1.0 due to floating-point rounding, `seed1 * 10.0 = 10.0`, and `10.0 as usize = 10` which exceeds `RATIO_A.len() - 1 = 9`. The modulo `% RATIO_A.len()` saves this from panic, so the arithmetic is safe, but the index mapping is uneven (index 0 gets double weight). This is cosmetic, not a crash risk.

### W-6: Line pipeline — silent data truncation for large generators

**File:** `crates/manifold-renderer/src/generators/line_pipeline.rs:109-119`

```rust
let pos_len = pos_bytes.len().min(pos_limit);
...
let inst_len = inst_bytes.len().min(inst_limit);
```

`MAX_POSITIONS = 1024` and `MAX_INSTANCES = 2048`. Duocylinder has 576 vertices and 1728 instances (1152 edges + 576 dots), which fits. But if a future generator exceeds these limits, data is silently truncated (no warning, no error). The truncation at line 122 `(instances.len() as u64).min(MAX_INSTANCES)` also silently clips instance count.

### W-7: `anim_progress` accumulation drift in LineGeneratorHelper

**File:** `crates/manifold-renderer/src/generators/line_pipeline.rs:245-249`

```rust
self.anim_progress += speed * (edge_count as f32 / 100.0);
let total = edge_count as f32;
if self.anim_progress >= total {
    self.anim_progress -= total;
}
```

This subtraction-based wrapping can drift over long shows if `anim_progress` overshoots `total` by more than `total` in a single frame (e.g., very high `speed` values). A safer wrap would use modulo: `self.anim_progress %= total`. Additionally, if `speed` is negative (not currently exposed but possible from param values), `anim_progress` decreases without bound since only positive overflow is handled.

### W-8: MRI Volume — discover_scans runs at generator creation time

**File:** `crates/manifold-renderer/src/generators/mri_volume.rs:63-64`

```rust
let scan_path = PathBuf::from("assets/mri-data/volumes");
let scans = discover_scans(&scan_path);
```

`discover_scans` at `mri_volume_loader.rs:69-83` performs recursive directory listing and sorting. This runs during `MriVolumeGenerator::new()`, which is called during `prewarm_all()` at startup (`registry.rs:50`). If the MRI data directory is on a slow/network filesystem or contains many scans, this blocks startup. Not a per-frame issue, but relevant for startup reliability.

### W-9: MRI Volume — Vec allocations in discovery and logging

**File:** `crates/manifold-renderer/src/generators/mri_volume.rs:69`

```rust
let axes: Vec<&str> = [...]
    .into_iter()
    .flatten()
    .collect();
```

This `Vec` allocation happens only at init during scan discovery logging, not per-frame. Low severity but notable as a pattern.

### W-10: Parametric Surface — needs_rebake uses exact epsilon comparison with f32::MIN sentinel

**File:** `crates/manifold-renderer/src/generators/parametric_surface.rs:79-80`

```rust
last_shape: f32::MIN,
last_morph: f32::MIN,
```

The initial sentinel values `f32::MIN` (-3.4e38) ensure the first frame always triggers a bake. The comparison at line 86 uses epsilon 0.00001. This is correct but if any param path ever produces NaN (from upstream bugs), the epsilon comparison `(NaN - x).abs() > 0.00001` evaluates to `false`, causing stale volume data. NaN values would need to come from outside this generator (param corruption), so this is low-probability.

---

## INFO

### I-1: PlasmaUniforms total size is 48 bytes (12 floats) — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/plasma.rs:18-31`

9 data fields (36 bytes) + `_pad: [f32; 3]` (12 bytes) = 48 bytes. Properly aligned.

### I-2: ConcentricTunnelUniforms total size is 48 bytes — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/concentric_tunnel.rs:20-33`

9 data fields (36 bytes) + `_pad: [f32; 3]` (12 bytes) = 48 bytes. Properly aligned.

### I-3: BasicShapesSnapUniforms total size is 32 bytes — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/basic_shapes_snap.rs:10-20`

6 data fields (24 bytes) + `_pad: [f32; 2]` (8 bytes) = 32 bytes. Properly aligned.

### I-4: LineUniforms total size is 32 bytes — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/line_pipeline.rs:22-32`

6 data fields (24 bytes, note `num_edges` is u32 = 4 bytes) + `_pad: [f32; 2]` (8 bytes) = 32 bytes. Properly aligned.

### I-5: EdgeInstance is 16 bytes — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/line_pipeline.rs:5-12`

3 u32 data fields (12 bytes) + `_pad: u32` (4 bytes) = 16 bytes. Properly aligned.

### I-6: SliceUniforms total size is 32 bytes — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/mri_volume.rs:26-37`

8 f32 fields = 32 bytes. Naturally aligned (multiple of 16).

### I-7: BakeUniforms and RaymarchUniforms — 16-byte aligned

**File:** `crates/manifold-renderer/src/generators/parametric_surface.rs:16-32`

BakeUniforms: 3 data + 1 pad = 16 bytes. RaymarchUniforms: 4 data = 16 bytes. Both aligned.

### I-8: `trigger_count as f32` — precision loss above 2^24

**File:** `crates/manifold-renderer/src/generators/plasma.rs:107`

```rust
trigger_count: ctx.trigger_count as f32,
```

`trigger_count` is u32. Above 16,777,216 (2^24), f32 cannot represent consecutive integers. At 120 BPM with quarter-note triggers, this would take ~97 days of continuous play. Not a realistic concern for a 4-hour show but worth noting. Same pattern in `basic_shapes_snap.rs:57`, `concentric_tunnel.rs:102`.

---

## VERIFIED SAFE

### VS-1: Resolution scaling — zero dimensions prevented

**File:** `crates/manifold-renderer/src/generator_renderer.rs:157-161`

`scaled_dimensions` clamps scale to `[0.125, 1.0]` and applies `.max(16)` to both width and height. Dimensions cannot be zero.

### VS-2: Compute dispatch threadgroup counts — correct

All 2D generators use `[ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1]`:
- `parametric_surface.rs:192` (raymarch)
- `plasma.rs:125`
- `basic_shapes_snap.rs:73`
- `concentric_tunnel.rs:118`
- `mri_volume.rs:252`

ParametricSurface bake uses `[VOL_SIZE / 4, VOL_SIZE / 4, VOL_SIZE / 4]` at line 158, where `VOL_SIZE = 128`. 128/4 = 32 workgroups per dimension, with `@workgroup_size(4,4,4)` = 64 threads, well under the 256 limit. Total volume coverage: 32*4 = 128 per axis. Correct.

### VS-3: `as u32` casts from params — all guarded by `.round()` and `.clamp()`

- `concentric_tunnel.rs:67` — `(ctx.params[SPEED].round() as usize).min(BEAT_VALUES.len() - 1)` — safe
- `concentric_tunnel.rs:72` — `(ctx.params[SNAP_MODE].round() as i32).clamp(MODE_SHAPE, MODE_BOTH)` — safe
- `plasma.rs:112` — `(pattern_type.round() as u32).min(PATTERN_COUNT - 1) as usize` — safe
- `wireframe_zoo.rs:160` — `.round() as u32` — only used for logging, actual shape from `trigger_count % SHAPE_COUNT`
- `mri_volume.rs:173-175` — `.round() as i32` then `.clamp(0, max)` — safe

### VS-4: `uv_scale` division — guarded against zero

All generators use the pattern: `if scale > 0.0 { 1.0 / scale } else { 1.0 }`:
- `parametric_surface.rs:136`
- `plasma.rs:95`
- `basic_shapes_snap.rs:56`
- `concentric_tunnel.rs:99`
- `mri_volume.rs:218`

### VS-5: No per-frame allocations in stateless generators

Plasma, BasicShapesSnap, and ConcentricTunnel are fully stateless — no persistent state, no Vecs, no per-frame allocations. All uniform data is stack-allocated via bytemuck.

### VS-6: Line-based generators — pre-allocated scratch buffers

**Files:** `tesseract.rs`, `duocylinder.rs`, `lissajous.rs`, `oscilloscope_xy.rs`

`LineGeneratorHelper` pre-allocates `projected_x/y/z`, `positions`, `instances`, `edge_depth`, `edge_sorted_idx` at init. The `prepare_instances` method uses `.clear()` + `.push()` which reuses existing capacity. No per-frame heap allocations after the first frame.

### VS-7: Parametric Surface — persistent volume texture not pooled (correct)

**File:** `crates/manifold-renderer/src/generators/parametric_surface.rs:57-67`

The 128^3 volume texture is created at init and persists for the generator's lifetime. It is NOT pooled (correct — pooled textures are for transient per-frame use).

### VS-8: MRI Volume — dirty-check prevents redundant file loads

**File:** `crates/manifold-renderer/src/generators/mri_volume.rs:189-191`

Slice loading is gated by `need_load` check comparing `slice_index`, `scan_index`, and `axis` against cached values. Same slice is never loaded twice.

### VS-9: No `.unwrap()` on fallible paths in any audited generator

All generators use pattern matching, `if let`, or `Option::is_some_and()` for fallible operations. The only `.unwrap()` is in `mri_volume_loader.rs:60` on `file_name().unwrap_or_default()` which uses `unwrap_or_default` (safe).

### VS-10: Parametric Surface — 3D volume texture not involved in blur pass swap

**File:** `crates/manifold-renderer/src/generators/parametric_surface.rs`

This generator uses a single 3D volume for SDF baking (write-only from compute) then reads it during raymarch. There is no multi-pass blur on the volume, so the "which texture has the result after N blur passes" concern does not apply.

### VS-11: generator_math.rs — all math operations are NaN-safe

**File:** `crates/manifold-renderer/src/generators/generator_math.rs`

`rotate_4d`, `rotate_3d`: only use `sin_cos()` and multiply/add — cannot produce NaN from finite inputs. `project_4d`: division can produce Inf (see W-1) but not NaN from finite inputs. `hash_beat`: `sin()` + `abs()` + `fract()` — cannot produce NaN from finite inputs.

### VS-12: Duocylinder — base_verts allocated once at init

**File:** `crates/manifold-renderer/src/generators/duocylinder.rs:37`

`Vec::with_capacity(VERTEX_COUNT)` at init, never reallocated.

### VS-13: Lissajous and OscilloscopeXY — edge arrays fixed at init

**Files:** `lissajous.rs:38-43`, `oscilloscope_xy.rs:38-43`

Both generators build edge_a/edge_b arrays once at init (256 edges for closed loop) and never modify them during render.

### VS-14: Registry — no hot-path allocations

**File:** `crates/manifold-renderer/src/generators/registry.rs`

`GeneratorRegistry::create()` allocates via `Box::new()` but is only called during clip acquisition (per-action, not per-frame). `prewarm_all()` runs once at startup.

### VS-15: Per-owner generator state — cleanup path verified

**File:** `crates/manifold-renderer/src/generator_renderer.rs:573-594`

`stop_clip()` removes from `active_clips` (returns render targets to pool). `layer_generators` persists per-layer by design (matches Unity — preserves trigger_count across clips). `release_all()` clears both maps on project switch. Generator type changes are handled by `handle_gen_type_changed()` which replaces the generator instance in `layer_generators`.

### VS-16: Compute dispatch — line generators use instanced rendering, not compute

**Files:** `tesseract.rs`, `duocylinder.rs`, `lissajous.rs`, `wireframe_zoo.rs`, `oscilloscope_xy.rs`

Line-based generators use `LinePipeline::draw()` which calls `draw_instanced()` (render pipeline), not compute dispatch. Threadgroup validation is not applicable.

### VS-17: MRI Volume loader — handles all common TIFF pixel formats

**File:** `crates/manifold-renderer/src/generators/mri_volume_loader.rs:102-111`

U8 (passthrough), U16 (shift), F32 (clamp+scale) are handled. Unknown formats return `Err` (gracefully handled at `mri_volume.rs:203-206`).

---

## Recommendations (prioritized)

1. **C-1 (MRI file I/O):** Move TIFF loading to an async task or pre-load all slices into a CPU-side ring buffer at scan selection time. This is the only true frame-drop risk found.

2. **W-1 (project_4d division):** Clamp `proj_dist` to a minimum of 1.5 (double the max w extent of 1.0) to prevent near-infinity projections. Or add a small epsilon to the denominator.

3. **W-2 (f32 time precision):** Pass `ctx.time` as a wrapped/modular value rather than absolute seconds, or use f64 in GeneratorContext. The current f32 field loses precision after ~4.6 hours.

4. **W-3 (normalize_shape allocation):** Pre-normalize all shape tables at compile time (const arrays) or normalize once at generator init and cache per-shape.

5. **W-7 (anim_progress drift):** Replace subtraction-based wrap with `self.anim_progress %= total` after the increment.
