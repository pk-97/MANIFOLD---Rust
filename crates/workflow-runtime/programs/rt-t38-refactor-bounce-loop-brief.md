You are a mechanical code-change generator. Read the brief and the current file, then output ONLY a JSON object, no prose, no markdown fences:

{"edits": [{"path": "<repo-relative path>", "find": "<exact text currently in the file>", "replace": "<replacement text>"}, ...],
 "writes": [],
 "commit_message": "<one line>"}

Rules:
- `find` must be copied EXACTLY from the current file below (whitespace included) and must be unique in the whole file — include enough surrounding lines to be unique.
- Make every edit this brief demands, nothing else.
- The one file you touch: `crates/manifold-gpu/src/metal/raytrace.rs`. All edits land inside the `SHADOW_RAYS_MSL` string constant (Metal Shading Language, not Rust).

# Brief: MB-A — GI bounce-loop refactor, behavior-identical (RAYTRACING_DESIGN.md section 11.4)

Three changes inside the MSL. The refactor MUST be byte-identical in output: a machine
byte-diff of a rendered scene gates this commit. That means every random-number stream
must be preserved exactly — same functions, same seeds, same order.

## Edit 1 — hoist `SUN_BOUNCE_INTENSITY_SCALE` to MSL file scope and add the shared helper

The kernel currently declares, at kernel scope (inside `trace_shadow_rays`):

    const float SUN_BOUNCE_INTENSITY_SCALE = 0.08;

Delete that kernel-scope declaration (keep its explanatory comment block where it is,
attached to the GI gather). Add at MSL FILE scope, immediately AFTER the closing brace of
`walk_with_alpha_test` and before the next item:

    // Multi-bounce GI MB2 (RAYTRACING_DESIGN.md section 11): ONE home for the
    // sun-bounce caster loop (invariant I-MB3) — called by the GI gather at
    // every path vertex and by the reflection block's hit shading.
    // `seed_base` preserves each call site's historical rand2 stream exactly
    // (load-bearing for I-MB1's byte identity). Folds the diffuse BRDF's
    // 1/pi via SUN_BOUNCE_INTENSITY_SCALE, named + tunable (0.02-0.3).
    constant float SUN_BOUNCE_INTENSITY_SCALE = 0.08;

    static float3 sun_bounce_at_hit(
        instance_acceleration_structure accel,
        device RtNormalSource* normal_sources,
        array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> material_textures,
        constant ShadowRayParams& p,
        uint n_casters,
        float3 hit_pos,
        float3 hit_n,
        float3 hit_albedo,
        float bias_eps,
        uint2 tid,
        uint seed_base)
    {
        float3 term = float3(0.0);
        for (uint sc = 0; sc < n_casters; sc++) {
            RtCasterParams sun_cst = p.casters[sc];
            if (sun_cst.kind != 0u) continue;
            float3 sdir = float3(sun_cst.dir_or_pos);
            ray sun_r;
            sun_r.origin = hit_pos + sdir * bias_eps;
            sun_r.direction = cone_sample(sdir, sun_cst.cone_or_size, rand2(tid, p.frame_index, seed_base + sc));
            sun_r.min_distance = bias_eps * 0.5;
            sun_r.max_distance = INFINITY;
            intersection_query<triangle_data, instancing> sun_q;
            sun_q.reset(sun_r, accel, RT_MASK_SHADOW_CASTER);
            float hit_sun_vis = walk_with_alpha_test(sun_q, normal_sources, material_textures, true) ? 0.0 : 1.0;
            float hit_ndotl = max(dot(hit_n, sdir), 0.0);
            term += hit_albedo * float3(sun_cst.color) * hit_sun_vis * hit_ndotl * SUN_BOUNCE_INTENSITY_SCALE;
        }
        return term;
    }

IMPORTANT: check the kernel's own parameter list for the exact spelling of the
acceleration-structure type (the `accel` parameter of `trace_shadow_rays`) and use that
same type for the helper's `accel` parameter. Same for `p`'s type. Copy the
`normal_sources` / `material_textures` parameter types from `walk_with_alpha_test`'s
signature verbatim.

## Edit 2 — restructure the GI gather into a bounce loop with throughput

