// The foam pass: the water's foam particles drawn over the march's hits into
// the foam mask `foam_mask.wgsl` reads, one texel a render pixel (composed
// after `water_field_common.wgsl`'s `FieldHit`). Each live particle (w its
// fade, 0 an empty slot) stands for a disc of foam FOAM_RADIUS times its fade
// in radius lying on the surface; at every water pixel within reach the mask
// sums the discs' area per unit area, each spread by the 2D cubic-spline
// kernel (Monaghan 1992) of smoothing length `s` (blend add). Discs placed
// independently cover 1 - exp(-tau) of the surface, tau that sum times the
// Foam Coverage, which the read side takes. `s` is FOAM_SMOOTHING, or
// FOAM_PIXELS pixels where a pixel on the water is wider, so foam is as fine
// as the particles up close and never finer than a pixel far off.
//
// A particle is a screen-aligned square of half side 2s round its centre; a
// pixel takes it only when the hit there is this water's and lies within 2s
// of the centre in the water's object space, so foam stays on the surface it
// floats on and never shows on a surface in front of or behind it.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // The water object's model matrix.
    model: mat4x4<f32>,
    // Clip space to the water's object space: its inverse model times the
    // camera's inverse view-projection.
    clip_to_local: mat4x4<f32>,
    // x width (the hits buffer's row length), y height, z this water's slot
    // + 1 as the hits name it, w the particle slots.
    frame: vec4<f32>,
    // Metres per pixel per metre of depth (perspective) in x, flat
    // (orthographic) in y; z the Foam Coverage; w 0.
    footprint: vec4<f32>,
};

struct Particle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> hits: array<FieldHit>;

const FP_PI: f32 = 3.14159265358979;
// A foam particle at full fade is a disc of foam 5 mm in radius.
const FOAM_RADIUS: f32 = 0.005;
// The kernel's smoothing length up close, metres: the size of the lacework
// foam particles draw.
const FOAM_SMOOTHING: f32 = 0.02;
// The least smoothing length in pixels, so distant foam does not alias.
const FOAM_PIXELS: f32 = 1.5;

struct FoamOut {
    @builtin(position) position: vec4<f32>,
    // The particle's centre in object space and its smoothing length.
    @location(0) @interpolate(flat) centre: vec3<f32>,
    @location(1) @interpolate(flat) smoothing: f32,
    // Its disc's area, pi r^2.
    @location(2) @interpolate(flat) area: f32,
};

// The cubic spline f(q) of the kernel, 0 from q = 2.
fn fp_spline(q: f32) -> f32 {
    if q < 1.0 {
        return 1.0 - 1.5 * q * q + 0.75 * q * q * q;
    }
    if q < 2.0 {
        let t = 2.0 - q;
        return 0.25 * t * t * t;
    }
    return 0.0;
}

@vertex
fn vs_foam(@builtin(vertex_index) vid: u32) -> FoamOut {
    var out: FoamOut;
    // A square at one point covers no pixel.
    out.position = vec4<f32>(0.0);
    let slot = vid / 6u;
    if slot >= u32(u.frame.w) {
        return out;
    }
    let p = particles[slot].position_radius;
    if !(p.w > 0.0) {
        return out;
    }
    let clip = u.view_proj * (u.model * vec4<f32>(p.xyz, 1.0));
    // Behind the camera.
    if !(clip.w > 0.0) {
        return out;
    }
    // A pixel's width on the water at the particle's depth.
    let pixel = u.footprint.x * clip.w + u.footprint.y;
    let s = max(FOAM_SMOOTHING, FOAM_PIXELS * pixel);
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    // Half side 2s, in pixels, then in clip space at the centre's depth.
    let half = 2.0 * s / pixel;
    let ndc = corners[vid % 6u] * half * 2.0 / u.frame.xy;
    out.position = clip + vec4<f32>(ndc * clip.w, 0.0, 0.0);
    out.centre = p.xyz;
    out.smoothing = s;
    let r = FOAM_RADIUS * min(p.w, 1.0);
    out.area = FP_PI * r * r;
    return out;
}

@fragment
fn fs_foam(in: FoamOut) -> @location(0) f32 {
    let pixel = vec2<u32>(in.position.xy);
    let hit = hits[pixel.y * u32(u.frame.x) + pixel.x];
    if hit.depth <= 0.0 || u32(round(unpack2x16float(hit.thickness_slot).y)) != u32(u.frame.z) {
        discard;
    }
    let uv = in.position.xy / u.frame.xy;
    let h = u.clip_to_local * vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), hit.depth, 1.0);
    let d = distance(h.xyz / h.w, in.centre);
    let s = in.smoothing;
    if d >= 2.0 * s {
        discard;
    }
    return u.footprint.z * in.area * 10.0 / (7.0 * FP_PI * s * s) * fp_spline(d / s);
}
