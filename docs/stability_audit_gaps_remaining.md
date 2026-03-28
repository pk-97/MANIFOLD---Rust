# Stability Audit — Remaining Gap Findings

Generated: 2026-03-28

This document covers findings from auditing files that were specified as remaining gaps
in the stability audit plan (STAB-1, STAB-2, STAB-5, STAB-9, STAB-11, STAB-12, STAB-13).

---

## Task 1: STAB-5 — Compositor

### 1a: Generator Uniform Double-Buffering

**Finding 1a-1** [VERIFIED SAFE] `crates/manifold-renderer/src/generator_renderer.rs:292-296`

The generator renderer uses a `UniformArena` (shared-memory Metal buffer). At the
start of `render_all()`, the arena is reset (cursor = 0) and a raw pointer is passed
to `GpuEncoder`. However, examining `crates/manifold-renderer/src/uniform_arena.rs:41-58`,
the arena is NOT actually used for GPU reads on the native Metal path. The comment at
`layer_compositor.rs:109-110` confirms: "arena buffer not read on native path."

The actual uniform data is passed via `GpuBinding::Bytes` (inline `set_bytes` on Metal),
which copies the uniform data directly into the command buffer at encode time. This is
visible at `layer_compositor.rs:120-122`:
```
GpuBinding::Bytes {
    binding: 0,
    data: bytemuck::bytes_of(uniforms),
}
```

And in `manifold-gpu/src/metal/encoder.rs:131-133`:
```
GpuBinding::Bytes { binding: b, data } => {
    enc.set_bytes(...)
}
```

`set_bytes` copies data inline into the Metal command buffer. Each dispatch gets its
own copy. There is no shared buffer that could be overwritten while the GPU reads it.
No double-buffering is needed because the data is baked into the command buffer.

The `UniformArena` still exists for offset tracking and is flushed at
`generator_renderer.rs:390` and `layer_compositor.rs:1182`, but the GPU never reads
from the arena buffer on the native path. It is only a capacity-tracking mechanism.

**Verdict: No race condition. Uniforms are safe via inline set_bytes.**

---

### 1b: Single-Layer Fast Path

**Finding 1b-1** [VERIFIED SAFE] `crates/manifold-renderer/src/layer_compositor.rs:1079-1089`

The compositor chooses between `composite_serial()` and `composite_parallel()` based
on `count_active_layers(frame)`. The single-layer case uses `composite_serial()`. Both
paths call the same two-phase pipeline:

- Serial path (`composite_serial`, line 736-745): calls `generate_layers()` then
  `blend_layers()`.
- Parallel path (`composite_parallel`, line 760-1053): duplicates the
  `generate_layers()` logic per-layer with per-layer command buffers, then calls the
  same `blend_layers()` for the final blend.

Both paths use the same `generate_layers()` / `blend_layers()` logic internally.
Within `generate_layers()`, the single-clip-no-effects fast path (line 536) produces
a `LayerOutput` with `opacity: layer_opacity * clip.opacity`, while the multi-clip
path (line 675) produces `opacity: layer_opacity` only (per-clip opacity is baked
during intra-layer compositing). This is correct because:
- Single-clip: clip opacity has not been applied yet, so it is multiplied in.
- Multi-clip: clip opacity was applied during the per-clip blend passes (line 623).

The same logic is duplicated in `composite_parallel` at lines 905-910 vs 1017-1022.

**Finding 1b-2** [VERIFIED SAFE] `crates/manifold-renderer/src/layer_compositor.rs:536-581`

The single-clip layer path stores per-clip transforms (translate, scale, rotation,
invert_colors) in `single_clip_transforms` (line 575-581). The multi-clip layer path
stores identity transforms (line 681-687). Both are consumed by `blend_layers()` at
line 711. This is correct: single-clip transforms are deferred to the blend pass,
while multi-clip transforms are baked during intra-layer compositing at lines 621-638.

**Finding 1b-3** [WARNING] `crates/manifold-renderer/src/layer_compositor.rs:1060-1089`

