// node.generate_instance_transforms — fusable BUFFER body (freeze §12, buffer
// domain), SOURCE. Fill an Array<InstanceTransform> with a procedural layout
// (grid / ring / spiral / random). Matches generate_instance_transforms.wgsl
// bit-for-bit (self-contained wang_hash / hash_to_unit — no external include).
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the output element written to
// buf_instances[idx]. The codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `dispatch_count` (= the OUTPUT
// capacity) is the wrapper guard; `active_count` is a PARAM (the inactive
// threshold) — slots in [active_count, capacity) collapse to zero. `max_capacity`
// is an allocation-only param the shader ignores (DCE drops it). Int params
// arrive as i32 (active_count/seed/max_capacity), the Enum `layout` as u32; cast
// to u32 where the hand used u32.
const GIT_LAYOUT_GRID: u32 = 0u;
const GIT_LAYOUT_RING: u32 = 1u;
const GIT_LAYOUT_SPIRAL: u32 = 2u;

fn git_wang_hash(seed: u32) -> u32 {
    var s = seed;
    s = (s ^ 61u) ^ (s >> 16u);
    s = s + (s << 3u);
    s = s ^ (s >> 4u);
    s = s * 0x27d4eb2du;
    s = s ^ (s >> 15u);
    return s;
}

fn git_hash_to_unit(seed: u32) -> f32 {
    return f32(git_wang_hash(seed) & 0x00ffffffu) / 16777216.0;
}

fn body(
    idx: u32,
    count: u32,
    max_capacity: i32,
    active_count: i32,
    layout_kind: u32,
    seed: i32,
    extent_x: f32,
    extent_y: f32,
    extent_z: f32,
    base_scale: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
) -> Element {
    let ac = u32(active_count);
    if idx >= ac {
        // Inactive slot -> zeroed transform.
        return Element(vec4<f32>(0.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 0.0));
    }

    let i = idx;
    var pos: vec3<f32>;
    let scale: f32 = base_scale;
    let rot: vec3<f32> = vec3<f32>(rot_x, rot_y, rot_z);

    if layout_kind == GIT_LAYOUT_GRID {
        // Cube root -> 3D grid. Side = ceil(N^(1/3)).
        let n = f32(ac);
        let side = max(u32(ceil(pow(n, 1.0 / 3.0))), 1u);
        let cx = i % side;
        let cy = (i / side) % side;
        let cz = i / (side * side);
        let denom = f32(max(side - 1u, 1u));
        let nx = f32(cx) / denom - 0.5;
        let ny = f32(cy) / denom - 0.5;
        let nz = f32(cz) / denom - 0.5;
        pos = vec3<f32>(nx * extent_x, ny * extent_y, nz * extent_z);
    } else if layout_kind == GIT_LAYOUT_RING {
        let t = f32(i) / f32(max(ac, 1u));
        let theta = t * 6.28318530718;
        pos = vec3<f32>(cos(theta) * extent_x * 0.5, 0.0, sin(theta) * extent_z * 0.5);
    } else if layout_kind == GIT_LAYOUT_SPIRAL {
        let t = f32(i) / f32(max(ac, 1u));
        let theta = t * 6.28318530718 * 4.0;
        let r = t;
        let y = (t - 0.5) * extent_y;
        pos = vec3<f32>(cos(theta) * r * extent_x * 0.5, y, sin(theta) * r * extent_z * 0.5);
    } else {
        // LAYOUT_RANDOM — uniform within extent box.
        let s = u32(seed);
        let h0 = git_hash_to_unit(i * 3u + 0u + s);
        let h1 = git_hash_to_unit(i * 3u + 1u + s);
        let h2 = git_hash_to_unit(i * 3u + 2u + s);
        pos = vec3<f32>((h0 - 0.5) * extent_x, (h1 - 0.5) * extent_y, (h2 - 0.5) * extent_z);
    }

    return Element(vec4<f32>(pos, scale), vec4<f32>(rot, 0.0));
}
