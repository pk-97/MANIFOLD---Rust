// RT P0 prototype — lighting trace kernels. Fable-authored core; harness must
// match the binding tables in BRIEF.md exactly. Not product code.
#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;

// Rust mirror: #[repr(C)], fields in this exact order. 16-byte aligned rows.
struct TraceParams {
    float3 sun_dir;        // normalized, points FROM surface TOWARD sun
    float  sun_cone;       // cone half-angle radians; 0.0 = hard shadows (mode A)
    float3 sun_color;      // linear HDR
    float  ao_radius;      // world units
    float3 env_zenith;     // linear env gradient, straight up
    uint   shadow_spp;
    float3 env_horizon;
    uint   ao_spp;
    uint   gi_spp;         // 0 = no GI (mode A): combine uses flat env ambient
    uint   frame_index;
    uint2  trace_size;     // lighting-trace resolution (== gbuffer size for A/C, half for B)
    uint2  gbuffer_size;   // raster G-buffer resolution
    uint   _pad0, _pad1;
};

struct Material {          // one entry per material; indexed via mat_index[primitive_id]
    float3 albedo;  float _p0;
    float3 emissive; float _p1;   // linear HDR, premultiplied by intensity
};

// ---------- sampling helpers ----------
static uint pcg(uint v) { v = v * 747796405u + 2891336453u; v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u; return (v >> 22u) ^ v; }
static float2 rand2(uint2 p, uint frame, uint ray) {
    uint s = pcg(p.x + pcg(p.y + pcg(frame * 61u + ray)));
    uint t = pcg(s);
    return float2((s & 0xFFFFFFu) / 16777216.0, (t & 0xFFFFFFu) / 16777216.0);
}
static float3 ortho_basis_x(float3 n) {
    return normalize(fabs(n.x) > 0.9 ? cross(n, float3(0, 1, 0)) : cross(n, float3(1, 0, 0)));
}
static float3 cosine_hemisphere(float3 n, float2 u) {
    float3 t = ortho_basis_x(n), b = cross(n, t);
    float r = sqrt(u.x), phi = 6.2831853 * u.y;
    return normalize(t * (r * cos(phi)) + b * (r * sin(phi)) + n * sqrt(max(0.0, 1.0 - u.x)));
}
static float3 cone_sample(float3 dir, float half_angle, float2 u) {
    if (half_angle <= 0.0) return dir;
    float cos_t = mix(1.0, cos(half_angle), u.x);
    float sin_t = sqrt(max(0.0, 1.0 - cos_t * cos_t));
    float phi = 6.2831853 * u.y;
    float3 t = ortho_basis_x(dir), b = cross(dir, t);
    return normalize(t * (sin_t * cos(phi)) + b * (sin_t * sin(phi)) + dir * cos_t);
}
static float3 env_color(float3 d, constant TraceParams& p) {
    return mix(p.env_horizon, p.env_zenith, saturate(d.y * 0.5 + 0.5));
}