The routing between serial and parallel is based on `count_active_layers()`. If this
count function returns a different number than the actual count processed by
`generate_layers()` / `composite_parallel()` (e.g., due to a subtle mute/solo edge
case), the wrong path could be chosen. Both `count_active_layers()` (line 244-263)
and the per-layer iteration in `composite_serial`/`composite_parallel` use the same
mute/solo check pattern:
```
if ld.is_muted || (any_solo && !ld.is_solo) { continue; }
```
The logic is identical across all three sites. Low risk, but a structural coupling
where a change in one site must be mirrored in the other two.

**Verdict: Paths produce equivalent output. Transform/opacity handling is correct.**

---

## Task 2: STAB-9 — FFI Safety

### 2a: Signal Handler Conflicts

**Finding 2a-1** [VERIFIED SAFE] Entire codebase + `assets/plugins/`

Grep for `signal`, `sigaction`, `SIGTERM`, `SIGINT`, `SIGHUP` across:
- All Rust source files
- `assets/plugins/DepthEstimator/DepthEstimatorPlugin.cpp`
- `crates/manifold-media/native/MetalEncoderPlugin.m`

**Result: No signal handlers are installed by any native plugin.** The
DepthEstimatorPlugin.cpp uses OpenCV and OpenVINO but does not register any signal
handlers. MetalEncoderPlugin.m uses AVFoundation and VideoToolbox without signal
handlers. The BlobDetector plugin (OpenCV-based) also has no signal handler
registration.

The only signal-related finding is in `MetalEncoderPlugin.m:506`:
`dispatch_semaphore_signal(sem)` which is GCD semaphore signaling (thread
synchronization), not Unix signal handling. This is safe.

**Note:** A prior audit finding (stability_audit_section_3_10.md:478-482) identified
that no `SIGPIPE` handling exists at the app level. This remains an open concern for
OSC/UDP socket writes but is not caused by native plugins.

### 2b: ABI Compatibility

**Finding 2b-1** [VERIFIED SAFE] `crates/manifold-media/build.rs:1-23`

The MetalEncoderPlugin.m and MetalVideoDecoderPlugin.m are compiled using `cc::Build`
with only the `-fobjc-arc` flag. This compiles Objective-C code with the system
clang. The FFI boundary is pure C (extern "C" functions). No C++ STL types cross the
boundary. Linking is against Apple system frameworks only (Metal, AVFoundation,
CoreVideo, CoreMedia, VideoToolbox). No ABI mismatch risk.

**Finding 2b-2** [VERIFIED SAFE] `crates/manifold-native/Cargo.toml:1-12`

The manifold-native crate has NO build.rs. It uses `libloading` to dynamically load
`DepthEstimator.bundle` and `BlobDetector.bundle` at runtime. The FFI boundary is
pure C function pointers (extern "C"). No C++ types cross the boundary.

**Finding 2b-3** [INFO] `crates/manifold-native/src/ffi/depth_ffi.rs:49`

`set_var("KMP_DUPLICATE_LIB_OK", "TRUE")` is called before loading the
DepthEstimator plugin. This is to prevent OpenMP from aborting when multiple copies
of libiomp are loaded (one from OpenCV, one potentially from libtorch). This matches
Unity's `EnsureOmpEnvironmentSafety` pattern. The `set_var` call is flagged as unsafe
in Rust 2024 because it can race with getenv from other threads. Since it is called
during static OnceLock initialization (before any plugin threads exist), it is safe
in practice.

**Finding 2b-4** [WARNING] `assets/plugins/DepthEstimator/DepthEstimatorPlugin.cpp:14-18`

The DepthEstimator plugin is compiled as a separate bundle (via `build.sh`) linking
against OpenCV 4.13 and OpenVINO 2541 dynamic libraries. These are C++ libraries
whose internal state (static variables, TLS) is managed by the C++ runtime loaded
with the bundle. The FFI boundary itself is clean C (extern "C"), but if the C++
runtime in the bundle uses a different allocator version than the host process (e.g.,
different libstdc++ or libc++ versions), memory corruption is theoretically possible.
In practice on macOS, the system libc++ is used by both, so this is low risk.

**ABI verdict: All FFI boundaries are pure C. No C++ STL crosses FFI. Low risk.**

---

## Task 3: Remaining Files

### STAB-1: ClipRenderer Trait

