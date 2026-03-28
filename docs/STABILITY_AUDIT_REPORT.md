# MANIFOLD — Live Performance Stability Audit Report

**Date:** 2026-03-28
**Auditor:** Claude Opus 4.6 (1M context), 10 parallel agents (5 initial + 5 gap coverage)
**Scope:** 13 stability audits + gap coverage pass, ~99K lines across 12 crates, focus on infinite-runtime safety
**Codebase state:** commit 2e955d7 (main)

## Executive Summary

- **Total findings: 351**
- **CRITICAL: 23** (will crash, corrupt, silently degrade, or leave the app vulnerable during a show)
- **WARNING: 52** (unsafe patterns, convention violations, minor resource concerns)
- **INFO: 65** (worth noting)
- **VERIFIED SAFE: 211** (audited and confirmed correct)

## Top 10 Most Dangerous Findings

| # | Severity | Location | Description | Impact |
|---|----------|----------|-------------|--------|
| 1 | CRITICAL | `main.rs:29-38` | No `panic::set_hook` installed | Combined with `panic=abort` + stripped symbols, a crash produces **zero diagnostics** — instant black screen, no log |
| 2 | CRITICAL | (missing from codebase) | No `IOPMAssertion` for display sleep prevention | macOS **will dim and sleep** during a multi-hour generative show with no mouse input |
| 3 | CRITICAL | `Cargo.toml:61-62` | `strip="symbols"` + `panic="abort"` with no hook | Even Apple's crash reporter cannot produce useful info — crash is completely opaque |
| 4 | CRITICAL | `effects/stylized_feedback.rs:140-147` | No NaN guard on feedback buffer reads | One frame of NaN from any upstream source = **permanent visual corruption** until clip stops |
| 5 | CRITICAL | `effects/stylized_feedback.rs:110`, `fx_stylized_feedback_compute.wgsl:31` | Zoom parameter can be 0.0 → division by zero in shader | User-triggerable Inf enters feedback loop permanently |
| 6 | CRITICAL | `metal/device.rs:67,79,93,463` | `new_texture`/`new_buffer` nil not checked | Under GPU memory pressure, Metal returns null → **crash on first use** (low probability on Apple Silicon) |
| 7 | CRITICAL | `generators/mri_volume.rs` (render path) | Synchronous TIFF file I/O on content thread during `render()` | **Blocks 60 FPS render loop** whenever slice position parameter changes — frame hitch during show |
| 8 | CRITICAL | `engine.rs:93,404,434` + all generators | f32 time/beat precision degrades after ~4 hours | **Timing drift and animation jitter** on long timelines — precision should never degrade |
| 9 | CRITICAL | `engine.rs:56` | `frame_count: i32` overflows after 414 days | **Installation failure** — counter wraps, logic depending on it breaks |
| 10 | WARNING | `metal/types.rs:175-178`, `content_pipeline.rs:220` | No GPU timeout handling | If GPU hangs, content thread **spins forever** — frozen app with no recovery |

## Confirmed Safe (no issues found)

- **Beat position derivation** — f64 master time, per-frame beat re-derivation, sub-microsecond precision after 4 hours
- **Delta time** — monotonic `Instant` subtraction, no float accumulation
- **Playback state machine** — complete transitions with re-entrancy guard, cannot get stuck
- **Frame pacing** — hybrid sleep+spin with 3ms macOS jitter margin
- **Autoreleasepool** — every content frame and export frame wrapped
- **Content thread independence** — never blocks on UI thread, lock-free hot path
- **Channel safety** — all sends handle disconnection without panic
- **Stall recovery** — skip-to-current (no GPU flood after stalls)
- **Lock-free MIDI clock** — correct Acquire/Release atomics, no shared mutable state
- **OSC deduplication** — latest-per-address, bounded growth
- **Clock source switching** — all accumulators reset cleanly
- **Scratch buffers** — all cleared before use each frame, none grow unboundedly
- **DataVersion counter** — u64, cannot overflow
- **Layer composite order** — deterministic sorted slice, not HashMap
- **Effect execution order** — deterministic Vec iteration
- **Effect cleanup chain** — all 6 stateful effects have cleanup_owner_state, fully connected
- **Compute shader bounds** — all shaders have early-return for out-of-bounds threads
- **Workgroup sizes** — all ≤ 256 invocations
- **Uniform alignment** — all `#[repr(C)]` with compile-time size assertions
- **IOSurface retain/release** — balanced via CF/Metal ownership rules
- **Zero transmute** — none in entire codebase
- **No completion handlers** — all GPU sync via MTLSharedEvent (eliminates capture-lifetime bugs)
- **Binary archive corruption** — graceful fallback to recompile
- **Save atomicity (V2)** — temp file + rename pattern
- **AHashMap cleanup** — all stateful maps have corresponding remove paths
- **Texture pool recycling** — frame-stamped with 5-second stale pruning
- **Ableton Link sync** — absolute beat positioning via `beat_at_time()`, zero drift risk
- **Generator uniforms** — `set_bytes` inline on Metal (baked into command buffer), no double-buffer needed
- **Single-layer fast path** — produces identical output to multi-layer path
- **No signal handler conflicts** — no native plugin installs signal handlers
- **ABI compatibility** — all FFI is pure C (`extern "C"`), no C++ STL crosses boundaries
- **Thread count bounded** — 5-8 steady state, no per-clip/per-effect spawning
- **File descriptors managed** — all handles closed via Drop impls
- **Window resize** — does not affect content thread (separate GPU device)
- **Export exclusivity** — replaces content thread loop, cannot run alongside playback
- **Version migration** — complete chain v1.0.0→v1.1.0, forward-compatible with unknown versions
- **All 15 stateless effects** — uniform pattern, no per-owner state, no per-frame allocs, all params safe
- **Numeric cast safety** — all `i32 as usize` bounded by `.get()`, all `usize as u32` bounded by `.min()`

