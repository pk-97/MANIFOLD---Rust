// node.tree: a tree grown from branching rules, after EZ-Tree by Dan
// Greenheck (MIT). A trunk and up to three levels of branches, each a chain
// of rings; leaf cards on the last level.
//
// grow_main runs once per level, a thread per branch. A branch starts at a
// point along its parent (`start` to the tip), turned about the parent by an
// even share of a full turn and tipped out by `angle`, with its radius a
// share of the parent's there. It then climbs section by section: each ring
// tapers, wanders by `gnarl`, twists, and turns toward `force` (up for lift,
// down for droop), thin wood turning the most; the trunk only wanders.
//
// bark_main writes each ring's vertices round the branch, index_main the
// two triangles of each quad between rings, leaves_main two crossed cards
// per leaf. Every random draw hashes (seed, level, branch, draw), so a tree
// is the same every time for the same rows.

struct TreeUniforms {
    // Per level 0 (trunk) to 3: branches, sections, sides round, children
    // each branch carries, and where its rings, bark vertices and bark
    // indices start.
    branches: vec4<u32>,
    sections: vec4<u32>,
    segments: vec4<u32>,
    children: vec4<u32>,
    ring_base: vec4<u32>,
    vert_base: vec4<u32>,
    index_base: vec4<u32>,
    // Per level: length in metres (the trunk's whole, a branch's before it
    // shortens up its parent), radius (the trunk's in metres, a branch's as
    // a share of its parent's where it starts), angle out from the parent
    // in radians, where along the parent branches start, gnarl and twist
    // in radians a section, and taper (the share of radius lost by the tip).
    length: vec4<f32>,
    radius: vec4<f32>,
    angle: vec4<f32>,
    start: vec4<f32>,
    gnarl: vec4<f32>,
    taper: vec4<f32>,
    twist: vec4<f32>,
    // xyz the way wood turns (unit), w how hard: radians a section on wood
    // a metre thick.
    force: vec4<f32>,
    // x leaf size in metres, y its spread (share), z angle out in radians,
    // w where along a branch leaves start.
    leaf: vec4<f32>,
    // x the last level, y leaves on each last-level branch, z cards a leaf,
    // w seed.
    counts: vec4<u32>,
    // x the level grow_main grows, y bark vertices, z bark indices, w leaf
    // vertices.
    level: vec4<u32>,
};

