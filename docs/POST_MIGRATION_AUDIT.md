# Post-Migration Architecture Audit

Date: 2026-03-27

## Results Summary

**Passed:** 17 | **Fixed:** 5 | **Flagged:** 3

## Passed
- **1. Zero wgpu on content thread**: Content hot path uses only manifold-gpu types. wgpu remnants are correctly isolated to UI thread and LED subsystem.
- **3. Dummy encoder**: No dummy encoder patterns found anywhere.
- **4. Texture pool coverage**: All `device.create_texture()` calls outside the pool are legitimate persistent state (feedback textures, fluid volumes, trail buffers) or one-time setup.
- **5. Async compute resource isolation**: Each layer gets its own command buffer, effect chain, and scratch ping-pong. No shared mutable GPU resources between layer command buffers. Compositor waits on all layer events before blending.
- **6. Function constants**: Bloom (4 variants), compositor blend (13 modes), plasma (5 patterns), stylized feedback (3 modes), edge glow (3 modes), fluid display (2 modes) all create specialized pipelines in `new()` and select by mode in dispatch. No mode uniforms that duplicate constant values.
- **7. Binary archive**: Archive loads at startup (`init_native_gpu`), pipelines are added after creation (`create_compute_pipeline`), archive is saved after all pipelines are created (`save_pipeline_archive` at app.rs:1106). Hash-based invalidation via `pipeline_hash(wgsl_source, entry_point)`. Fallback to normal compilation when archive load fails.
- **8. Stale feature flags**: `hal-encoding` was the only stale flag. Removed (see Fixed). No `native-metal` flag found. `profiling` feature is active and correctly used.
- **9. Memory leaks**: TexturePool uses frame-stamped recycling — textures only reused after `frames_in_flight` frames. `cleanup_stopped_clips` calls `cleanup_clip_owner` on the effect registry. Pool has `clear()` for resolution changes. No unbounded accumulation in steady state.
- **10. Uniform arena sizing**: 64KB default (256 uniform writes at 256-byte alignment). `flush()` doubles capacity when exceeded and recreates the shared buffer. `reset()` resets cursor each frame — no overwrite of in-flight data because the arena buffer is separate from the per-layer command buffers that use inline `set_bytes` (not buffer offsets).
- **11. Event/fence reuse**: `async_signal_base` monotonically increases per frame by the number of active layers. Each layer signals `base + layer_index`, compositor waits for `base + layer_count`. `signal_event()` auto-increments for the main frame event. No value reuse within fence lifetime.
- **12. IOSurface bridge integrity**: Only the compositor command buffer copies to the IOSurface texture (content_pipeline.rs:491). Triple-buffered: `write_surface_index` cycles 0→1→2. `publish_front()` called only after GPU completion confirmed via `native_event.is_done()`. Surface swap happens after `native_enc.commit()`.
- **13. Pipeline creation error handling**: All pipeline creation uses `unwrap_or_else(|e| panic!("{label}: ..."))` with descriptive messages including shader label, error, and MSL source. Binary archive failures log warnings but don't crash. Naga parse/validation errors panic with full context.
- **15. Test coverage**: 485 tests pass. WGSL validation test auto-discovers all `.wgsl` files. No tests reference stale wgpu content-thread types. Tests are pure logic (editing, playback, serialization, UI parity) — GPU tests require device and are not run in CI.
- **19. Unsafe code audit**: All unsafe blocks in manifold-gpu are Metal FFI (retain/release, encoder pointer management, buffer writes). Lifetimes are correct — retained objects released in Drop impls. `GpuEncoder::commit()` ends encoder then commits. `TexturePool` uses `UnsafeCell` with single-thread guarantee. `shared_texture.rs` unsafe blocks correctly manage IOSurface FFI with null checks.
- **20. Thread safety**: No `Arc<GpuTexture>` or `Arc<GpuBuffer>` found. IOSurface is the only cross-thread resource (by design). TexturePool is content-thread-only. `SharedOutputView` with `wgpu::TextureView` exists but is the fallback bridge for non-macOS (not used on macOS).

