// grid_uv_field — write Array<vec2<f32>> of grid-cell-center UVs.
//
// For grid_size N, idx in [0, N²): col = idx % N, row = idx / N.
// Output uv = ((col + 0.5) / N, (row + 0.5) / N) — centered samples,
// matching the legacy DigitalPlants compute pass UV mapping.

struct GridUniforms {
    grid_size: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> u: GridUniforms;
@group(0) @binding(1) var<storage, read_write> uv_out: array<vec2<f32>>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let count = u.grid_size * u.grid_size;
    if idx >= count { return; }

    let col = idx % u.grid_size;
    let row = idx / u.grid_size;
    let inv_n = 1.0 / f32(u.grid_size);
    uv_out[idx] = vec2<f32>(
        (f32(col) + 0.5) * inv_n,
        (f32(row) + 0.5) * inv_n,
    );
}
