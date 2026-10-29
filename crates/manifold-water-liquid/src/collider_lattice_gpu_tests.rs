//! `collider_lattice.wgsl` fills a moving shape's atlas block with what the
//! CPU rule gives on its `MeshBvh` at every node.

use std::sync::Arc;

use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::scene::transform::Transform;
use manifold_physics::TriangleMesh;
use manifold_physics::mesh_bvh::MeshBvh;

use crate::bodies::{BodiesStatus, LiquidBodies};
use crate::body_buffers::LiquidBodyBuffers;
use crate::fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry};

/// A closed box about the origin with half sides `half`, outward wound.
fn box_mesh(half: [f32; 3]) -> TriangleMesh {
    let v = |x: usize, y: usize, z: usize| {
        [
            [-half[0], half[0]][x],
            [-half[1], half[1]][y],
            [-half[2], half[2]][z],
        ]
    };
    TriangleMesh {
        vertices: vec![
            v(0, 0, 0),
            v(1, 0, 0),
            v(1, 1, 0),
            v(0, 1, 0),
            v(0, 0, 1),
            v(1, 0, 1),
            v(1, 1, 1),
            v(0, 1, 1),
        ],
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [0, 4, 7],
            [0, 7, 3],
        ],
    }
}

/// A bodies set with one moving Inflow over a scaled cube at `cell`.
fn bodies_at(geometry: &Arc<PreparedFluidGeometry>, cell: f32) -> LiquidBodies {
    let role = FluidRole {
        geometry: Arc::clone(geometry),
        kind: FluidRoleKind::Inflow,
        transform: Transform {
            scale: [1.5, 1.0, 0.75],
            ..Transform::default()
        },
        enabled: true,
        velocity: [0.0; 3],
        inherit_motion: 0.0,
        friction: 0.0,
    };
    let mut bodies = LiquidBodies::with_regions();
    assert_eq!(
        bodies.prepare(&[Some(role)], &[], cell, true),
        Ok(BodiesStatus::Ready)
    );
    bodies
}

#[test]
fn gpu_lattice_matches_the_cpu_rule_at_every_node() {
    let geometry = Arc::new(PreparedFluidGeometry::new(vec![box_mesh([0.5; 3])]));
    // A cell size whose lattice has an odd node count, so the tail half runs.
    let (bodies, cell) = (0..100)
        .map(|step| 0.113f32 + 0.001 * step as f32)
        .map(|cell| (bodies_at(&geometry, cell), cell))
        .find(|(bodies, _)| bodies.lattice_builds()[0].nodes() % 2 == 1)
        .expect("some cell gives an odd node count");
    let build = bodies.lattice_builds()[0].clone();
    let nodes = build.nodes() as usize;
    assert!(nodes > 2000, "{nodes} nodes at cell {cell}");
    assert_eq!(bodies.atlas_words(), build.words());

    let device = manifold_gpu::testkit::test_device();
    let mut native = device.create_encoder("collider_lattice proof");
    let mut buffers = LiquidBodyBuffers::default();
    buffers
        .upload(
            &mut GpuEncoder::new(&mut native, &device),
            &bodies,
            false,
            "collider_lattice proof",
        )
        .expect("the atlas is admitted");
    native.commit_and_wait().expect("GPU work completes");
    let atlas = buffers.atlas().expect("an atlas");
    let bytes = manifold_gpu::testkit::read_bytes(atlas, build.words() as usize * 4);
    let words: &[u32] = bytemuck::cast_slice(&bytes);

    let bvh = MeshBvh::build(&geometry.meshes).unwrap();
    let (band, shell) = (build.band(), build.spacing);
    let [dx, dy, _] = build.dims.map(|n| n as usize);
    let half_of = |n: usize| {
        let word = words[build.word_base as usize + n / 2];
        half::f16::from_bits((word >> (16 * (n % 2))) as u16).to_f32()
    };
    let mut checked = 0;
    let (mut inside, mut outside) = (0, 0);
    for n in 0..nodes {
        let index = [n % dx, (n / dx) % dy, n / (dx * dy)];
        let p: [f32; 3] =
            std::array::from_fn(|a| build.origin[a] + build.spacing * index[a] as f32);
        let d = bvh.nearest(p, band);
        // A node on the surface sits on the rule's seam between inside and shell.
        if d < 1e-3 {
            continue;
        }
        let w = bvh.winding(p).abs();
        let expected = if (w - 0.5).abs() < 0.25 {
            d - shell
        } else if w > 0.5 {
            -d
        } else {
            d
        };
        let got = half_of(n);
        let tolerance = 2e-3 * expected.abs() + 1e-4;
        assert!(
            (got - expected).abs() <= tolerance,
            "node {index:?}: got {got}, expected {expected}"
        );
        checked += 1;
        inside += usize::from(expected < 0.0);
        outside += usize::from(expected > 0.0);
    }
    // The lattice lines up with the cube, so its face nodes are skipped.
    assert!(checked > nodes * 3 / 4, "{checked} of {nodes} nodes");
    assert!(inside > 0 && outside > 0, "both sides of the surface ran");
    // The odd tail: the last word's high half is +infinity.
    assert!(half_of(nodes).is_infinite() && half_of(nodes) > 0.0);
}
