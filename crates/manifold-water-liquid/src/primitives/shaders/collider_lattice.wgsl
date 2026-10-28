// Builds one moving shape's distance lattice in the atlas from its mesh tree.
// Appended to collider_bvh.wgsl, which provides cb_phi over the cb_node /
// cb_triangle reads defined here.
//
// One thread per atlas word, two nodes each: node n = 2w + k sits at index
// (n % dx, (n / dx) % dy, n / (dx dy)), x fastest, at origin + spacing * index
// in the shape's local units. A word packs the even node in its low half. A
// node past node_count (the odd tail) is +infinity, which reads as far outside.

struct Params {
    // xyz: local position of node (0, 0, 0); w: node spacing.
    origin_spacing: vec4<f32>,
    // xyz: nodes per axis; w: the lattice's first atlas word.
    dims_word: vec4<u32>,
    // x: node count; y: tree root node; z: tree first triangle.
    counts: vec4<u32>,
    // x: query band (the lattice's diagonal); y: shell half thickness for open surfaces.
    band_shell: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> triangles: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> atlas: array<u32>;

fn cb_node(i: u32) -> vec4<f32> {
    return nodes[i];
}

fn cb_triangle(i: u32) -> vec4<f32> {
    return triangles[i];
}

// Words per dispatch row: 4096 workgroups of 64 threads, so a lattice of any
// size stays within a dispatch dimension's limit.
const ROW_WORDS: u32 = 262144u;

fn node_phi(n: u32) -> f32 {
    let dx = params.dims_word.x;
    let dy = params.dims_word.y;
    let index = vec3<f32>(vec3<u32>(n % dx, (n / dx) % dy, n / (dx * dy)));
    let p = params.origin_spacing.xyz + params.origin_spacing.w * index;
    return cb_phi(params.counts.y, params.counts.z, p, params.band_shell.x, params.band_shell.y);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = gid.y * ROW_WORDS + gid.x;
    if 2u * w >= params.counts.x {
        return;
    }
    let even = node_phi(2u * w);
    if 2u * w + 1u >= params.counts.x {
        // The odd tail's high half is the half-precision +infinity.
        atlas[params.dims_word.w + w] = pack2x16float(vec2<f32>(even, 0.0)) | 0x7c000000u;
        return;
    }
    atlas[params.dims_word.w + w] = pack2x16float(vec2<f32>(even, node_phi(2u * w + 1u)));
}
