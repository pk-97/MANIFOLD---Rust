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
// per leaf and leaf_index_main their triangles. cards_main writes, after
// the leaves, the cards that stand in for them far away: two crossed over
// each last-level branch, and CROWN_CARDS standing round the trunk through
// the whole crown. lods_main writes the bark's and the leaves' detail
// levels in mesh_lods.rs's layout: 1 drops the last level of wood and
// keeps one card a leaf; 2 keeps a coarse trunk and the branch cards; 3 the
// coarse trunk and the crown cards; 4 draws nothing. Every random draw
// hashes (seed, level, branch, draw), so a tree is the same every time for
// the same rows.

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
    // x how much the branches off the trunk shorten toward its top: 0.5
    // halves the highest, 1 brings them to a point, a cone.
    shape: vec4<f32>,
    // The detail levels' errors in metres, 1 to 4.
    lod_error: vec4<f32>,
    // Index counts of the bark's level 1 and the leaves' 1 to 3.
    lod_count: vec4<u32>,
    // x leaf cards, y branch cards, z crown cards, w the first branch
    // card's vertex; the crown cards follow the branch cards.
    cards: vec4<u32>,
    // The crown, an ellipsoid round the trunk: x its radius, y and z the
    // heights of its bottom and top.
    crown: vec4<f32>,
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
@group(0) @binding(5) var<storage, read_write> leaf_indices: array<u32>;
@group(0) @binding(6) var<storage, read_write> bark_lods: array<u32>;
@group(0) @binding(7) var<storage, read_write> leaf_lods: array<u32>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
const X: vec3<f32> = vec3<f32>(1.0, 0.0, 0.0);
const Y: vec3<f32> = vec3<f32>(0.0, 1.0, 0.0);
const Z: vec3<f32> = vec3<f32>(0.0, 0.0, 1.0);
// LOD_HEADER_WORDS in mesh_lods.rs.
const LOD_HEADER: u32 = 16u;
// The far trunk: every COARSE-th share of the trunk's rings and sides, so
// COARSE sections of COARSE sides (COARSE_INDICES in tree.rs).
const COARSE: u32 = 3u;
const COARSE_INDICES: u32 = 54u;
// Random streams apart from the branch levels.
const LEAF_STREAM: u32 = 8u;
const LEAF_TURN_STREAM: u32 = 9u;
const BRANCH_TURN_STREAM: u32 = 10u;
// The golden angle as a share of a turn: branches this far round from the
// last never line up.
const GOLDEN: f32 = 0.381966;
// The share of light that reaches the middle of the crown and its bottom,
// as games bake it into foliage, multiplied into every vertex's colour.
const CROWN_DEEP: f32 = 0.3;
const CROWN_LOW: f32 = 0.6;
// How far a leaf's normal bends from its card toward the crown's outward
// normal, so the tree shades as one rounded mass, not as loose cards.
const CROWN_BEND: f32 = 0.8;

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
        // Spaced evenly up the parent, each a little off its slot, and
        // round it by the golden angle, so none crowd and none line up.
        let t = mix(u.start[l], 1.0, (f32(i) + random(l, b, 0u)) / f32(per));
        let at = along(l - 1u, parent, t);
        let turn = TAU * (random(l - 1u, parent, 1u) + f32(i) * GOLDEN
            + 0.1 * random(BRANCH_TURN_STREAM, b, l));
        rot = normalize(qmul(at.rot, qmul(qaxis(Y, turn), qaxis(X, u.angle[l]))));
        pos = at.pos;
        radius = u.radius[l] * at.radius;
        // Branches shorten toward the top of their parent; off the trunk,
        // by the Cone row.
        span = u.length[l] * (1.0 - select(0.5, u.shape.x, l == 1u) * t);
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
    vert.color = vec4<f32>(vec3<f32>(crown(vert.position).w), 1.0);
    bark[v] = vert;
}

// Corner `k` (0 to 5) of a quad's two triangles: 0 1 2, 0 2 3.
fn quad_corner(k: u32) -> u32 {
    return select(select(k - 2u, 0u, k == 3u), k, k < 3u);
}

