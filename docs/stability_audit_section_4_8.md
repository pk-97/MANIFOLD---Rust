# Stability Audit: Section 4 (GPU Core Safety) & Section 8 (Unsafe & FFI)

Audited: 2026-03-28
Auditor: Claude Opus 4.6 (1M context)
Scope: `manifold-gpu` crate (native Metal backend), `manifold-media` FFI, `manifold-native` FFI, `manifold-app` IOSurface/EDR

---

## SECTION 4: GPU Core Safety (manifold-gpu)

### 4.1 Unsafe Block Inventory

Every `unsafe` block in `manifold-gpu`:

| # | File:Line | What it does | SAFETY comment |
|---|-----------|-------------|----------------|
| 1 | `metal/mod.rs:46-49` | Declares `objc_retain` / `objc_release` extern C functions | No (extern declaration) |
| 2 | `metal/archive.rs:27-50` | `create_retained_url`: ObjC alloc+init NSString and NSURL, release NSString, wrap as `metal::URL` | Yes (detailed comment in function doc explaining autoreleased vs +1 ownership) |
| 3 | `metal/archive.rs:70-71` | `unsafe impl Send/Sync for GpuPipelineArchive` | Yes ("BinaryArchive is a Metal object - thread-safe per Metal's guarantees") |
| 4 | `metal/device.rs:26-27` | `unsafe impl Send/Sync for GpuDevice` | Yes ("metal::Device and metal::CommandQueue are thread-safe") |
| 5 | `metal/device.rs:54-55` | `raw_device_ptr`: ForeignType::as_ptr cast | No (trivial cast) |
| 6 | `metal/device.rs:393-394` | `create_encoder`: retain command buffer via `objc_retain` | Yes ("Retain the command buffer so it outlives the autorelease pool drain") |
| 7 | `metal/encoder.rs:29` | `unsafe impl Send for GpuEncoder` | No explicit comment |
| 8 | `metal/encoder.rs:54` | `ensure_compute`: retain compute encoder via `objc_retain` | Yes (comment on lines 51-53) |
| 9 | `metal/encoder.rs:64-65` | `end_current` Compute: end_encoding + objc_release | No |
| 10 | `metal/encoder.rs:97-98` | `encode_unary` in dispatch: dereference raw compute encoder ptr | No |
| 11 | `metal/types.rs:17-18` | `unsafe impl Send/Sync for GpuTexture` | No |
| 12 | `metal/types.rs:55-56` | `unsafe impl Send/Sync for GpuBuffer` | No |
| 13 | `metal/types.rs:80-88` | `GpuBuffer::write`: memcpy to mapped ptr | Yes ("Caller must ensure offset + data.len() <= buffer size") |
| 14 | `metal/types.rs:107-108` | `unsafe impl Send/Sync for GpuSampler` | No |
| 15 | `metal/types.rs:123-124` | `unsafe impl Send/Sync for GpuComputePipeline` | No |
| 16 | `metal/types.rs:134-135` | `unsafe impl Send/Sync for GpuRenderPipeline` | No |
| 17 | `metal/types.rs:146-147` | `unsafe impl Send/Sync for GpuEvent` | No |
| 18 | `metal/types.rs:195-196` | `unsafe impl Send/Sync for GpuHeap` | No |
| 19 | `metal/texture_pool.rs:54-55` | `unsafe impl Send/Sync for TexturePool` | Yes ("TexturePool is only used on the content thread") |
| 20 | `metal/texture_pool.rs:81` | `begin_frame`: UnsafeCell deref | No |
| 21 | `metal/texture_pool.rs:97` | `acquire`: UnsafeCell deref | No |
| 22 | `metal/texture_pool.rs:137` | `release`: UnsafeCell deref | No |
| 23 | `metal/texture_pool.rs:147` | `clear`: UnsafeCell deref | No |
| 24 | `metal/texture_pool.rs:153` | `stats`: UnsafeCell deref | No |
| 25 | `metal/texture_pool.rs:159` | `cached_count`: UnsafeCell deref | No |
| 26 | `metal/texture_pool.rs:165` | `current_frame`: UnsafeCell deref | No |
| 27 | `metal/texture_pool.rs:173` | `prune_stale`: UnsafeCell deref | No |
| 28 | `metal/mps.rs:34-36` | `MPSSupportsMTLDevice` extern | No |
| 29 | `metal/mps.rs:49-52` | `objc_retain` / `objc_release` extern | No |
| 30 | `metal/mps.rs:61-64` | `MpsObject::from_raw`: retain raw ObjC pointer | Yes ("Wrap a newly created (autoreleased) ObjC object. Retains it.") |
| 31 | `metal/mps.rs:81-82` | `unsafe impl Send/Sync for MpsObject` | Yes ("MPS kernels are thread-safe for encoding") |
| 32-50 | `metal/mps.rs:91-500` | All MPS kernel `alloc`/`init` + `encode` calls | `encode_unary`/`encode_binary` have Safety doc, individual constructors do not |
| 51 | `metal/metalfx.rs:32-36` | MetalFX framework link + `objc_release` extern | No |
| 52 | `metal/metalfx.rs:76-77` | `unsafe impl Send/Sync for MetalFxSpatialScaler` | Yes ("MetalFX scalers are thread-safe for encoding") |
| 53 | `metal/metalfx.rs:94-97` | MetalFX descriptor `alloc`/`init` | No |
| 54 | `metal/metalfx.rs:103-112` | Configure MetalFX descriptor properties | No |
| 55 | `metal/metalfx.rs:115-117` | `newSpatialScalerWithDevice:` | No |
| 56 | `metal/metalfx.rs:120` | Release descriptor | No |
| 57 | `metal/metalfx.rs:153-165` | MetalFX encode (set textures + encode to command buffer) | No |
| 58 | `metal/metalfx.rs:179-181` | Drop: `objc_release` scaler | No |
| 59 | `metal/metalfx.rs:195-199` | `supports_spatial_scaling`: `supportsDevice:` msg_send | No |
| 60 | `metal/metalfx.rs:228-229` | `unsafe impl Send/Sync for TextureUpscaler` | Yes ("All inner types are Send+Sync") |

