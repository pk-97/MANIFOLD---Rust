# Infrared blackout — investigation handoff (2026-06-12)

## Symptom (from Peter, on-stage / GraphTestsV4.manifold)

Specific chain orders kill the image to **black**. Reported shape:
`WireframeZoo generator → Infrared (palette = Arctic) → QuadMirror`.

- The frame is **black**, not washed/dark. Earlier (2026-06-11) reports of a
  *dark wash* were a different, already-fixed bug.
- It **flashes correct for a few frames, then goes black.**
- **Nudging the palette knob re-flashes the visuals for a few frames, then they
  go black again.**
- Order-dependent: `Infrared → QuadMirror` blacks out; `QuadMirror → Infrared`
  renders fine (see the three screenshots in the originating session).
- **Reproduces with `MANIFOLD_CHAIN_FUSION=0`.** → The fusion compiler is NOT
  the cause. Do not re-investigate fusion; that was ruled out by Peter directly.

## What is already ruled out

1. **Chain fusion / freeze compiler** — Peter confirmed it still happens with
   `MANIFOLD_CHAIN_FUSION=0`. Eight fused-vs-per-card repro tests
   (`preset_runtime.rs`, `chain_fusion_tests`) all pass: non-default palette,
   alpha-0 wireframe background, disabled-card-gap segments, half-res chain
   input, mid-show background swap, frame-stability. None reproduce it.
2. **`output_dims` plumbing asymmetry** — `gradient_ramp`'s 256×1 `output_dims`
   override IS correctly threaded into `ExecutionPlan::resource_dims`
   (execution_plan.rs:403/470) and `resolve_dims` (execution.rs:42). No
   write-at-256×1 / reason-at-canvas mismatch in the plan itself.
3. **The 256×1 LUT right-size is correct and must stay.** Peter: "that's a
   genuine improvement for a LUT, we don't need a 4K LUT." Do NOT revert
   commit `27ede1c1`. The fix is elsewhere.

## Leading hypothesis (unproven — needs the real app)

"Flash for a few frames, then black; re-flash on param change" is the signature
of a **held output served stale/black by the memoization / constant-subgraph
hoisting path** after it latches.

Infrared's graph (`assets/effect-presets/Infrared.json`):
`source → [10× gradient_ramp → mux_texture(selector=palette)] → color_lut → out`

- `gradient_ramp`, `mux_texture`, `lut1d`/`color_lut` are declared **pure**, so
  they are **hoistable + sticky** (execution.rs ~620-636): after frame 0 the
  executor stops re-running them and serves their **held output slots** without
  re-evaluating (the memo skip, execution.rs ~721-753). `color_lut` reads the
  live `source` so it is NOT hoistable and runs every frame — it reads the
  **held** mux LUT.
- Two changes landed **the same day** as the regression:
  - `27ede1c1` — `gradient_ramp` now outputs a fixed **256×1** strip (not
    canvas-sized).
  - `80e99f2b` — `TexturePool::evict_resolution_mismatch(render_w, render_h)`,
    called from `ContentPipeline::resize()` (content_pipeline.rs:1435) on every
    canvas change (window resize, render-scale, perform↔edit toggle, HDR). It
    drops every FREE pool entry whose resolution ≠ render dims. **256×1 LUT
    strips are a permanent resolution mismatch**, so they are always eviction
    candidates.

The suspected mechanism: a memo-held 256×1 LUT slot's backing texture gets
returned to the pool and then evicted/recycled (the recycle-safety window is
`frames_in_flight` frames — "a few frames"), after which the still-latched memo
slot reads black/garbage. The palette nudge forces `mux` to re-run for a couple
frames (re-flash) before the memo re-latches onto the dead slot.

**This is a hypothesis, not a confirmed root cause.** The mechanism by which a
*sticky* (never-`free_after`-released) resource's texture reaches the free list
is the missing link — likely a chain rebuild (resize/canvas change drops the old
executor, releasing its held RenderTargets to the pool) followed by
`evict_resolution_mismatch`, or a non-idempotent re-acquire. Confirm before
fixing.