## Fixed
- **2. Dead code cleanup**: Removed stale `hal-encoding` feature flag code from `blit.rs` (compute pipeline fields, cfg-gated compute blit path, `#[allow(dead_code)]` annotations). Removed empty `[features]` section and stale `check-cfg` for `hal-encoding` from `manifold-renderer/Cargo.toml`.
- **16. Debug logging on hot path**: Changed per-clip `eprintln!` in `generator_renderer.rs:224` to `log::debug!` (fires once per clip activation, not per-frame, but `eprintln!` is wrong for production code).
- **17. PERF timing prints**: Gated the periodic `[PERF/NATIVE]` print (content_pipeline.rs:523-538) behind `#[cfg(feature = "profiling")]`. The timing `Instant::now()` calls remain (cheap, underscore-prefixed) — only the `eprintln!` output is suppressed in non-profiling builds.
- **18. String allocations in labels**: Replaced per-frame `format!("Layer {layer_idx}")` in async compute encoder creation (layer_compositor.rs:865) with static `"Layer"` string. All other `format!` calls in label paths are init-time only (BlendResources::new, PingPong::new, ensure_layer_bufs).

## Flagged (needs human review)
- **1. LED controller on content thread**: `content_thread.rs:114,289,1240` — LED controller uses `self.gpu.device` (wgpu::Device) for `initialize()` and `poll_readback()` on the content thread. This is a known gap (marked `// TODO: LED output needs migration to manifold_gpu types` at line 284). LED processing is disabled in the native Metal render path. Requires its own migration pass.
- **1. SharedOutputView**: `content_pipeline.rs:22` — `SharedOutputView` wraps `wgpu::TextureView` for cross-thread sharing. On macOS, IOSurface is used instead (this struct is only for the future non-macOS fallback path). Not a bug, but the wgpu type on the content thread side is architecturally vestigial.
- **14. Clippy cleanliness**: `cargo clippy --workspace -- -D warnings` passes clean. The only warnings are from the ObjC build system (deprecated `tracksWithMediaType:` in manifold-media native code) — not Rust clippy warnings.

## Detailed Findings

### 1. Zero wgpu on content thread
**Files checked:** content_pipeline.rs, content_thread.rs, gpu_encoder.rs, all effects, all generators, layer_compositor.rs, compositor.rs, effect_chain.rs, generator_renderer.rs, render_target.rs

**Result:** The content hot path (`render_content_native` → generators → effect chains → compositor → IOSurface copy) uses exclusively `manifold_gpu` types: `GpuDevice`, `GpuEncoder`, `GpuTexture`, `GpuBuffer`, `GpuComputePipeline`. Zero `wgpu::Device`, `wgpu::Queue`, or `wgpu::CommandEncoder` on the hot path.

**Remaining wgpu on content thread (non-hot-path):**
- `SharedOutputView` with `wgpu::TextureView` — fallback bridge for non-macOS (unused on macOS)
- LED controller init and readback (`self.gpu.device`) — pending migration
- `gpu_profiler.rs` — behind `#[cfg(feature = "profiling")]`, uses wgpu timestamp queries

### 2. Dead code cleanup
**Found:** `blit.rs` had `#[cfg(all(target_os = "macos", feature = "hal-encoding"))]` on compute pipeline fields and dispatch path. Since `hal-encoding` feature was removed from all Cargo.toml files, this code was permanently dead. `manifold-renderer/Cargo.toml` still had `check-cfg` allowance for the removed feature.

**Fixed:** Removed all hal-encoding cfg attributes, compute pipeline fields, and compute blit dispatch path from `blit.rs`. Removed stale Cargo.toml entries.

### 3. Dummy encoder
**Files checked:** content_pipeline.rs, gpu_encoder.rs

**Result:** No dummy encoder creation found. All encoders are created for actual GPU work.

### 4. Texture pool coverage
**Files checked:** All `device.create_texture()` calls in manifold-renderer/src/

