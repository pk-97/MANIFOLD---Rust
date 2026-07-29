You are a mechanical code-change generator. Read the brief and the code excerpts, then output ONLY a JSON object, no prose, no markdown fences:

{"edits": [{"path": "<repo-relative path>", "find": "<exact text currently in the file>", "replace": "<replacement text>"}, ...],
 "writes": [],
 "commit_message": "<one line>"}

Rules:
- `find` must be copied EXACTLY from the excerpts (whitespace included) and must be unique in the whole file — include enough surrounding lines to be unique.
- Make every edit the brief demands in both files, nothing else.
- If the brief and the excerpts disagree, trust the excerpts.

# Brief: R2 Step 1 — specular history plumbing, INERT
## What you are building (design: RAYTRACING_DESIGN §9.6 R2 + RD6/RD10)

Stable reflections (R2) adds temporal accumulation for the reflection channel INSIDE the existing `accumulate_irradiance` kernel. This step is PLUMBING ONLY: the specular history ping-pong pair + new kernel arguments, bound but semantically inert (pass-through write, no history read). A later step writes the reprojection math. Every edit below mirrors an existing RT-R1/T1-C pattern — when unsure, copy the irradiance-history pattern exactly.

**THE R1 INCIDENT YOU MUST NOT REPEAT:** twice in R1, kernel signatures grew but the pipeline SLOT MAPS in `MetalShadowRayTracer::new` were not extended — writes went nowhere and tests read zeros. Every signature change below has a matching slot-map change; they are listed together per site.

## Edits (exactly these files, in the worktree)

### 1. `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`

a. Struct field: beside the existing `rt_irr_history: [Option<GpuTexture>; 2]` declaration (find via `rg -n "rt_irr_history" crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` — field decl near :810, constructor init near :1060), add `rt_refl_history: [Option<GpuTexture>; 2]` with a doc comment `// RT-R2 (RD6): specular history ping-pong pair — same lifecycle + same ping clock as rt_irr_history (I-R2: one reset path, one flip).` Init `[None, None]` everywhere `rt_irr_history` is initialized.

b. `ensure_rt_irradiance` (:1741): immediately after the `self.rt_irr_history = [...]` block (:1782-1786), allocate the pair — `Rgba16Float`, full `width`×`height`, labels `"node.render_scene rt_refl_history_a (RT-R2)"` / `"..._b (RT-R2)"`. Same `.map(Some)` shape. Being inside this realloc block IS the RESET-not-resized rule — add no new reset logic.

c. `accumulate_irradiance` call site (:4292): the read/write index locals already exist (`read_idx`, `write_idx`, :4210-4211). Add:
```rust
let refl_history_read = self.rt_refl_history[read_idx].as_ref().expect("ensured above");
let refl_history_write = self.rt_refl_history[write_idx].as_ref().expect("ensured above");
```
and pass `irr_full`'s reflection sibling `refl_full` (already bound at :4158) plus these two plus `gi_materials_buffer` to the updated call — argument order per the new trait signature below. NO new flip variable: the existing `self.rt_history_ping = write_idx` (:4310) covers both channels.

### 2. `crates/manifold-gpu/src/metal/raytrace.rs`

d. Trait `ShadowRayTracer::accumulate_irradiance` (:2204): before `label: &str`, add:
```rust
// RT-R2 (RD6): reflection channel — current-frame filtered reflections
// (`.a` = hit distance), specular history ping-pong, and the material
// table (roughness source for the reprojection blend, Step 2).
hi_refl: &GpuTexture,
refl_history_read: &GpuTexture,
refl_history_write: &GpuTexture,
gi_materials: &GpuBuffer,
```

e. MSL kernel `accumulate_irradiance` (:1421, inside `SHADOW_RAYS_MSL`): add parameters after `moments_write`:
```msl
texture2d<float>                     hi_refl             [[texture(11)]],
texture2d<float>                     refl_history_read   [[texture(12)]],
texture2d<float, access::write>      refl_history_write  [[texture(13)]],
constant GiMaterial*                 gi_materials        [[buffer(3)]],
```
(`GiMaterial` is already defined in this MSL source — verify with `rg -n "struct GiMaterial"`.)
Body changes — PASS-THROUGH ONLY, two sites: in the `p.reset != 0u` branch (:1455-1461) add `refl_history_write.write(hi_refl.read(tid), tid);`; at the tail writes (:1553-1556) add the identical line. Do NOT read `refl_history_read` or `gi_materials` anywhere (bound-not-read is intentional this step; MSL does not warn on unused params).

f. `accumulate_pipeline` slot map (:2343-2362): append `(11, SlotKind::Texture), (12, SlotKind::Texture), (13, SlotKind::Texture), (3, SlotKind::Buffer),` with a comment `// RT-R2 (RD6): hi_refl / refl history pair / gi_materials — the R1 slot-map incident class; signatures and slot maps change together.`

