// node.polytope_vertices — emit one of five Platonic-solid vertex
// sets as an Array<MeshVertex>.
//
// The set is mathematically closed at five (Euclid, ~300 BC), so
// per-shape coordinates are baked here as switch-case lookups, not
// a user-extensible table. The shape selector is a uniform driven
// by the primitive's port-shadowable `shape` input.
//
// Vertex positions normalise to magnitude POLYTOPE_DEFAULT_RADIUS
// (0.25) in-shader. This is the legacy PROJ_SCALE from the original
// line generators — baked in so downstream node.project_3d.proj_scale
// defaults to 1.0 (the user-facing "default zoom") instead of needing
// a graph-side math node to multiply outer-card Scale by 0.25. The
// constant lives where it belongs — inside the primitive responsible
// for screen-friendly vertex magnitudes — not in the binding layer
// or as a graph node.
//
// Edges are NOT written here — node.polytope_edges is the paired
// CPU-only producer that emits the Array<EdgePair> topology.
//
// Output normals point radially outward from origin (matches the
// natural normal for a vertex on a convex polyhedron, useful if the
// downstream consumer wants shaded vertices).

struct PolytopeUniforms {
    shape: u32,           // 0=Tetra, 1=Cube, 2=Octa, 3=Icosa, 4=Dodeca
    vert_capacity: u32,
    _pad0: u32,
    _pad1: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: PolytopeUniforms;
@group(0) @binding(1) var<storage, read_write> vert_dst: array<MeshVertex>;

const PHI: f32 = 1.618034;
const INV_PHI: f32 = 0.618034;

// Default screen-friendly polytope radius. Matches the legacy
// generators::generator_math::PROJ_SCALE so visual size is bit-
// identical against the pre-graph WireframeZoo renderer.
const POLYTOPE_DEFAULT_RADIUS: f32 = 0.25;

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

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.vert_capacity {
        return;
    }

    let nverts = vert_count_for_shape(u.shape);
    if i >= nverts {
        // Pad unused vertex slots — keeps the downstream
        // rotate/project chain from reading garbage. Edges referencing
        // these slots can't exist (the paired polytope_edges atom only
        // emits indices for the active shape).
        vert_dst[i].position = vec3<f32>(0.0);
        vert_dst[i]._pad0 = 0.0;
        vert_dst[i].normal = vec3<f32>(0.0, 1.0, 0.0);
        vert_dst[i]._pad1 = 0.0;
        return;
    }

    let raw = vert_for_shape(u.shape, i);
    let len = length(raw);
    // Normalise to POLYTOPE_DEFAULT_RADIUS (0.25), not unit sphere.
    let pos = select(raw * (POLYTOPE_DEFAULT_RADIUS / len), vec3<f32>(0.0), len < 1e-8);

    vert_dst[i].position = pos;
    vert_dst[i]._pad0 = 0.0;
    // Outward-radial normal (vertex of a convex polyhedron points away
    // from origin). Length-1 (not scaled) so downstream lighting sees a
    // unit normal regardless of the position radius.
    let normal = select(raw / len, vec3<f32>(0.0, 1.0, 0.0), len < 1e-8);
    vert_dst[i].normal = normal;
    vert_dst[i]._pad1 = 0.0;
}
