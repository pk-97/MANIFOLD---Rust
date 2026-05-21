// node.wireframe_shape — emit one of five wireframe shapes
// (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as
// a paired Array<MeshVertex> + Array<EdgePair>.
//
// Vertex positions and edge connectivity ported from
// generators/wireframe_zoo.rs's CPU-side const tables. The shader
// normalises vertices to the unit sphere in-shader so the output is
// directly comparable across shapes regardless of the unnormalised
// source magnitudes.
//
// Each thread writes its slot in both buffers (or pads / sentinels
// the slot if out of range for the current shape). Sentinel for
// unused edge slots is EdgePair(u32::MAX, u32::MAX) — matches the
// Rust `EdgePair::SENTINEL` and is what node.render_lines skips on.
//
// Output normals point radially outward from origin (matches the
// natural normal for a vertex on a convex polyhedron, useful if the
// downstream consumer wants shaded vertices).

struct WireframeUniforms {
    shape: u32,          // 0=Tetra, 1=Cube, 2=Octa, 3=Icosa, 4=Dodeca
    vert_capacity: u32,
    edge_capacity: u32,
    _pad: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

struct EdgePair {
    a: u32,
    b: u32,
};

@group(0) @binding(0) var<uniform> u: WireframeUniforms;
@group(0) @binding(1) var<storage, read_write> vert_dst: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> edge_dst: array<EdgePair>;

const PHI: f32 = 1.618034;
const INV_PHI: f32 = 0.618034;
const SENTINEL: u32 = 0xFFFFFFFFu;

// ── Vertex tables ───────────────────────────────────────────────
// Vertex counts per shape: tetra=4, cube=8, octa=6, icosa=12, dodeca=20.

fn tetra_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case 1u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case 2u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case 3u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn cube_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>(-1.0, -1.0, -1.0); }
        case 1u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case 2u: { return vec3<f32>( 1.0,  1.0, -1.0); }
        case 3u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case 4u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        case 5u: { return vec3<f32>( 1.0, -1.0,  1.0); }
        case 6u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case 7u: { return vec3<f32>(-1.0,  1.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn octa_vert(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>( 1.0,  0.0,  0.0); }
        case 1u: { return vec3<f32>(-1.0,  0.0,  0.0); }
        case 2u: { return vec3<f32>( 0.0,  1.0,  0.0); }
        case 3u: { return vec3<f32>( 0.0, -1.0,  0.0); }
        case 4u: { return vec3<f32>( 0.0,  0.0,  1.0); }
        case 5u: { return vec3<f32>( 0.0,  0.0, -1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn icosa_vert(i: u32) -> vec3<f32> {
    switch i {
        case  0u: { return vec3<f32>(-1.0,  PHI,  0.0); }
        case  1u: { return vec3<f32>( 1.0,  PHI,  0.0); }
        case  2u: { return vec3<f32>(-1.0, -PHI,  0.0); }
        case  3u: { return vec3<f32>( 1.0, -PHI,  0.0); }
        case  4u: { return vec3<f32>( 0.0, -1.0,  PHI); }
        case  5u: { return vec3<f32>( 0.0,  1.0,  PHI); }
        case  6u: { return vec3<f32>( 0.0, -1.0, -PHI); }
        case  7u: { return vec3<f32>( 0.0,  1.0, -PHI); }
        case  8u: { return vec3<f32>( PHI,  0.0, -1.0); }
        case  9u: { return vec3<f32>( PHI,  0.0,  1.0); }
        case 10u: { return vec3<f32>(-PHI,  0.0, -1.0); }
        case 11u: { return vec3<f32>(-PHI,  0.0,  1.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn dodeca_vert(i: u32) -> vec3<f32> {
    switch i {
        // Cube vertices (8)
        case  0u: { return vec3<f32>( 1.0,  1.0,  1.0); }
        case  1u: { return vec3<f32>( 1.0,  1.0, -1.0); }
        case  2u: { return vec3<f32>( 1.0, -1.0,  1.0); }
        case  3u: { return vec3<f32>( 1.0, -1.0, -1.0); }
        case  4u: { return vec3<f32>(-1.0,  1.0,  1.0); }
        case  5u: { return vec3<f32>(-1.0,  1.0, -1.0); }
        case  6u: { return vec3<f32>(-1.0, -1.0,  1.0); }
        case  7u: { return vec3<f32>(-1.0, -1.0, -1.0); }
        // XY-plane rectangle (4)
        case  8u: { return vec3<f32>( 0.0,  PHI,  INV_PHI); }
        case  9u: { return vec3<f32>( 0.0,  PHI, -INV_PHI); }
        case 10u: { return vec3<f32>( 0.0, -PHI,  INV_PHI); }
        case 11u: { return vec3<f32>( 0.0, -PHI, -INV_PHI); }
        // XZ-plane rectangle (4)
        case 12u: { return vec3<f32>( INV_PHI, 0.0,  PHI); }
        case 13u: { return vec3<f32>( INV_PHI, 0.0, -PHI); }
        case 14u: { return vec3<f32>(-INV_PHI, 0.0,  PHI); }
        case 15u: { return vec3<f32>(-INV_PHI, 0.0, -PHI); }
        // YZ-plane rectangle (4)
        case 16u: { return vec3<f32>( PHI,  INV_PHI, 0.0); }
        case 17u: { return vec3<f32>( PHI, -INV_PHI, 0.0); }
        case 18u: { return vec3<f32>(-PHI,  INV_PHI, 0.0); }
        case 19u: { return vec3<f32>(-PHI, -INV_PHI, 0.0); }
        default: { return vec3<f32>(0.0); }
    }
}

fn vert_count_for_shape(shape: u32) -> u32 {
    switch shape {
        case 0u: { return 4u; }   // Tetra
        case 1u: { return 8u; }   // Cube
        case 2u: { return 6u; }   // Octa
        case 3u: { return 12u; }  // Icosa
        case 4u: { return 20u; }  // Dodeca
        default: { return 0u; }
    }
}

fn vert_for_shape(shape: u32, i: u32) -> vec3<f32> {
    switch shape {
        case 0u: { return tetra_vert(i); }
        case 1u: { return cube_vert(i); }
        case 2u: { return octa_vert(i); }
        case 3u: { return icosa_vert(i); }
        case 4u: { return dodeca_vert(i); }
        default: { return vec3<f32>(0.0); }
    }
}

// ── Edge tables ─────────────────────────────────────────────────
// Edge counts per shape: tetra=6, cube=12, octa=12, icosa=30, dodeca=30.

fn tetra_edge(i: u32) -> vec2<u32> {
    switch i {
        case 0u: { return vec2<u32>(0u, 1u); }
        case 1u: { return vec2<u32>(0u, 2u); }
        case 2u: { return vec2<u32>(0u, 3u); }
        case 3u: { return vec2<u32>(1u, 2u); }
        case 4u: { return vec2<u32>(1u, 3u); }
        case 5u: { return vec2<u32>(2u, 3u); }
        default: { return vec2<u32>(0u); }
    }
}

fn cube_edge(i: u32) -> vec2<u32> {
    switch i {
        case  0u: { return vec2<u32>(0u, 1u); }
        case  1u: { return vec2<u32>(1u, 2u); }
        case  2u: { return vec2<u32>(2u, 3u); }
        case  3u: { return vec2<u32>(3u, 0u); }
        case  4u: { return vec2<u32>(4u, 5u); }
        case  5u: { return vec2<u32>(5u, 6u); }
        case  6u: { return vec2<u32>(6u, 7u); }
        case  7u: { return vec2<u32>(7u, 4u); }
        case  8u: { return vec2<u32>(0u, 4u); }
        case  9u: { return vec2<u32>(1u, 5u); }
        case 10u: { return vec2<u32>(2u, 6u); }
        case 11u: { return vec2<u32>(3u, 7u); }
        default: { return vec2<u32>(0u); }
    }
}

fn octa_edge(i: u32) -> vec2<u32> {
    switch i {
        case  0u: { return vec2<u32>(0u, 2u); }
        case  1u: { return vec2<u32>(0u, 3u); }
        case  2u: { return vec2<u32>(0u, 4u); }
        case  3u: { return vec2<u32>(0u, 5u); }
        case  4u: { return vec2<u32>(1u, 2u); }
        case  5u: { return vec2<u32>(1u, 3u); }
        case  6u: { return vec2<u32>(1u, 4u); }
        case  7u: { return vec2<u32>(1u, 5u); }
        case  8u: { return vec2<u32>(2u, 4u); }
        case  9u: { return vec2<u32>(2u, 5u); }
        case 10u: { return vec2<u32>(3u, 4u); }
        case 11u: { return vec2<u32>(3u, 5u); }
        default: { return vec2<u32>(0u); }
    }
}

fn icosa_edge(i: u32) -> vec2<u32> {
    switch i {
        case  0u: { return vec2<u32>(0u,  1u); }
        case  1u: { return vec2<u32>(0u,  5u); }
        case  2u: { return vec2<u32>(0u,  7u); }
        case  3u: { return vec2<u32>(0u, 10u); }
        case  4u: { return vec2<u32>(0u, 11u); }
        case  5u: { return vec2<u32>(1u,  5u); }
        case  6u: { return vec2<u32>(1u,  7u); }
        case  7u: { return vec2<u32>(1u,  8u); }
        case  8u: { return vec2<u32>(1u,  9u); }
        case  9u: { return vec2<u32>(2u,  3u); }
        case 10u: { return vec2<u32>(2u,  4u); }
        case 11u: { return vec2<u32>(2u,  6u); }
        case 12u: { return vec2<u32>(2u, 10u); }
        case 13u: { return vec2<u32>(2u, 11u); }
        case 14u: { return vec2<u32>(3u,  4u); }
        case 15u: { return vec2<u32>(3u,  6u); }
        case 16u: { return vec2<u32>(3u,  8u); }
        case 17u: { return vec2<u32>(3u,  9u); }
        case 18u: { return vec2<u32>(4u,  5u); }
        case 19u: { return vec2<u32>(4u,  9u); }
        case 20u: { return vec2<u32>(4u, 11u); }
        case 21u: { return vec2<u32>(5u,  9u); }
        case 22u: { return vec2<u32>(5u, 11u); }
        case 23u: { return vec2<u32>(6u,  7u); }
        case 24u: { return vec2<u32>(6u,  8u); }
        case 25u: { return vec2<u32>(6u, 10u); }
        case 26u: { return vec2<u32>(7u,  8u); }
        case 27u: { return vec2<u32>(7u, 10u); }
        case 28u: { return vec2<u32>(8u,  9u); }
        case 29u: { return vec2<u32>(10u, 11u); }
        default: { return vec2<u32>(0u); }
    }
}

fn dodeca_edge(i: u32) -> vec2<u32> {
    switch i {
        case  0u: { return vec2<u32>(0u,  8u); }
        case  1u: { return vec2<u32>(0u, 12u); }
        case  2u: { return vec2<u32>(0u, 16u); }
        case  3u: { return vec2<u32>(1u,  9u); }
        case  4u: { return vec2<u32>(1u, 13u); }
        case  5u: { return vec2<u32>(1u, 16u); }
        case  6u: { return vec2<u32>(2u, 10u); }
        case  7u: { return vec2<u32>(2u, 12u); }
        case  8u: { return vec2<u32>(2u, 17u); }
        case  9u: { return vec2<u32>(3u, 11u); }
        case 10u: { return vec2<u32>(3u, 13u); }
        case 11u: { return vec2<u32>(3u, 17u); }
        case 12u: { return vec2<u32>(4u,  8u); }
        case 13u: { return vec2<u32>(4u, 14u); }
        case 14u: { return vec2<u32>(4u, 18u); }
        case 15u: { return vec2<u32>(5u,  9u); }
        case 16u: { return vec2<u32>(5u, 15u); }
        case 17u: { return vec2<u32>(5u, 18u); }
        case 18u: { return vec2<u32>(6u, 10u); }
        case 19u: { return vec2<u32>(6u, 14u); }
        case 20u: { return vec2<u32>(6u, 19u); }
        case 21u: { return vec2<u32>(7u, 11u); }
        case 22u: { return vec2<u32>(7u, 15u); }
        case 23u: { return vec2<u32>(7u, 19u); }
        case 24u: { return vec2<u32>(8u,  9u); }
        case 25u: { return vec2<u32>(10u, 11u); }
        case 26u: { return vec2<u32>(12u, 14u); }
        case 27u: { return vec2<u32>(13u, 15u); }
        case 28u: { return vec2<u32>(16u, 17u); }
        case 29u: { return vec2<u32>(18u, 19u); }
        default: { return vec2<u32>(0u); }
    }
}

fn edge_count_for_shape(shape: u32) -> u32 {
    switch shape {
        case 0u: { return 6u; }   // Tetra
        case 1u: { return 12u; }  // Cube
        case 2u: { return 12u; }  // Octa
        case 3u: { return 30u; }  // Icosa
        case 4u: { return 30u; }  // Dodeca
        default: { return 0u; }
    }
}

fn edge_for_shape(shape: u32, i: u32) -> vec2<u32> {
    switch shape {
        case 0u: { return tetra_edge(i); }
        case 1u: { return cube_edge(i); }
        case 2u: { return octa_edge(i); }
        case 3u: { return icosa_edge(i); }
        case 4u: { return dodeca_edge(i); }
        default: { return vec2<u32>(0u); }
    }
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;

    // Write vertex slot (if in range).
    if i < u.vert_capacity {
        let nverts = vert_count_for_shape(u.shape);
        if i < nverts {
            let raw = vert_for_shape(u.shape, i);
            let len = length(raw);
            let pos = select(raw / len, vec3<f32>(0.0), len < 1e-8);
            vert_dst[i].position = pos;
            vert_dst[i]._pad0 = 0.0;
            // Outward-radial normal (vertex of a convex polyhedron
            // points away from origin). Same as the normalised
            // position for convex shapes.
            vert_dst[i].normal = pos;
            vert_dst[i]._pad1 = 0.0;
        } else {
            // Pad unused vertex slots — keeps the downstream
            // rotate/project chain from reading garbage.
            vert_dst[i].position = vec3<f32>(0.0);
            vert_dst[i]._pad0 = 0.0;
            vert_dst[i].normal = vec3<f32>(0.0, 1.0, 0.0);
            vert_dst[i]._pad1 = 0.0;
        }
    }

    // Write edge slot (if in range).
    if i < u.edge_capacity {
        let nedges = edge_count_for_shape(u.shape);
        if i < nedges {
            let e = edge_for_shape(u.shape, i);
            edge_dst[i].a = e.x;
            edge_dst[i].b = e.y;
        } else {
            // Sentinel for unused edge slots — node.render_lines
            // skips on `a == SENTINEL`.
            edge_dst[i].a = SENTINEL;
            edge_dst[i].b = SENTINEL;
        }
    }
}