Replace the whole GI gather block — from `float3 gi = float3(0.0);` through `gi /=
float(p.gi_spp);` inclusive, INCLUDING its interior sun-bounce loop (now the helper's
job) — with:

    float3 gi = float3(0.0);
    // MB4 (RAYTRACING_DESIGN.md section 11.2): fixed path depth + per-extension
    // energy fold. MB-A ships the loop at depth 1 (byte-identical to the
    // pre-loop gather); MB-B raises the depth to 2. Range 1-3.
    const uint RT_GI_MAX_BOUNCES = 1u;
    // ~1/pi, range 0.1-0.5. Consumed only when RT_GI_MAX_BOUNCES > 1: each
    // path extension multiplies throughput by the intermediate surface's
    // albedo times this fold (MB5 — the primary surface stays demodulated,
    // D3 discipline; carried intermediate albedo IS the colour bleed).
    const float RT_GI_THROUGHPUT_FOLD = 0.318;
    if (p.gi_spp > 0) {
        for (uint s = 0; s < p.gi_spp; s++) {
            ray gr;
            gr.origin = sec_origin;
            gr.min_distance = bias_eps * 0.5;
            gr.max_distance = INFINITY;
            gr.direction = cosine_hemisphere(n, blue_noise_sample(tid, p.frame_index, s, p.gi_spp));
            float3 throughput = float3(1.0);
            for (uint bounce = 0u; bounce < RT_GI_MAX_BOUNCES; bounce++) {
                intersection_query<triangle_data, instancing> gi_q;
                gi_q.reset(gr, accel, RT_MASK_VISIBLE);
                if (!walk_with_alpha_test(gi_q, normal_sources, material_textures, false)) { break; }
                uint oi = gi_q.get_committed_instance_id();
                uint gi_pid = gi_q.get_committed_primitive_id();
                float2 gi_bary = gi_q.get_committed_triangle_barycentric_coord();
                float gi_dist = gi_q.get_committed_distance();
                float3 hit_emissive = float3(gi_materials[oi].emissive);
                float3 hit_albedo = float3(gi_materials[oi].albedo);
                float3 hit_pos = gr.origin + gr.direction * gi_dist;
                float3 hit_n = fetch_interpolated_normal(normal_sources, oi, gi_pid, gi_bary);
                float3 bounce_term = sun_bounce_at_hit(
                    accel, normal_sources, material_textures, p, n_casters,
                    hit_pos, hit_n, hit_albedo, bias_eps, tid,
                    400u + s * MAX_RT_CASTERS);
                gi += throughput * (hit_emissive + bounce_term);
                if (bounce + 1u < RT_GI_MAX_BOUNCES) {
                    throughput *= hit_albedo * RT_GI_THROUGHPUT_FOLD;
                    gr.origin = hit_pos + hit_n * bias_eps;
                    // Extension directions use the plain hash stream (seed
                    // base 600u), NOT blue_noise_sample — the blue-noise
                    // sequence is budgeted per first-bounce sample index.
                    gr.direction = cosine_hemisphere(hit_n, rand2(tid, p.frame_index, 600u + s * MAX_RT_CASTERS + bounce));
                }
            }
        }
        gi /= float(p.gi_spp);
    }

Keep the existing explanatory comment block that sits above the current GI gather (the
one describing "one-bounce GI gather ... emissive + sun-bounce") — leave it in place
above the replacement, and keep the comment block that documented
SUN_BOUNCE_INTENSITY_SCALE's tuning range with the GI gather if it is attached there.
Preserve the seed expression `400u + s * MAX_RT_CASTERS` exactly: with the helper adding
`+ sc` per caster this reproduces the original `400u + s * MAX_RT_CASTERS + sc` stream.

## Edit 3 — the reflection block's sun-bounce loop calls the helper

In the reflection hit-shading block (the one computing `sun_bounce_term` with its own
`for (uint sc = 0; sc < n_casters; sc++)` caster loop using seed `500u + sc`), replace
the ENTIRE loop — from `float3 sun_bounce_term = float3(0.0);` through its closing brace
— with:

    float3 sun_bounce_term = sun_bounce_at_hit(
        accel, normal_sources, material_textures, p, n_casters,
        hit_pos, hit_n, hit_albedo, bias_eps, tid, 500u);

(`hit_pos`, `hit_n`, `hit_albedo` already exist in that block; seed base 500u + the
helper's `+ sc` reproduces the original `500u + sc` stream.)

Do NOT touch anything else in the reflection block, the shadow/AO blocks, the ambient
plumbing, or any Rust code.

commit_message: "MB-A: GI gather bounce loop + shared sun_bounce_at_hit helper, depth 1 (RAYTRACING_DESIGN.md section 11, I-MB1/I-MB3)"

# Current file

{{file:crates/manifold-gpu/src/metal/raytrace.rs}}
