// node.render_scene: the instances an instanced draw needs. An instance that
// is all zero is a hole and draws nothing (zero scale collapses every vertex
// onto the origin); producers that compact their live instances to the
// front leave a tail of holes. The draw's instance word becomes one past the
// last instance that is not a hole, so the tail is never drawn and the image
// is unchanged. Eight-word argument blocks, as live_draw_args.wgsl.

struct Trim {
    slot: u32,
    // Instances the draw may reach: the buffer's capacity or the wired count.
    count: u32,
    // Fixed draws: whole-triangle vertices, or indices when indexed.
    vertices: u32,
    _pad0: u32,
}

@group(0) @binding(0) var<uniform> params: Trim;
// InstanceTransform: pos_scale then rot_pad.
@group(0) @binding(1) var<storage, read> instances: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> args: array<atomic<u32>>;

var<workgroup> last: atomic<u32>;

// A draw without a live mesh extent: its CPU vertex count and no instances
// yet, for trim_instances to raise.
@compute @workgroup_size(1)
fn fixed_args() {
    let base = params.slot * 8u;
    atomicStore(&args[base], params.vertices);
    atomicStore(&args[base + 1u], 0u);
    atomicStore(&args[base + 2u], 0u);
    atomicStore(&args[base + 3u], 0u);
    atomicStore(&args[base + 4u], 0u);
}

// After the block is written with zero instances: one past the last
// instance below `count` that is not a hole, maxed into the instance word.
@compute @workgroup_size(256)
fn trim_instances(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    if li == 0u {
        atomicStore(&last, 0u);
    }
    workgroupBarrier();
    let stride = groups.x * 256u;
    for (var i = gid.x; i < params.count; i = i + stride) {
        if any(instances[2u * i] != vec4<f32>(0.0)) || any(instances[2u * i + 1u] != vec4<f32>(0.0)) {
            atomicMax(&last, i + 1u);
        }
    }
    workgroupBarrier();
    if li == 0u {
        let end = atomicLoad(&last);
        if end > 0u {
            atomicMax(&args[params.slot * 8u + 1u], end);
        }
    }
}