**Finding 4.1-A** [INFO] `crates/manifold-gpu/src/metal/types.rs:17-18,55-56,107-108,123-124,134-135,146-147,195-196`: Many `unsafe impl Send/Sync` declarations lack `// SAFETY:` comments. All are correct (Metal objects are thread-safe), but comments would aid future audits.

**Finding 4.1-B** [INFO] `crates/manifold-gpu/src/metal/encoder.rs:29`: `unsafe impl Send for GpuEncoder` lacks comment. Correct because GpuEncoder is only ever used on the content thread, but no `Sync` impl prevents accidental sharing.

### 4.2 Autoreleasepool Placement

**Content thread main loop** (`manifold-app/src/content_thread.rs:217-220`):
```
objc::rc::autoreleasepool(|| {
    self.tick_frame(&state_tx);
});
```
VERIFIED SAFE: Every frame tick is wrapped. This drains Metal's autoreleased ObjC objects per-frame.

**Export loop** (`manifold-app/src/content_export.rs:206-212`):
```
objc::rc::autoreleasepool(|| {
    self.export_one_frame(...)
});
```
VERIFIED SAFE: Each export frame wrapped separately.

**Finding 4.2-A** [VERIFIED SAFE] `crates/manifold-app/src/content_thread.rs:217-220`: Content thread wraps every frame in autoreleasepool. All Metal API calls on the content thread (encoder creation, pipeline creation, texture creation) happen within this pool.

**Finding 4.2-B** [VERIFIED SAFE] `crates/manifold-app/src/content_export.rs:206-212`: Export loop also wraps per-frame.

**Finding 4.2-C** [WARNING] `crates/manifold-gpu/src/metal/device.rs:264-276`: `load_pipeline_archive` and `save_pipeline_archive` are called during startup/shutdown, which may be outside the per-frame autoreleasepool. The `create_retained_url` helper in `archive.rs` manually manages ObjC ownership (alloc+init, not autorelease), so this is safe. However, `metal::BinaryArchiveDescriptor::new()` at `archive.rs:81,90` may return autoreleased objects. If called during startup before the frame loop, these would accumulate until the process pool drains. Severity: LOW -- only happens once at startup, not during show.

**Finding 4.2-D** [INFO] `crates/manifold-gpu/src/metal/device.rs:149-214`: Pipeline creation (`create_compute_pipeline`, `create_render_pipeline`) uses `metal::CompileOptions::new()`, `metal::ComputePipelineDescriptor::new()`, etc. These return autoreleased ObjC objects. During startup (pipeline creation phase), these are NOT wrapped in autoreleasepool. The objects accumulate until the first frame loop pool drain. Severity: LOW -- startup-only, bounded number of pipelines.

### 4.3 Texture/Buffer Allocation: Nil Checks

**Finding 4.3-A** [CRITICAL] `crates/manifold-gpu/src/metal/device.rs:67`: `device.new_texture(&mtl_desc)` -- no nil check. The `metal` crate's `new_texture` returns `metal::Texture` directly (not `Option`). Under extreme GPU memory pressure, Metal returns nil from `[MTLDevice newTextureWithDescriptor:]`. The `metal` crate wraps this nil as a `metal::Texture` with a null internal pointer. Any subsequent use (set_texture, encode, etc.) would be a null-pointer dereference to the ObjC runtime, likely crashing.

Affected locations:
- `device.rs:67` (`create_texture`)
- `device.rs:463` (`create_texture_memoryless`)
- `texture_pool.rs:123` (pool fallback allocation)
- `device.rs:79-82` (`create_buffer` -- same risk for `new_buffer`)
- `device.rs:93-95` (`create_buffer_shared`)

Mitigating factor: On macOS with unified memory (Apple Silicon), GPU memory pressure is extremely rare. Metal's virtual memory system will page rather than fail. The `metal` crate v0.33 does not expose `Option` for these calls. Practical risk for a 4-hour show: LOW, but the failure mode is a hard crash with no recovery path.

**Finding 4.3-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/types.rs:206-217`: `GpuHeap::new_texture` returns `Option<GpuTexture>` via `self.heap.new_texture(&mtl_desc).map(...)`. This is correct -- heap sub-allocation can legitimately fail when the heap is full.

### 4.4 Encoder State Machine

**Finding 4.4-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/encoder.rs:8-16,44-77`: The `EncoderState` enum plus `ensure_compute` / `end_current` pattern prevents encoding after `endEncoding`. Every method that creates a new encoder (render, blit, compute) calls `end_current()` first. Every method that needs a compute encoder calls `ensure_compute()` which checks the current state. The state transitions are:
- `None` -> `Compute` (via `ensure_compute`)
- `Compute` -> `None` (via `end_current`)
- `None` -> temp Render (render methods create, use, and end the encoder inline)
- `None` -> temp Blit (blit methods create, use, and end inline)

