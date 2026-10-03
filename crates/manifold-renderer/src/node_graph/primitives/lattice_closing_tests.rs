//! f64 geometric reference: triangulate the grown zero surface with the
//! vendored FLIP table, then measure every node against every triangle.
//! Deliberately no band-local search, sparse schedule or shader arithmetic.
#[derive(Clone, Copy)]
struct DVec3([f64; 3]);
impl DVec3 {
    const X: Self = Self([1.0, 0.0, 0.0]);
    fn new(x: f64, y: f64, z: f64) -> Self {
        Self([x, y, z])
    }
    fn splat(v: f64) -> Self {
        Self([v; 3])
    }
    fn dot(self, other: Self) -> f64 {
        (0..3).map(|i| self.0[i] * other.0[i]).sum()
    }
    fn cross(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| {
            self.0[(i + 1) % 3] * other.0[(i + 2) % 3] - self.0[(i + 2) % 3] * other.0[(i + 1) % 3]
        }))
    }
    fn length_squared(self) -> f64 {
        self.dot(self)
    }
    fn length(self) -> f64 {
        self.length_squared().sqrt()
    }
    fn distance(self, other: Self) -> f64 {
        (self - other).length()
    }
    fn lerp(self, other: Self, t: f64) -> Self {
        self + (other - self) * t
    }
}
impl std::ops::Add for DVec3 {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] + other.0[i]))
    }
}
impl std::ops::Sub for DVec3 {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] - other.0[i]))
    }
}
impl std::ops::Mul<f64> for DVec3 {
    type Output = Self;
    fn mul(self, other: f64) -> Self {
        Self(self.0.map(|v| v * other))
    }
}