**Direct device allocation (legitimate):**
- `render_target.rs:23` — `RenderTarget::new()` fallback when pool not available
- `blob_tracking.rs:536` — persistent per-owner detection texture
- `wireframe_depth.rs:267,425` — persistent depth/flow state textures
- `mycelium.rs:179,188` — persistent trail ping-pong (feedback state)
- `fluid_simulation.rs:337-368` — persistent density/velocity fields
- `fluid_simulation_3d.rs:329,350` — persistent 3D volumes
- `mri_volume.rs:127` — one-time data upload texture
- `parametric_surface.rs:59` — persistent volume texture
- `layer_bitmap_gpu.rs:237` — UI-thread wgpu texture (not content)

All are persistent state or one-time setup. Transient textures go through `TexturePool::acquire()` or `RenderTargetPool::get()`.

### 5. Async compute resource isolation
**Files checked:** layer_compositor.rs (composite_parallel, lines 750-1060)

**Verified:**
- Each layer gets its own `GpuEncoder` wrapping a per-layer `device.create_encoder("Layer")` command buffer
- Each layer uses a unique effect chain index (`effect_chain_idx++`) — no sharing
- Multi-clip layers get unique scratch buffers (`layer_buf_idx++`) — no sharing
- The only shared mutable state is `self.effect_registry` — but effects use per-owner-key state (hash of clip_id/layer_id), so different layers access different buckets
- Compositor command buffer waits via `wait_event(async_event, final_signal)` before blending

### 6. Function constants
**Files checked:** bloom.rs, layer_compositor.rs (blend), plasma.rs, stylized_feedback.rs, edge_glow.rs, fluid_simulation.rs (display)

**Verified for each:**
- Bloom: 4 variants (blur_h, blur_v, threshold, composite) — `create_specialized_compute_pipeline` with `("BLOOM_PASS", "0u"/"1u"/"2u"/"3u")`
- Compositor blend: 13 modes — `create_specialized_compute_pipeline` with `("u.blend_mode", "{mode}u")` for each mode 0..12
- Plasma: 5 patterns — `create_specialized_compute_pipeline` with `("u.pattern_type", "{n}u")`
- Stylized feedback: 3 modes — specialized pipeline selection based on feedback_mode
- Edge glow: 3 modes — specialized pipeline selection based on edge detection mode
- Fluid display: 2 modes (mono/color) — `create_specialized_compute_pipeline` with color_mode specialization

All select correct pipeline in dispatch code. No duplicate mode uniforms.

### 7. Binary archive
**Files checked:** archive.rs, mod.rs (create_compute_pipeline), content_pipeline.rs (init_native_gpu), app.rs (save call)

**Flow:**
1. `init_native_gpu()` → `load_pipeline_archive(cache_dir/pipeline_cache.metallib)` → loads or creates empty archive
2. Each `create_compute_pipeline()` → sets `binary_archives` on descriptor → auto-populates on miss via `add_compute_pipeline_functions_with_descriptor`
3. After all pipelines created: `save_pipeline_archive()` → serializes to disk if dirty
4. Cache invalidation: `pipeline_hash(wgsl_source, entry_point)` — if shader changes, hash changes, pipeline recompiles, archive updated

**Fallback:** If archive loading fails → creates empty archive → compiles normally → saves new archive.

### 8. Stale feature flags
**Files checked:** All Cargo.toml files

**Found and fixed:** `hal-encoding` in manifold-renderer/Cargo.toml check-cfg.
**Clean:** No `native-metal` feature. `profiling` feature is active and correctly gated.

### 9. Memory leaks
**Files checked:** TexturePool (mod.rs:687-849), render_target_pool.rs, effect_registry.rs, content_pipeline.rs (cleanup_stopped_clips)

**Verified:**
- `TexturePool::release()` returns textures with frame stamp → `acquire()` recycles after `frames_in_flight` frames
- `TexturePool::begin_frame()` increments frame counter (no pruning, but pool is bounded by peak usage)
- `cleanup_stopped_clips()` → `cleanup_clip_owner()` → removes per-owner effect state from registry
- `RenderTargetPool::release()` either returns to TexturePool or caches locally
- Pool `clear()` exists for resolution change cleanup