// ---------- lighting trace ----------
// Dispatch: trace_size grid. G-buffer sampled at the matching full-res texel.
// Outputs (trace_size):
//   out_sv  rg16f  : r = sun visibility [0,1], g = AO [0,1]
//   out_gi  rgba16f: rgb = demodulated incident irradiance (env+emissive), a = trace depth (view dist, for bilateral)
kernel void trace_lighting(
    primitive_acceleration_structure accel   [[buffer(0)]],
    constant TraceParams&            p       [[buffer(1)]],
    constant Material*               mats    [[buffer(2)]],
    constant uint*                   mat_index [[buffer(3)]],   // per primitive_id
    texture2d<float>                 g_wpos  [[texture(0)]],    // rgba32f: xyz world pos, w = view dist (0 = void)
    texture2d<float>                 g_nrm   [[texture(1)]],    // rgba16f: xyz world normal
    texture2d<float, access::write>  out_sv  [[texture(2)]],
    texture2d<float, access::write>  out_gi  [[texture(3)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size));
    gpix = min(gpix, p.gbuffer_size - 1);

    float4 wp = g_wpos.read(gpix);
    if (wp.w <= 0.0) {   // void background
        out_sv.write(float4(1, 1, 0, 0), tid);
        out_gi.write(float4(0, 0, 0, 0), tid);
        return;
    }
    float3 n = normalize(g_nrm.read(gpix).xyz);
    float3 origin = wp.xyz + n * 1e-3;

    intersector<triangle_data> shadow_i;
    shadow_i.assume_geometry_type(geometry_type::triangle);
    shadow_i.force_opacity(forced_opacity::opaque);
    shadow_i.accept_any_intersection(true);

    ray r;
    r.origin = origin;
    r.min_distance = 0.0;

    // Sun visibility
    float vis = 1.0;
    if (p.shadow_spp > 0) {
        vis = 0.0;
        r.max_distance = INFINITY;
        for (uint s = 0; s < p.shadow_spp; s++) {
            r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, s));
            if (shadow_i.intersect(r, accel).type == intersection_type::none) vis += 1.0;
        }
        vis /= float(p.shadow_spp);
    }

    // AO
    float ao = 1.0;
    if (p.ao_spp > 0) {
        ao = 0.0;
        r.max_distance = p.ao_radius;
        for (uint s = 0; s < p.ao_spp; s++) {
            r.direction = cosine_hemisphere(n, rand2(tid, p.frame_index, 100u + s));
            if (shadow_i.intersect(r, accel).type == intersection_type::none) ao += 1.0;
        }
        ao /= float(p.ao_spp);
    }

    // One-bounce gather: emissive on hit, env on miss. Demodulated (no local albedo).
    float3 irr = float3(0.0);
    if (p.gi_spp > 0) {
        intersector<triangle_data> gi_i;
        gi_i.assume_geometry_type(geometry_type::triangle);
        gi_i.force_opacity(forced_opacity::opaque);
        r.max_distance = INFINITY;
        for (uint s = 0; s < p.gi_spp; s++) {
            r.direction = cosine_hemisphere(n, rand2(tid, p.frame_index, 200u + s));
            auto hit = gi_i.intersect(r, accel);
            if (hit.type == intersection_type::none) {
                irr += env_color(r.direction, p);
            } else {
                irr += mats[mat_index[hit.primitive_id]].emissive;
            }
        }
        irr /= float(p.gi_spp);   // cosine-weighted estimator: pdf cancels n·l and 1/pi
    }
    out_sv.write(float4(vis, ao, 0, 0), tid);
    out_gi.write(float4(irr, wp.w), tid);
}