const CORNERS: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 0, 1],
    [0, 0, 1],
    [0, 1, 0],
    [1, 1, 0],
    [1, 1, 1],
    [0, 1, 1],
];
const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];
fn point(i: usize, n: usize, h: f64) -> DVec3 {
    DVec3::new((i % n) as f64, ((i / n) % n) as f64, (i / (n * n)) as f64) * h
}
fn table() -> Vec<Vec<usize>> {
    let source = include_str!("../../../../manifold-fluids/native/flip_engine/polygonizer3d.cpp");
    let start = source
        .find("_triTable[256][16] = {")
        .expect("upstream MC table");
    source[start..]
        .lines()
        .skip(1)
        .filter(|line| line.trim_start().starts_with('{'))
        .take(256)
        .map(|line| {
            line.trim()
                .trim_matches(['{', '}', ',', ';', ' '])
                .split(',')
                .map(|v| v.trim().parse::<i32>().unwrap())
                .take_while(|v| *v >= 0)
                .map(|v| v as usize)
                .collect()
        })
        .collect()
}
fn triangles(values: &[f64], n: usize, h: f64) -> Vec<[DVec3; 3]> {
    let table = table();
    let mut out = Vec::new();
    for z in 0..n - 1 {
        for y in 0..n - 1 {
            for x in 0..n - 1 {
                let ids = CORNERS.map(|[a, b, c]| x + a + n * (y + b + n * (z + c)));
                let case = (0..8)
                    .filter(|&c| values[ids[c]] < 0.)
                    .fold(0, |v, c| v | (1 << c));
                for edges in table[case].chunks_exact(3) {
                    out.push(std::array::from_fn(|k| {
                        let (a, b) = EDGES[edges[k]];
                        let (a, b) = (ids[a], ids[b]);
                        point(a, n, h).lerp(point(b, n, h), values[a] / (values[a] - values[b]))
                    }));
                }
            }
        }
    }
    out
}
// Closest-point regions from Ericson's Real-Time Collision Detection, 5.1.5.
fn triangle_distance(p: DVec3, [a, b, c]: [DVec3; 3]) -> f64 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0. && d2 <= 0. {
        return ap.length();
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0. && d4 <= d3 {
        return bp.length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0. && d1 >= 0. && d3 <= 0. {
        return (p - (a + ab * (d1 / (d1 - d3)))).length();
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0. && d5 <= d6 {
        return cp.length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0. && d2 >= 0. && d6 <= 0. {
        return (p - (a + ac * (d2 / (d2 - d6)))).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0. && d4 - d3 >= 0. && d5 - d6 >= 0. {
        return (p - (b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))))).length();
    }
    let normal = ab.cross(ac);
    if normal.length_squared() == 0. {
        return ap.length().min(bp.length()).min(cp.length());
    }
    ap.dot(normal).abs() / normal.length()
}
fn redistance(values: &[f64], n: usize, h: f64, band: f64) -> Vec<f64> {
    let mesh = triangles(values, n, h);
    values
        .iter()
        .enumerate()
        .map(|(i, &phi)| {
            let p = point(i, n, h);
            let distance = mesh
                .iter()
                .map(|&tri| triangle_distance(p, tri))
                .fold(band, f64::min);
            if phi < 0. { -distance } else { distance }
        })
        .collect()
}
fn close(values: &[f64], n: usize, h: f64, distance: f64) -> Vec<f64> {
    if distance == 0. {
        return values.to_vec();
    }
    let grown: Vec<_> = values.iter().map(|v| v - distance).collect();
    redistance(&grown, n, h, distance + 2. * h)
        .into_iter()
        .map(|v| v + distance)
        .collect()
}
fn spheres(n: usize, h: f64, centres: &[DVec3], radius: f64) -> Vec<f64> {
    (0..n * n * n)
        .map(|i| {
            centres
                .iter()
                .map(|c| point(i, n, h).distance(*c) - radius)
                .fold(f64::INFINITY, f64::min)
        })
        .collect()
}
fn height(values: &[f64], n: usize, h: f64, x: usize, z: usize) -> f64 {
    for y in (0..n - 1).rev() {
        let a = values[x + n * (y + n * z)];
        let b = values[x + n * (y + 1 + n * z)];
        if a < 0. && b >= 0. {
            return (y as f64 + a / (a - b)) * h;
        }
    }
    panic!("no top crossing at {x}/{z}");
}
#[test]
fn fluid_fill_pits_f64_closing_fills_two_sphere_pit_and_preserves_lone_radius() {
    let n = 16;
    let h = 0.25;
    let center = DVec3::splat(2.);
    let field = spheres(
        n,
        h,
        &[center - DVec3::X * 0.6, center + DVec3::X * 0.6],
        0.8,
    );
    let closed = close(&field, n, h, 0.45);
    let before = height(&field, n, h, 8, 8);
    let after = height(&closed, n, h, 8, 8);
    assert!(after > before + 0.055, "pit {before} -> {after}");
    let lone = spheres(n, h, &[center], 0.8);
    let lone_closed = close(&lone, n, h, 0.45);
    let radius = height(&lone_closed, n, h, 8, 8) - 2.;
    assert!(
        (radius - 0.8).abs() < 0.025,
        "MC discretization: radius {radius}"
    );
}
#[test]
fn fluid_fill_pits_f64_jittered_sheet_flattens_and_zero_is_bit_identical() {
    let n = 16;
    let h = 0.2;
    let centres: Vec<_> = (0..7)
        .flat_map(|z| {
            (0..7).map(move |x| {
                let hash = (x * 13 + z * 17) % 11;
                DVec3::new(
                    x as f64 * 0.5 + 0.03 * hash as f64,
                    1. + 0.006 * (hash as f64 - 5.),
                    z as f64 * 0.5,
                )
            })
        })
        .collect();
    let field = spheres(n, h, &centres, 0.36);
    let closed = close(&field, n, h, 0.4);
    let roughness = |v: &[f64]| {
        let heights: Vec<_> = (4..12)
            .flat_map(|z| (4..12).map(move |x| height(v, n, h, x, z)))
            .collect();
        let mean = heights.iter().sum::<f64>() / heights.len() as f64;
        (heights.iter().map(|y| (y - mean).powi(2)).sum::<f64>() / heights.len() as f64).sqrt()
    };
    assert!(
        roughness(&closed) < roughness(&field) * 0.85,
        "sheet {} -> {}",
        roughness(&field),
        roughness(&closed)
    );
    let values = [-0.0, 0.0, -1., f64::from_bits(0x7ff8000000000042)];
    assert_eq!(
        close(&values, 0, 0., 0.)
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        values.map(f64::to_bits)
    );
}
#[test]
fn fluid_fill_pits_standalone_shaders_validate() {
    use crate::node_graph::freeze::codegen::standalone_for_spec;
    for source in [
        standalone_for_spec::<super::offset_lattice::OffsetLattice>().unwrap(),
        standalone_for_spec::<super::redistance_lattice::RedistanceLattice>().unwrap(),
    ] {
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}
#[cfg(feature = "gpu-proofs")]
#[path = "lattice_closing_gpu_tests.rs"]
mod gpu_tests;

fn fused_redistance_offset() -> String {
    use crate::node_graph::NodeInstanceId;
    use crate::node_graph::freeze::codegen::{
        FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use crate::node_graph::primitive::PrimitiveSpec;
    fn member<P: PrimitiveSpec>(id: u32, input: InputSource) -> RegionNode<'static> {
        RegionNode {
            node_id: NodeInstanceId(id),
            fusion_kind: P::FUSION_KIND,
            body: P::WGSL_BODY.unwrap(),
            params: P::PARAMS,
            inputs: vec![input],
            input_access: P::INPUT_ACCESS.to_vec(),
            node_inputs: P::INPUTS,
            node_outputs: P::OUTPUTS,
            node_includes: P::WGSL_INCLUDES,
            derived_uniforms: P::DERIVED_UNIFORMS,
            type_id: P::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba32float",
            stencil_fetch: false,
            quantize_f16: false,
        }
    }
    generate_fused(&FusionRegion {
        nodes: vec![
            member::<super::redistance_lattice::RedistanceLattice>(0, InputSource::External(0)),
            member::<super::offset_lattice::OffsetLattice>(1, InputSource::Node(NodeInstanceId(0))),
        ],
        num_external_inputs: 1,
        outputs: vec![(NodeInstanceId(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: Some(crate::node_graph::freeze::classify::CapacityExpr::Slot(0)),
    })
    .expect("redistance and shrink fuse")
    .wgsl
}
#[test]
fn fluid_fill_pits_fused_shader_validates() {
    let source = fused_redistance_offset();
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
}