struct Ring {
    pos: vec3<f32>,
    radius: f32,
    rot: vec4<f32>,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: TreeUniforms;
@group(0) @binding(1) var<storage, read_write> rings: array<Ring>;
@group(0) @binding(2) var<storage, read_write> bark: array<MeshVertex>;
@group(0) @binding(3) var<storage, read_write> bark_indices: array<u32>;
@group(0) @binding(4) var<storage, read_write> leaves: array<MeshVertex>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
const X: vec3<f32> = vec3<f32>(1.0, 0.0, 0.0);
const Y: vec3<f32> = vec3<f32>(0.0, 1.0, 0.0);
const Z: vec3<f32> = vec3<f32>(0.0, 0.0, 1.0);
// Random streams apart from the branch levels.
const LEAF_STREAM: u32 = 8u;
const LEAF_TURN_STREAM: u32 = 9u;

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// A number in [0, 1) for draw `k` of branch `b` in stream `l`.
fn random(l: u32, b: u32, k: u32) -> f32 {
    let h = pcg(pcg(pcg(pcg(u.counts.w) + l) + b) + k);
    return f32(h >> 8u) / 16777216.0;
}

fn qmul(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(a.w * b.xyz + b.w * a.xyz + cross(a.xyz, b.xyz), a.w * b.w - dot(a.xyz, b.xyz));
}

fn qrot(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return v + 2.0 * cross(q.xyz, cross(q.xyz, v) + q.w * v);
}

fn qaxis(axis: vec3<f32>, angle: f32) -> vec4<f32> {
    return vec4<f32>(axis * sin(angle * 0.5), cos(angle * 0.5));
}

fn nlerp(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    let near = select(b, -b, dot(a, b) < 0.0);
    return normalize(mix(a, near, t));
}

// Branch `b` of level `l`, `t` (0 to 1) of the way from base to tip.
fn along(l: u32, b: u32, t: f32) -> Ring {
    let n = u.sections[l];
    let base = u.ring_base[l] + b * (n + 1u);
    let f = clamp(t, 0.0, 1.0) * f32(n);
    let k = min(u32(f), n - 1u);
    let a = f - f32(k);
    let r0 = rings[base + k];
    let r1 = rings[base + k + 1u];
    return Ring(mix(r0.pos, r1.pos, a), mix(r0.radius, r1.radius, a), nlerp(r0.rot, r1.rot, a));
}

@compute @workgroup_size(64, 1, 1)
fn grow_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let l = u.level.x;
    let b = id.x;
    if b >= u.branches[l] {
        return;
    }
    var pos = vec3<f32>(0.0);
    var rot = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    var radius = u.radius[0];
    var span = u.length[0];
    if l > 0u {
        let per = u.children[l - 1u];
        let parent = b / per;
        let i = b % per;
        let t = mix(u.start[l], 1.0, random(l, b, 0u));
        let at = along(l - 1u, parent, t);
        let turn = TAU * (random(l - 1u, parent, 1u) + f32(i) / f32(per));
        rot = normalize(qmul(at.rot, qmul(qaxis(Y, turn), qaxis(X, u.angle[l]))));
        pos = at.pos;
        radius = u.radius[l] * at.radius;
        // Branches shorten toward the top of their parent.
        span = u.length[l] * (1.0 - 0.5 * t);
    }
    let n = u.sections[l];
    let step = span / f32(n);
    let base = u.ring_base[l] + b * (n + 1u);
    for (var s = 0u; s <= n; s++) {
        let r = radius * (1.0 - u.taper[l] * f32(s) / f32(n));
        rings[base + s] = Ring(pos, r, rot);
        pos += qrot(rot, Y) * step;
        let gx = (random(l, b, 2u + 2u * s) * 2.0 - 1.0) * u.gnarl[l];
        let gz = (random(l, b, 3u + 2u * s) * 2.0 - 1.0) * u.gnarl[l];
        rot = normalize(qmul(rot, qmul(qaxis(X, gx), qmul(qaxis(Z, gz), qaxis(Y, u.twist[l])))));
        // Branches turn toward the force, thin wood the most, never past
        // it. The trunk stands.
        let dir = qrot(rot, Y);
        let axis = cross(dir, u.force.xyz);
        let sine = length(axis);
        if l > 0u && sine > 1e-5 {
            let angle = acos(clamp(dot(dir, u.force.xyz), -1.0, 1.0));
            let turn = min(angle, u.force.w / max(2.0 * r, 0.01));
            rot = normalize(qmul(qaxis(axis / sine, turn), rot));
        }
    }
}

// The level whose block holds item `i`, given each level's first item.
fn level_of(i: u32, bases: vec4<u32>) -> u32 {
    var l = 0u;
    for (var k = 1u; k <= u.counts.x; k++) {
        if i >= bases[k] {
            l = k;
        }
    }
    return l;
}

@compute @workgroup_size(64, 1, 1)
fn bark_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let v = id.x;
    if v >= u.level.y {
        return;
    }
    let l = level_of(v, u.vert_base);
    let g = u.segments[l];
    let local = v - u.vert_base[l];
    let ring_in_level = local / (g + 1u);
    let j = local % (g + 1u);
    let ring = rings[u.ring_base[l] + ring_in_level];
    let a = TAU * f32(j) / f32(g);
    let out = qrot(ring.rot, vec3<f32>(cos(a), 0.0, sin(a)));
    var vert: MeshVertex;
    vert.position = ring.pos + out * ring.radius;
    vert.normal = out;
    vert.uv = vec2<f32>(f32(j) / f32(g), f32(ring_in_level % (u.sections[l] + 1u)));
    vert.tangent = vec4<f32>(qrot(ring.rot, vec3<f32>(-sin(a), 0.0, cos(a))), 1.0);
    vert.color = vec4<f32>(1.0);
    bark[v] = vert;
}

