// A liquid frame's face grid (liquid::grid::PublishedFaces, used by
// node.liquid_frame and node.matter_frame): publish one axis of a tick's
// faces (LIQUID_SOLVER_SEAM_DESIGN.md section 3.2), one thread per face. A
// tick whose stats flag a non-finite record (word 0) or narrow-band capacity
// shortage (when this producer supplies the extended stats) is never
// published: the faces keep the previous tick's,
// as the particle frame does.
// The interior sentinel/capacity seam follows Ferstl et al. (2016).

struct FaceParams {
    len: u32,
    _pad0: u32,
    _pad1: u32,
    has_shortage: u32,
}

@group(0) @binding(0) var<uniform> params: FaceParams;
@group(0) @binding(1) var<storage, read> source: array<f32>;
@group(0) @binding(2) var<storage, read> stats: array<u32>;
@group(0) @binding(3) var<storage, read_write> faces: array<f32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.len || stats[0] != 0u || (params.has_shortage != 0u && stats[NARROW_BAND_SHORTAGE_WORD] != 0u) {
        return;
    }
    faces[i] = source[i];
}

@compute @workgroup_size(256)
fn interior_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i < params.len {
        faces[i] = bitcast<f32>(0x7f7fffffu);
    }
}