// Word `w` of the whole bark's triangle list.
fn bark_index(w: u32) -> u32 {
    let q = w / 6u;
    let l = level_of(w, u.index_base);
    let g = u.segments[l];
    let s = u.sections[l];
    let local = q - u.index_base[l] / 6u;
    let branch = local / (s * g);
    let rest = local % (s * g);
    let i = rest / g;
    let j = rest % g;
    // Counter-clockwise seen from outside the branch: a c a+1, a+1 c c+1.
    let a = u.vert_base[l] + (branch * (s + 1u) + i) * (g + 1u) + j;
    let c = a + g + 1u;
    let corner = array<u32, 6>(a, c, a + 1u, a + 1u, c, c + 1u);
    return corner[w % 6u];
}

@compute @workgroup_size(64, 1, 1)
fn index_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = id.x;
    if w < u.level.z {
        bark_indices[w] = bark_index(w);
    }
}

// The crown's outward normal at `p` (xyz) and the light that reaches it
// (w). The crown is a cone round the trunk at p's height, as wide as
// `crown.x` at its bottom and narrowing by Cone toward its top; light is
// dim on the trunk and deep inside, full at the crown's skin, and dimmer
// low down, under the rest of the crown.
fn crown(p: vec3<f32>) -> vec4<f32> {
    let r = max(u.crown.x, 0.1);
    let tall = max(u.crown.z - u.crown.y, 0.1);
    let axis = along(0u, 0u, p.y / max(u.length[0], 1e-3)).pos;
    let out = vec2<f32>(p.x - axis.x, p.z - axis.z);
    let off = length(out);
    let up = clamp((p.y - u.crown.y) / tall, 0.0, 1.0);
    let wide = r * max(1.0 - u.shape.x * up, 0.15);
    let side = select(vec2<f32>(1.0, 0.0), out / off, off > 1e-4);
    // The cone's slope tips its normal up; near the top, more so.
    let rise = r * u.shape.x / tall + 2.0 * max(up - 0.7, 0.0);
    let n = normalize(vec3<f32>(side.x, rise, side.y));
    let light = mix(CROWN_DEEP, 1.0, smoothstep(0.0, 1.0, off / wide)) * mix(CROWN_LOW, 1.0, up);
    return vec4<f32>(n, light);
}

fn leaf_vertex(p: vec3<f32>, n: vec3<f32>, t: vec3<f32>, uv: vec2<f32>) -> MeshVertex {
    let c = crown(p);
    let card = select(n, -n, dot(n, c.xyz) < 0.0);
    var vert: MeshVertex;
    vert.position = p;
    vert.normal = normalize(mix(card, c.xyz, CROWN_BEND));
    vert.uv = uv;
    vert.tangent = vec4<f32>(t, 1.0);
    vert.color = vec4<f32>(vec3<f32>(c.w), 1.0);
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
    // Leaves never outgrow their branch: on one shorter than half its
    // level's length, as near the top of a cone, they shrink with it.
    let reach = length(along(last, b, 1.0).pos - along(last, b, 0.0).pos);
    let fit = clamp(reach / (0.5 * u.length[last]), 0.1, 1.0);
    let size = fit * u.leaf.x * (1.0 + u.leaf.y * (random(LEAF_STREAM, leaf, 1u) * 2.0 - 1.0));
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
        let o = (leaf * cards + c) * 4u;
        leaves[o] = v0;
        leaves[o + 1u] = v1;
        leaves[o + 2u] = v2;
        leaves[o + 3u] = v3;
    }
}

// Word `w` of the leaves' full triangle list: card w / 6's two triangles.
fn leaf_index(w: u32) -> u32 {
    return (w / 6u) * 4u + quad_corner(w % 6u);
}

@compute @workgroup_size(64, 1, 1)
fn leaf_index_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = id.x;
    // With no leaves, one degenerate triangle.
    if u.cards.x == 0u {
        if w < 3u {
            leaf_indices[w] = 0u;
        }
        return;
    }
    if w < u.cards.x * 6u {
        leaf_indices[w] = leaf_index(w);
    }
}

// Card `o` (its four vertices from `o`) from `base` to `tip`, `x` either
// side, the leaf picture's tip at `tip`.
fn stand_in(o: u32, base: vec3<f32>, tip: vec3<f32>, x: vec3<f32>) {
    let n = normalize(cross(x, tip - base));
    let t = normalize(x);
    leaves[o] = leaf_vertex(base - x, n, t, vec2<f32>(0.0, 1.0));
    leaves[o + 1u] = leaf_vertex(base + x, n, t, vec2<f32>(1.0, 1.0));
    leaves[o + 2u] = leaf_vertex(tip + x, n, t, vec2<f32>(1.0, 0.0));
    leaves[o + 3u] = leaf_vertex(tip - x, n, t, vec2<f32>(0.0, 0.0));
}