---

*Full findings for each of the 13 audit tasks follow in the section files:*

**Initial audit (5 agents):**
- `stability_audit_section_1_2.md` — Tasks 1-2: Playback Engine, MIDI/OSC
- `stability_audit_section_3_10.md` — Tasks 3, 10: Content Thread, Platform Hardening
- `stability_audit_section_4_8.md` — Tasks 4, 8: GPU Core Safety, Unsafe/FFI
- `stability_audit_section_5_6_7.md` — Tasks 5-7: Compositor, Effects/Generators, Shaders
- `stability_audit_section_9_11_12_13.md` — Tasks 9, 11-13: Numeric Casts, Media, Serialization, Memory Growth

**Gap coverage (5 agents):**
- `stability_audit_gaps_effects.md` — 15 remaining stateless effects
- `stability_audit_gaps_generators.md` — 14 remaining generators (including MRI volume CRITICAL)
- `stability_audit_gaps_shaders.md` — 5 remaining shaders + fast_math impact analysis
- `stability_audit_gaps_numeric_misc.md` — Numeric cast gaps, HashMap determinism, Link sync, FD/thread count
- `stability_audit_gaps_remaining.md` — Compositor gaps, FFI safety, 14 remaining unread files

---

## All Findings by Severity

### CRITICAL (23)

| ID | File:Line | Description |
|----|-----------|-------------|
| 10.1 | `main.rs:29-38` | No `panic::set_hook` — zero crash diagnostics with `panic=abort` |
| 10.2 | (missing) | No `IOPMAssertion` — macOS will sleep during show |
| 10.12 | `Cargo.toml:61-62` | `strip=symbols` + `panic=abort` + no hook = opaque crashes |
| 5-C1 | `effects/stylized_feedback.rs:140-147` | No NaN guard on feedback reads — permanent visual corruption |
| 5-C2 | `effects/stylized_feedback.rs:110`, WGSL `:31` | Zoom=0 → division by zero → Inf in feedback loop |
| 4.3-A | `metal/device.rs:67,79,93,463`, `texture_pool.rs:123` | Texture/buffer allocation nil not checked — crash under memory pressure |
| G-MRI | `generators/mri_volume.rs` (render path) | Synchronous TIFF file I/O on content thread blocks 60 FPS render loop |
| W1→C | `engine.rs:93` | `current_beat: f32` precision degrades after 4hr — timing drift on long timelines |
| W2→C | `engine.rs:404,434` | `current_time_double as f32` loses precision (~1ms at 4hr, ~8ms at 24hr) — affects all downstream scheduling |
| W3→C | `engine.rs:56` | `frame_count: i32` overflows after 414 days — installation failure |
| G-GEN2→C | (all generators) | `ctx.time: f32` loses precision after ~4.6 hours — animation drift |
| G-GEN-FC→C | (all generators) | `frame_count: u32` in `StatefulBase` wraps after ~2.2 years at 60fps |
| 10.3→C | (missing) | No App Nap disabling — macOS throttles content thread when not frontmost |
| 10.9→C | `main.rs` (missing) | No SIGPIPE handling — broken pipe silently kills the process |
| 10.11→C | `main.rs` (missing) | No multiple-instance protection — second instance causes GPU contention + port conflicts |
| 3.9→C | `content_thread.rs:121-127` | `SCHED_RR` may fail without privileges, no QoS fallback — macOS can demote content thread |
| W5→C | `live_clip_manager.rs:73` | No timeout for orphaned phantom clips on MIDI disconnect |
| W6→C | `midi_input.rs:58-66` | No MIDI device disconnect detection or auto-reconnect |
| W7→C | `midi_input.rs:117` | Unbounded mpsc channel for MIDI note events — grows during content thread stalls |
| W8→C | `midi_input.rs:106,367` | Telemetry counters `i32` overflow after ~19hr at max throughput |
| 4.13-A→C | `content_pipeline.rs:220` | No GPU timeout — content thread spins forever on GPU hang |
| 10.5→C | (missing) | No wgpu device lost handler — UI thread crash with no recovery |

