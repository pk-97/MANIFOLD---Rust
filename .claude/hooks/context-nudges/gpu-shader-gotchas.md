
# GPU / shader gotchas

## Alignment & layout
- WGSL `vec3<f32>` has 16-byte alignment in storage buffers — Rust structs must pad to match or data is silently misaligned.
- Uniform and storage structs must match WGSL alignment exactly: 16-byte padding, vec3 → vec4, field order identical.
- WGSL files with multiple entry points must have identically-sized uniforms at the same binding index — naga generates broken Metal code otherwise (GPU hang, no compile error).

## Encoder / resources
- GpuEncoder owns `metal::CommandBuffer`; zero wgpu types anywhere in the content GPU path.
- GpuEncoder's per-slot bind cache must invalidate across setBytes ↔ setBuffer transitions — stale hits leave the wrong resource bound and cause GPU page faults.
- `copy_texture_to_texture` is a CROP (no scaling) — resize via `GpuEncoder::resize_sample`; a same-size assert now enforces this.
- Metal ray query `accept_any_intersection(true)` guarantees only `.type`, NOT `.distance` — never use its distance as a diagnostic or shading input.

## Display / presentation
- NEVER unify CVDisplayLinks: heavy GPU work (presenter blit) in a vsync callback that also times other consumers makes CoreVideo skip callbacks and starve everyone.
- Fullscreen CAMetalLayer engages Direct Display — present on every vsync or WindowServer thrashes all displays.
- Bloom/halation run at native resolution (`HDR_BUFFER_DIVISOR = 1`) — headroom allows it post-Metal-migration.

## Serde
- All serialized structs use camelCase JSON (Unity project format) — getting this wrong silently breaks project loading.
---
name: metal-resource-residency-bugs
description: "GPU memory-error class agents miss — driver reclaims undeclared resources; symptoms, suspect list, and the cure-decomposition method (BUG-jddy, 2026-07-27)"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 6fcf2579-33b2-426d-a9d6-5c6e2916f1cb
  modified: 2026-07-26T23:46:01.400Z
---

# Metal resource-residency bugs — the class agents struggle with

BUG-jddy (RT static-death) cost ~3 days because every agent (Fable review lane, GLM consult, DeepSeek fix lane) reached for *data-content* theories (stale caches, address spaces, buffer lifetimes in Rust) when the mechanism was *driver bookkeeping*: Metal reclaims GPU resources that no submitted command declares usage on (`useResource`). Static scene → no refits → TLAS-referenced BLASes + instance buffer undeclared → reclaimed ~5 frames later. Details: [[rt-static-death-stopgap-handoff]].

**Why:** GPU memory errors don't look like memory errors. No crash, no warning, no validation error — the shader just reads plausible garbage, and only in scenarios where *nothing is changing*. Agents debug logic; this class lives in housekeeping.

**How to apply — suspect this class when ALL of these hold:**
- GPU output is wrong/garbage/zeroed, but CPU-side state probes all read healthy.
- The failure correlates with *inactivity* (nothing changed for N frames), not with an action. Action-correlated bugs are logic; inactivity-correlated bugs are lifetime/residency.
- A seemingly unrelated per-frame operation "cures" it. Don't ask "why does the cure's data matter" — decompose the cure into its atomic actions (write / GPU command / flag / resource reference) and bisect which atom is load-bearing. The cure IS the bisect ([[cure-test-before-deep-reads]]).

**The Metal suspect list, in order:**
1. Missing `useResource` for indirectly-reached resources — anything behind a bindless GPU address, a BLAS referenced by a TLAS, an instance buffer consumed at build time. Metal's "binding makes transitives resident" doc claim is NOT sufficient (proven 2026-07-27).
2. CPU-written shared buffers with no per-frame GPU command touching them.
3. `constant` address space on anything the CPU rewrites (hygiene, not proven harmful — but `device` is the honest space for per-frame CPU writes).
4. Resources referenced only by command buffers that have already completed.

**Reusable diagnostic signature:** shadow/AO survived, GI/reflections died — the split case told us traversal was fine and instance-data-following reads were broken. Always enumerate what the survivors share that the dead lack ([[the-split-case-is-the-diagnosis]]).

**Debug tooling reality:** no static lint exists for this class. `MTL_VALIDATION=1` catches crashes, not silent reclamation. The only reliable oracle is a scenario run (rt-capture-style) that holds the system in the inactivity state.
