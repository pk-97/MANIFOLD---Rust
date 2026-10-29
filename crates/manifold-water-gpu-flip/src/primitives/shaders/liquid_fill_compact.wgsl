// node.liquid_fill's compaction: of the `n` filled sites' particles (two vec4
// words a FluidParticle, the radius in the first one's w), the live ones
// (radius > 0) move to the front of `live` in site order, so a fill holds only
// the water it places. `flag` marks them in `scan`; the caller scans it
// inclusively; `scatter` writes particle i to slot scan[i] - 1.

struct Params {
    n: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> sites: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read_write> scan: array<u32>;
@group(0) @binding(3) var<storage, read_write> live: array<vec4<u32>>;

fn alive(i: u32) -> bool {
    return bitcast<f32>(sites[2u * i].w) > 0.0;
}

@compute @workgroup_size(256)
fn flag(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.n {
        return;
    }
    scan[gid.x] = select(0u, 1u, alive(gid.x));
}

@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.n || !alive(i) {
        return;
    }
    let to = scan[i] - 1u;
    live[2u * to] = sites[2u * i];
    live[2u * to + 1u] = sites[2u * i + 1u];
}