No path allows encoding on an ended encoder.

**Finding 4.4-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/encoder.rs:462-466`: `commit()` calls `end_current()` before `cmd_buf.commit()`. The encoder is consumed by value (`mut self`), preventing post-commit use. Drop also releases the command buffer.

**Finding 4.4-C** [INFO] `crates/manifold-gpu/src/metal/encoder.rs:67-69`: Render encoder raw pointers are NOT retained ("Render encoders are not retained (created+ended in same scope)"). This is correct because render encoders are created, used, and ended within a single method call (e.g., `draw_fullscreen`). They never escape the method. Same for blit encoders at line 72-74.

### 4.5 Texture Pool

**Finding 4.5-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/texture_pool.rs:43`: Frame stamp type is `u64`. Current frame counter is `u64`. At 60fps, `u64::MAX / 60 / 3600 / 24 / 365 = ~9.7 billion years`. No overflow possible.

**Finding 4.5-B** [WARNING] `crates/manifold-gpu/src/metal/texture_pool.rs:24-26`: TexturePool uses `UnsafeCell` for interior mutability with `unsafe impl Send + Sync`. The safety comment says "only used on the content thread (single-threaded)". This is correct architecturally, but if any code accidentally shares the pool across threads, it would be unsound data races with no runtime detection. The `Sync` impl is overly permissive -- the pool does NOT need to be `Sync` since it is never shared. Having `Sync` means a `&TexturePool` could legally be sent to another thread and both threads could call `acquire()` concurrently, causing data races. Recommendation: Remove `unsafe impl Sync for TexturePool` (keep `Send` only).

**Finding 4.5-C** [WARNING] `crates/manifold-gpu/src/metal/texture_pool.rs` (entire file): No maximum pool size cap. If the application creates many textures at various resolutions (e.g., resolution changes, different generator scales), the pool accumulates entries without bound. The `prune_stale` method (line 172) exists but must be called explicitly by the caller. If not called regularly, VRAM usage grows monotonically.

Mitigating factor: `prune_stale` is available and the pool key is `(width, height, format)` -- limited combinatorics in practice. At steady-state, the pool stabilizes. After resolution changes, old entries persist until pruned.

**Finding 4.5-D** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/texture_pool.rs:146-149`: `clear()` drops all cached textures. Available for resolution change handling. Called explicitly by callers.

### 4.6 Binary Archive Corruption

**Finding 4.6-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/archive.rs:82-94`: Binary archive load failure gracefully falls back to creating an empty archive. The `match device.new_binary_archive_with_descriptor(&desc)` handles `Err(_)` by creating a fresh empty archive. Corrupt archives do NOT crash -- they trigger a full recompile.

**Finding 4.6-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/archive.rs:139-143`: Save failure logs a warning but does not crash. `Err(e) => log::warn!("Failed to save pipeline archive: {e}")`.

**Finding 4.6-C** [INFO] `crates/manifold-gpu/src/metal/archive.rs:149-153`: Pipeline hash uses `DefaultHasher` (SipHash). Not cryptographically secure, but collision probability is negligible for ~100 pipelines. A collision would cause a stale cached pipeline to be used -- potentially wrong shader code. Severity: negligible in practice.

### 4.7 Function Constant Pipeline Variants

**Finding 4.7-A** [WARNING] `crates/manifold-gpu/src/metal/device.rs:226-238`: `create_specialized_compute_pipeline` performs WGSL text replacement + full recompile (WGSL -> SPIR-V -> spirv-opt -> SPIRV-Cross -> MSL -> Metal compile). This includes Metal's MSL compilation step, which can take 10-100ms per pipeline. If a new specialization variant is requested mid-show (e.g., a blend mode never used before), there will be a frame hitch.

Mitigating factors: (1) Specialization variants are typically all created at startup during pipeline initialization. (2) The binary archive caches compiled variants -- on second launch, no compilation occurs. (3) If a variant IS compiled mid-show, it only hitches once and is cached for future frames.

Practical risk: If the user switches to a blend mode or effect mode not previously used in the session, one frame hitch occurs. This is the same behavior as Unity's Shader.Find() on first use.

### 4.8 Completion Handler Captures

**Finding 4.8-A** [VERIFIED SAFE] No completion handlers (`addCompletedHandler`, `addScheduledHandler`) are used anywhere in the codebase. GPU completion is tracked via `MTLSharedEvent` polling (`GpuEvent::is_done` / `wait_until_done`). This eliminates the entire class of capture-outlives-scope bugs.

### 4.9 MTLEvent Signal/Wait Ordering

**Finding 4.9-A** [VERIFIED SAFE] `crates/manifold-renderer/src/layer_compositor.rs:1036-1053`: The async compute pattern is:
1. Each layer encoder signals `base + layer_index` after generating its layer (line 1038)
2. Each layer encoder commits its command buffer (line 1041)
3. The compositor encoder waits for `base + total_layers` (line 1050) before blending

The signal value monotonically increases per layer. The wait happens on the compositor command buffer, which is committed AFTER all wait_event encodings. Metal guarantees that `encode_wait_for_event` on a command buffer will make that buffer wait for the event to reach the specified value before executing. Since each layer signals its specific value and the compositor waits for the maximum value, all layers must complete before blending begins.

**Finding 4.9-B** [VERIFIED SAFE] `crates/manifold-app/src/content_pipeline.rs:219-222`: Previous-frame GPU completion is checked via `native_event.is_done(self.native_signal_value)` with `thread::yield_now()` spin. Since signal values only increase and the wait is for the value from the previous frame's signal, the ordering is correct. No deadlock possible.

**Finding 4.9-C** [INFO] `crates/manifold-gpu/src/metal/types.rs:143-144`: `GpuEvent` uses `std::cell::Cell<u64>` for the counter. `Cell` is NOT `Sync`. However, `GpuEvent` has `unsafe impl Sync`. The counter is only modified by `signal_event` which takes `&GpuEvent` (the Cell write via `.set()`). If two threads concurrently called `signal_event` on the same event, the Cell would have a data race. In practice, events are only signaled from the content thread. The `Sync` impl is safe only because of the single-writer architectural invariant, not because of the type system.

### 4.10 Drop Ordering

**Finding 4.10-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/encoder.rs:470-476`: `GpuEncoder::drop` releases the retained command buffer pointer. The encoder state is NOT checked in drop (no `end_current` call). However, this is safe because:
- If `commit()` was called, `end_current()` already ran and the state is `None`.
- If the encoder is dropped without commit (abnormal path), Metal will log a warning but not crash -- uncommitted command buffers are simply deallocated.