// ---------- joint bilateral upsample (mode B: half-res lighting -> full res) ----------
// Dispatch: gbuffer_size grid. Guides: full-res depth (g_wpos.w) + normal vs
// trace-res depth stored in out_gi.a. 3x3 tap over low-res neighborhood.
kernel void upsample_lighting(
    constant TraceParams&           p       [[buffer(1)]],
    texture2d<float>                g_wpos  [[texture(0)]],
    texture2d<float>                g_nrm   [[texture(1)]],
    texture2d<float>                lo_sv   [[texture(2)]],
    texture2d<float>                lo_gi   [[texture(3)]],
    texture2d<float, access::write> hi_sv   [[texture(4)]],
    texture2d<float, access::write> hi_gi   [[texture(5)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float4 wp = g_wpos.read(tid);
    if (wp.w <= 0.0) { hi_sv.write(float4(1,1,0,0), tid); hi_gi.write(float4(0), tid); return; }
    float3 n = normalize(g_nrm.read(tid).xyz);

    float2 lo_uv = (float2(tid) + 0.5) / float2(p.gbuffer_size) * float2(p.trace_size);
    int2 lo_c = int2(lo_uv - 0.5);
    float2 acc_sv = 0.0; float3 acc_gi = 0.0; float wsum = 0.0;
    for (int dy = 0; dy <= 1; dy++)
    for (int dx = 0; dx <= 1; dx++) {
        int2 q = clamp(lo_c + int2(dx, dy), int2(0), int2(p.trace_size) - 1);
        float4 gi = lo_gi.read(uint2(q));
        float2 f = saturate(1.0 - fabs(lo_uv - 0.5 - float2(q)));
        float w_bilin = f.x * f.y;
        float w_depth = exp(-fabs(gi.a - wp.w) / max(wp.w * 0.02, 1e-4));
        uint2 gq = uint2((float2(q) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size));
        float w_nrm = pow(saturate(dot(n, normalize(g_nrm.read(gq).xyz))), 8.0);
        float w = max(w_bilin * w_depth * w_nrm, 1e-5);
        acc_sv += lo_sv.read(uint2(q)).rg * w;
        acc_gi += gi.rgb * w;
        wsum += w;
    }
    hi_sv.write(float4(acc_sv / wsum, 0, 0), tid);
    hi_gi.write(float4(acc_gi / wsum, 0), tid);
}

// ---------- shade + combine (full res) ----------
// color = albedo * (sun_ndotl * sun_color * vis + irradiance * ao) + GGX spec * vis
// gi_spp==0 (mode A): irradiance := flat env ambient. Output linear HDR rgba16f;
// tonemap happens in the PNG writer (harness).
kernel void shade_combine(
    constant TraceParams&           p        [[buffer(1)]],
    texture2d<float>                g_wpos   [[texture(0)]],
    texture2d<float>                g_nrm    [[texture(1)]],
    texture2d<float>                g_alb    [[texture(2)]],   // rgba8 srgb-read or linear rgba16f
    texture2d<float>                g_mat    [[texture(3)]],   // r = metallic, g = roughness
    texture2d<float>                sv       [[texture(4)]],
    texture2d<float>                gi       [[texture(5)]],
    texture2d<float, access::write> out_hdr  [[texture(6)]],
    constant float3&                cam_pos  [[buffer(2)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float4 wp = g_wpos.read(tid);
    if (wp.w <= 0.0) {
        float3 bg = env_color(float3(0, 0, 1), p) * 0.02;   // near-void background
        out_hdr.write(float4(bg, 1), tid);
        return;
    }
    float3 n   = normalize(g_nrm.read(tid).xyz);
    float3 alb = g_alb.read(tid).rgb;
    float2 mr  = g_mat.read(tid).rg;
    float  metallic = mr.r, rough = max(mr.g, 0.05);
    float2 vis_ao = sv.read(tid).rg;
    float3 irr = (p.gi_spp > 0) ? gi.read(tid).rgb
                                : mix(p.env_horizon, p.env_zenith, 0.5) * 0.5;

    float3 v = normalize(cam_pos - wp.xyz);
    float3 l = p.sun_dir;
    float3 h = normalize(v + l);
    float ndl = saturate(dot(n, l)), ndv = saturate(dot(n, v));
    float ndh = saturate(dot(n, h)), vdh = saturate(dot(v, h));

    // GGX + Smith + Schlick, standard metallic-roughness
    float a2 = rough * rough * rough * rough;
    float d = a2 / max(3.14159265 * pow(ndh * ndh * (a2 - 1.0) + 1.0, 2.0), 1e-6);
    float k = (rough + 1.0) * (rough + 1.0) / 8.0;
    float g = (ndv / (ndv * (1.0 - k) + k)) * (ndl / (ndl * (1.0 - k) + k));
    float3 f0 = mix(float3(0.04), alb, metallic);
    float3 f = f0 + (1.0 - f0) * pow(1.0 - vdh, 5.0);
    float3 spec = d * g * f / max(4.0 * ndv * ndl, 1e-4);
    float3 kd = (1.0 - f) * (1.0 - metallic);

    float3 color = (kd * alb / 3.14159265 + spec) * p.sun_color * ndl * vis_ao.r
                 + alb * irr * vis_ao.g;
    out_hdr.write(float4(color, 1), tid);
}
