//! GPU value proofs for the face grid producers
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.2 (Grid outputs), I16):
//! each solver's lattice, filled with a velocity field at its own sample
//! positions, comes out of its component atom as that field at the seam's
//! face positions (`liquid::grid::face_position`).

use crate::water::primitives::face_sample_component::FaceSampleComponent;
use crate::testkit::liquid_surface::{Harness, params, read};
use crate::water::primitives::matter_face_component::MatterFaceComponent;
use crate::exec::effect_node::ParamValues;
use crate::water::fluid_particles::FaceSample;
use crate::water::liquid::grid::{face_coords, face_len, face_position};
use crate::water::liquid::lattice::PADDING_NODES;
use crate::water::matter::MatterGridNode;
use crate::parameters::ParamValue;
use crate::primitive::Primitive;

/// Unequal sides, so a swapped axis shows.
const N: [u32; 3] = [6, 5, 4];
const H: f32 = 0.25;
/// The authored box's minimum corner, m.
const MIN: [f32; 3] = [-0.5, 0.1, 0.3];

/// v(x) = c + A·x, in m/s.
struct Field {
    c: [f64; 3],
    a: [[f64; 3]; 3],
}

impl Field {
    fn uniform() -> Self {
        Self { c: [0.7, -1.3, 0.4], a: [[0.0; 3]; 3] }
    }

    /// A linear field with every shear term set.
    fn shear() -> Self {
        Self { c: [0.2, -0.5, 0.9], a: [[0.3, 1.1, -0.7], [-0.4, 0.25, 0.8], [0.6, -0.9, -0.15]] }
    }