g. `impl ShadowRayTracer for MetalShadowRayTracer::accumulate_irradiance` (:2692): match the trait signature; bind the three textures and the buffer into the encoder EXACTLY as the existing texture/buffer binds there do (indices 11/12/13/3 matching the MSL).

h. Sweep for other implementors/callers: `rg -n "impl ShadowRayTracer" crates/` and `rg -n "accumulate_irradiance" crates/ --type rust` — update every implementor and call site to the new signature. If anything beyond the two named files matches, STOP and report (the blast radius is bigger than briefed).


# Current code

### crates/manifold-gpu/src/metal/raytrace.rs (excerpts — the full file is larger; your `find` strings must be exact and unique in the FULL file, so quote generously)

// --- lines 1415-1570 ---
// reprojection also rejects) — SVGF's standard disocclusion test. Every
// history channel is PING-PONGED (`*_read`/`*_write` are two distinct
// textures, swapped by the caller each frame): a single read_write texture
// would race, since one thread's write destination (`tid`) can be another
// thread's read source (`prev_tid`) within the same dispatch, with no
// ordering guarantee between compute threads.
kernel void accumulate_irradiance(
    constant AccumulateParams&           p                    [[buffer(1)]],
    // RT-T2-C (object motion): per-object world→prev-world delta
    // (`prev_model * inverse(model)`), indexed by the primary-hit object
    // id carried in `hi_normal.w`. Identity for a static object.
    constant float4x4*                   obj_motion           [[buffer(2)]],
    texture2d<float>                     hi_irr               [[texture(0)]],
    depth2d<float>                       depth_tex            [[texture(1)]],
    texture2d<float>                     hi_normal            [[texture(2)]],
    texture2d<float>                     history_read         [[texture(3)]],
    texture2d<float, access::write>      history_write        [[texture(4)]],
    texture2d<float>                     depth_history_read   [[texture(5)]],
    texture2d<float, access::write>      depth_history_write  [[texture(6)]],
    texture2d<float>                     normal_history_read  [[texture(7)]],
    texture2d<float, access::write>      normal_history_write [[texture(8)]],
    // RT-T1-D (BUG-312): per-texel luminance moments (r=mean, g=mean-of-
    // squares) — the SAME ping-pong-history discipline as the depth/
    // normal pairs above, feeding `atrous_filter`'s variance-adaptive luma
    // sigma (one-frame-lagged, like every other history read here).
    // `Rg32Float` (not `Rg16Float`): `moment2 - moment1*moment1` is a
    // difference of two close, similarly-scaled numbers — half-float's
    // ~3-decimal-digit precision would swallow variances at the 1e-4 to
    // 1e-5 scale this filter needs to resolve (catastrophic cancellation).
    texture2d<float>                     moments_read         [[texture(9)]],
    texture2d<float, access::write>      moments_write        [[texture(10)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.size.x || tid.y >= p.size.y) return;
    float4 cur = hi_irr.read(tid);
    float  cur_depth = depth_tex.read(tid, 0);
    float4 cur_n4 = hi_normal.read(tid);
    float3 cur_normal = cur_n4.xyz;
    float  cur_luma = luma(cur.xyz);

    if (p.reset != 0u) {
        history_write.write(cur, tid);
        depth_history_write.write(float4(cur_depth, 0, 0, 0), tid);
        normal_history_write.write(float4(cur_normal, 0), tid);
        moments_write.write(float4(cur_luma, cur_luma * cur_luma, 0, 0), tid);
        return;
    }

    // RT-T2-C: camera motion AND object motion. `wp` (this frame, world
    // space) is first carried back to where this OBJECT placed that
    // surface point last frame via `obj_motion` (world→prev-world,
    // identity for static objects; camera-only when the pixel has no
    // object id — void, or a shadow-only frame that cast no primary
    // ray), then reprojected through the previous camera. Without the
    // object term, a moving object's pixels failed the depth/normal test
    // below every frame and lost ALL temporal amortization mid-gesture —
    // visible shimmer until motion stopped (the residual BUG-320 left).
    bool valid = false;
    float3 blended = cur.xyz;
    float moment1 = cur_luma;
    float moment2 = cur_luma * cur_luma;
    if (cur_depth < 1.0 - 1e-6) {
        float2 uv = (float2(tid) + 0.5) / float2(p.size);
        float4 clip = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, cur_depth, 1.0);
        float4 wh = p.inv_view_proj * clip;
        float3 wp = wh.xyz / wh.w;
        // BUG-322: carry BOTH the position and the NORMAL into the
        // previous frame's object space. T2-C rotated only the position;
        // `normal_history` stores world-space normals, so on a ROTATING
        // object the stored normal is in last frame's orientation while
        // `cur_normal` is in this frame's. Comparing them raw makes the
        // validity test below fail by exactly the object's rotation —
        // history rejected every frame, back to raw 2-6 spp, i.e. the
        // shimmer. Curvature amplifies it (a rotating curved surface
        // shows a different normal per pixel per frame), which is why
        // Peter's DamagedHelmet shimmered while flat flowers did not,
        // and why an earlier TRANSLATION-only oracle saw nothing wrong:
        // translation leaves normals untouched.
        float3 cur_normal_prev = cur_normal;
        if (cur_n4.w >= 0.0) {
            uint oid = uint(cur_n4.w + 0.5);
            if (oid < p.obj_count) {
                float4x4 m = obj_motion[oid];
                wp = (m * float4(wp, 1.0)).xyz;
                // Rotation/scale block only — a normal is a direction, so
                // the translation column must not apply. Non-uniform
                // scale would strictly want the inverse-transpose, but
                // this matrix is `prev_model * inverse(model)`: for the
                // rigid and uniformly-scaled transforms scene objects
                // carry it is already a similarity, where the plain 3x3
                // preserves direction exactly. Normalized below, so any
                // uniform scale factor drops out.
                float3x3 r = float3x3(m[0].xyz, m[1].xyz, m[2].xyz);
                float3 n = r * cur_normal;
                float len = length(n);
                cur_normal_prev = len > 1e-6 ? n / len : cur_normal;
            }
        }

        float4 prev_clip = p.prev_view_proj * float4(wp, 1.0);
        if (prev_clip.w > 1e-6) {
            float3 prev_ndc = prev_clip.xyz / prev_clip.w;
            float2 prev_uv = float2(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
            if (all(prev_uv >= 0.0) && all(prev_uv <= 1.0) && prev_ndc.z >= 0.0 && prev_ndc.z <= 1.0) {
                int2 pt = clamp(int2(prev_uv * float2(p.size)), int2(0), int2(p.size) - 1);
                uint2 prev_tid = uint2(pt);
                float  stored_depth  = depth_history_read.read(prev_tid).r;
                float3 stored_normal = normal_history_read.read(prev_tid).xyz;
                // DEPTH_REJECT_THRESHOLD: raw NDC-z units — directly
                // comparable without linearizing (same discipline
                // `upsample_shadow`'s depth guide already uses). 5e-3
                // rejects a genuinely different surface/depth layer while
                // tolerating one shared surface's own NDC-z precision
                // noise across a single frame of camera motion.
                const float DEPTH_REJECT_THRESHOLD = 5e-3;
                // NORMAL_REJECT_COS_THRESHOLD: cosine of the angle between
                // the reprojected history's normal and THIS frame's normal
                // carried back into that frame's object orientation
                // (`cur_normal_prev`, BUG-322) — 0.9 (~26 degrees) rejects
                // a silhouette/edge texel whose reprojection lands on a
                // different face while tolerating the same surface's normal
                // drifting slightly under one frame of motion. Comparing in
                // ONE consistent orientation is what makes the threshold
                // mean "different surface" rather than "the object turned".
                const float NORMAL_REJECT_COS_THRESHOLD = 0.9;
                bool depth_ok = fabs(stored_depth - prev_ndc.z) < DEPTH_REJECT_THRESHOLD;
                bool normal_ok = dot(normalize(stored_normal), cur_normal_prev) > NORMAL_REJECT_COS_THRESHOLD;
                if (depth_ok && normal_ok) {
                    float4 hist = history_read.read(prev_tid);
                    blended = mix(hist.xyz, cur.xyz, p.alpha);
                    valid = true;
                    float2 stored_moments = moments_read.read(prev_tid).rg;
                    moment1 = mix(stored_moments.r, cur_luma, p.alpha);
                    moment2 = mix(stored_moments.g, cur_luma * cur_luma, p.alpha);
                }
            }
        }
    }
    history_write.write(valid ? float4(blended, 0) : cur, tid);
    depth_history_write.write(float4(cur_depth, 0, 0, 0), tid);
    normal_history_write.write(float4(cur_normal, 0), tid);
    moments_write.write(float4(moment1, moment2, 0, 0), tid);
}

// RT-T1-B value-level test surface ONLY (`docs/RAYTRACING_DESIGN.md` §8
// Tier-1 item 2's gate: "kernel-visible normal for a known 2-triangle
// fixture matches CPU expected"). Exercises the EXACT SAME
// `fetch_interpolated_normal` helper `trace_shadow_rays` calls internally,
// against caller-supplied instance/primitive/barycentric inputs — no ray
// tracing or RNG involved, so the interpolation math alone is under test,
// deterministically. Not part of the production dispatch path (never
// called by `render_scene.rs`) — see `manifold_gpu::raytrace::
// debug_fetch_interpolated_normal`, its only caller.
struct DebugFetchNormalParams {
    uint instance_id;
    uint primitive_id;
// --- lines 1700-1730 ---
    }
}