### WARNING (52)

#### Playback & Timing (Task 1)
| ID | File:Line | Description |
|----|-----------|-------------|
| W4 | `video_time.rs:33`, `engine.rs:1312` | Loop wrapping uses `%` instead of floor-based repeat |

#### Content Thread (Task 3)
| ID | File:Line | Description |
|----|-----------|-------------|
| 3.3a | `content_command.rs:171-175` | Command channel silently drops editing commands when full (64 cap) |
| 3.7b | `generator_renderer.rs:62` | Generator `layer_generators` HashMap not cleaned on clip stop |
| 3.8b | `content_commands.rs:180` | No explicit effect state clear on project load |

#### GPU Core (Task 4)
| ID | File:Line | Description |
|----|-----------|-------------|
| 4.5-B | `texture_pool.rs:24-55` | `TexturePool` has `unsafe impl Sync` but uses UnsafeCell — UB if shared |
| 4.5-C | `texture_pool.rs` | No max pool size cap — VRAM grows after resolution changes |
| 4.7-A | `metal/device.rs:226-238` | Runtime pipeline compilation mid-show causes frame hitch |
| 4.10-B | `metal/encoder.rs:470-476` | Drop doesn't call `end_current()` — leaks retained compute encoder |

#### Compositor & Rendering (Task 5)
| ID | File:Line | Description |
|----|-----------|-------------|
| 5-W1 | `layer_compositor.rs:710-731` | NaN propagation from generators through compositor (no sanitization) |
| 5-W2 | `uniform_arena.rs:44-58` | UniformArena silently drops data on first overflow frame |
| 5-W3 | `layer_compositor.rs:486,825` | Per-frame Vec allocation for `layer_outputs` |

#### Effects & Generators (Task 6)
| ID | File:Line | Description |
|----|-----------|-------------|
| 6-W1 | `wireframe_depth.rs:632,685` | Per-dirty-frame CPU allocations (~200KB) for DNN uploads |
| 6-W2 | `fluid_simulate.wgsl:163` | No explicit force magnitude clamp on vector field |

#### Unsafe & FFI (Task 8)
| ID | File:Line | Description |
|----|-----------|-------------|
| 8.2-A | `blob_ffi.rs:94`, `depth_ffi.rs:193` | C++ plugin exceptions cross FFI boundary as UB |
| 8.3-A | `blob_ffi.rs:82-106` | BlobDetector output count not validated |
| 8.7-A | `texture_pool.rs:81-173` | UnsafeCell aliasing risk with Sync impl |
| 8.8-D | (architecture) | Video decode worker threads lack autoreleasepool |
| 8.11-A | `mps.rs:150,185,214,244` | MPS kernel creation can throw NSException |
| 8.11-X | `metalfx.rs:188-200` | `supports_spatial_scaling` discards return, always returns true |

#### Numeric Casts (Task 9)
| ID | File:Line | Description |
|----|-----------|-------------|
| F9-5 | `blob_tracking.rs:51-52` | NaN→u32 silently produces 0; saved by `.max(16)` but fragile |
| F9-15 | `fluid_simulation_3d.rs:73` | NaN→u32 silently produces 0; saved by match fallback but fragile |

#### Platform (Task 10)
| ID | File:Line | Description |
|----|-----------|-------------|
| 10.10 | `content_thread.rs:121-127` | No QoS escalation fallback if SCHED_RR fails |