**Note:** TexturePool does not actively prune old entries. Textures sit until recycled or `clear()` is called. This is correct for steady-state but could accumulate after many resolution changes without explicit clear.

### 10. Uniform arena sizing
**Files checked:** uniform_arena.rs

**Verified:**
- 64KB default capacity (256 writes × 256-byte alignment)
- `push_bytes()` grows capacity tracking and recreates buffer in `flush()` if exceeded
- `reset()` resets cursor each frame — no overwrite risk
- On native path, arena buffer is NOT used for GPU reads — `set_bytes()` copies inline data per dispatch. Arena offset tracking is preserved for compatibility but data path is inline.

### 11. Event/fence reuse
**Files checked:** layer_compositor.rs (async_signal_base), mod.rs (GpuEvent, signal_event, signal_event_value)

**Verified:**
- `signal_event()` auto-increments: `counter.get() + 1` → `counter.set(value)` → `encode_signal_event`
- `signal_event_value()` uses caller-specified value (for async compute layer signals)
- `async_signal_base` starts at 0, increments by `layer_signal_idx` each frame → monotonically increasing
- Each layer signals unique value: `base + layer_signal_idx` (1-indexed)
- Compositor waits for `base + total_layers` (the last signal)
- No value reuse possible — values only increase

### 12. IOSurface bridge integrity
**Files checked:** shared_texture.rs, content_pipeline.rs (render_content_native)

**Verified:**
- Only `native_enc.copy_texture_to_texture()` at content_pipeline.rs:491 writes to IOSurface texture
- No layer command buffer references IOSurface textures (they reference clip textures from GeneratorRenderer)
- `write_surface_index` cycles 0→1→2 (triple buffering)
- `publish_front()` called at content_pipeline.rs:233 only AFTER `native_event.is_done(native_signal_value)` confirms GPU completion
- Surface swap (`write_surface_index = (idx + 1) % 3`) happens AFTER `native_enc.commit()`
- `surface_signal_values[]` tracks per-surface fence values; surface reuse waits for that value

### 13. Pipeline creation error handling
**Files checked:** mod.rs (create_compute_pipeline, create_render_pipeline)

**Pattern:** `unwrap_or_else(|e| panic!("{label}: MTL library compile error: {e}\nMSL source:\n{msl_source}"))` for library compilation, `unwrap_or_else(|e| panic!("{label}: function '{name}' not found: {e}. Available: {names:?}"))` for function lookup, `unwrap_or_else(|e| panic!("{label}: MTL compute PSO error: {e}"))` for pipeline state.

All pipeline creation failures are fatal with descriptive panic messages including shader label, error, and available functions. Binary archive add failures are non-fatal (log::warn). This is correct — pipeline compilation is startup-only.

### 14. Clippy cleanliness
`cargo clippy --workspace -- -D warnings` passes clean. No Rust warnings.

### 15. Test coverage
485 tests pass. Test breakdown:
- `manifold-editing/tests/` — service integration, undo roundtrip, command roundtrips (55+4+10 tests)
- `manifold-io/tests/` — project loading (1 test)
- `manifold-playback/tests/` — engine tick, live clip (46+24 tests)
- `manifold-renderer/tests/` — WGSL validation (214 tests — auto-discovers all shaders)
- `manifold-ui/tests/` — UI parity (41 tests)

No tests reference stale wgpu content-thread types. WGSL validation test uses naga directly (no GPU device needed), ensuring all shaders parse and validate.

### 16. Debug logging on hot path
**Files checked:** All effects, generators, layer_compositor, effect_chain, generator_renderer, gpu_encoder, manifold-gpu, content_pipeline

**Per-frame code (hot path):**
- `effect_chain.rs:137`: `log::debug!` — in filter closure, but `debug!` is compiled out in release. OK.
- `generator_renderer.rs:224`: Was `eprintln!`, now `log::debug!` (FIXED). Per-clip init, not per-frame.

**Init-time logging (correctly kept):**
- `wireframe_depth.rs:783,787`: `log::info!` worker spawn
- `mri_volume.rs:67-95`: `log::info!` data loading
- `archive.rs:44,92`: `log::info!` archive load/save
- `metalfx.rs:121,227,230`: `eprintln!` init probes (one-time)
- `mod.rs:743`: `log::info!` TexturePool creation

