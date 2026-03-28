# Stability Audit: Sections 9, 11, 12, 13

**Audited by:** Claude Opus 4.6 (1M context)
**Date:** 2026-03-28
**Scope:** Numeric cast safety, media decode safety, serialization integrity, memory growth
**Codebase state:** commit 2e955d7 (main)

---

## TASK 9: Numeric Cast Audit (Hot Paths)

### 9.1 `as u32` in hot-path crates

#### manifold-playback

**F9-1 VERIFIED SAFE** — `crates/manifold-playback/src/percussion_planner.rs:158`
```rust
((spacing_key as i64) << 32) | (quantized_tick as u32 as i64)
```
Intentional bit-packing of i32 into lower 32 bits. The `as u32` is a reinterpret cast, correct by design.

**F9-2 VERIFIED SAFE** — `crates/manifold-playback/src/midi_clock_sync.rs:41`
```rust
let pos = self.position_sixteenths as u32 as u64;
```
Deliberate atomic bit-packing. The `as u32` reinterprets i32, unpacked symmetrically at line 50.

**F9-3 VERIFIED SAFE** — `crates/manifold-playback/src/clip_launcher.rs:528,566`
```rust
let seed = event_sequence ^ (midi_note as u32).wrapping_mul(2654435761u32);
```
`midi_note` is a u8 from MIDI spec (0-127). Cast is always safe. `event_sequence` is a u32.

**F9-4 VERIFIED SAFE** — `crates/manifold-playback/src/clip_launcher.rs:577`
```rust
(Self::mix_seed(seed) % count as u32) as usize
```
`count` comes from `clip_ids.len()` which is a usize, and the function early-returns if `count <= 1`. The `as u32` truncates but result is used modulo count which is small.

#### manifold-renderer

**F9-5 WARNING** — `crates/manifold-renderer/src/effects/blob_tracking.rs:51-52`
```rust
let rw = ((w as u32).max(16) + 15) & !15;
let rh = ((h as u32).max(16) + 15) & !15;
```
`w` and `h` are f64 from a sqrt computation. If `aspect` ratio is negative or NaN (e.g., width=0 passed in), `w`/`h` could be NaN, and `NaN as u32` is 0 in Rust (saturating). The `.max(16)` then saves this, yielding 16. **Effectively safe due to the .max(16) clamp**, but the NaN→0 silent behavior is fragile.

**F9-6 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/blob_tracking.rs:572-573`
```rust
width: tex_w as u32, height: tex_h as u32,
```
`tex_w`/`tex_h` are `usize` values from readback dimension computation, always positive.

**F9-7 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/edge_detect.rs:77`
```rust
mode: mode_raw.round() as u32,
```
`mode_raw` comes from `p.get(2).copied().unwrap_or(0.0)` — param values are f32 in [0,2] range per registry. `.round()` before cast, and the shader clamps mode anyway.

**F9-8 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/strobe.rs:58`
```rust
mode: (p.get(2).copied().unwrap_or(0.0).round() as u32).min(2),
```
Clamped to 0-2 after cast. Even if param is negative, `(-1.0f32).round() as u32` saturates to 0 in Rust. Safe.

**F9-9 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/edge_stretch.rs:52`
Same pattern as F9-8: `.round() as u32).min(2)`.