    fn at(&self, x: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|a| self.c[a] + (0..3).map(|b| self.a[a][b] * x[b]).sum::<f64>())
    }
}

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn component_params(nodes: [u32; 3], axis: u32) -> ParamValues {
    let mut p = params(&[("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32)]);
    p.insert("axis".into(), ParamValue::Enum(axis));
    p
}

fn run_component<P: Primitive>(
    harness: &mut Harness,
    prim: &mut P,
    input: (&'static str, crate::bindings::Slot),
    len: usize,
    step_params: &ParamValues,
) -> Vec<f32> {
    let out = harness.array::<f32>(&[], len);
    let (_, errors) = harness.run(prim, &[input], &[("out", out.0)], step_params);
    assert!(errors.is_empty(), "{errors:?}");
    read(&out.1, len)
}

/// Each seam face of `got` against `want(axis, face)`, within 1e-5 m/s.
fn assert_faces(axis: usize, got: &[f32], want: impl Fn([u32; 3]) -> f64, what: &str) {
    assert_eq!(got.len() as u64, face_len(N, axis));
    for (i, &g) in got.iter().enumerate() {
        let f = face_coords(N, axis, i);
        let w = want(f);
        assert!((f64::from(g) - w).abs() <= 1e-5, "{what} axis {axis} face {f:?}: {g} vs {w}");
    }
}

fn seam_position(axis: usize, f: [u32; 3]) -> [f64; 3] {
    face_position(MIN, H, axis, f).map(f64::from)
}

/// GPU FLIP's lattice: (n+1)³ padded cells, face a of cell p at
/// m + p·h with ½ on the other two axes; weight 1 where the face exists.
fn gpu_flip_lattice(field: &Field) -> Vec<FaceSample> {
    let m = N.map(|n| n + 1);
    (0..m.iter().product::<u32>())
        .map(|i| {
            let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
            let mut s = FaceSample::default();
            for a in 0..3 {
                if (0..3).all(|b| b == a || p[b] < N[b]) {
                    s.velocity[a] = field.at(seam_position(a, p))[a] as f32;
                    s.weight[a] = 1.0;
                }
            }
            s
        })
        .collect()
}

/// The padded lattice over the box: n + 7 nodes, the first at m − 3h. The
/// MPM component reads its box from it.
fn lattice_nodes() -> [u32; 3] {
    N.map(|n| n + 1 + 2 * PADDING_NODES)
}

/// The padded lattice whose native FLIP solver grid has N cells: GPU FLIP
/// solves on nodes − 4 cells.
fn gpu_flip_nodes() -> [u32; 3] {
    N.map(|n| n + 4)
}

fn node_position(p: [u32; 3]) -> [f64; 3] {
    std::array::from_fn(|b| f64::from(MIN[b]) + (f64::from(p[b]) - f64::from(PADDING_NODES)) * f64::from(H))
}

fn matter_grid(field: &Field) -> Vec<MatterGridNode> {
    let n = lattice_nodes();
    (0..n.iter().product::<u32>())
        .map(|i| {
            let p = [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])];
            let v = field.at(node_position(p));
            MatterGridNode { velocity_mass: [v[0] as f32, v[1] as f32, v[2] as f32, 1.0], ..Default::default() }
        })
        .collect()
}

/// I16 for GPU FLIP: a uniform and a sheared field come out at the seam's face
/// positions.
#[test]
fn liquid_face_grid_layout_gpu_flip() {
    let mut harness = Harness::new();
    for (name, field) in [("uniform", Field::uniform()), ("shear", Field::shear())] {
        let lattice = gpu_flip_lattice(&field);
        let input = harness.array(&lattice, lattice.len());
        for axis in 0..3 {
            let got = run_component(
                &mut harness,
                &mut FaceSampleComponent::new(),
                ("faces", input.0),
                face_len(N, axis) as usize,
                &component_params(gpu_flip_nodes(), axis as u32),
            );
            assert_faces(axis, &got, |f| field.at(seam_position(axis, f))[axis], &format!("GPU FLIP {name}"));
        }
    }
}

/// I16 for MPM: the mean of the four nodes around each face centre is the
/// field there, for a linear field exactly.
#[test]
fn liquid_face_grid_layout_matter() {
    let mut harness = Harness::new();
    let nodes = lattice_nodes();
    for (name, field) in [("uniform", Field::uniform()), ("shear", Field::shear())] {
        let grid = matter_grid(&field);
        let input = harness.array(&grid, grid.len());
        for axis in 0..3 {
            let got = run_component(
                &mut harness,
                &mut MatterFaceComponent::new(),
                ("grid", input.0),
                face_len(N, axis) as usize,
                &component_params(nodes, axis as u32),
            );
            assert_faces(axis, &got, |f| field.at(seam_position(axis, f))[axis], &format!("MPM {name}"));
        }
    }
}

/// A GPU FLIP face with weight 0 reads 0, whatever velocity it holds.
#[test]
fn face_sample_component_zeroes_unweighted_faces() {
    let mut harness = Harness::new();
    let mut rng = Rng(0x5eed_face);
    let mut lattice = gpu_flip_lattice(&Field::shear());
    for s in &mut lattice {
        for a in 0..3 {
            if rng.unit() < 0.4 {
                s.weight[a] = 0.0;
                s.velocity[a] = 99.0;
            }
        }
    }
    let input = harness.array(&lattice, lattice.len());
    let m = N.map(|n| n + 1);
    let mut zeroed = 0;
    for axis in 0..3 {
        let got = run_component(
            &mut harness,
            &mut FaceSampleComponent::new(),
            ("faces", input.0),
            face_len(N, axis) as usize,
            &component_params(gpu_flip_nodes(), axis as u32),
        );
        assert_faces(
            axis,
            &got,
            |f| {
                let s = lattice[(f[0] + m[0] * (f[1] + m[1] * f[2])) as usize];
                if s.weight[axis] > 0.0 { f64::from(s.velocity[axis]) } else { 0.0 }
            },
            "GPU FLIP weights",
        );
        zeroed += got.iter().filter(|&&v| v == 0.0).count();
    }
    assert!(zeroed > 100, "the fixture zeroes faces, got {zeroed}");
}

/// An MPM face reads the mean over its nodes with mass only; with none, 0.
#[test]
fn matter_face_component_skips_empty_nodes() {
    let mut harness = Harness::new();
    let mut rng = Rng(0x0e_4d7e);
    let mut grid = matter_grid(&Field::shear());
    for node in &mut grid {
        if rng.unit() < 0.5 {
            node.velocity_mass = [55.0, -55.0, 55.0, 0.0];
        }
    }
    let input = harness.array(&grid, grid.len());
    let n = lattice_nodes();
    let pad = PADDING_NODES;
    let mut empty = 0;
    for axis in 0..3 {
        let got = run_component(
            &mut harness,
            &mut MatterFaceComponent::new(),
            ("grid", input.0),
            face_len(N, axis) as usize,
            &component_params(n, axis as u32),
        );
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        assert_faces(
            axis,
            &got,
            |f| {
                let (mut sum, mut hits) = (0.0, 0);
                for (db, dc) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let mut q = f.map(|x| x + pad);
                    q[b] += db;
                    q[c] += dc;
                    let node = grid[(q[0] + n[0] * (q[1] + n[1] * q[2])) as usize].velocity_mass;
                    if node[3] > 0.0 {
                        sum += f64::from(node[axis]);
                        hits += 1;
                    }
                }
                if hits == 0 { 0.0 } else { sum / f64::from(hits) }
            },
            "MPM masses",
        );
        empty += got.iter().filter(|&&v| v == 0.0).count();
    }
    assert!(empty > 5, "the fixture empties faces, got {empty}");
}