### 17. PERF timing prints
**Found:** content_pipeline.rs:262-538 — 4× `Instant::now()` + `elapsed()` per frame, plus `eprintln!("[PERF/NATIVE] ...")` every 60 frames.

**Fixed:** Gated the `eprintln!` block behind `#[cfg(feature = "profiling")]`. Timing variables retained (cheap, underscore-prefixed, used by profiler when enabled).

### 18. String allocations in labels
**Found:** `format!("Layer {layer_idx}")` at layer_compositor.rs:865 — per-frame in async compute path.

**Fixed:** Replaced with static `"Layer"` string. All other `format!` calls in label paths are init-time only.

### 19. Unsafe code audit
**Files checked:** manifold-gpu/src/ (all), gpu_encoder.rs, content_pipeline.rs, shared_texture.rs

**manifold-gpu unsafe blocks:**
- `objc_retain`/`objc_release` — correct ObjC memory management. Retained objects always released in Drop.
- `GpuEncoder` command buffer pointer — retained on creation (mod.rs:391), released in Drop (mod.rs:1308). Compute encoders additionally retained/released (ensure_compute/end_current).
- `GpuBuffer::write()` — documented safety requirements (offset + len <= size, no GPU overlap). Frame-stamped pool prevents GPU overlap.
- `TexturePool` UnsafeCell — single-thread guarantee (content thread only).
- `MetalFxSpatialScaler` msg_send! — correct selectors, null checks on creation.

**shared_texture.rs unsafe blocks:**
- IOSurface creation — CFMutableDictionary + IOSurfaceCreate. Null check on result.
- `import_texture` / `import_texture_native` — msg_send for `newTextureWithDescriptor:iosurface:plane:`. Null check on result. Correct ownership transfer (from_ptr takes +1 retain).

No use-after-free risks identified. All pointers have clear ownership and lifetime guarantees.

### 20. Thread safety
**Verified:**
- No `Arc<GpuTexture>`, `Arc<GpuBuffer>`, or static/global GPU resources found
- IOSurface is the ONLY cross-thread resource (by design, with atomic front_index)
- TexturePool is content-thread-only (UnsafeCell, single-thread guarantee)
- GpuEncoder is content-thread-only (not Send across thread boundary in practice)
- SharedOutputView crosses threads but only carries wgpu::TextureView (unused on macOS)

### 21. Dead pipeline paths
**Files checked:** All effects and generators for unused pipeline fields, all TODO/FIXME/HACK markers

**No dead pipelines found.** Every `native_pipeline` or specialized pipeline field is referenced in the dispatch path.

**MPS API:** 27 operations exposed (blur, Sobel, scale, etc.). `MpsLanczosScale` is actively used by `TextureUpscaler`. Other MPS operations are available but not called by any effect (as documented — API available for future use). Not dead code — intentional API surface.

**TODO markers on content thread:**
- `content_pipeline.rs:391` — "TODO: Port VideoRenderer to manifold-gpu types"
- `content_thread.rs:284` — "TODO: LED output needs migration to manifold_gpu types"
- These are known gaps, not dead code.

### 22. End-to-end dispatch verification
**Traced path for each frame:**

1. `content_pipeline::render_content()` → `render_content_native()`
2. Creates `GpuEncoder` → `GeneratorRenderer::render_all()` → each generator's `generate()` dispatches compute/render via `GpuEncoder`
3. Creates second `GpuEncoder` → `Compositor::render()` → `composite_serial()` or `composite_parallel()`
4. `generate_layers()` — each layer: clips → effects → layer output texture
5. `blend_layers()` — serial blend of all layer outputs into main accumulator
6. Master effects → tonemap → final output
7. `copy_texture_to_texture()` to IOSurface
8. `signal_event()` + `commit()`

No effects that create pipelines but never encode work. No generators that allocate but never write. No early returns gated on always-true/false conditions after migration (the `has_native_encoder()` check was fully removed).