/// CPU mirror of the MSL `GiMaterial` struct — RT-P3's per-instance
/// emissive/albedo table for the GI gather's emissive-hit + sun-bounce
/// terms. Field order and packing MUST match exactly (P0 §5.1 kernel
/// lesson).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GiMaterial {
    pub albedo: [f32; 3],
    _pad0: f32,
    pub emissive: [f32; 3],
    _pad1: f32,
    /// RT-R1: x = metallic, y = roughness — read straight off
    /// `d.uniforms.pbr_metallic_roughness` (render_scene.rs:332), the SAME
    /// resolved factors `fs_pbr` shades with. z/w reserved.
    pub metallic_roughness: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<GiMaterial>() == 48);

impl GiMaterial {
    pub fn new(albedo: [f32; 3], emissive: [f32; 3], metallic_roughness: [f32; 4]) -> Self {
        Self {
            albedo,
            _pad0: 0.0,
            emissive,
            _pad1: 0.0,
            metallic_roughness,
        }
// --- lines 2195-2245 ---
    /// validating against `depth_history_read`/`normal_history_read`
    /// before trusting it (falls back to `hi_irr` alone on mismatch or
    /// disocclusion) — `params.reset` discards history outright (cold
    /// start / post-cut, driven by the SHARED `TemporalResetDetector` —
    /// RT-D2). Every history channel is a `(read, write)` PING-PONG PAIR:
    /// the caller must pass last frame's write-target as this frame's
    /// read-target and swap after the call — a single read_write texture
    /// would race (see the kernel's own doc comment).
    #[allow(clippy::too_many_arguments)]
    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        params_buffer: &GpuBuffer,
        // RT-T2-C: per-object world→prev-world motion matrices
        // (`params.obj_count` entries of column-major `[[f32; 4]; 4]`).
        obj_motion: &GpuBuffer,
        hi_irr: &GpuTexture,
        depth_tex: &GpuTexture,
        hi_normal: &GpuTexture,
        history_read: &GpuTexture,
        history_write: &GpuTexture,
        depth_history_read: &GpuTexture,
        depth_history_write: &GpuTexture,
        normal_history_read: &GpuTexture,
        normal_history_write: &GpuTexture,
        // RT-T1-D (BUG-312): per-texel luminance moments ping-pong pair —
        // see the `atrous_filter`/`accumulate_irradiance` MSL kernel doc
        // comments.
        moments_read: &GpuTexture,
        moments_write: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
pub struct MetalShadowRayTracer {
    trace_pipeline: GpuComputePipeline,
    upsample_pipeline: GpuComputePipeline,
    /// RT-T1-D (BUG-312): the dilated edge-aware à-trous filter pipeline.
    atrous_pipeline: GpuComputePipeline,
    accumulate_pipeline: GpuComputePipeline,
    /// RT-T1-B value-test-only surface (`debug_fetch_interpolated_normal`'s
    /// only caller) — see the MSL `debug_fetch_interpolated_normal` kernel's
    /// doc comment. Always compiled (tiny kernel, negligible cost); never
    /// dispatched by the production `render_scene.rs` path.
    debug_fetch_normal_pipeline: GpuComputePipeline,
    /// RT-T2-A: 1x1 fully-opaque texture bound into every one of
    /// `trace_shadow_rays`'s `alpha_textures` slots that this frame's
// --- lines 2335-2400 ---
                (6, SlotKind::Texture), // src_n
                (7, SlotKind::Texture), // dst_n
                // RT-R1 (§9.3): src_refl / dst_refl — see the trace
                // pipeline's slot-map note (T3 missed these too).
                (8, SlotKind::Texture),
                (9, SlotKind::Texture),
            ]),
        );
        let accumulate_pipeline = compile_pipeline(
            device,
            &library,
            "accumulate_irradiance",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (2, SlotKind::Buffer), // RT-T2-C: obj_motion, MSL [[buffer(2)]]
                (0, SlotKind::Texture), // RT-T1-C: hi_irr
                (1, SlotKind::Texture), // RT-T1-C: depth_tex
                (2, SlotKind::Texture), // RT-T1-C: hi_normal
                (3, SlotKind::Texture), // RT-T1-C: history_read
                (4, SlotKind::Texture), // RT-T1-C: history_write
                (5, SlotKind::Texture), // RT-T1-C: depth_history_read
                (6, SlotKind::Texture), // RT-T1-C: depth_history_write
                (7, SlotKind::Texture), // RT-T1-C: normal_history_read
                (8, SlotKind::Texture), // RT-T1-C: normal_history_write
                (9, SlotKind::Texture),  // RT-T1-D: moments_read
                (10, SlotKind::Texture), // RT-T1-D: moments_write
            ]),
        );
        let debug_fetch_normal_pipeline = compile_pipeline(
            device,
            &library,
            "debug_fetch_interpolated_normal",
            identity_slot_map(&[
                (0, SlotKind::Buffer),
                (1, SlotKind::Buffer),
                (2, SlotKind::Buffer),
            ]),
        );

        let dummy_alpha_tex = create_dummy_alpha_texture(device);

        Self {
            trace_pipeline,
            upsample_pipeline,
            atrous_pipeline,
            accumulate_pipeline,
            debug_fetch_normal_pipeline,
            dummy_alpha_tex,
        }
    }

    /// RT-T1-B value-test-only entry point (`docs/RAYTRACING_DESIGN.md` §8
    /// Tier-1 item 2's gate) — dispatches the SAME `fetch_interpolated_normal`
    /// MSL helper `trace_shadow_rays` uses internally, against caller-
    /// supplied `(instance_id, primitive_id, barycentric)` inputs, no ray
    /// tracing/RNG involved. Synchronous (commits and waits) — test-only
    /// call pattern, never used on a hot path.
    pub fn debug_fetch_interpolated_normal(
        &self,
        device: &GpuDevice,
        normal_sources: &GpuBuffer,
        instance_id: u32,
        primitive_id: u32,
        bary: [f32; 2],
    ) -> [f32; 3] {
        #[repr(C)]
// --- lines 2685-2760 ---
                },
            ],
            groups,
            label,
        );
    }

    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        params_buffer: &GpuBuffer,
        // RT-T2-C: per-object world→prev-world motion matrices
        // (`params.obj_count` entries of column-major `[[f32; 4]; 4]`).
        obj_motion: &GpuBuffer,
        hi_irr: &GpuTexture,
        depth_tex: &GpuTexture,
        hi_normal: &GpuTexture,
        history_read: &GpuTexture,
        history_write: &GpuTexture,
        depth_history_read: &GpuTexture,
        depth_history_write: &GpuTexture,
        normal_history_read: &GpuTexture,
        normal_history_write: &GpuTexture,
        moments_read: &GpuTexture,
        moments_write: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(accumulate_params_bytes(params));
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.accumulate_pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: params_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: obj_motion,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: hi_irr,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: hi_normal,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: history_read,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: history_write,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: depth_history_read,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: depth_history_write,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: normal_history_read,
                },
                GpuBinding::Texture {

### crates/manifold-renderer/src/node_graph/primitives/render_scene.rs (excerpts — the full file is larger; your `find` strings must be exact and unique in the FULL file, so quote generously)

// --- lines 805-840 ---
    /// RT-R1 (§9.3): full-res reflection scratch for à-trous ping-pong
    /// (mirrors `rt_irr_full_b`). Inert/bind-only until T5.
    rt_refl_full_b: Option<manifold_gpu::GpuTexture>,
    /// RT-T1-C (RAYTRACING_DESIGN.md §8 Tier-1 item 1, BUG-311): the
    /// temporally-accumulated demodulated irradiance, its per-pixel depth,
    /// and its per-pixel normal history, each a PING-PONG PAIR —
    /// `accumulate_irradiance`'s reprojection reads the PREVIOUS frame's
    /// write target as this frame's read source (a single read_write
    /// texture would race across threads within one dispatch — see the
    /// kernel's own doc comment). `rt_history_ping` selects which slot is
    /// currently the read side; flipped after every `accumulate_irradiance`
    /// call, reset or not.
    rt_irr_history: [Option<manifold_gpu::GpuTexture>; 2],
    rt_depth_history: [Option<manifold_gpu::GpuTexture>; 2],
    rt_normal_history: [Option<manifold_gpu::GpuTexture>; 2],
    /// RT-T1-D (BUG-312): per-texel luminance moments (mean, mean-of-
    /// squares) — the SAME ping-pong-history discipline as the three
    /// pairs above, indexed by the SAME `rt_history_ping`. Feeds
    /// `atrous_filter`'s variance-adaptive luma sigma.
    rt_moments_history: [Option<manifold_gpu::GpuTexture>; 2],
    /// Set once `accumulate_irradiance` has run at least once against the
    /// CURRENT `rt_moments_history` allocation — false right after
    /// `ensure_rt_irradiance` (re)allocates (fresh texture content is
    /// undefined), so `atrous_pass` knows not to read it that one frame.
    /// Independent of `rt_reset_detector`'s cut/strobe decision (a
    /// different question: "has this texture ever been written", not
    /// "should color history be discarded this frame").
    rt_moments_valid: bool,
    rt_history_ping: usize,
    /// RT-T1-C: current-frame half-res/full-res primary-hit vertex normal
    /// (same half/full lifecycle as `rt_irr_half`/`rt_irr_full` — produced
    /// fresh every RT-ready frame, not persistent history).
    rt_normal_half: Option<manifold_gpu::GpuTexture>,
    rt_normal_full: Option<manifold_gpu::GpuTexture>,
    /// RT-T1-D: second full-res scratch set for the à-trous filter's
    /// ping-pong between `upsample_shadow`'s output and each dilated
// --- lines 1055-1075 ---
            rt_normal_sources: None,
            rt_normal_sources_capacity: 0,
            rt_irr_half: None,
            rt_irr_full: None,
            rt_refl_half: None,
            rt_refl_full: None,
            rt_refl_full_b: None,
            rt_irr_history: [None, None],
            rt_depth_history: [None, None],
            rt_normal_history: [None, None],
            rt_moments_history: [None, None],
            rt_moments_valid: false,
            rt_history_ping: 0,
            rt_normal_half: None,
            rt_normal_full: None,
            rt_mask_full_b: None,
            rt_irr_full_b: None,
            rt_normal_full_b: None,
            rt_atrous_params_buffer: None,
            rt_irr_width: 0,
            rt_irr_height: 0,
// --- lines 1735-1815 ---
    /// texture, resized with the scene's own output resolution (mirrors
    /// `ensure_rt_masks`'s lifecycle). Returns `true` when the history
    /// texture was freshly (re)allocated this call — its content is
    /// undefined until the caller's next `accumulate_irradiance` call,
    /// which MUST pass `reset: true` in that case (a dimension change is
    /// itself a discontinuity, same as a cut).
    fn ensure_rt_irradiance(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) -> bool {
        if self.rt_irr_width == width && self.rt_irr_height == height && self.rt_irr_history[0].is_some() {
            return false;
        }
        let half_w = width.div_ceil(2).max(1);
        let half_h = height.div_ceil(2).max(1);
        let make = |w: u32, h: u32, format: manifold_gpu::GpuTextureFormat, label: &'static str| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label,
                mip_levels: 1,
            })
        };
        let rgba16 = manifold_gpu::GpuTextureFormat::Rgba16Float;
        self.rt_irr_half = Some(make(half_w, half_h, rgba16, "node.render_scene rt_irr_half (RT-P2)"));
        self.rt_irr_full = Some(make(width, height, rgba16, "node.render_scene rt_irr_full (RT-P2)"));
        // RT-R1 (§9.3): half-res reflection-radiance output — same lifecycle
        // as `rt_irr_half` (the dispatch writes it; T5's kernel is the writer;
        // inert/bind-only until then).
        self.rt_refl_half = Some(make(half_w, half_h, rgba16, "node.render_scene rt_refl_half (RT-R1)"));
        // RT-R1 (§9.3): full-res reflection-radiance output target & atrous
        // scratch (mirror `rt_irr_full`/`rt_irr_full_b`). Inert until T5.
        self.rt_refl_full = Some(make(width, height, rgba16, "node.render_scene rt_refl_full (RT-R1)"));
        self.rt_refl_full_b = Some(make(width, height, rgba16, "node.render_scene rt_refl_full_b (RT-R1 atrous)"));
        // RT-T1-C: current-frame primary-hit normal, same half/full
        // lifecycle as irradiance above (not persistent history).
        self.rt_normal_half = Some(make(half_w, half_h, rgba16, "node.render_scene rt_normal_half (RT-T1-C)"));
        self.rt_normal_full = Some(make(width, height, rgba16, "node.render_scene rt_normal_full (RT-T1-C)"));
        // RT-T1-D: second full-res scratch set for the à-trous filter's
        // ping-pong (same lifecycle as irradiance/normal above — not
        // persistent history, rewritten fresh every RT-ready frame).
        self.rt_irr_full_b = Some(make(width, height, rgba16, "node.render_scene rt_irr_full_b (RT-T1-D atrous)"));
        self.rt_normal_full_b = Some(make(width, height, rgba16, "node.render_scene rt_normal_full_b (RT-T1-D atrous)"));
        // RT-T1-C: ping-pong history pairs (irradiance, depth, normal) —
        // see this struct's field doc comment for why two textures each.
        self.rt_irr_history = [
            make(width, height, rgba16, "node.render_scene rt_irr_history_a (RT-T1-C)"),
            make(width, height, rgba16, "node.render_scene rt_irr_history_b (RT-T1-C)"),
        ]
        .map(Some);
        self.rt_depth_history = [
            make(width, height, manifold_gpu::GpuTextureFormat::R32Float, "node.render_scene rt_depth_history_a (RT-T1-C)"),
            make(width, height, manifold_gpu::GpuTextureFormat::R32Float, "node.render_scene rt_depth_history_b (RT-T1-C)"),
        ]
        .map(Some);
        self.rt_normal_history = [
            make(width, height, rgba16, "node.render_scene rt_normal_history_a (RT-T1-C)"),
            make(width, height, rgba16, "node.render_scene rt_normal_history_b (RT-T1-C)"),
        ]
        .map(Some);
        // RT-T1-D (BUG-312): luminance-moments ping-pong history — `Rg32Float`
        // (not `Rg16Float`) so `moment2 - moment1*moment1` doesn't collapse to
        // noise under half-float's ~3-decimal-digit precision at the 1e-4 to
        // 1e-5 variance scale this filter needs (see the MSL kernel's doc
        // comment for the full cancellation argument).
        self.rt_moments_history = [
            make(width, height, manifold_gpu::GpuTextureFormat::Rg32Float, "node.render_scene rt_moments_history_a (RT-T1-D)"),
            make(width, height, manifold_gpu::GpuTextureFormat::Rg32Float, "node.render_scene rt_moments_history_b (RT-T1-D)"),
        ]
        .map(Some);
        self.rt_moments_valid = false;
        self.rt_history_ping = 0;
        self.rt_irr_width = width;
        self.rt_irr_height = height;
        true
    }

    /// CPU-mapped `ShadowRayParams` upload buffer, allocated once and
    /// reused every frame (matches this file's `light_buffers` ring
