// Internal whitewater stage pass. The source is an unpadded cell-centred
// distance lattice and both arrays use x-fastest indexing. A destination cell
// shifted inward by padding reads the source at the same coordinate; every
// exterior destination receives the configured exterior value.

struct Params {
    cells_x: u32,
    cells_y: u32,
    cells_z: u32,
    padding: u32,
    exterior: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256)
fn pad_distance_lattice(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let idx = global_id.x;
    if idx >= params.dispatch_count {
        return;
    }

    let cells = vec3<u32>(params.cells_x, params.cells_y, params.cells_z);
    let side = cells + vec3<u32>(2u * params.padding);
    let coord = vec3<u32>(
        idx % side.x,
        (idx / side.x) % side.y,
        idx / (side.x * side.y),
    );
    let source = vec3<i32>(coord) - vec3<i32>(i32(params.padding));
    if any(source < vec3<i32>(0)) || any(source >= vec3<i32>(cells)) {
        dst[idx] = params.exterior;
        return;
    }

    let source_idx = u32(source.x) + cells.x * (u32(source.y) + cells.y * u32(source.z));
    if source_idx >= arrayLength(&src) {
        dst[idx] = params.exterior;
        return;
    }
    dst[idx] = src[source_idx];
}
