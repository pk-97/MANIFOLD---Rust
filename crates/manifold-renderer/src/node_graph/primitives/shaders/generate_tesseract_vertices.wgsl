// node.generate_tesseract_vertices — emit the 16 corner vertices of
// a 4D unit hypercube (tesseract) into an Array<Vec4Vertex>.
//
// Bit pattern: vertex i has coordinates (sign(i&1), sign(i&2),
// sign(i&4), sign(i&8)). Matches the legacy generators/tesseract.rs
// CPU code (TesseractGenerator::new). Downstream consumers
// (rotate_4d → project_4d) operate on Vec4Vertex; edge connectivity
// for wireframe rendering is the consumer's concern.

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
    let x = select(-1.0, 1.0, (i & 1u) != 0u);
    let y = select(-1.0, 1.0, (i & 2u) != 0u);
    let z = select(-1.0, 1.0, (i & 4u) != 0u);
    let w = select(-1.0, 1.0, (i & 8u) != 0u);
    dst[i].pos = vec4<f32>(x, y, z, w);
}