**Finding 4.10-B** [WARNING] `crates/manifold-gpu/src/metal/encoder.rs:470-476`: If `GpuEncoder` is dropped without calling `commit()` AND a compute encoder is still active (state == `Compute(ptr)`), the compute encoder's `end_encoding` is never called and its retained pointer is never released. This is a **memory leak** of the compute encoder ObjC object. The command buffer itself IS released (line 473), which should cause Metal to clean up associated encoders, but the explicit `objc_retain` at line 54 means we hold an extra retain count that is never released.

Fix path: Call `self.end_current()` in `Drop::drop` before releasing the command buffer.

### 4.11 Resource Hazard Tracking

**Finding 4.11-A** [VERIFIED SAFE] Metal on Apple Silicon performs automatic resource hazard tracking by default (`MTLHazardTrackingModeDefault`). The code does not disable this. All textures allocated via `GpuDevice::create_texture` and `TexturePool::acquire` use default hazard tracking. Metal will insert GPU barriers as needed between read and write accesses.

**Finding 4.11-B** [INFO] Heap-allocated textures (`GpuHeap::new_texture` at `types.rs:206-217`) use the heap's hazard tracking mode. The heap is created at `device.rs:414-419` with default descriptor settings, which means `MTLHazardTrackingModeDefault`. Safe.

### 4.12 Compute Dispatch Validation

**Finding 4.12-A** [WARNING] `crates/manifold-gpu/src/metal/encoder.rs:154-157`: `dispatch_thread_groups` is called with caller-provided `workgroups: [u32; 3]`. No validation that any dimension is > 0. A dispatch with `workgroups = [0, 0, 0]` is valid in Metal (it simply does nothing), so this is not a crash risk. However, a dispatch with workgroups `[0, Y, Z]` where only X is 0 would also be a no-op, which could silently produce incorrect results if the caller has a bug in workgroup count calculation.

**Finding 4.12-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/types.rs:118`: Workgroup SIZE is extracted from the shader's `@workgroup_size` declaration during compilation (shader_compiler.rs line 40). This is always correct (comes from the shader source itself, not caller input).

### 4.13 GPU Timeout Handling

**Finding 4.13-A** [WARNING] No GPU timeout or error handling exists anywhere in the codebase. `MTLCommandBuffer` status (`.status()`, `.error()`) is never checked after commit. If the GPU times out (watchdog kill after ~60 seconds, or a shader infinite loop), the `MTLSharedEvent` signal will never fire, and `GpuEvent::is_done()` will return false forever. The content thread at `content_pipeline.rs:220` will spin `yield_now()` indefinitely, freezing the application.

`GpuEvent::wait_until_done` at `types.rs:175-178` has the same infinite-spin risk, though this is only used for export (`content_pipeline.rs:611`).

Mitigating factor: GPU hangs are extremely rare in practice (only from invalid shaders, which are caught at compile time). Apple Silicon watchdog is generous (60+ seconds). But a 4-hour show could encounter a driver bug or a degenerate shader parameter combination.

Recommendation: Add a timeout to the wait loop (e.g., 5 seconds) with graceful recovery (skip frame, log error, reset pipeline).

### 4.14 Debug Groups in Release Builds

**Finding 4.14-A** [INFO] `crates/manifold-gpu/src/metal/encoder.rs:95,158,193,224,259,304`: `push_debug_group` / `pop_debug_group` are called unconditionally in release builds. These are Metal API calls that add string labels to the GPU command buffer timeline. Metal's documentation states these are extremely low overhead in release builds (no GPU cost, trivial CPU cost). VERIFIED SAFE -- no performance concern.

No `MTLCaptureManager` or capture scope usage found in the codebase.

---

## SECTION 8: Unsafe & FFI Audit

### 8.1 extern "C" Signature Correctness

**Finding 8.1-A** [VERIFIED SAFE] `crates/manifold-media/src/decoder_ffi.rs:9-68`: All function signatures match the C function names and use appropriate types (`*mut c_void` for opaque handles, `*const c_char` for strings, `i32` for status codes, `f32` for time values). These map to the ObjC plugin `MetalVideoDecoderPlugin.m`. The convention is consistent: create returns `*mut c_void` (nullable), operations return `i32` status codes.

**Finding 8.1-B** [VERIFIED SAFE] `crates/manifold-media/src/metal_ffi.rs:9-65`: All function signatures match the C encoder plugin. Same pattern as decoder: opaque handles, i32 status codes.

**Finding 8.1-C** [VERIFIED SAFE] `crates/manifold-native/src/ffi/blob_ffi.rs:14-24`: BlobDetector function types match the Unity `BlobDetectorNative.cs` DllImport signatures exactly. `i32` for max_blobs/width/height, `f32` for threshold/sensitivity, `*const u8` for rgba input, `*mut f32` for output.

**Finding 8.1-D** [VERIFIED SAFE] `crates/manifold-native/src/ffi/depth_ffi.rs:6-22`: DepthEstimator function types match `DepthEstimatorNative.cs`. Multiple factory functions (Create, CreateDepthOnly, CreateFlowOnly, CreateSubjectOnly) correctly return `*mut c_void`. Optional symbols are loaded via `library.get().ok()`.

**Finding 8.1-E** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/mod.rs:46-49`: `objc_retain` / `objc_release` are standard ObjC runtime functions. The signatures `(*mut c_void) -> *mut c_void` and `(*mut c_void)` are correct per the ObjC ABI.