**Finding 3.1-1** [VERIFIED SAFE] `crates/manifold-playback/src/renderer.rs:1-51`

The `ClipRenderer` trait is a clean abstraction with no unsafe code, no state, and
no allocations. It is purely an interface definition. The `StubRenderer` test impl
(lines 52-151) uses `HashMap` (not AHashMap) but this is test-only code, never on
the hot path.

**Finding 3.1-2** [INFO] `crates/manifold-playback/src/renderer.rs:9`

The trait requires `Any + Send`. The `Send` bound is correct because renderers are
created on the content thread and stay there. `Any` is for downcasting (e.g.,
`as_any_mut()` at line 48-49), which is used by the app layer to access typed
renderer methods not in the trait (e.g., `render_all()` on GeneratorRenderer).

---

### STAB-2: OSC Sender

**Finding 3.2-1** [WARNING] `crates/manifold-playback/src/osc_sender.rs:165-173`

`encode_osc_float()` allocates a new `Vec<u8>` with capacity 64 on every call. This
function is called from `late_update()` which runs per-frame when transport or seek
changes occur. In normal playback this is infrequent (only on play/stop/seek), but
a rapidly seeking user could trigger per-frame allocations. Given the small size
(64 bytes, stack-allocatable) and infrequent trigger, this is low severity.

**Finding 3.2-2** [VERIFIED SAFE] `crates/manifold-playback/src/osc_sender.rs:146`

Socket errors are silently ignored: `let _ = socket.send(&packet)`. This matches
Unity's behavior (catches SocketException and does nothing). Correct for a live
performance app where a failed OSC send should never halt playback.

**Finding 3.2-3** [VERIFIED SAFE] `crates/manifold-playback/src/osc_sender.rs:47`

The UDP socket is bound to `0.0.0.0:0` (ephemeral port) and connected to the
destination. Socket creation only happens in `enable_sender()`, not per-frame. The
socket is dropped in `disable_sender()`. No resource leak.

### STAB-2: OSC Registry

**Finding 3.2-4** [VERIFIED SAFE] `crates/manifold-playback/src/osc_registry.rs:30-46`

The `OscParameterRegistry` uses a `HashMap<String, FloatSetter>` which grows as
parameters are registered. Registration happens at subsystem initialization, not
per-frame. `unregister_by_prefix()` (line 136-148) is used for cleanup. The number
of registered parameters is bounded by the number of effect parameters in the project
(typically ~50-200). No unbounded growth.

**Finding 3.2-5** [INFO] `crates/manifold-playback/src/osc_registry.rs:26`

The registry uses `std::sync::Mutex` (not `parking_lot::Mutex`). Since access is
infrequent (registration at init, dispatch per-frame), the performance difference
is negligible. The global singleton via `OnceLock<Mutex<...>>` is correct.

**Finding 3.2-6** [INFO] `crates/manifold-playback/src/osc_registry.rs:37`

Uses `HashMap` instead of `AHashMap`. This is acceptable here because the registry
is not on the hot path (parameter lookup during OSC dispatch, not per-frame rendering).

---

### STAB-5: GPU Context (UI thread wgpu)

**Finding 3.5-1** [WARNING] `crates/manifold-renderer/src/gpu.rs:29`

`request_adapter()` uses `.expect("Failed to find a suitable GPU adapter")` which
will panic if no adapter is available. Same at line 77 for `request_device()`. This
is acceptable for startup (no GPU = cannot run), but there is no device lost handler
registered. A prior audit (stability_audit_section_3_10.md:419) already flagged the
missing `on_uncaptured_error()` handler. If the UI thread wgpu device is lost
mid-show (e.g., GPU reset, external display disconnect), the app will crash on the
next wgpu API call with no graceful recovery.

