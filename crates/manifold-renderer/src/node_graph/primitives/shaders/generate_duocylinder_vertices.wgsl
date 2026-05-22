// node.generate_duocylinder_vertices — emit a parametric 4D torus
// grid (duocylinder surface) scaled to magnitude 0.25 (the curated
// wireframe-fits-on-screen default matching legacy
// generator_math::PROJ_SCALE) into an Array<Vec4Vertex>.
//
// Parameterised by (u, v) ∈ [0, 2π)² with grid_size steps each, giving
// grid_size² vertices arranged in (u, v) row-major order: index =
// iu * grid_size + iv. Coordinates: (cos u, sin u, cos v, sin v)
// * (0.25 / sqrt(2)).
//
// Why bake the scale here: vertices come out at magnitude 0.25 already
// (every duocylinder point has |v| = sqrt(2) before the bake), so
// downstream node.project_4d's `proj_scale` defaults to 1.0 — outer-
// card Scale binds to it directly. Same pattern as
// node.wireframe_shape and node.generate_tesseract_vertices. Note:
// 4D perspective is non-linear in w, so this bake does not reproduce
// the legacy generator's projected pixels bit-exactly.

struct Uniforms {
    grid_size: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Vec4Vertex {
    pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> dst: array<Vec4Vertex>;

const TAU: f32 = 6.28318530717958647692;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let iu = gid.x;
    let iv = gid.y;
    let n = u.grid_size;
    if iu >= n || iv >= n {
        return;
    }
    let idx = iu * n + iv;
    if idx >= u.capacity {
        return;
    }
    let step = TAU / f32(n);
    let uu = f32(iu) * step;
    let vv = f32(iv) * step;
    // 0.25 / sqrt(2) ≈ 0.176776695 — every duocylinder vertex has
    // 4D magnitude sqrt(2), so this normalises to a 0.25 sphere.
    let k = 0.176776695;
    dst[idx].pos = vec4<f32>(cos(uu) * k, sin(uu) * k, cos(vv) * k, sin(vv) * k);
}