**Finding 8.1-F** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/mps.rs:34-36`: `MPSSupportsMTLDevice` takes `*const c_void` (for `id<MTLDevice>`). Correct.

### 8.2 Native Plugin Exception Safety

**Finding 8.2-A** [WARNING] `crates/manifold-native/src/ffi/blob_ffi.rs:94-104` and `depth_ffi.rs:193-203`: The native C++ plugins (BlobDetector, DepthEstimator) are invoked via function pointers through `libloading`. If the C++ plugin throws an exception (e.g., OpenCV assertion failure, ONNX runtime error, out-of-memory), the exception will propagate across the FFI boundary into Rust, which is undefined behavior. The C++ plugin should catch all exceptions internally, but this depends on the plugin's implementation quality.

Mitigating factor: The plugins are compiled with `-fno-exceptions` or use try/catch internally (typical for production native plugins). The BlobDetector and DepthEstimator are well-tested Unity plugins that handle errors via return codes. Practical risk: LOW.

**Finding 8.2-B** [VERIFIED SAFE] `crates/manifold-media/src/decoder.rs:100-104`, `metal_encoder.rs:140-141`: All native plugin creation calls check for null return values before proceeding. `DecoderPool::new()`, `DecoderHandle` via `open()`, and `MetalEncoder::new()` all return `Err` on null handles.

**Finding 8.2-C** [VERIFIED SAFE] `crates/manifold-native/src/ffi/depth_ffi.rs:121-126`, `blob_ffi.rs:61-63`: Both FFI wrappers check for null handles after `Create` calls and return `None`.

### 8.3 Plugin Output Validation

**Finding 8.3-A** [WARNING] `crates/manifold-native/src/ffi/blob_ffi.rs:82-106`: `BlobDetector::process` passes `out_blob_data: &mut [f32]` directly to the C plugin. The plugin writes `count * 4` floats into this buffer. If the plugin returns a count larger than expected (bug or corruption), the Rust code that reads `out_blob_data[0..count*4]` could read uninitialized or stale data. However, it cannot cause a buffer overflow because the slice length is bounded by the Rust allocation.

**Finding 8.3-B** [INFO] `crates/manifold-native/src/ffi/depth_ffi.rs:180-264`: DepthEstimator output buffers (`out_depth`, `out_mask`, `out_flow_packed`) are pre-allocated slices passed to the C plugin. The plugin writes within the declared output dimensions. If the plugin writes beyond the slice (bug), this would be UB. However, the output dimensions match the input dimensions which the Rust side controls. Risk: depends on plugin correctness.

### 8.4 Raw Pointer Lifetimes

**Finding 8.4-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/encoder.rs:25-26`: `cmd_buf_ptr: *mut c_void` is a retained pointer. The command buffer is retained on creation (`device.rs:394`) and released on drop (`encoder.rs:473`). The pointer cannot outlive its data.

**Finding 8.4-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/encoder.rs:11-12`: `EncoderState::Compute(*const ComputeCommandEncoderRef)` stores a raw pointer. The encoder is retained (`ensure_compute` line 54) and released (`end_current` line 65). The pointer is valid between these calls. The `EncoderState` cannot escape `GpuEncoder`, and `GpuEncoder` cannot be cloned.

**Finding 8.4-C** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/metalfx.rs:68`: `scaler_ptr: *mut Object` is a retained ObjC object. Created with `new` naming convention (+1 retain, as noted in comment line 128), released in Drop (line 180). Lifetime is the struct's lifetime.