@compute @workgroup_size(64, 1, 1)
fn index_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let q = id.x;
    if q * 6u >= u.level.z {
        return;
    }
    let l = level_of(q * 6u, u.index_base);
    let g = u.segments[l];
    let s = u.sections[l];
    let local = q - u.index_base[l] / 6u;
    let branch = local / (s * g);
    let rest = local % (s * g);
    let i = rest / g;
    let j = rest % g;
    // Counter-clockwise seen from outside the branch.
    let a = u.vert_base[l] + (branch * (s + 1u) + i) * (g + 1u) + j;
    let c = a + g + 1u;
    let o = q * 6u;
    bark_indices[o] = a;
    bark_indices[o + 1u] = c;
    bark_indices[o + 2u] = a + 1u;
    bark_indices[o + 3u] = a + 1u;
    bark_indices[o + 4u] = c;
    bark_indices[o + 5u] = c + 1u;
}

fn leaf_vertex(p: vec3<f32>, n: vec3<f32>, t: vec3<f32>, uv: vec2<f32>) -> MeshVertex {
    var vert: MeshVertex;
    vert.position = p;
    vert.normal = n;
    vert.uv = uv;
    vert.tangent = vec4<f32>(t, 1.0);
    vert.color = vec4<f32>(1.0);
    return vert;
}

@compute @workgroup_size(64, 1, 1)
fn leaves_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let leaf = id.x;
    let per = u.counts.y;
    let last = u.counts.x;
    let total = u.branches[last] * per;
    // With no leaves the output holds one degenerate triangle.
    if total == 0u {
        if leaf < 3u {
            leaves[leaf] = leaf_vertex(vec3<f32>(0.0), Y, X, vec2<f32>(0.0));
        }
        return;
    }
    if leaf >= total {
        return;
    }
    let b = leaf / per;
    let i = leaf % per;
    let at = along(last, b, mix(u.leaf.w, 1.0, random(LEAF_STREAM, leaf, 0u)));
    let turn = TAU * (random(LEAF_TURN_STREAM, b, 0u) + f32(i) / f32(per));
    let rot = qmul(at.rot, qmul(qaxis(Y, turn), qaxis(X, u.leaf.z)));
    let size = u.leaf.x * (1.0 + u.leaf.y * (random(LEAF_STREAM, leaf, 1u) * 2.0 - 1.0));
    let cards = u.counts.z;
    for (var c = 0u; c < cards; c++) {
        let q = qmul(rot, qaxis(Y, f32(c) * PI * 0.5));
        let x = qrot(q, X) * (size * 0.5);
        let y = qrot(q, Y) * size;
        let n = qrot(q, Z);
        let t = normalize(x);
        // The picture's top is the leaf's tip.
        let v0 = leaf_vertex(at.pos - x, n, t, vec2<f32>(0.0, 1.0));
        let v1 = leaf_vertex(at.pos + x, n, t, vec2<f32>(1.0, 1.0));
        let v2 = leaf_vertex(at.pos + x + y, n, t, vec2<f32>(1.0, 0.0));
        let v3 = leaf_vertex(at.pos - x + y, n, t, vec2<f32>(0.0, 0.0));
        let o = (leaf * cards + c) * 6u;
        leaves[o] = v0;
        leaves[o + 1u] = v1;
        leaves[o + 2u] = v2;
        leaves[o + 3u] = v0;
        leaves[o + 4u] = v2;
        leaves[o + 5u] = v3;
    }
}
