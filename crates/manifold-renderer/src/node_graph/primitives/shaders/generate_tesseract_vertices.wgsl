// node.generate_tesseract_vertices — emit the 16 corner vertices of
// a 4D unit hypercube (tesseract) scaled to magnitude 0.25 (the
// curated wireframe-fits-on-screen default matching legacy
// generator_math::PROJ_SCALE) into an Array<Vec4Vertex>.
//
// Bit pattern: vertex i has coordinates ((±1, ±1, ±1, ±1) * 0.125) —
// the (±1) sign pattern follows (sign(i&1), sign(i&2), sign(i&4),
// sign(i&8)), and the 0.125 = 0.25 / 2.0 scaling normalises the
// max-magnitude corner (sqrt(4) = 2) to 0.25.
//
// Why bake the scale here: with vertices pre-normalised to 0.25,
// downstream node.project_4d's `proj_scale` defaults to 1.0 — outer-
// card Scale binds to it directly and gives "Scale 1.0 = default
// zoom" UX without a math node in the graph. Same pattern as
// node.wireframe_shape. Note: 4D perspective is non-linear in w, so
// this bake does not reproduce the legacy generator's projected
// pixels bit-exactly — accepted trade-off (legacy outer scale=1.0
// looked identical to PROJ_SCALE=0.25 within rounding).

struct Uniforms {
    capacity: u32,
    _pad0: u32,
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
    // 0.25 / 2.0 = 0.125 — bakes the screen-fit factor (matching
    // legacy PROJ_SCALE = 0.25) into the vertex magnitudes so the
    // downstream project_4d.proj_scale defaults cleanly to 1.0.
    let k = 0.125;
    let x = select(-1.0, 1.0, (i & 1u) != 0u) * k;
    let y = select(-1.0, 1.0, (i & 2u) != 0u) * k;
    let z = select(-1.0, 1.0, (i & 4u) != 0u) * k;
    let w = select(-1.0, 1.0, (i & 8u) != 0u) * k;
    dst[i].pos = vec4<f32>(x, y, z, w);
}
