// node.render_scene: one object's indirect draw arguments from its mesh's live
// extent (GPU_FLUID_SURFACE_DESIGN.md P6b). Whole triangles, never past the
// vertex buffer's capacity; Metal's four words: vertex count, instance count,
// vertex start, base instance.

struct LiveArgs {
    word: u32,
    per_item: u32,
    capacity: u32,
    instances: u32,
    slot: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: LiveArgs;
@group(0) @binding(1) var<storage, read> counts: array<u32>;
@group(0) @binding(2) var<storage, read_write> args: array<u32>;

@compute @workgroup_size(1)
fn write_draw_args() {
    let per_item = max(params.per_item, 1u);
    let items = min(counts[params.word], params.capacity / per_item);
    let base = params.slot * 4u;
    args[base] = items * per_item / 3u * 3u;
    args[base + 1u] = params.instances;
    args[base + 2u] = 0u;
    args[base + 3u] = 0u;
}