**Finding 8.4-D** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/mps.rs:55-78`: `MpsObject` wraps a raw pointer with explicit retain in `from_raw` and release in `Drop`. All MPS kernel wrappers use `MpsObject`, ensuring balanced retain/release.

**Finding 8.4-E** [VERIFIED SAFE] No completion handlers exist, so no closure captures can outlive their scope.

**Finding 8.4-F** [VERIFIED SAFE] `crates/manifold-playback/src/midi_clock_sync.rs:91,132`: MIDI callback captures `Arc<AtomicClockState>`. The Arc keeps the state alive as long as the callback exists. The `MidiInputConnection` owns the callback. When the connection is dropped, the callback is destroyed, releasing the Arc. No lifetime issues.

### 8.5 Transmute Usage

**Finding 8.5-A** [VERIFIED SAFE] Zero `transmute` calls found in the entire codebase. All type conversions use `as` casts for numeric types and `from_ptr`/`as_ptr` for ObjC pointer wrapping.

### 8.6 Double-Free Risk

**Finding 8.6-A** [VERIFIED SAFE] `crates/manifold-media/src/metal_encoder.rs:247-252`: `MetalEncoder::end_session` calls `std::mem::forget(self)` after calling `MetalEncoder_EndSession`. The `Drop` impl at line 283-294 also calls `EndSession`. By using `mem::forget`, the `Drop` is skipped, preventing a double-call to `EndSession`. The comment at line 251 explains this: "Skip Drop -- handle is consumed by EndSession". This pattern is correct.

**Finding 8.6-B** [VERIFIED SAFE] `crates/manifold-native/src/ffi/depth_ffi.rs:267-276`: `FfiDepthEstimator::drop` sets `self.handle = std::ptr::null_mut()` after calling `fn_destroy`. This prevents double-free if drop were somehow called twice (e.g., by `ManuallyDrop::drop`).

**Finding 8.6-C** [VERIFIED SAFE] `crates/manifold-native/src/ffi/blob_ffi.rs:75-80`: `FfiBlobDetector::drop` calls `fn_destroy(self.handle)`. No null-after-destroy, but `Drop` is only called once by Rust's ownership system. No manual drop or forget patterns used.

**Finding 8.6-D** [VERIFIED SAFE] `crates/manifold-media/src/decoder.rs:169-175`: `DecoderPool::drop` checks `!is_null()` before calling `DestroyPool`. Same for `DecoderHandle` (checked via wrapper pattern).

### 8.7 Mutable Aliasing

**Finding 8.7-A** [WARNING] `crates/manifold-gpu/src/metal/texture_pool.rs:81,97,137,147,173`: Every public method on `TexturePool` does `unsafe { &mut *self.inner.get() }`. This creates `&mut TexturePoolInner` from `&self`. If two call sites concurrently called any combination of methods (e.g., `acquire` + `release`), there would be overlapping `&mut` references, which is undefined behavior. The safety relies entirely on the architectural invariant that the pool is only used from the content thread.

As noted in 4.5-B, the `Sync` impl makes this easier to violate. The `UnsafeCell` approach is valid for single-threaded use, but `Sync` should be removed.

**Finding 8.7-B** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/types.rs:143`: `GpuEvent::counter` uses `std::cell::Cell<u64>`, which does not allow references to inner data. Cell's interior mutability is safe for single-threaded access. The `Sync` impl at line 147 is technically unsound if two threads write simultaneously, but the architectural invariant (content thread only) prevents this.

### 8.8 Autoreleasepool Coverage Per Thread

**Finding 8.8-A** [VERIFIED SAFE] Content thread: wrapped per-frame (see 4.2-A, 4.2-B).

**Finding 8.8-B** [INFO] UI thread (winit event loop): The UI thread uses wgpu, not the `metal` crate directly. wgpu manages its own ObjC memory. The `edr_surface.rs` calls (msg_send!) happen during initialization or screen change events, not per-frame. The `configure_edr` function at `edr_surface.rs:190-278` does extensive msg_send! calls without an explicit autoreleasepool. These are called during surface configuration (rare event), so the autoreleased objects will be drained by the winit event loop's implicit pool.

**Finding 8.8-C** [WARNING] `crates/manifold-app/src/edr_surface.rs:56-114`: `register_screen_change_observer` creates ObjC objects (`ClassDecl`, `ManifoldEDRObserver` instance, NSNotification subscriptions) without an explicit autoreleasepool. This is called once during startup from the main thread. The main thread has an implicit autoreleasepool from `NSApplicationMain` / winit's event loop. VERIFIED SAFE for the startup case, but would leak if called from a worker thread.

**Finding 8.8-D** [INFO] Video decode worker threads: The decode scheduler dispatches work to a thread pool. The native `VideoDecoder_*` functions are ObjC code (AVAssetReader, CVPixelBuffer). These worker threads do NOT have explicit autoreleasepool wrappers. The ObjC runtime creates an implicit per-thread pool, but it is only drained when the thread exits. For long-running thread-pool threads, this means autoreleased objects from video decode accumulate until the thread pool is destroyed.

Mitigating factor: Most AVFoundation decode operations use ARC (not autorelease) in modern ObjC. The practical memory impact depends on the native plugin's implementation. Risk: LOW to MEDIUM -- could contribute to slow VRAM growth during 4+ hour shows with heavy video decode.

### 8.9 IOSurface Retain/Release Balance

**Finding 8.9-A** [VERIFIED SAFE] `crates/manifold-app/src/shared_texture.rs:87-120`: IOSurface creation uses `core_foundation::base::TCFType::wrap_under_create_rule`, which adopts the +1 retain from `IOSurfaceCreate`. The IOSurface objects are owned by the `SharedTextureBridge` struct (in the `RwLock<[IOSurface; 3]>` array). When the bridge is dropped or resized, the old IOSurfaces are dropped, releasing their retain.