This is the UI thread device only (content thread uses native Metal which has its own
separate handling concern). Severity is lower because UI device loss is rare on macOS
(Metal doesn't have the GPU timeout/TDR behavior of Windows).

### STAB-5: GPU Types

**Finding 3.5-2** [VERIFIED SAFE] `crates/manifold-renderer/src/gpu_types.rs:1-7`

This file is a legacy re-export shim. All GPU types have been migrated to
`manifold_gpu`. No alignment issues — the file contains only a comment.

### STAB-5: GPU Readback

**Finding 3.5-3** [WARNING] `crates/manifold-renderer/src/gpu_readback.rs:91`

`try_read()` allocates a new `Vec<u8>` of size `width * 4 * height` on every call.
At 1920x1080 this is ~8MB per readback. Readback happens every frame for blob
tracking (per a recent commit message: "blob tracking readback every frame"). This is
a per-frame heap allocation on the content thread hot path.

Mitigation: The allocation size is consistent frame-to-frame, so the allocator's
internal free list will likely reuse the same pages. But it would be cleaner to use
a pre-allocated buffer that is reused across frames.

**Finding 3.5-4** [VERIFIED SAFE] `crates/manifold-renderer/src/gpu_readback.rs:51-73`

The readback uses `gpu.device.create_buffer_shared()` which creates a Metal shared-
memory buffer (CPU+GPU coherent on Apple Silicon). The `copy_texture_to_buffer` blit
is encoded on the native encoder. GPU completion is guaranteed by the comment at
line 8: "GPU completion is guaranteed by wait_previous_frame() in the content pipeline."

The raw pointer `native_shared_ptr` (line 19) is stored and used in `try_read()` on
the next frame. Since `wait_previous_frame()` ensures the GPU has finished before the
next frame starts encoding, the read is safe. The `unsafe impl Send` at line 24 is
justified.

**Finding 3.5-5** [INFO] `crates/manifold-renderer/src/gpu_readback.rs:104-106`

After reading, the native buffer is dropped (`native_readback_buf = None`). A new
buffer is created on the next `submit()`. This is a per-frame Metal buffer creation
and destruction. On Apple Silicon this is fast (shared memory, no staging copy), but
a persistent double-buffered readback buffer would eliminate the allocation entirely.

### STAB-5: Background Worker

**Finding 3.5-6** [VERIFIED SAFE] `crates/manifold-renderer/src/background_worker.rs:53-67`

The worker thread loop is:
```rust
while let Ok(first) = req_rx.recv() {
    let mut latest = first;
    while let Ok(newer) = req_rx.try_recv() {
        latest = newer;
    }
    let result = processor(latest);
    if res_tx.send(result).is_err() { break; }
}
```

If the processor panics, the thread will unwind and exit. The `recv()` on the main
thread side will return `Err(RecvError)` which is handled at line 170 (`Err(_) => None`).
The `Drop` impl (line 178-188) drops the sender first (causing worker `recv()` to
return Err), then joins the thread. If the thread panicked, `join()` returns
`Err(Box<dyn Any>)` which is silently ignored via `let _ = handle.join()`.

This means a panic in the native plugin processor (e.g., OpenCV assertion failure in
DepthEstimator) will:
1. Kill the worker thread silently
2. All subsequent `try_recv()` calls return None
3. The feature degrades gracefully (no depth estimation / blob detection)

**This is correct behavior for a live performance app.** No thread leak, no panic
propagation.

**Finding 3.5-7** [VERIFIED SAFE] `crates/manifold-renderer/src/background_worker.rs:56-61`

The "drain to latest" pattern discards stale requests when the worker is busy. This
is correct for real-time systems where only the most recent input matters (e.g.,
latest video frame for depth estimation). No unbounded queue growth.

---

### STAB-11: Tonemap Blit

**Finding 3.11-1** [WARNING] `crates/manifold-renderer/src/tonemap_blit.rs:189`

`blit_to_rect()` creates a new `wgpu::BindGroup` on every call:
```rust
let bind_group = device.create_bind_group(...)
```
This runs on the UI thread (wgpu path). In wgpu, bind group creation is lightweight
(no GPU allocation, just a descriptor), but it does allocate a heap object. Since
this is called once or twice per UI frame (main viewport + optional external monitor),
the allocation is small and infrequent relative to the frame budget.

**Finding 3.11-2** [VERIFIED SAFE] `crates/manifold-renderer/src/tonemap_blit.rs:14-20`

The `Uniforms` struct is properly aligned: `mode: u32` + `_pad: [u32; 3]` = 16 bytes
total. Matches the WGSL struct alignment requirement.

**Finding 3.11-3** [VERIFIED SAFE] `crates/manifold-renderer/src/tonemap_blit.rs:153-158`

The uniform buffer is created once in `new()` and reused via `queue.write_buffer()`
on each blit. No per-frame buffer allocation on the GPU side.

### STAB-11: Layer Bitmap GPU

**Finding 3.11-4** [INFO] `crates/manifold-renderer/src/layer_bitmap_gpu.rs:278-283`

One `unsafe` block exists for reinterpreting `&[Color32]` as `&[u8]`:
```rust
let bytes: &[u8] = unsafe {
    std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4)
};
```
`Color32` is `#[repr(C)]` with 4 `u8` fields. This transmute is safe because:
- `Color32` has no padding (4 x u8 = 4 bytes, alignment = 1)
- The source and destination have the same memory layout
- The slice lengths are correct (pixels.len() * 4 bytes)

**Finding 3.11-5** [WARNING] `crates/manifold-renderer/src/layer_bitmap_gpu.rs:374-383`

Per-frame vertex and index buffer creation:
```rust
let vertex_buffer = device.create_buffer_init(...)
let index_buffer = device.create_buffer_init(...)
```
These allocate new wgpu buffers every frame. The vertex/index data is built from
reusable scratch vectors (`self.vertices`, `self.indices` at lines 77-78), but the
GPU buffers are not reused. With typical layer counts (4-16 layers), the buffers are
small (hundreds of bytes), so this is low severity.

**Finding 3.11-6** [VERIFIED SAFE] `crates/manifold-renderer/src/layer_bitmap_gpu.rs:326-333`

Per-frame globals bind group creation. Same pattern as tonemap_blit — lightweight
wgpu descriptor, low overhead. Acceptable on the UI thread.

---

### STAB-12: Export Session

**Finding 3.12-1** [VERIFIED SAFE] `crates/manifold-media/src/export_session.rs:71-79`

`ExportSession` holds a `MetalEncoder`, `ExportConfig`, and frame tracking state.
There is no shared state with the playback engine. The session owns its encoder
exclusively. The `new_with_device()` variant (line 181) shares the Metal device
pointer for same-device encoding (avoids cross-device sync), but this is safe because
Metal device objects are thread-safe (reference-counted Objective-C objects).

**Finding 3.12-2** [VERIFIED SAFE] `crates/manifold-media/src/export_session.rs:164-172`

`encode_frame()` is unsafe because it receives a raw `*mut c_void` Metal texture
pointer. The safety contract is documented: "metal_texture_ptr must be a valid
id<MTLTexture>". The caller (content_pipeline in manifold-app) provides a valid
texture from the content thread's render output. No aliasing or lifetime issues.

**Finding 3.12-3** [VERIFIED SAFE] `crates/manifold-media/src/export_session.rs:296-347`

`finalize()` consumes `self` (takes ownership), ensuring the session cannot be used
after finalization. It calls `encoder.end_session()` to write the MP4 trailer, then
optionally muxes audio via FFmpeg subprocess, then cleans up the temp video-only
file. Error handling is correct throughout.

### STAB-12: Audio Muxer

**Finding 3.12-4** [VERIFIED SAFE] `crates/manifold-media/src/audio_muxer.rs:52-124`

`AudioMuxer::mux()` spawns an FFmpeg subprocess via `std::process::Command`. This is
a blocking call (`cmd.output()`) which is acceptable because export finalization is
not on the real-time path. All errors (FFmpeg not found, process spawn failure, non-
zero exit) are propagated as `MuxError`. No FFI involved — pure subprocess invocation.

**Finding 3.12-5** [INFO] `crates/manifold-media/src/audio_muxer.rs:109`

`cmd.output()` captures stdout and stderr. For large exports, FFmpeg can produce
substantial stderr output (progress info). `String::from_utf8_lossy` at line 112
will allocate a potentially large string if FFmpeg writes many lines. This only
matters on error path and is not a live performance concern.

---

### STAB-13: Project Manifest

**Finding 3.13-1** [VERIFIED SAFE] `crates/manifold-io/src/manifest.rs:6-23`

All fields in `ProjectManifest` have `#[serde(default)]` or
`#[serde(default = "...")]`. This means missing fields during deserialization will
not cause errors — they get default values. The `#[serde(rename_all = "camelCase")]`
attribute ensures Unity JSON compatibility.

**Finding 3.13-2** [VERIFIED SAFE] `crates/manifold-io/src/manifest.rs:43-59`

`SnapshotEntry` also has `#[serde(default)]` on all fields, plus
`#[serde(skip_serializing_if = "Option::is_none")]` on the optional `label` field.
The `is_auto` field is renamed to `"auto"` for Unity compat.

### STAB-13: Project Struct

**Finding 3.13-3** [VERIFIED SAFE] `crates/manifold-core/src/project.rs:13-52`

All fields in the `Project` struct have appropriate serde attributes:
- `#[serde(default)]` on all required fields
- `#[serde(default, skip_serializing_if = "Option::is_none")]` on optional fields
- `#[serde(skip)]` on `last_saved_path` (runtime-only)
- Custom `#[serde(rename = "...")]` on fields that need Unity compat names

The `Default` impl (lines 254-276) provides sensible defaults for all fields.

**Finding 3.13-4** [VERIFIED SAFE] `crates/manifold-core/src/project.rs:76-100`

`on_after_deserialize()` performs essential post-load initialization:
- Rebuilds runtime caches (video library lookup, MIDI dictionary, clip lookup)
- Validates and ensures tempo map integrity
- Syncs BPM from tempo map
- Clamps saved playhead to >= 0
- Aligns effect params to definitions
- Reindexes layers

All steps are idempotent and handle missing data gracefully via `default` values.

---

## Summary Table

| ID | Severity | File:Line | Description |
|----|----------|-----------|-------------|
| 1a-1 | VERIFIED SAFE | generator_renderer.rs:292 | Uniforms use inline set_bytes, no double-buffer needed |
| 1b-1 | VERIFIED SAFE | layer_compositor.rs:1079 | Serial/parallel paths produce equivalent output |
| 1b-2 | VERIFIED SAFE | layer_compositor.rs:536 | Transform handling correct for single vs multi-clip |
| 1b-3 | WARNING | layer_compositor.rs:1060 | Mute/solo logic duplicated in 3 sites (structural coupling) |
| 2a-1 | VERIFIED SAFE | assets/plugins/* | No signal handlers in native plugins |
| 2b-1 | VERIFIED SAFE | manifold-media/build.rs:7 | MetalEncoder uses pure C FFI, system frameworks only |
| 2b-2 | VERIFIED SAFE | manifold-native/Cargo.toml | Runtime dlopen with pure C boundary |
| 2b-3 | INFO | ffi/depth_ffi.rs:49 | set_var before plugin load (race-safe in practice) |
| 2b-4 | WARNING | DepthEstimatorPlugin.cpp:14 | C++ libs in bundle, but FFI boundary is clean C |
| 3.1-1 | VERIFIED SAFE | playback/renderer.rs:9 | ClipRenderer trait is clean, no unsafe, no leaks |
| 3.1-2 | INFO | playback/renderer.rs:9 | Any + Send bounds correct for downcasting pattern |
| 3.2-1 | WARNING | osc_sender.rs:165 | Per-send Vec<u8> allocation (low frequency, low severity) |
| 3.2-2 | VERIFIED SAFE | osc_sender.rs:146 | Socket errors silently ignored (matches Unity) |
| 3.2-3 | VERIFIED SAFE | osc_sender.rs:47 | Socket created once, no per-frame creation |
| 3.2-4 | VERIFIED SAFE | osc_registry.rs:30 | Registry bounded by parameter count, not per-frame |
| 3.2-5 | INFO | osc_registry.rs:26 | Uses std::sync::Mutex (acceptable, not hot path) |
| 3.2-6 | INFO | osc_registry.rs:37 | Uses HashMap (acceptable, not hot path) |
| 3.5-1 | WARNING | gpu.rs:29 | No device lost handler on UI wgpu device |
| 3.5-2 | VERIFIED SAFE | gpu_types.rs:1 | Legacy shim, no content |
| 3.5-3 | WARNING | gpu_readback.rs:91 | Per-frame 8MB Vec allocation in try_read() |
| 3.5-4 | VERIFIED SAFE | gpu_readback.rs:51 | Shared-memory readback safe via wait_previous_frame |
| 3.5-5 | INFO | gpu_readback.rs:104 | Per-frame Metal buffer create/destroy for readback |
| 3.5-6 | VERIFIED SAFE | background_worker.rs:53 | Worker panic degrades gracefully, no thread leak |
| 3.5-7 | VERIFIED SAFE | background_worker.rs:56 | Drain-to-latest prevents unbounded queue growth |
| 3.11-1 | WARNING | tonemap_blit.rs:189 | Per-frame bind group creation (lightweight, UI thread) |
| 3.11-2 | VERIFIED SAFE | tonemap_blit.rs:14 | Uniform alignment correct (16 bytes) |
| 3.11-3 | VERIFIED SAFE | tonemap_blit.rs:153 | Uniform buffer reused, not per-frame |
| 3.11-4 | INFO | layer_bitmap_gpu.rs:278 | Unsafe Color32-to-u8 transmute is sound |
| 3.11-5 | WARNING | layer_bitmap_gpu.rs:374 | Per-frame vertex/index buffer creation (small, UI thread) |
| 3.11-6 | VERIFIED SAFE | layer_bitmap_gpu.rs:326 | Globals bind group creation acceptable |
| 3.12-1 | VERIFIED SAFE | export_session.rs:71 | Export session owns encoder exclusively |
| 3.12-2 | VERIFIED SAFE | export_session.rs:164 | Unsafe encode_frame has correct safety contract |
| 3.12-3 | VERIFIED SAFE | export_session.rs:296 | Finalize consumes self, correct error handling |
| 3.12-4 | VERIFIED SAFE | audio_muxer.rs:52 | FFmpeg subprocess, no FFI, proper error handling |
| 3.12-5 | INFO | audio_muxer.rs:109 | Large stderr capture on error (not live-path concern) |
| 3.13-1 | VERIFIED SAFE | manifest.rs:6 | All fields have serde(default), Unity compat |
| 3.13-2 | VERIFIED SAFE | manifest.rs:43 | SnapshotEntry serde correct |
| 3.13-3 | VERIFIED SAFE | project.rs:13 | All Project fields have serde(default) |
| 3.13-4 | VERIFIED SAFE | project.rs:76 | Post-deserialize init is idempotent and safe |

## Findings by Severity

### CRITICAL: 0

### WARNING: 7
1. **1b-3**: Compositor mute/solo logic duplicated in 3 sites
2. **2b-4**: C++ runtime in native plugin bundle (low actual risk)
3. **3.2-1**: Per-send OSC packet allocation
4. **3.5-1**: No wgpu device lost handler (UI thread)
5. **3.5-3**: Per-frame 8MB readback allocation on content thread
6. **3.11-1**: Per-frame wgpu bind group creation (UI thread)
7. **3.11-5**: Per-frame wgpu vertex/index buffer creation (UI thread)

### INFO: 7
- 2b-3, 3.1-2, 3.2-5, 3.2-6, 3.5-5, 3.11-4, 3.12-5

### VERIFIED SAFE: 22

---

## Actionable Recommendations (Priority Order)

1. **[3.5-3] Readback allocation** — Pre-allocate a persistent `Vec<u8>` in
   `ReadbackRequest` and reuse it across frames. This eliminates an 8MB per-frame
   allocation on the content thread hot path. Paired with [3.5-5], a double-buffered
   persistent readback buffer would also eliminate per-frame Metal buffer creation.

2. **[3.5-1] wgpu device lost handler** — Register `device.on_uncaptured_error()` and
   a device lost callback on the UI wgpu device. At minimum, log the error. Ideally,
   attempt device recreation or graceful degradation (stop UI rendering, keep content
   thread running).

3. **[1b-3] Compositor mute/solo deduplication** — Extract the mute/solo check into a
   shared helper to prevent the three sites from diverging. Low risk but structural
   improvement.

4. **[3.11-1, 3.11-5] UI thread per-frame allocations** — Cache bind groups (keyed by
   texture view) and reuse vertex/index buffers across frames. These are on the UI
   thread which has more frame budget, so lower priority than content thread issues.
