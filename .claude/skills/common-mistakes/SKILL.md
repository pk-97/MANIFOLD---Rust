---
name: common-mistakes
description: Recurring bugs and mistakes from past sessions, by area. Invoke BEFORE touching an unfamiliar subsystem to check its known traps.
---
# Common Mistakes Guide

Mined from 266 conversation sessions. These are real bugs that have occurred multiple
times. Check this list BEFORE starting work in any of these areas.

## GPU / Metal / Shader

### 1. Uniform struct misalignment with WGSL
**Bug:** Silent corruption — wrong colors, flickering, white dots, geometry in wrong position.
**Why it happens:** Rust struct field order or padding doesn't match WGSL layout. No compile error.
**Rule:** `#[repr(C)]`, 16-byte aligned, `_pad` fields, field order MUST match WGSL exactly.
**Verify:** Count bytes in both Rust and WGSL. Print `std::mem::size_of::<MyUniform>()` and compare.

### 2. Shared uniform buffer across multiple dispatches
**Bug:** All passes render with the last-written values. Blend modes wrong, blur artifacts.
**Why it happens:** One buffer bound to N compute/render passes. Last `set_uniform` wins.
**Rule:** Each dispatch that needs different uniform values needs its own buffer or offset.

### 3. wgpu types on the content thread
**Bug:** Re-introduces the overhead the Metal migration eliminated. Panics or stalls.
**Why it happens:** When fixing a GPU panic, reaching for the familiar wgpu path (e.g. `Queue::submit()`, `map_async`).
**Rule:** NEVER use `wgpu::*` on the content thread. Use `manifold-gpu` types only. If you need GPU readback, use `MTLResourceOptions::StorageModeShared` + CPU-side copy.

### 4. 3D workgroup size exceeding Metal limit
**Bug:** Pipeline creation crash.
**Why it happens:** Copying 2D pattern (`@workgroup_size(16,16)` = 256) to 3D (`8,8,8` = 512).
**Rule:** `max_compute_invocations_per_workgroup` = 256. Use `@workgroup_size(4,4,4)` for 3D.

### 5. R16Float used for storage textures
**Bug:** Metal validation error or crash.
**Why it happens:** Seems like a reasonable format for single-channel storage.
**Rule:** `R16Float` does NOT support `STORAGE_BINDING` on Metal. Use `R32Float` or `Rgba16Float`.

### 6. presentsWithTransaction on background thread
**Bug:** Output window goes black. Presents silently discarded.
**Why it happens:** `presentsWithTransaction = true` needs a CA transaction (main thread only).
**Rule:** Only call from main thread. CVDisplayLink callbacks are background threads.

### 7. Texture pool recycling within same frame
**Bug:** GPU memory aliasing — layers sharing textures, flickering, corruption.
**Why it happens:** Texture returned to pool and reissued while GPU is still reading it.
**Rule:** Frame-stamped recycling. Delay N frames (N = frames in flight) before reuse.

### 8. Math rounding divergence from Unity
**Bug:** Twitchy behavior, values oscillating frame-to-frame.
**Why it happens:** `x as i32` truncates. Unity `Mathf.RoundToInt()` rounds.
**Rule:** Use `x.round() as i32`. Also: `Sign(0) = 1.0` (Unity), NOT `0.0`.

## Compositor / Rendering

### 9. Clips not sorted descending by layer_index before compositor
**Bug:** Wrong blend order. Lower layers render on top.
**Why it happens:** After project load or clip list rebuild, insertion order != layer order.
**Rule:** Always sort clips descending by `layer_index` before passing to compositor.
**This is a RECURRING bug** — it has broken at least 3 times.

### 10. HDR intermediate textures at quarter resolution
**Bug:** Blocky bloom/halation with no smooth rolloff.
**Why it happens:** Matching generator internal resolution (0.25x) for effect buffers.
**Rule:** HDR intermediates need HALF resolution minimum. Quarter is always blocky.

## Effects / Generators

### 11. Synthesizing effect code from descriptions instead of reading source
**Bug:** Behavioral divergence — wrong constants, missing math ops, broken output.
**Why it happens:** Agent writes code from memory of what an effect "should do" rather than reading the actual shader/uniform code.
**Rule:** ALWAYS read the actual shader `.wgsl` file and Rust uniform struct BEFORE modifying any effect. Match every constant, format, and math op exactly.

### 12. Effect/generator removed from Vec registry without compiler error
**Bug:** Effect silently disappears. No crash, no warning, just doesn't render.
**Why it happens:** Registries are `Vec<Box<dyn Trait>>` — removing an entry compiles clean.
**Rule:** After removing/adding anything in a registry, grep for the type ID to verify all 6-7 locations are consistent (see CLAUDE.md recipes).