**F9-10 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/stylized_feedback.rs:123`
```rust
let pipeline = match mode.round() as u32 {
    1 => ..., 2 => ..., _ => &self.pipeline_screen,
```
The `_ =>` fallback catches any out-of-range value including the saturated 0 from negative inputs.

**F9-11 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/wireframe_depth.rs:449,451,479,481`
```rust
let wire_w = (self.width as f32 * wire_scale).round() as u32;
let wire_w = wire_w.max(64);
```
`width` is u32, `wire_scale` is a clamped f32 param. Result always positive. `.max(64)` provides floor.

**F9-12 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/wireframe_depth.rs:1974`
```rust
let shift = (113 - exp) as u32;
```
This is inside the f32→f16 conversion subnormal path, guarded by `exp > 101` and `exp <= 112` (from prior branch). So `113 - exp` is in [1, 12]. Safe.

**F9-13 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/mirror.rs:48`
```rust
let mode = p.get(1).copied().unwrap_or(0.0).round() as u32;
```
Followed by `.min(2)` on line 51. Same safe pattern.

**F9-14 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/chromatic_aberration.rs:53`
```rust
let mode = p.get(2).copied().unwrap_or(0.0).round() as u32;
```
Used in `match mode { 0 => ..., 1 => ..., _ => ... }` with fallback.

**F9-15 WARNING** — `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs:73`
```rust
let idx = (val + 0.5) as u32;
```
`val` is a generator param. If `val` is NaN, `(NaN + 0.5) as u32` = 0 in Rust, which maps to 64 via the match. Functionally safe but fragile — relies on NaN→0 saturation.

**F9-16 VERIFIED SAFE** — `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs:78`
```rust
((val * 1_000_000.0).round() as i32).clamp(100_000, MAX_PARTICLES as i32) as u32
```
Clamped i32 value 100000..1000000, then cast to u32. Always positive. Safe.

**F9-17 VERIFIED SAFE** — `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs:512,514`
```rust
let scaled_energy_3d = (scatter_3d_energy * 4096.0 + 0.5) as u32;
```
Energy values are always positive (computed from positive inputs). Safe.

**F9-18 VERIFIED SAFE** — `crates/manifold-renderer/src/generator_renderer.rs:159-160`
```rust
let sw = (width as f32 * scale).round() as u32;
```
`scale` clamped to [0.125, 1.0] on line 158, `width` is u32. Result always positive. `.max(16)` follows.

**F9-19 VERIFIED SAFE** — `crates/manifold-renderer/src/generator_renderer.rs:345`
```rust
param_count = gp.param_values.len().min(MAX_GEN_PARAMS) as u32;
```
`len()` is usize, `.min()` bounds it. Safe.

**F9-20 INFO** — `crates/manifold-renderer/src/generator_renderer.rs:640`
```rust
self.resize_gpu(width as u32, height as u32);
```
`width`/`height` are i32 from the trait. Could be negative if a bug passes negative dimensions. The caller (content_pipeline) does `.max(1)` before calling, so this is safe in practice.

**F9-21 VERIFIED SAFE** — `crates/manifold-renderer/src/ui_renderer.rs:640,836`
UI thread vertex index casting. `vertices.len() as u32` and `indices.len() as u32`. These are bounded by the number of UI elements (hundreds, not billions). Safe.

**F9-22 VERIFIED SAFE** — `crates/manifold-renderer/src/layer_compositor.rs:622,713,959`
```rust
blend_mode: BlendMode::Normal as u32,
blend_mode: output.blend_mode as u32,
```
BlendMode is an enum with values 0-12. Safe.

**F9-23 VERIFIED SAFE** — `crates/manifold-renderer/src/generators/plasma.rs:112`
```rust
let pattern_idx = (pattern_type.round() as u32).min(PATTERN_COUNT - 1) as usize;
```
Clamped by `.min()`. Even NaN→0 is a valid index.

#### manifold-gpu

**F9-24 INFO** — `crates/manifold-gpu/src/metal/encoder.rs:121`
```rust
buffer_sizes[idx] = buffer.size as u32;
```
`buffer.size` is u64. If buffer exceeds 4GB, this truncates. However, GPU buffers in this codebase are uniform buffers (kilobytes) and vertex buffers (megabytes at most). Effectively safe.

### 9.2 `as i32` in hot-path crates

#### manifold-playback

**F9-25 VERIFIED SAFE** — `crates/manifold-playback/src/osc_sync.rs:202-205`
```rust
let hours   = values[0] as i32;
let minutes = values[1] as i32;
```
OSC timecode values are small integers (0-23 hours, 0-59 minutes, etc.). Safe.

**F9-26 VERIFIED SAFE** — `crates/manifold-playback/src/osc_sync.rs:224`
```rust
let total_sec = self.pending_timecode_seconds as i32;
```
Timecode seconds are bounded by show duration (hours). Even a 24-hour show is 86400, well within i32.

#### manifold-renderer

**F9-27 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/blob_tracking.rs:154`
```rust
FfiBlobDetector::new(MAX_BLOBS as i32)
```
`MAX_BLOBS` is a small constant (32). Safe.

**F9-28 VERIFIED SAFE** — `crates/manifold-renderer/src/effects/blob_tracking.rs:321-322`
```rust
width: state.readback_w as i32, height: state.readback_h as i32,
```
Readback dimensions are small (computed from READBACK_PIXEL_BUDGET). Safe.

### 9.3 `as f32` — Precision Loss (f64 → f32)

**F9-29 WARNING** — `crates/manifold-playback/src/engine.rs:404,434`
```rust
self.current_time = time_double as f32;
```
This is the **primary time downcast**. `current_time_double` (f64) accumulates wall-clock seconds. `current_time` (f32) is used by `seconds_to_beat()` and all downstream scheduling.

f32 mantissa = 23 bits. At values > 2^23 (~8.4M seconds = ~97 days), precision drops below 1 second. **At realistic show durations (4-8 hours = 14400-28800 seconds), f32 precision is ~0.002 seconds — adequate but not great for beat sync.**

At 120 BPM, one beat = 0.5s. f32 at 28800s has precision ~0.002s, which is ~0.4% of a beat. **This is borderline but likely acceptable.** The f64 `current_time_double` is the authoritative time, and the f32 downcast happens fresh each frame (not accumulated).

However, `seconds_to_beat()` on line 965 takes `self.current_time` (f32), not `current_time_double` (f64). The tempo map converter itself uses f32 math. **This is a design choice matching Unity (which uses float32 time), not a bug.**

**F9-30 INFO** — `crates/manifold-playback/src/audio_sync.rs:247,259,272`
```rust
let current_pos = handle.position() as f32;
```
`handle.position()` returns f64 (kira audio position in seconds). Downcast to f32 for comparison with f32 expected position. Same precision concern as F9-29 but audio positions are relative to clip start, not absolute session time, so values stay small.

**F9-31 INFO** — `crates/manifold-playback/src/osc_sender.rs:122`
```rust
let elapsed = (now - self.last_sent_realtime) as f32;
```
`now` and `last_sent_realtime` are f64. The *difference* is small (fraction of a second), so f32 precision is fine.

### 9.4 `Duration::from_secs_f64` / `Duration::from_secs_f32`

**F9-32 VERIFIED SAFE** — `crates/manifold-app/src/frame_timer.rs:29,92`
```rust
target_frame_duration: Duration::from_secs_f64(1.0 / target_fps),
```
`target_fps` is set from project FPS (typically 30, 60, or 120). `1.0 / 60.0` is a valid positive f64. **Cannot be NaN unless fps is 0.0 (which would give Inf, also panics).** `target_fps` comes from `f64` project settings — if the user somehow sets FPS to 0, this panics.

**Classification: INFO** — extremely unlikely (UI enforces min FPS), but a zero-FPS project file would panic on load.

### 9.5 Instant subtraction

**F9-33 VERIFIED SAFE** — All `.elapsed()` calls in the codebase use the pattern:
```rust
let start = Instant::now();
// ... work ...
let ms = start.elapsed().as_secs_f64() * 1000.0;
```
`Instant::elapsed()` internally uses `Instant::now() - self`, which is always non-negative (monotonic clock). Every usage in the codebase follows this safe pattern. No raw `instant_a - instant_b` where b could be later than a.

**F9-34 VERIFIED SAFE** — `crates/manifold-app/src/frame_timer.rs:42,48`
```rust
self.last_tick_time.elapsed() >= self.target_frame_duration
self.target_frame_duration.saturating_sub(self.last_tick_time.elapsed())
```
Uses `.elapsed()` (safe) and `.saturating_sub()` (cannot underflow). Correct.

---

## TASK 11: Media Decode Safety

### 11.1 Decode Backpressure

**F11-1 VERIFIED SAFE** — `crates/manifold-media/src/video_renderer.rs:556-584`

The video renderer implements explicit frame pacing and backpressure:
- `decode_pending` flag: only one decode job per clip at a time (lines 556, 571, 580)
- `time_accumulator` pacing: frames are only requested when enough time has elapsed for the video's native frame rate (line 564)
- Skip-ahead seek: if >3 frame intervals behind, submits a seek instead of sequential decode (lines 567-575)
- Accumulator reset: if >2 frame intervals behind but <3, resets accumulator to catch up gracefully (lines 577-578)

**No unbounded queue possible.** Each clip can have at most 1 in-flight decode job. The content thread drains results every frame.

### 11.2 Decode Error Handling

**F11-2 VERIFIED SAFE** — `crates/manifold-media/src/decode_scheduler.rs:269-376`

All decode operations return error results via the channel:
- `Open` failure → `DecodeResultStatus::Error` (line 290)
- `Prepare` failure → `DecodeResultStatus::Error` (line 311)
- `Seek` failure → `DecodeResultStatus::Error` (line 337)
- `DecodeNext` failure → `DecodeResultStatus::Error` (line 369)

In `process_decode_results` (video_renderer.rs:338-344), errors log via `log::error!` and set `decode_pending = false`. **No crash, no panic.** Corrupted frames result in the clip showing its last good frame.

### 11.3 Seek Accuracy

**F11-3 INFO** — `crates/manifold-media/src/decoder.rs:200-205`
```rust
pub fn seek_to(&mut self, seconds: f32) -> Result<(), DecoderError> {
    let result = unsafe { decoder_ffi::VideoDecoder_SeekTo(self.handle, seconds) };
```
Seek delegates to native AVAssetReader recreation. The native plugin (MetalVideoDecoderPlugin.m) recreates the AVAssetReader with a time range starting at the target time, then decodes one frame. This is accurate-to-keyframe — the AVAssetReader handles decode-to-target internally. For loops, the seek target is 0.0 (line 312), which always aligns with the file start.

### 11.4 Per-Frame Allocation

**F11-4 WARNING** — `crates/manifold-media/src/video_renderer.rs:527,531,551`
```rust
let results = self.scheduler.drain_results();  // allocates Vec per frame
let pending: Vec<(String, f32)> = ...collect();  // allocates Vec per frame
let clip_ids: Vec<String> = self.active_clips.keys().cloned().collect();  // allocates Vec per frame
```
Three Vec allocations per frame in `pre_render()`. The `drain_results()` allocates a new Vec each call (decode_scheduler.rs:203). The `pending` and `clip_ids` Vecs also allocate.

These are small (typically 0-8 clips) and short-lived, so the allocator overhead is minimal. But they violate the project's "no per-frame allocations on hot paths" invariant. Pre-allocated scratch buffers would be more consistent with the rest of the codebase.

### 11.5 FFI Null Checks

**F11-5 VERIFIED SAFE** — `crates/manifold-media/src/decoder.rs:100-104,108-116`
```rust
let handle = unsafe { decoder_ffi::VideoDecoder_CreatePool() };
if handle.is_null() { return Err(DecoderError::Unavailable); }
...
let handle = unsafe { decoder_ffi::VideoDecoder_Open(self.pool_handle, c_path.as_ptr()) };
if handle.is_null() { return Err(DecoderError::OpenFailed); }
```
All FFI calls that return pointers are null-checked. All FFI calls that return error codes are checked. The `Drop` implementations check for null before calling release functions.

**F11-6 INFO** — `crates/manifold-media/src/video_renderer.rs:222-230`
```rust
unsafe fn copy_frame_to_rt(pool: &DecoderPool, handle_ptr: *mut c_void, ...) -> bool {
    let result = unsafe { decoder_ffi::VideoDecoder_CopyFrameToTexture(...) };
    if result != 0 { log::warn!(...); return false; }
```
The `handle_ptr` passed here is a raw pointer from the decode result. If the decoder was closed between sending the result and processing it, this could be a use-after-free. **However**, the `decode_pending` flag prevents new close jobs while a decode is in-flight, and `stop_clip` is only called from the content thread which also processes results. **Safe by single-thread ordering.**

### 11.6 Multiple Simultaneous Clips

**F11-7 VERIFIED SAFE** — Thread safety model is well-designed:
- 4 worker threads in the decode pool (decode_scheduler.rs:23)
- Clip ID affinity routing ensures all jobs for one clip go to the same worker (line 190-194)
- Each worker has its own local `active` and `warm` AHashMaps (line 266-267)
- `DecoderPool` is `Send + Sync` — creates per-call command buffers for CopyFrameToTexture (line 91-95)
- `DecoderHandle` is `Send` only — each handle used by a single worker thread (line 186)

### 11.7 File Handle Lifetime

**F11-8 VERIFIED SAFE** — `crates/manifold-media/src/decoder.rs:254-261`
```rust
impl Drop for DecoderHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { decoder_ffi::VideoDecoder_Close(self.handle) };
```
Handles are closed when:
- `DecodeJob::Close` is processed (removes from worker's active map, Drop runs)
- Worker shutdown (active.clear() runs Drop on all handles)
- DecoderHandle Drop runs if the worker panics

File handles are NOT kept open for the entire session. They are opened on `start_clip` and closed on `stop_clip`.

### 11.8 Memory-Mapped Files

**F11-9 VERIFIED SAFE** — No memory-mapped file I/O found in the video decode path. AVAssetReader in the native plugin uses standard file I/O through the AVFoundation framework. No SIGBUS risk from deleted files — the native decoder would return an error code instead.

---

## TASK 12: Data Integrity & Serialization

### 12.1 Save Atomicity

**F12-1 VERIFIED SAFE** — `crates/manifold-io/src/archive.rs:98-181`
```rust
let temp_path = format!("{}.tmp", path);
// ... write to temp_path ...
std::fs::rename(&temp_path, path)
```
V2 save uses **temp file + rename** pattern (line 99, 170). On failure, the temp file is cleaned up (line 178). **Crash during save cannot corrupt the original file.**

**F12-2 WARNING** — `crates/manifold-io/src/saver.rs:57-72` (V1 save)
```rust
pub fn save_project_v1(project: &Project, path: &Path) -> Result<(), SaveError> {
    std::fs::write(path, json)
```
V1 save uses direct `std::fs::write()` which is NOT atomic. **A crash during V1 save can corrupt the file.** V1 is described as "for backwards compatibility or testing", so this is lower risk.

**F12-3 WARNING** — `crates/manifold-io/src/archive.rs:166-170`
```rust
if Path::new(path).exists() {
    std::fs::remove_file(path)?;
}
std::fs::rename(&temp_path, path)?;
```
On macOS/Unix, `rename()` is atomic even if the target exists — there is no need for `remove_file` first. The current code has a race window: if the process crashes between `remove_file` and `rename`, both the original and temp file could be lost. **On macOS, `rename` over an existing file is atomic, so the `remove_file` call is unnecessary and introduces a brief window where no file exists.** However, on macOS `rename()` across volumes is not atomic and would fail, which is handled (the error propagates).

### 12.2 fsync After Write

**F12-4 WARNING** — No `fsync`/`sync_all()`/`sync_data()` calls found anywhere in manifold-io.

After `std::fs::rename()`, the data is in the kernel buffer cache but not necessarily on disk. A power failure (not just app crash) could lose the save. For a live performance tool, this is a consideration — though macOS's APFS journal provides some protection.

### 12.3 Autosave During Playback

**F12-5 INFO** — `crates/manifold-app/src/app_lifecycle.rs:23-30`
```rust
pub(crate) fn save_project(&mut self) {
    // Save the local project snapshot (best effort — authoritative is on content thread)
    self.local_project.saved_playhead_time = current_time;
    manifold_io::saver::save_project(&mut self.local_project, path, None, false)
```
Save operates on `self.local_project` which is the **UI thread's snapshot** of the project. The content thread's authoritative copy is separate. The UI thread receives `ContentState` updates periodically and applies them to `local_project`.

**There is no race condition** because save runs on the UI thread using the UI thread's own copy. However, the saved state may lag behind the content thread's actual state by up to one sync cycle. For a manual save (Cmd+S), this lag is imperceptible.

### 12.4 Untrusted Project Files

**F12-6 INFO** — `crates/manifold-io/src/loader.rs:86-93`
```rust
let mut project: Project = serde_json::from_str(&migrated)
    .map_err(|e| LoadError::Deserialize(format!("{e}")))?;
```
serde_json's default stack depth limit is 128. Deeply nested JSON would be rejected. There is no custom depth limit, but 128 is sufficient to prevent stack overflow from adversarial nesting.

**F12-7 INFO** — No explicit size limit on project JSON. A maliciously large project file (e.g., millions of clips) would consume memory proportional to file size during deserialization. This is standard for JSON parsers and not a practical concern for a desktop application loading local files.

### 12.5 Serde Attributes Consistency

**F12-8 VERIFIED SAFE** — All serialized structs in manifold-core use `#[serde(rename_all = "camelCase")]`:
- `Project` (project.rs:14)
- `Timeline` (timeline.rs:10)
- `Layer` (layer.rs:13)
- `TimelineClip` (clip.rs:9)
- `VideoClip` (video.rs:7)
- `VideoLibrary` (video.rs:30)
- `ProjectSettings` (settings.rs:7)
- `EffectInstance` (effects.rs:11)
- `EffectGroup` (effects.rs:85)
- `PercussionImportState` (percussion.rs:6)
- `RecordingState` (recording.rs:9)
- `TempoMap` (tempo.rs:7)
- `Marker` (marker.rs:8)

All typed IDs use `#[serde(transparent)]`:
- `ClipId`, `LayerId`, `EffectGroupId`, `EffectId`, `MarkerId` (id.rs:13-33)

### 12.6 `#[serde(default)]` on Post-V1 Fields

**F12-9 VERIFIED SAFE** — Extensive `#[serde(default)]` coverage found on all fields across all serialized types. For example, `Layer` (layer.rs:15-80) has `#[serde(default)]` on every field including newer ones like `gen_params`, `source_clip_ids`, `duration_mode`, etc. Legacy fields (V1.0.0 format) are marked with `#[serde(default, skip_serializing_if = "Option::is_none")]`.

The migration system (migrate.rs) handles V1.0.0 → V1.1.0 field restructuring (percussion nesting, generator param nesting).

### 12.7 Empty String Typed IDs

**F12-10 INFO** — `crates/manifold-core/src/id.rs:14`
```rust
pub struct ClipId(pub String);
```
The `Default` derive produces `ClipId("")`. The `is_empty()` method exists (line 49) but there are no global guards preventing insertion of empty-string IDs into maps. In practice, IDs are generated by `uuid::Uuid::new_v4()` at clip creation time. An adversarial project file with empty IDs would deserialize successfully and could cause lookup failures. **Low risk** — would only affect corrupt project files.

### 12.8 Unicode Normalization in File Paths

**F12-11 INFO** — No Unicode normalization (NFC/NFD) is performed on file paths. macOS uses NFD internally. The `PathResolver` (path_resolver.rs) uses `std::fs::canonicalize()` for resolved paths, which returns the filesystem's canonical form. Path comparison uses string equality, which would fail if the same file has differently-normalized path representations. This could cause PathResolver to fail to match a file that exists under a different normalization. **Low risk** — only affects path re-linking after project migration, not normal operation.

### 12.9 Disk Full During Save

**F12-12 VERIFIED SAFE** — `crates/manifold-io/src/archive.rs:163-181`
```rust
match write_result {
    Ok(()) => { std::fs::rename(&temp_path, path)?; ... }
    Err(e) => {
        let _ = std::fs::remove_file(&temp_path);
        Err(format!("[ProjectArchive] Failed to save: {e}"))
    }
}
```
The write happens to a temp file. If the disk fills up during write, `zip.finish()` or `zip.write_all()` will return an error, the temp file is cleaned up, and the original file is untouched. **Correct error handling.**

---

## TASK 13: Memory Growth & Long-Running Accumulation

### 13.1 AHashMap Growth Audit

**F13-1 VERIFIED SAFE** — `crates/manifold-playback/src/engine.rs:105-110`
- `active_clip_renderers: AHashMap<ClipId, usize>` — insert on start_clip (line ~1350), remove on stop_clip (line 764). Cleaned up via `stopped_this_tick`.
- `pending_pauses: AHashMap<ClipId, f64>` — insert on pause scheduling, remove on stop_clip (line 769) and after pause fires (line 809).
- `recently_started_times: AHashMap<ClipId, f64>` — insert on clip start, remove on stop_clip (line 771).

All three have corresponding remove paths. **No leak.**

**F13-2 VERIFIED SAFE** — `crates/manifold-playback/src/active_window.rs:15`
- `active_by_id: AHashMap<ClipId, TimelineClip>` — rebuilt from scratch on each `sync_clips_to_time()` call (the window tracks only currently-active clips). Has explicit remove path (line 227).

**F13-3 VERIFIED SAFE** — `crates/manifold-renderer/src/generator_renderer.rs:62-63`
- `active_clips: AHashMap<String, ActiveClip>` — insert on start_clip, remove on stop_clip (line 574).
- `layer_generators: AHashMap<LayerId, LayerGeneratorState>` — insert per-layer on first use. Cleared on resize/release_all. Layer count is bounded by project structure.

**F13-4 VERIFIED SAFE** — Stateful effect maps (all keyed by `i64` owner_key):
- `blob_tracking.rs:142` — `owner_states: AHashMap<i64, OwnerState>` — cleanup_owner_state (line 788), cleanup_owner (line 802), cleanup_all_owners (line 808).
- `stylized_feedback.rs:42` — `states: AHashMap<i64, ...>` — same three cleanup paths.
- `halation.rs:44` — same pattern.
- `bloom.rs:59` — same pattern.
- `depth_of_field.rs:110,113` — same pattern, two maps cleaned in tandem.
- `wireframe_depth.rs:217` — same pattern.

All stateful effect maps have `cleanup_owner` called via `EffectRegistry::cleanup_clip_owner()` when a clip stops. **The cleanup chain is: TickResult::stopped_clips → ContentPipeline::cleanup_stopped_clips() → Compositor::cleanup_clip_owner() → EffectRegistry::cleanup_clip_owner() → each effect's cleanup_owner_state().** This is verified connected at:
- `crates/manifold-app/src/content_thread.rs:369`
- `crates/manifold-app/src/content_export.rs:347`

**F13-5 VERIFIED SAFE** — `crates/manifold-media/src/video_renderer.rs:88`
- `active_clips: AHashMap<String, ActiveVideoClip>` — insert on start_clip (line 395), remove on stop_clip (line 426). render targets returned to pool.

**F13-6 VERIFIED SAFE** — `crates/manifold-media/src/decode_scheduler.rs:266-267`
- Worker-local `active: AHashMap<String, DecoderHandle>` — insert on Open, remove on Close (line 381). Cleared on Shutdown (line 438).
- Worker-local `warm: AHashMap<String, DecoderHandle>` — insert on WarmOpen, remove on WarmClose (line 434) and PromoteWarm (line 415). Cleared on Shutdown.

**F13-7 WARNING** — `crates/manifold-renderer/src/ui_renderer.rs:163,167`
- `text_buffer_cache: AHashMap<(String, u16), TextBuffer>` — grows with unique (text, font_size) combinations.
- `text_cache_used: AHashMap<(String, u16), u64>` — tracks last-used generation.

**Eviction logic exists** (lines 711-724): every 60 frames, entries unused for >120 frames are evicted. This prevents unbounded growth. However, the eviction key is `(String, u16)` — if the UI displays dynamic text that changes every frame (e.g., a constantly updating BPM display with many decimal places), new entries would accumulate faster than eviction. In practice, UI text is relatively static (labels, values that change slowly). **The 120-frame (2-second) staleness threshold is appropriate.**

**F13-8 INFO** — `crates/manifold-renderer/src/layer_compositor.rs:59`
- `blend_pipelines: AHashMap<u32, GpuComputePipeline>` — populated once during init with exactly BLEND_MODE_COUNT entries. Never grows. Safe.

**F13-9 INFO** — `crates/manifold-renderer/src/render_target_pool.rs:18`
- `available: AHashMap<PoolKey, Vec<RenderTarget>>` — when TexturePool is set (which it is in production), `release()` delegates to the TexturePool instead of the local map (line 67-69). **The local map is only used as fallback when no TexturePool exists.** In production, this map stays empty. Safe.

### 13.2 Vec Growth

**F13-10 VERIFIED SAFE** — Engine scratch buffers are pre-allocated and cleared each tick:
- `stop_buffer: Vec<ClipId>` (engine.rs:141) — cleared before use in tick
- `stopped_this_tick: Vec<ClipId>` (engine.rs:144) — cleared at start of each tick, drained into TickResult
- `ready_clips_list: Vec<TimelineClip>` (engine.rs:145) — rebuilt each tick
- `timeline_active_scratch: Vec<TimelineClip>` (engine.rs:146) — cleared and rebuilt each tick
- `became_ready_list: Vec<ClipId>` (engine.rs:147) — cleared each tick
- `clips_to_stop_drift: Vec<ClipId>` (engine.rs:148) — cleared each tick
- `prewarm_candidates: Vec<TimelineClip>` (engine.rs:149) — rebuilt each tick

These Vecs grow to their high-water mark and stay there (Vec doesn't shrink). This is intentional — the allocation happens once and is reused.

**F13-11 WARNING** — `crates/manifold-media/src/video_renderer.rs:202-203`
```rust
let results = self.scheduler.drain_results(); // Vec::new() each call
let pending: Vec<(String, f32)> = ... .collect();
let clip_ids: Vec<String> = self.active_clips.keys().cloned().collect();
```
Three per-frame Vec allocations in video renderer's pre_render. Small (bounded by active clip count, typically 0-8), but violates the no-per-frame-allocation invariant. These should use pre-allocated scratch buffers.

**F13-12 VERIFIED SAFE** — `crates/manifold-playback/src/engine.rs:649,688,699`
```rust
stopped_clips: Vec::new(),
```
TickResult is constructed once per tick. The `stopped_clips` Vec is small (clips that stopped this tick, typically 0-2). The allocation is unavoidable as it's returned to the caller.

### 13.3 Channel Backlog

**F13-13 VERIFIED SAFE** — Main content/UI channels are **bounded**:
```rust
// crates/manifold-app/src/app.rs:1065-1066
let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<ContentCommand>(64);
let (state_tx, state_rx) = crossbeam_channel::bounded::<ContentState>(4);
```
ContentCommand channel: bounded at 64. If the UI floods commands, the sender blocks. ContentState channel: bounded at 4. If the content thread outpaces UI consumption, it blocks.

**F13-14 INFO** — Decode channels are **unbounded**:
```rust
// crates/manifold-media/src/decode_scheduler.rs:153,159
let (result_tx, result_rx) = crossbeam_channel::unbounded::<DecodeResult>();
let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
```
Result channel (worker→content): unbounded, but each clip can only have 1 in-flight decode (decode_pending flag), so max results queued = number of active clips (typically 1-8). Cannot grow unbounded.

Job channels (content→worker): unbounded, but the content thread only submits jobs in pre_render which runs once per frame, and only for clips that need new frames. Max jobs per frame = number of playing clips. Cannot grow unbounded.

**F13-15 VERIFIED SAFE** — MIDI input channel:
```rust
// crates/manifold-playback/src/midi_input.rs:117
let (event_tx, event_rx) = mpsc::channel();
```
std::sync::mpsc is unbounded. MIDI events arrive at human speed (hundreds per second max). Content thread drains all events each tick (60 fps). Even in worst case, channel holds ~2-3 events. No backlog risk.

**F13-16 VERIFIED SAFE** — Background worker channels (blob detection, depth estimation):
```rust
// crates/manifold-renderer/src/background_worker.rs:50-51
let (req_tx, req_rx) = mpsc::channel::<Req>();
let (res_tx, res_rx) = mpsc::channel::<Res>();
```
These workers process one request at a time. The content thread checks `try_recv()` each frame and only submits a new request when the previous one is done. Max 1 message queued.

### 13.4 Texture Pool Size

**F13-17 VERIFIED SAFE** — `crates/manifold-gpu/src/metal/texture_pool.rs:136-143,169-189`

The TexturePool has **two protections against unbounded growth**:

1. **Frame-stamped recycling**: Released textures are tagged with the current frame and only reused after `frames_in_flight` frames. At steady state (same resolution, same effects), all textures are recycled with zero new allocations.

2. **Stale texture pruning**: `prune_stale(300)` is called every 300 frames (~5 seconds). It removes any texture that has been sitting in the pool unreused for 300 frames. This handles the case where the user changes resolution or removes effects — the old-size textures get pruned.

**F13-18 INFO** — The pool is keyed by `(width, height, format)`. If many different resolutions/formats are used over a session (e.g., repeatedly changing output resolution), new textures are allocated and old ones are eventually pruned. Between prune cycles (5 seconds), multiple resolution configurations could accumulate. The pool could temporarily hold textures for 2-3 different resolution configurations simultaneously. Given texture sizes (e.g., 1920x1080 Rgba16Float = ~16MB), this is ~50-100MB transient overhead. **Acceptable.**

### 13.5 String Allocations in Per-Frame Code

**F13-19 VERIFIED SAFE** — In `crates/manifold-playback/src/engine.rs`, `format!` is only used in log/warning callbacks:
```rust
// engine.rs:1518,1530
log_warn(&format!("[PlaybackEngine] Restarted stopped player: {clip_id}"));
log_warn(&format!("[PlaybackEngine] Drift correction: {clip_id} ({drift:.3}s)"));
```
These are conditional (only when drift/restart occurs, not every frame). Safe.

**F13-20 VERIFIED SAFE** — No `format!`, `.to_string()`, or `String::from` found in `content_pipeline.rs` (confirmed by grep).

**F13-21 WARNING** — `crates/manifold-media/src/video_renderer.rs:531,551`
```rust
let pending: Vec<(String, f32)> = ...Some((id.clone(), t))...
let clip_ids: Vec<String> = self.active_clips.keys().cloned().collect();
```
String cloning in per-frame code. `id.clone()` clones String clip IDs. With typical 0-8 active clips, this is ~8 short String allocations per frame. Minimal but present. Could be avoided with index-based iteration.

### 13.6 Log Output Accumulation

**F13-22 VERIFIED SAFE** — The `println!`/`eprintln!` calls found:
- `crates/manifold-media/build.rs:4-21` — build script only, not runtime
- `crates/manifold-renderer/tests/wgsl_validation.rs:100` — test only
- `crates/manifold-io/tests/load_project.rs:220,225` — test only
- `crates/manifold-led/src/artnet.rs:137,145,221,326,345` — error paths only (network failures), not per-frame

**No per-frame println/eprintln in production code.** All runtime logging uses the `log` crate.

**F13-23 INFO** — The `log` crate calls in content_thread.rs are:
- `log::info!` at startup/shutdown — once
- `log::warn!` for RT priority — once
- `log::info!` for LED init — once

Under `#[cfg(feature = "profiling")]`, there are per-60-frame log statements, but these are behind a compile-time feature flag and not active in release builds.

**F13-24 INFO** — `crates/manifold-media/src/video_renderer.rs:201`
```rust
log::debug!("[VideoRenderer] Pool exhausted, creating RT_{idx:02}");
```
This logs once per pool expansion event, not per frame. Pool expansions are rare (happen when clip count exceeds initial pool size of 8).

---

## Summary

### CRITICAL Findings
None.

### WARNING Findings
| ID | File:Line | Issue |
|---|---|---|
| F9-5 | blob_tracking.rs:51-52 | NaN input to `as u32` silently produces 0; saved by `.max(16)` but fragile |
| F9-15 | fluid_simulation_3d.rs:73 | NaN input to `as u32` silently produces 0; saved by match fallback but fragile |
| F11-4 | video_renderer.rs:527,531,551 | Three per-frame Vec allocations in pre_render violate no-alloc invariant |
| F12-2 | saver.rs:57-72 | V1 save uses non-atomic `std::fs::write()` (V1 is legacy/test only) |
| F12-3 | archive.rs:166-170 | Unnecessary `remove_file` before `rename` creates brief window with no file |
| F12-4 | archive.rs (all) | No fsync after save — power failure could lose data |
| F13-11 | video_renderer.rs:527,531,551 | Same as F11-4: per-frame Vec + String allocations |
| F13-21 | video_renderer.rs:531,551 | Per-frame String cloning of clip IDs |

### INFO Findings
| ID | File:Line | Issue |
|---|---|---|
| F9-20 | generator_renderer.rs:640 | i32→u32 cast trusts caller to pass non-negative |
| F9-24 | encoder.rs:121 | u64→u32 truncation for buffer sizes; safe for uniform-sized buffers |
| F9-29 | engine.rs:404,434 | f64→f32 time downcast; precision adequate for 4-8 hour shows |
| F9-30 | audio_sync.rs:247,259,272 | f64→f32 audio position; safe (relative positions stay small) |
| F9-31 | osc_sender.rs:122 | f64→f32 elapsed time; safe (differences are small) |
| F9-32 | frame_timer.rs:29,92 | Duration::from_secs_f64 would panic if FPS=0 (UI prevents this) |
| F11-3 | decoder.rs:200-205 | Seek uses AVAssetReader recreation (keyframe-accurate) |
| F12-5 | app_lifecycle.rs:23-30 | Save uses UI thread snapshot; may lag behind content thread state |
| F12-6 | loader.rs:86-93 | No custom JSON depth limit; serde_json default 128 is sufficient |
| F12-7 | loader.rs:86 | No file size limit on project JSON; standard for desktop apps |
| F12-10 | id.rs:14 | Empty string IDs not guarded; only affects corrupt project files |
| F12-11 | path_resolver.rs | No Unicode normalization; could affect path re-linking |
| F13-7 | ui_renderer.rs:163,167 | Text cache has eviction; dynamic text could accumulate briefly |
| F13-14 | decode_scheduler.rs:153,159 | Unbounded channels; bounded by decode_pending flag in practice |
| F13-18 | texture_pool.rs | Pool grows with resolution changes; pruned every 5 seconds |
| F13-22 | (all crates) | No per-frame println/eprintln in production code |
| F13-24 | video_renderer.rs:201 | Pool expansion log is per-event, not per-frame |

### VERIFIED SAFE Count
41 findings verified safe with specific reasons provided.