// --- lines 4150-4320 ---
                let depth_tex = self.opaque_depth_snapshot.as_ref().expect("ensured above");
                let mask_half = self.rt_mask_half.as_ref().expect("ensured above");
                let mask_full = self.rt_mask_full.as_ref().expect("ensured above");
                let irr_half = self.rt_irr_half.as_ref().expect("ensured above");
                let irr_full = self.rt_irr_full.as_ref().expect("ensured above");
                let normal_half = self.rt_normal_half.as_ref().expect("ensured above");
                let normal_full = self.rt_normal_full.as_ref().expect("ensured above");
                let refl_half = self.rt_refl_half.as_ref().expect("ensured above");
                let refl_full = self.rt_refl_full.as_ref().expect("ensured above");
                let _refl_full_b = self.rt_refl_full_b.as_ref().expect("ensured above");
                tracer.dispatch_shadow_rays(
                    gpu.native_enc,
                    accel,
                    &params,
                    params_buffer,
                    gi_materials_buffer,
                    normal_sources_buffer,
                    &alpha_textures,
                    depth_tex,
                    mask_half,
                    irr_half,
                    normal_half,
                    refl_half,
                    // RT-R1 (§9.3 RD4): the env mip chain the reflection
                    // miss branch samples — dummy when the scene has no
                    // IBL chain (the miss then reads the same nothing the
                    // raster IBL would). dummy_texture is ensured upstream
                    // of evaluate's RT block (3491).
                    self.prefiltered_specular.as_ref().unwrap_or(
                        self.dummy_texture.as_ref().expect("ensured at 3491"),
                    ),
                    "node.render_scene RT-D3/RT-P2/RT-P3 trace_shadow_rays",
                );
                tracer.upsample_shadow(
                    gpu.native_enc,
                    params_buffer,
                    depth_tex,
                    mask_half,
                    mask_full,
                    irr_half,
                    irr_full,
                    normal_half,
                    normal_full,
                    refl_half,
                    refl_full,
                    "node.render_scene RT-D3/RT-P2 upsample_shadow",
                );

                // RT-T1-D (RAYTRACING_DESIGN.md §8 Tier-1 item 3, BUG-312):
                // ATROUS_ITERATIONS total spatial-filter passes on the RT
                // lighting signal — `upsample_shadow` above is pass 1 (the
                // half->full resample, now also normal-weighted); the two
                // dilated `atrous_pass` calls below are passes 2-3, steps
                // 1 then 2 (brief's committed range: 2-3 total). An EVEN
                // count of dilated passes (2 here) lands the final result
                // back in `mask_full`/`irr_full`/`normal_full` — the
                // buffers `accumulate_irradiance` below already reads — via
                // the `_full_b` scratch set, so no downstream rebinding is
                // needed.
                const ATROUS_ITERATIONS: u32 = 3;
                let read_idx = self.rt_history_ping;
                let write_idx = 1 - read_idx;
                let moments_read = self.rt_moments_history[read_idx].as_ref().expect("ensured above");
                let atrous_params_buffer = self.rt_atrous_params_buffer.as_ref().expect("ensured above");
                let mask_full_b = self.rt_mask_full_b.as_ref().expect("ensured above");
                let irr_full_b = self.rt_irr_full_b.as_ref().expect("ensured above");
                let normal_full_b = self.rt_normal_full_b.as_ref().expect("ensured above");
                let refl_full_b = self.rt_refl_full_b.as_ref().expect("ensured above");
                let history_valid = self.rt_moments_valid;
                for pass in 0..(ATROUS_ITERATIONS - 1) {
                    // T1-D: dilation starts at 2, not 1 — the AO/GI trace
                    // dispatch is HALF-res (D11), so every 2x2 block of
                    // full-res texels shares the identical raw noise
                    // sample; a step=1 tap frequently lands in the SAME
                    // block (an exact duplicate, not an independent noisy
                    // sample) and does nothing to reduce variance. step=2
                    // is the smallest offset guaranteed to cross into an
                    // adjacent (independently-sampled) half-res block.
                    let step = 2u32 << pass;
                    let (src_sv, src_irr, src_n, src_refl, dst_sv, dst_irr, dst_n, dst_refl) = if pass % 2 == 0 {
                        (mask_full, irr_full, normal_full, refl_full, mask_full_b, irr_full_b, normal_full_b, refl_full_b)
                    } else {
                        (mask_full_b, irr_full_b, normal_full_b, refl_full_b, mask_full, irr_full, normal_full, refl_full)
                    };
                    let atrous_params = manifold_gpu::raytrace::AtrousParams::new([width, height], step, history_valid);
                    tracer.atrous_pass(
                        gpu.native_enc,
                        &atrous_params,
                        atrous_params_buffer,
                        depth_tex,
                        moments_read,
                        src_sv,
                        dst_sv,
                        src_irr,
                        dst_irr,
                        src_n,
                        dst_n,
                        src_refl,
                        dst_refl,
                        "node.render_scene RT-T1-D atrous_pass",
                    );
                }

                // RAYTRACING_DESIGN.md §5.2 P2/D3, RT-D2: the ONE call site
                // deciding "discard temporal history this frame" for this
                // node's irradiance accumulator — ORs in a just-allocated
                // history texture (dimension change) rather than adding a
                // second reset path. `detect_reset` must run exactly once
                // per frame this accumulator advances (its own contract);
                // §8.2 D22 (T2-B) hoisted the actual call to
                // `reset_decision` above `will_rt_accumulate_this_frame`
                // gates identically to this `if rt_ready` branch (nested in
                // the same `rt_enabled && has_casters` block), so it's
                // `Some` here.
                let reset = reset_decision.expect("will_rt_accumulate_this_frame implies Some")
                    || std::mem::take(&mut self.rt_irr_needs_reset);
                // RT-T1-C: `prev_view_proj` is the SAME local captured
                // above (BUG-311) before `self.prev_view_proj` was
                // overwritten to this frame's `view_proj` — exactly what
                // MetalFX's own velocity pass reprojects with.
                let accumulate_params = manifold_gpu::raytrace::AccumulateParams::new(
                    [width, height],
                    IRRADIANCE_ACCUM_ALPHA,
                    reset,
                    shadow_caster_draws.len() as u32,
                    inv_view_proj,
                    prev_view_proj,
                );
                let accumulate_params_buffer =
                    self.rt_accumulate_params_buffer.as_ref().expect("ensured above");
                // RT-T1-C: ping-pong — read last frame's write slot (same
                // `read_idx`/`write_idx` the à-trous pass above already
                // used for `moments_read`), write the OTHER (stale-from-
                // two-frames-ago, about to be fully overwritten) slot, then
                // flip so next frame reads what was just written.
                let irr_history_read = self.rt_irr_history[read_idx].as_ref().expect("ensured above");
                let irr_history_write = self.rt_irr_history[write_idx].as_ref().expect("ensured above");
                let depth_history_read = self.rt_depth_history[read_idx].as_ref().expect("ensured above");
                let depth_history_write = self.rt_depth_history[write_idx].as_ref().expect("ensured above");
                let normal_history_read = self.rt_normal_history[read_idx].as_ref().expect("ensured above");
                let normal_history_write = self.rt_normal_history[write_idx].as_ref().expect("ensured above");
                let moments_write = self.rt_moments_history[write_idx].as_ref().expect("ensured above");
                tracer.accumulate_irradiance(
                    gpu.native_enc,
                    &accumulate_params,
                    accumulate_params_buffer,
                    obj_motion_buffer,
                    irr_full,
                    depth_tex,
                    normal_full,
                    irr_history_read,
                    irr_history_write,
                    depth_history_read,
                    depth_history_write,
                    normal_history_read,
                    normal_history_write,
                    moments_read,
                    moments_write,
                    "node.render_scene RT-P2/RT-T1-C/RT-T1-D accumulate_irradiance",
                );
                self.rt_history_ping = write_idx;
                self.rt_moments_valid = true;

                // RAYTRACING_DESIGN.md §5.2 P3 (D5, "emissive-colored
                // volumetric glow"): every emissive object becomes an
                // extra Point-mode entry in the SAME march light table
                // every Sun/Point light already populates — a real,
                // physically-motivated light source in the existing
                // march, not a separate glow pass. Position = the
                // object's model-matrix translation (same "translation as
                // interior stand-in" convention this file's Blend-group

Output the JSON ChangeSet now.
