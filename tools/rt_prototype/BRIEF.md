# RT P0 prototype — lane brief

Executes P0 of `docs/RAYTRACING_DESIGN.md` (read §5 P0 first). Standalone measurement
binary, NOT product code, NOT a workspace member. One commit, then stop for review.

## Non-negotiables

- `shaders/rt_trace.metal` is Fable-authored and AUTHORITATIVE. Do not change its
  logic. Mechanical fixes only (a misspelled builtin, a binding attribute the
  compiler rejects) — list every such fix in your final report.
- Standalone cargo project at `tools/rt_prototype/` (own `[package]`, no workspace
  membership: add `[workspace]` empty table). Build:
  `cargo build --release --manifest-path "tools/rt_prototype/Cargo.toml"`
- No edits anywhere outside `tools/rt_prototype/`.
- Forbidden (design doc): integrating into manifold-renderer; building a denoiser;
  any material-system work.

## The binary

```
rt_prototype --scan <path.glb> --out <dir> [--size 3840x2160] [--frames 120]
```

Headless. For each mode (below): render `--frames` frames, write final frame as
`<out>/mode_<x>.png` (tonemapped: ACES-approx + sRGB encode), print timing block.
Also write `raster_only.png` (shadow_spp=0, ao_spp=0, gi_spp=0 → flat ambient) for
context.

## Modes (TraceParams per mode)

| Mode | G-buffer res | trace_size | shadow_spp/sun_cone | ao_spp | gi_spp | Output |
|---|---|---|---|---|---|---|
| A | 4K (=--size) | 4K | 1 / 0.0 (hard) | 0 | 0 | combine at 4K |
| B | 4K | half (1920x1080) | 4 / 0.015 | 4 | 4 | upsample→combine at 4K |
| C | 1440p (2560x1440) | 1440p | 4 / 0.015 | 4 | 4 | combine at 1440p → MetalFX spatial → 4K |

Defaults: sun_dir normalize((0.4,0.8,0.3)), sun_color (8,7.6,7), env_zenith
(0.35,0.45,0.7), env_horizon (0.12,0.10,0.09), ao_radius = 0.3 × scan bounding-sphere
radius. If the GLB has emissive materials use them; else force the material with the
smallest triangle count to emissive (6,2,1)×20 so D4 gets exercised. Log which.

## Harness responsibilities (all Rust, objc2-metal like manifold-gpu)

1. **GLB load** (`gltf` crate): positions, normals, uv, albedo texture (or factor),
   metallic/roughness, emissive factor. Merge all meshes into ONE geometry (single
   vertex/index buffer). Build `Material` array + per-triangle `mat_index: Vec<u32>`
   matching the MSL structs (16-byte layout, see .metal comments).
2. **Acceleration structure**: `MTLAccelerationStructureTriangleGeometryDescriptor` →
   primitive AS build. Time it (CPU wall + cmdbuf GPU time). Refit: sine-displace
   vertex Y (`y += 0.05*r*sin(3t + x)`) each frame for 60 frames, refit AS, report
   avg refit ms. Enable objc2-metal features `MTLAccelerationStructure`,
   `MTLAccelerationStructureCommandEncoder`, `MTLAccelerationStructureTypes` (+ the
   feature list manifold-gpu uses).
3. **Raster G-buffer pass**: offscreen render pipeline. Targets exactly:
   g_wpos rgba32f (xyz world pos, w view-distance; clear w=0), g_nrm rgba16f,
   g_alb rgba16f (linear), g_mat rg16f (metallic, roughness), depth32. Static camera:
   frame the scan bounding sphere (dist = 2.2×radius, 15° elevation, look-at center).
4. **Compute passes** per .metal binding tables: trace_lighting → (mode B only)
   upsample_lighting → shade_combine. Threadgroups 8x8.
5. **MetalFX spatial** (mode C): crib `crates/manifold-gpu/src/metal/metalfx.rs`
   (copy the pattern into this crate; do NOT depend on manifold-gpu).
6. **Timing block** per mode, stdout, exactly:
   ```
   MODE <A|B|C> size=<WxH> trace=<WxH>
   bvh_build_ms=<f> bvh_refit_ms_avg=<f>
   gbuffer_ms=<f> trace_ms=<f> upsample_ms=<f> combine_ms=<f> metalfx_ms=<f>
   frame_ms_avg=<f> fps=<f>
   ```
   GPU times from command-buffer GPUStartTime/GPUEndTime (one cmdbuf per pass for
   attribution), averaged over `--frames` after 10 warmup frames.
7. **PNG writer**: `png` crate; ACES-approx tonemap + sRGB on CPU from an rgba16f
   readback.

## Report back (final message)

Compile status, the full timing blocks for A/B/C, PNG paths, every mechanical .metal
fix you made, and anything that smells wrong (all-black output, zero-cost passes,
refit slower than build). Do NOT interpret the numbers — that happens at review.