#### Media & Serialization (Tasks 11-12)
| ID | File:Line | Description |
|----|-----------|-------------|
| F11-4 | `video_renderer.rs:527,531,551` | Three per-frame Vec+String allocations in pre_render |
| F12-2 | `saver.rs:57-72` | V1 save uses non-atomic write (legacy) |
| F12-3 | `archive.rs:166-170` | Unnecessary `remove_file` before `rename` creates brief no-file window |
| F12-4 | `archive.rs` | No fsync after save — power failure could lose data |

---

## Gap Coverage Findings (Pass 2)

### New WARNING findings from gap audit

#### Remaining Effects (STAB-6 gap)
| ID | File:Line | Description |
|----|-----------|-------------|
| G-FX1 | `infrared.rs:71-72` | `1.0 / width`, `1.0 / height` texel_size — division by zero if dimensions are 0 (mitigated by compositor guarantees) |
| G-FX2 | `edge_detect.rs:78-79` | Same texel_size division pattern |
| G-FX3 | `transform.rs:107`, `voronoi_prism.rs:57` | Aspect ratio division — same pattern |

#### Remaining Generators (STAB-7 gap)
| ID | File:Line | Description |
|----|-----------|-------------|
| G-GEN1 | `tesseract.rs`, `duocylinder.rs` | `project_4d` division by `w - proj_dist` — can approach zero, producing extreme vertex positions |
| G-GEN2 | (all generators) | `ctx.time` is f32 — loses precision after ~4.6 hours (1ms resolution). Affects all time-based animations. |
| G-GEN3 | `wireframe_zoo.rs` | `normalize_shape()` allocates a new `Vec` every frame — per-frame heap allocation on hot path |
| G-GEN4 | (multiple generators) | `anim_progress` subtraction-based wrapping can drift with high speed values |
| G-GEN5 | (line generators) | Silent truncation beyond MAX_POSITIONS/MAX_INSTANCES limits — visual artifacts |
| G-GEN6 | `mri_volume.rs` | 3D texture dimensions from TIFF header not validated — could allocate enormous textures |
| G-GEN7-10 | (various) | Minor: trigger_count f32 precision, per-frame Vec in parametric_surface, etc. |

#### Remaining Shaders (STAB-8 gap)
| ID | File:Line | Description |
|----|-----------|-------------|
| G-SH1 | `fluid_scatter_3d.wgsl:79` | `@workgroup_size(8,8,8)` = 512 invocations — exceeds documented 256 convention (works on Apple Silicon) |
| G-SH2 | `compositor_blend_compute.wgsl:118-120` | ColorDodge `select()` evaluates Inf-producing branch — fragile under fast_math |

#### fast_math Impact
| ID | File:Line | Description |
|----|-----------|-------------|
| G-FM1 | `metal/device.rs:160,298` | `set_fast_math_enabled(true)` global — makes `clamp(NaN, ...)` behavior undefined on feedback paths |

#### Numeric/Platform/Misc Gaps
| ID | File:Line | Description |
|----|-----------|-------------|
| G-NUM1 | `active_clip_renderers` AHashMap iteration | Non-deterministic — multiple generators on same layer could flicker |
| G-NUM2 | `gpu_readback.rs:91` | Per-frame ~8MB `Vec<u8>` allocation on content thread hot path |
| G-NUM3 | `gpu.rs:29` | No wgpu device lost handler on UI thread — crash with no recovery |
| G-NUM4 | `process_runner.rs` | Dropped JoinHandles for external process I/O threads |
| G-NUM5 | Various UI renderer | Per-frame wgpu bind group + buffer allocations (low severity, cacheable) |
| G-NUM6 | `mri_volume.rs` | No async loading — blocks content thread during TIFF decode |
| G-NUM7 | `metal_encoder.rs` | Frame index `i32` overflows after ~165 hours at 60fps (safe for shows) |

### Key Confirmed Safe from Gap Audit

| Category | Reason |
|----------|--------|
| Ableton Link sync | Absolute beat positioning via `beat_at_time()`, f64 precision, zero drift |
| Generator uniforms | `set_bytes` inline on Metal — data baked into command buffer, no race |
| Single-layer fast path | Identical output to multi-layer path, verified |
| All FFI boundaries | Pure C, no C++ STL crossings, no signal handlers in plugins |
| Thread count | Bounded 5-8 steady state, no per-clip spawning |
| File descriptors | All managed via Drop impls, no accumulation |
| Export exclusivity | Replaces content thread, cannot run alongside playback |
| Version migration | Complete v1.0.0→v1.1.0 chain, forward-compatible |
| All 15 stateless effects | Uniform safe pattern, no state, no allocs |
| All numeric casts audited | `i32 as usize` bounded, `usize as u32` bounded, no float equality bugs |
