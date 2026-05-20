// node.generate_duocylinder_vertices — emit a parametric 4D torus
// grid (duocylinder surface) into an Array<Vec4Vertex>.
//
// Parameterised by (u, v) ∈ [0, 2π)² with grid_size steps each, giving
// grid_size² vertices arranged in (u, v) row-major order: index =
// iu * grid_size + iv. Coordinates: (cos u, sin u, cos v, sin v).
// Matches legacy generators/duocylinder.rs CPU code bit-for-bit.
//
// Downstream consumers (rotate_4d → project_4d) handle the rest of
// the pipeline; edge connectivity (neighbors in u and v) is the
// consumer's concern.

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
    dst[idx].pos = vec4<f32>(cos(uu), sin(uu), cos(vv), sin(vv));
}