### 13. Unknown EffectTypeId defaulting to Transform on load
**Bug:** Removed effects become Transform, applying spurious geometry changes.
**Why it happens:** Deserializer fallback matches unrecognized strings to a default.
**Rule:** Unknown type IDs must be stripped on load, not defaulted.

## Content Thread / Architecture

### 14. Writing to param_values instead of base_param_values
**Bug:** Slider snaps back after drag. Undo/redo broken.
**Why it happens:** `param_values` is the live/modulated copy overwritten every tick by playback. `base_param_values` is the user-owned source of truth.
**Rule:** UI writes to `base_param_values`. Always.

### 15. CVDisplayLink callback doing heavy work
**Bug:** Frame drops, stuttering, output goes black.
**Why it happens:** CVDisplayLink skips the next callback if the current one overruns vsync.
**Rule:** Callbacks must be < 1μs. Signal a condvar or set a flag, nothing more.

### 16. Command buffer references dropped before GPU finishes
**Bug:** GPU stalls, content thread locks up, command channel fills.
**Why it happens:** Rust drops MTLCommandBuffer before GPU execution completes.
**Rule:** Hold command buffer references until the completion handler fires.

### 17. Three CVDisplayLinks unified into one
**Bug:** Everything breaks. Three separate reverts required.
**Why it happens:** Seems simpler. Is catastrophically wrong.
**Rule:** Content, presenter, and UI each need independent CVDisplayLinks. NEVER unify them.

## UI Scrolling / Clipping

### 18. ScrollContainer without content_height set
**Bug:** Scroll is frozen at 0. Elements don't move when scroll offset is set.
**Why it happens:** `set_scroll_offset()` clamps to `max_scroll()` which requires `content_height`. If content_height is 0 (the default), everything clamps to 0.
**Rule:** Always call `set_content_height()` after building content — even for externally-driven scroll (e.g. LayerHeader mirrors viewport's Y scroll).

### 19. Scroll speed double-multiplication
**Bug:** Scrolling is N× too fast after refactoring scroll handling.
**Why it happens:** The app layer normalizes scroll delta (winit `LineDelta × 20px`). If ScrollContainer also applies a multiplier, the speed compounds. The old Inspector `SCROLL_SPEED=1.0` was correct because delta was pre-scaled.
**Rule:** `ScrollContainer::SCROLL_SPEED` is `1.0`. The app layer handles normalization in `app.rs` (`LINE_DELTA_PX = 20.0`). Don't add another multiplier.

### 20. Text/icon clipping at scroll boundaries
**Bug:** Text renders past the clip region boundary while rect backgrounds are correctly clipped.
**Why it happens:** Text renderer clips at the glyph level (skip entire glyphs outside clip), but partially-overlapping glyphs are drawn in full. Rects use GPU scissor rects which clip at the pixel level.
**Rule:** Text and icons must clip at the pixel level by adjusting quad positions AND atlas UVs at the clip boundary (see `native_text.rs` glyph clipping code).

### 21. Scissor batch state lost across multiple sub-region renders
**Bug:** Text ghosting — old text values persist under new text on inspector cards during playback modulation. Only affects cards with a dirty sub-region rendered AFTER them (e.g., gen params + effect card both modulating).
**Why it happens:** `render_sub_region()` was calling `begin_scissor_tracking()` which clears `scissor_batches`. When the cache manager renders N dirty sub-regions in a loop before a single `prepare_and_draw()`, each sub-region's `begin_scissor_tracking()` discards the previous sub-region's scissor batches. The rect commands (backgrounds) are generated but never drawn. The text renderer is a separate pipeline that accumulates independently, so new text IS drawn — but without the background rects to clear old atlas content, old text persists underneath.
**Rule:** `render_sub_region` must use `begin_scissor_tracking_additive()` (preserves previous batches). Only the first traversal in a cycle (e.g. `render_tree`, `render_overlay_range`) should clear scissor batches. Any method that renders multiple sub-regions into a single prepare/draw cycle must preserve accumulated scissor state.
**Diagnostic clue:** If text updates during interaction (triggers full panel render) but not during playback (uses incremental sub-region path), suspect scissor batch or rect command loss in the incremental path.

## UI Actions / Dispatch

### 22. DispatchResult::handled() when structural() is needed
**Bug:** Button click does nothing — data toggles but UI doesn't update.
**Why it happens:** Handler returns `DispatchResult::handled()` (no rebuild) instead of `DispatchResult::structural()`. The data changes on both UI and content threads, but the UI tree is never rebuilt, so visual state (button colors, styles set at build time) stays stale. The button *works* but *looks* broken.
**Rule:** If a dispatch handler changes data that affects UI appearance set at `build()` time (button styles, visibility, layout), return `DispatchResult::structural()`. Only use `handled()` for changes that `sync_values()` can patch incrementally (slider positions, label text).
**Check first:** When a UI button/toggle "doesn't work", verify the handler's `DispatchResult` before investigating click detection or data flow.
