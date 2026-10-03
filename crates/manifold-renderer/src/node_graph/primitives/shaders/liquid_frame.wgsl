// node.liquid_frame — write one particle frame of the seam
// (LIQUID_SOLVER_SEAM_DESIGN.md section 3.1) from a particle liquid's state.
// One thread per slot of the frame: records below `count` are copied with id
// 0, since a solver may reorder its state every tick (amendment 5); slots past
// it get radius 0 (amendment 4). A tick whose stats flag a non-finite record
// or narrow-band capacity shortage (word 17) is never published: the frame
// repeats the previous one (amendment 2).
// Interior publication supports Ferstl et al. (2016), Narrow Band FLIP,
// doi:10.1111/cgf.12825; this publication gate is MANIFOLD integration.

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct FrameParams {
    count: u32,
    previous_count: u32,
    slots: u32,
    _pad0: u32,
}

@group(0) @binding(0) var<uniform> params: FrameParams;
@group(0) @binding(1) var<storage, read> state: array<FluidParticle>;
@group(0) @binding(2) var<storage, read> stats: array<u32>;
@group(0) @binding(3) var<storage, read> previous: array<FluidParticle>;
@group(0) @binding(4) var<storage, read_write> frame: array<FluidParticle>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.slots {
        return;
    }
    var f: FluidParticle;
    f.position_radius = vec4<f32>(0.0);
    f.velocity = vec3<f32>(0.0);
    f.id = 0u;
    if stats[0] != 0u || stats[17] != 0u {
        if i < params.previous_count {
            f = previous[i];
        }
        frame[i] = f;
        return;
    }
    if i < params.count {
        f = state[i];
        f.id = 0u;
    }
    frame[i] = f;
}