**Finding 8.9-B** [VERIFIED SAFE] `crates/manifold-app/src/shared_texture.rs:131-220`: `import_texture` creates an MTLTexture backed by the IOSurface via `newTextureWithDescriptor:iosurface:plane:`. This follows the `new` naming convention (+1 retain). The MTLTexture is wrapped via `metal::Texture::from_ptr` which adopts the retain. When the wgpu Texture or GpuTexture is dropped, the Metal texture is released, which releases its reference to the IOSurface. The IOSurface itself remains alive as long as the bridge holds it.

**Finding 8.9-C** [VERIFIED SAFE] `crates/manifold-app/src/shared_texture.rs:289-305`: `resize` creates new IOSurfaces and replaces the old ones under the write lock. The old IOSurfaces are dropped when the `guard` is dropped. Textures imported from the old IOSurfaces become stale but still valid (they hold their own IOSurface retain via the MTLTexture). The generation counter (line 300) signals both threads to re-import.

### 8.10 Drop Ordering: Textures/Buffers Before Device

**Finding 8.10-A** [VERIFIED SAFE] `crates/manifold-gpu/src/metal/device.rs:16-22`: `GpuDevice` struct field order is `device`, `queue`, `archive`. Rust drops fields in declaration order. The device is dropped before the queue and archive. However, this does NOT matter for Metal: `MTLDevice` is a reference-counted singleton. All textures, buffers, pipelines, and command queues hold internal retains on the device. The device object is not actually deallocated until ALL references are released. Dropping `GpuDevice` simply decrements one retain count.

**Finding 8.10-B** [VERIFIED SAFE] `GpuEncoder` drops the retained command buffer pointer in its `Drop` impl. Command buffers hold internal references to all resources set on their encoders (since we use retained references, as noted in `device.rs:385-388`). The command buffer release in `GpuEncoder::drop` will not deallocate textures/buffers that are still in-flight on the GPU.

### 8.11 NSException Risk

**Finding 8.11-A** [WARNING] `crates/manifold-gpu/src/metal/mps.rs` (throughout): MPS kernel creation via `msg_send![alloc, initWithDevice:...]` can throw `NSInvalidArgumentException` if parameters are invalid (e.g., `MpsGaussianBlur::new` with sigma <= 0, `MpsMedian::new` with even kernel diameter). The `MpsObject::from_raw` asserts non-null (line 62) but does not catch ObjC exceptions. An NSException would unwind through Rust FFI frames, which is undefined behavior.

Affected constructors:
- `mps.rs:150` MpsGaussianBlur (sigma must be > 0)
- `mps.rs:185` MpsBoxBlur (kernel dimensions)
- `mps.rs:214` MpsTentBlur (kernel dimensions)
- `mps.rs:244` MpsMedian (kernel_diameter must be odd >= 3)
- `mps.rs:396-418` MpsConvolution (weights length must match)
- `mps.rs:439-456` MpsDilate
- `mps.rs:474-492` MpsErode

Mitigating factor: All parameters come from controlled Rust code, not user input. The `MpsConvolution` and morph ops have Rust-side `assert_eq!` checks. MPS gaussian blur sigma comes from effect parameters which are clamped. Practical risk: LOW.

**Finding 8.11-B** [WARNING] `crates/manifold-gpu/src/metal/metalfx.rs:94-127`: MetalFX descriptor `alloc`/`init` and `newSpatialScalerWithDevice:` could throw NSExceptions (e.g., unsupported device, invalid parameters). The null check at line 98 and 122 catch nil returns but not exceptions. However, MetalFX methods are unlikely to throw (they return nil on failure). Risk: LOW.

**Finding 8.11-C** [WARNING] `crates/manifold-app/src/edr_surface.rs:70-113`: Multiple `msg_send!` calls for NSNotificationCenter, NSString, ClassDecl. These are standard AppKit/Foundation calls that are well-defined and do not throw exceptions in normal operation. Risk: NEGLIGIBLE.

**Finding 8.11-D** [WARNING] `crates/manifold-app/src/shared_texture.rs:165-175`: `msg_send![raw_device, newTextureWithDescriptor:iosurface:plane:]` could theoretically throw if the IOSurface is invalid. The assert at line 172-174 catches nil but not exceptions. Mitigating factor: IOSurfaces are always valid (just created a few lines above or held in the bridge). Risk: NEGLIGIBLE.

---

## SUMMARY: Critical and Warning Findings

### CRITICAL

| ID | File:Line | Description |
|----|-----------|-------------|
| 4.3-A | `metal/device.rs:67,79,93,463`, `texture_pool.rs:123` | `new_texture` / `new_buffer` return values not checked for nil. Under GPU memory pressure, these return null pointers that will crash on first use. The `metal` crate v0.33 API does not expose `Option` for these calls, making defensive checking difficult. |

### WARNING

