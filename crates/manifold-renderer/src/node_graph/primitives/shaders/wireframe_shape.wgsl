// node.generate_platonic_solid — emit one of five Platonic solid
// vertex sets (Tetrahedron / Cube / Octahedron / Icosahedron /
// Dodecahedron) into an Array<MeshVertex>.
//
// Vertex positions ported from wireframe_zoo.rs (unnormalised; the
// CPU code normalised them at render time to the unit sphere — this
// primitive does the normalisation in-shader so the output is
// directly comparable across shapes).
//
// Output normals point radially outward from origin (matches the
// natural normal for a vertex on a convex polyhedron, useful if the
// downstream consumer wants shaded vertices).

struct PlatonicUniforms {
    shape: u32,        // 0=Tetra, 1=Cube, 2=Octa, 3=Icosa, 4=Dodeca
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: PlatonicUniforms;
@group(0) @binding(1) var<storage, read_write> dst: array<MeshVertex>;

const PHI: f32 = 1.618034;
const INV_PHI: f32 = 0.618034;

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

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }

    let nverts = vert_count_for_shape(u.shape);
    if i >= nverts {
        dst[i].position = vec3<f32>(0.0);
        dst[i]._pad0 = 0.0;
        dst[i].normal = vec3<f32>(0.0, 1.0, 0.0);
        dst[i]._pad1 = 0.0;
        return;
    }

    let raw = vert_for_shape(u.shape, i);
    let len = length(raw);
    let pos = select(raw / len, vec3<f32>(0.0), len < 1e-8);

    dst[i].position = pos;
    dst[i]._pad0 = 0.0;
    // Outward-radial normal (vertex of a convex polyhedron points away
    // from origin). Same as the normalised position for convex shapes.
    dst[i].normal = pos;
    dst[i]._pad1 = 0.0;
}
