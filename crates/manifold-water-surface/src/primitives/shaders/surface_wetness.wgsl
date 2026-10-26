// Surface wetness: per lattice node, how wet the surface is there, 0 to 1.
//
// Where the water's level set is below one node spacing (water at the node or
// within a cell of it) the node is wet. Elsewhere its wetness is the last
// frame's times `decay`, the fraction of the film still on after the time
// since then. One pass per run; the buffer is the state, so `wetness` is both
// read and written.

struct Params {
    h: f32,
    decay: f32,
    node_count: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> field: array<f32>;
@group(0) @binding(2) var<storage, read_write> wetness: array<f32>;

@compute @workgroup_size(64)
fn cs_wetness(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = gid.x;
    if n >= params.node_count {
        return;
    }
    wetness[n] = select(wetness[n] * params.decay, 1.0, field[n] < params.h);
}