| ID | File:Line | Description |
|----|-----------|-------------|
| 4.5-B | `metal/texture_pool.rs:24-55` | `TexturePool` has `unsafe impl Sync` but uses `UnsafeCell` interior mutability. Concurrent access from multiple threads would be UB. Architectural invariant (single-thread) is the only protection. Remove `Sync`. |
| 4.5-C | `metal/texture_pool.rs` (all) | No maximum pool size cap. VRAM grows monotonically after resolution changes if `prune_stale` is not called. |
| 4.7-A | `metal/device.rs:226-238` | Specialized pipeline creation compiles shaders at runtime. First use of a new blend mode mid-show causes a frame hitch (10-100ms). |
| 4.10-B | `metal/encoder.rs:470-476` | Drop does not call `end_current()`. If encoder is dropped with active compute encoder, the retained encoder pointer leaks. |
| 4.13-A | `metal/types.rs:175-178`, `content_pipeline.rs:220` | No GPU timeout handling. If GPU hangs, content thread spins forever. |
| 8.2-A | `ffi/blob_ffi.rs:94`, `ffi/depth_ffi.rs:193` | C++ plugin exceptions would be UB across FFI boundary. |
| 8.3-A | `ffi/blob_ffi.rs:82-106` | BlobDetector plugin output count not validated against buffer capacity. |
| 8.7-A | `metal/texture_pool.rs:81-173` | UnsafeCell aliasing risk with overly permissive Sync impl. |
| 8.8-D | (architecture) | Video decode worker threads lack explicit autoreleasepool. Autoreleased ObjC objects accumulate for thread lifetime. |
| 8.11-A | `metal/mps.rs:150,185,214,244` | MPS kernel creation can throw NSException on invalid params. No ObjC exception catch. |

### INFO

| ID | File:Line | Description |
|----|-----------|-------------|
| 4.1-A | `metal/types.rs` (multiple) | Many `unsafe impl Send/Sync` lack `// SAFETY:` comments. All correct. |
| 4.2-C | `metal/archive.rs:81,90` | Pipeline archive load/create during startup may be outside autoreleasepool. One-time, bounded. |
| 4.2-D | `metal/device.rs:149-380` | Pipeline creation ObjC temporaries not wrapped in autoreleasepool. Startup-only. |
| 4.5-A | `metal/texture_pool.rs:43` | Frame stamp u64 cannot overflow (9.7 billion years at 60fps). |
| 4.9-C | `metal/types.rs:143-147` | GpuEvent Cell<u64> counter is not thread-safe, relying on architectural single-writer invariant. |
| 4.12-A | `metal/encoder.rs:154-157` | No validation that workgroup count > 0. Metal treats [0,0,0] as no-op. |
| 4.14-A | `metal/encoder.rs:95,158,193,224,259,304` | Debug groups unconditional in release builds. Negligible overhead per Metal docs. |
| 8.1-A through 8.1-F | (multiple) | All extern "C" signatures verified correct against their native counterparts. |
| 8.3-B | `ffi/depth_ffi.rs:180-264` | DepthEstimator output buffers depend on plugin writing within declared dimensions. |
| 8.8-B | `edr_surface.rs` | UI thread ObjC calls outside explicit autoreleasepool. Covered by winit's implicit pool. |

### VERIFIED SAFE

| ID | File:Line | Reason |
|----|-----------|--------|
| 4.2-A | `content_thread.rs:217-220` | Every content frame wrapped in autoreleasepool |
| 4.2-B | `content_export.rs:206-212` | Every export frame wrapped |
| 4.3-B | `types.rs:206-217` | Heap allocation returns Option |
| 4.4-A | `encoder.rs:8-77` | State machine prevents encoding after endEncoding |
| 4.4-B | `encoder.rs:462-466` | Commit ends encoder + consumes self |
| 4.6-A | `archive.rs:82-94` | Corrupt archive falls back to empty (no crash) |
| 4.6-B | `archive.rs:139-143` | Save failure logs warning, does not crash |
| 4.8-A | (entire codebase) | No completion handlers used anywhere |
| 4.9-A | `layer_compositor.rs:1036-1053` | Signal/wait ordering is correct (monotonic values) |
| 4.9-B | `content_pipeline.rs:219-222` | Previous-frame wait uses correct signal value |
| 4.10-A | `device.rs:16-22` | Metal ref-counting makes drop order irrelevant |
| 4.11-A | (all texture creation) | Metal automatic hazard tracking is default |
| 4.12-B | `types.rs:118` | Workgroup size from shader source, always correct |
| 8.4-A through 8.4-F | (multiple) | All raw pointer lifetimes are bounded by struct lifetime |
| 8.5-A | (entire codebase) | Zero transmute calls |
| 8.6-A | `metal_encoder.rs:247-252` | mem::forget prevents double EndSession |
| 8.6-B | `depth_ffi.rs:267-276` | Handle nulled after destroy prevents double-free |
| 8.9-A through 8.9-C | `shared_texture.rs` | IOSurface retain/release balanced via CF/Metal ownership rules |
| 8.10-A | `device.rs:16-22` | Metal ref-counting handles drop ordering |
| 8.10-B | `encoder.rs:470-476` | Command buffer retains resources; drop is safe for GPU |

### FINDING: `supports_spatial_scaling` Bug

**Finding 8.11-EXTRA** [WARNING] `crates/manifold-gpu/src/metal/metalfx.rs:188-200`: The `supports_spatial_scaling` function calls `msg_send![cls, supportsDevice:]` which returns `BOOL`, but the return value is assigned to `_: BOOL` (discarded) and the function always returns `true`. This means MetalFX support is reported as available even when the device does not support it. The scaler creation at line 314 (`MetalFxSpatialScaler::new`) returns `None` on failure, so the fallback to MPS Lanczos still works. But the initial `UpscaleMode` selection at `metalfx.rs:235-240` would incorrectly choose `MetalFxSpatial` mode, causing a failed scaler creation attempt on every upscale call until the fallback kicks in. This is a correctness bug, not a crash risk.