## Why the unit harness can't reproduce it (important)

All eight repro tests build via `PresetRuntime::try_build(..., pool: None, ...)`.
With `pool: None` the renderer `MetalBackend` uses a bare `RenderTargetPool`
with **no heap `TexturePool` attached** (`set_texture_pool` never called), so
**`evict_resolution_mismatch` and heap-pool recycling are never exercised**, and
the harness never changes canvas or advances frames the way the app does. To
reproduce in-harness you must:
- construct a real `manifold_gpu::TexturePool`, attach it via
  `MetalBackend::set_texture_pool`, and
- drive a **canvas/resolution change** mid-run (`PresetRuntime::resize`, or the
  `ContentPipeline::resize` path) so `evict_resolution_mismatch` fires against
  the live 256×1 LUT strips, then keep rendering and assert non-black.

The `from_json_str_with_device` constructors DO build a pool-backed
`MetalBackend`, but they're generator-shaped (no external `source` input);
Infrared needs a source feed.

## Recommended next steps (in order)

1. **Reproduce live with instrumentation.** Per the repo's runtime-bug rule
   (println → reproduce → read logs), add `eprintln!`s at: the memo-skip serve
   (execution.rs ~752), the sticky `free_after` exemption (execution.rs ~1110),
   `MetalBackend` acquire/release of the gradient_ramp/mux output ResourceIds,
   and `evict_resolution_mismatch`. Reproduce the GraphTestsV4 state and read
   which fires in the frame the image goes black. `MANIFOLD_POOL_STATS=1` logs
   the pool report every ~300 frames.
2. **Confirm the resize correlation.** Ask whether the blackout coincides with a
   window resize / render-scale toggle / perform-mode switch / HDR toggle — all
   call `ContentPipeline::resize` → `evict_resolution_mismatch`. If yes, the
   eviction path is confirmed.
3. **Likely fix shape (once confirmed):** the eviction (and the memo's slot
   liveness) must treat sub-canvas, hoisted/sticky LUT strips as **persistent /
   non-evictable**, the same way feedback-state and memo-held resources are
   meant to be exempt. The `evict_resolution_mismatch` doc claims sticky/
   persistent resources "are held by the executor and never returned here" —
   verify that claim holds across a **chain rebuild** (the old executor drops
   and releases its held targets to the pool). That drop→release→evict window is
   the prime suspect.

## Key code references

- `crates/manifold-renderer/src/node_graph/execution.rs`
  - `compute_live_steps` (473) — mux branch pruning / liveness
  - hoistable+sticky build (606-637); memo skip (721-753); sticky `free_after`
    exemption (1101-1119)
- `crates/manifold-renderer/src/node_graph/metal_backend.rs` — `set_texture_pool`
  (228), `install_texture_2d`/`replace_texture_2d` (252/277), acquire/release/
  `slot_for` (497/552/570)
- `crates/manifold-gpu/src/metal/texture_pool.rs` — `acquire` recycle window
  (84-92), `evict_resolution_mismatch` (185), `report` (216)
- `crates/manifold-app/src/content_pipeline.rs` — `resize` →
  `evict_resolution_mismatch` (1416-1436)
- `crates/manifold-renderer/src/node_graph/primitives/gradient_ramp.rs` —
  `output_dims` 256×1 override (~110)
- `assets/effect-presets/Infrared.json` — the graph
- Repro tests: `crates/manifold-renderer/src/preset_runtime.rs`,
  `chain_fusion_tests` (search `infrared_quadmirror`, `infrared_alone`,
  `fused_segment_`)

## Suspect commits (all 2026-06-11, same day as regression)

- `27ede1c1` Right-size gradient_ramp output to a 256x1 LUT strip (KEEP — do not
  revert; the fix must accommodate it)
- `80e99f2b` Texture pool: evict old-resolution allocations on canvas change
- `49e3af33` resolution and memory workstream (3 commits)