// Thread k < branch cards / 2: two crossed cards over the leafy part of
// last-level branch k, as wide and as long as its leaves reach, one flat
// across it and one standing up through it. Then one thread per crown
// card: an upright card round the trunk, turned an even share of half a
// turn from the last, from the crown's bottom to its top and as wide.
@compute @workgroup_size(64, 1, 1)
fn cards_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let k = id.x;
    let pairs = u.cards.y / 2u;
    if k < pairs {
        let last = u.counts.x;
        let root = along(last, k, 0.0).pos;
        let base = along(last, k, u.leaf.w).pos;
        let end = along(last, k, 1.0).pos;
        let fit = clamp(length(end - root) / (0.5 * u.length[last]), 0.1, 1.0);
        let up = normalize(end - base + vec3<f32>(0.0, 1e-5, 0.0));
        let leaf = fit * u.leaf.x;
        let tip = end + up * leaf * cos(u.leaf.z);
        var flat = cross(up, Y);
        if length(flat) < 0.1 {
            flat = qrot(along(last, k, 1.0).rot, X);
        }
        flat = normalize(flat);
        let half = leaf * (0.5 + sin(u.leaf.z));
        stand_in(u.cards.w + k * 8u, base, tip, flat * half);
        stand_in(u.cards.w + k * 8u + 4u, base, tip, cross(flat, up) * half);
        return;
    }
    let c = k - pairs;
    if c >= u.cards.z {
        return;
    }
    let base = along(0u, 0u, u.crown.y / max(u.length[0], 1e-3)).pos;
    let top = along(0u, 0u, 1.0).pos;
    let a = PI * f32(c) / f32(u.cards.z);
    let x = vec3<f32>(cos(a), 0.0, sin(a)) * u.crown.x;
    stand_in(u.cards.w + (u.cards.y + c) * 4u, base, top, x);
}

// Word `k` of the far trunk's triangle list.
fn coarse_index(k: u32) -> u32 {
    let q = k / 6u;
    let g = u.segments[0];
    let rings = u.sections[0] / COARSE;
    let sides = g / COARSE;
    let a = u.vert_base[0] + (q / COARSE) * rings * (g + 1u) + (q % COARSE) * sides;
    let c = a + rings * (g + 1u);
    let corner = array<u32, 6>(a, c, a + sides, a + sides, c, c + sides);
    return corner[k % 6u];
}

// Word `w` of a `lods` header for four levels from `start`, `count` words
// long.
fn lod_header(w: u32, start: vec4<u32>, count: vec4<u32>) -> u32 {
    if w == 0u {
        return 4u;
    }
    if w > 12u {
        return 0u;
    }
    let k = (w - 1u) / 3u;
    switch (w - 1u) % 3u {
        case 0u: { return start[k]; }
        case 1u: { return count[k]; }
        default: { return bitcast<u32>(u.lod_error[k]); }
    }
}

@compute @workgroup_size(64, 1, 1)
fn lods_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = id.x;
    let b1 = u.lod_count.x;
    let far = LOD_HEADER + b1;
    if w < LOD_HEADER {
        let start = vec4<u32>(LOD_HEADER, far, far, far + COARSE_INDICES);
        bark_lods[w] = lod_header(w, start, vec4<u32>(b1, COARSE_INDICES, COARSE_INDICES, 0u));
    } else if w < far {
        // The wood above the last level comes first in the full list.
        bark_lods[w] = bark_index(w - LOD_HEADER);
    } else if w < far + COARSE_INDICES {
        bark_lods[w] = coarse_index(w - far);
    }
    let l = u.lod_count.yzw;
    let first = vec3<u32>(LOD_HEADER, LOD_HEADER + l.x, LOD_HEADER + l.x + l.y);
    if w < LOD_HEADER {
        let start = vec4<u32>(first, first.z + l.z);
        leaf_lods[w] = lod_header(w, start, vec4<u32>(l, 0u));
    } else if w < first.y {
        // One card a leaf: its first.
        let k = w - first.x;
        leaf_lods[w] = (k / 6u) * u.counts.z * 4u + quad_corner(k % 6u);
    } else if w < first.z + l.z {
        // The branch cards, then the crown cards, as they lie after the
        // leaves' own.
        let k = w - first.y;
        leaf_lods[w] = u.cards.w + (k / 6u) * 4u + quad_corner(k % 6u);
    }
}
