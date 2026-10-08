// node.hypercube_vertices — emit the 16 corner vertices of a 4D
// hypercube into an Array<Vec4Vertex>, with a continuous `dimension`
// control that collapses the higher axes toward zero so the shape
// morphs point → line → square → cube → tesseract as `dimension`
// ramps 1 → 4.
//
// Vertex i = (sign(i&1), sign(i&2), sign(i&4), sign(i&8)) * 0.125
//            * clamp(dimension - axis, 0, 1)   for axis x=0,y=1,z=2,w=3.
//
// The 0.125 = 0.25 / 2 bake normalises the max-magnitude corner
// (sqrt(4) = 2) to magnitude 0.25 — the legacy PROJ_SCALE screen-fit
// factor — so downstream node.project_4d's `proj_scale` defaults to
// 1.0 ("Scale 1.0 = default zoom") with no graph-side math node. At
// dimension = 4 every axis is fully present, so the output is bit-
// identical to the legacy generate_tesseract_vertices bake.

struct Uniforms {
    capacity: u32,
    dimension: f32,
    _pad1: u32,
    _pad2: u32,
};

struct Vec4Vertex {
    pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> dst: array<Vec4Vertex>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }
    if i >= 16u {
        dst[i].pos = vec4<f32>(0.0);
        return;
    }
    let k = 0.125;
    let sx = select(-1.0, 1.0, (i & 1u) != 0u);
    let sy = select(-1.0, 1.0, (i & 2u) != 0u);
    let sz = select(-1.0, 1.0, (i & 4u) != 0u);
    let sw = select(-1.0, 1.0, (i & 8u) != 0u);
    // present-fraction per axis: clamp(dimension - axisIndex, 0, 1).
    let px = clamp(u.dimension - 0.0, 0.0, 1.0);
    let py = clamp(u.dimension - 1.0, 0.0, 1.0);
    let pz = clamp(u.dimension - 2.0, 0.0, 1.0);
    let pw = clamp(u.dimension - 3.0, 0.0, 1.0);
    dst[i].pos = vec4<f32>(sx * k * px, sy * k * py, sz * k * pz, sw * k * pw);
}
