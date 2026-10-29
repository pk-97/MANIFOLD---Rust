//! `collider_bvh.wgsl` answers what `MeshBvh` answers on the CPU, for two
//! trees concatenated into one buffer the way every Water shares them.

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuDevice};
use manifold_physics::TriangleMesh;
use manifold_physics::mesh_bvh::MeshBvh;

use crate::bodies::COLLIDER_BVH as LIBRARY;

const HARNESS: &str = r"
struct Params {
    bases: vec4<u32>,
    band: f32,
    shell: f32,
    _pad0: f32,
    _pad1: f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> tris: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> points: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> out: array<f32>;

fn cb_node(i: u32) -> vec4<f32> { return nodes[i]; }
fn cb_triangle(i: u32) -> vec4<f32> { return tris[i]; }

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= arrayLength(&points) {
        return;
    }
    let point = points[i];
    let second = point.w > 0.5;
    let node_base = select(params.bases.x, params.bases.z, second);
    let triangle_base = select(params.bases.y, params.bases.w, second);
    out[3u * i] = cb_nearest(node_base, triangle_base, point.xyz, params.band);
    out[3u * i + 1u] = cb_winding(node_base, triangle_base, point.xyz);
    out[3u * i + 2u] = cb_phi(node_base, triangle_base, point.xyz, params.band, params.shell);
}
";

/// The unit cube [0, 1]³, each face split into `n` × `n` quads and wound
/// outward, without the faces in `skip` (axis, side).
fn cube(n: usize, skip: Option<(usize, usize)>) -> TriangleMesh {
    let mut mesh = TriangleMesh {
        vertices: Vec::new(),
        triangles: Vec::new(),
    };
    for axis in 0..3 {
        for side in 0..2 {
            if skip == Some((axis, side)) {
                continue;
            }
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let corner = |i: usize, j: usize| {
                let mut p = [0.0f32; 3];
                p[axis] = side as f32;
                p[u] = i as f32 / n as f32;
                p[v] = j as f32 / n as f32;
                p
            };
            for i in 0..n {
                for j in 0..n {
                    let quad = [
                        corner(i, j),
                        corner(i + 1, j),
                        corner(i + 1, j + 1),
                        corner(i, j + 1),
                    ];
                    for [a, b, c] in [[0, 1, 2], [0, 2, 3]] {
                        let (pa, mut pb, mut pc) = (quad[a], quad[b], quad[c]);
                        let e1: [f32; 3] = std::array::from_fn(|k| pb[k] - pa[k]);
                        let e2: [f32; 3] = std::array::from_fn(|k| pc[k] - pa[k]);
                        let normal = e1[u] * e2[v] - e1[v] * e2[u];
                        let outward = if side == 1 { 1.0 } else { -1.0 };
                        if normal * outward < 0.0 {
                            std::mem::swap(&mut pb, &mut pc);
                        }
                        let base = mesh.vertices.len() as u32;
                        mesh.vertices.extend([pa, pb, pc]);
                        mesh.triangles.push([base, base + 1, base + 2]);
                    }
                }
            }
        }
    }
    mesh
}

fn shared(device: &GpuDevice, data: &[[f32; 4]]) -> GpuBuffer {
    let buffer = device.create_buffer((data.len() * 16) as u64, BufferUse::Upload);
    // SAFETY: a shared buffer of exactly these vec4s; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
    buffer
}

/// The design's rule (WATER_COLLIDERS_DESIGN.md C3) on the CPU queries.
fn cpu_phi(bvh: &MeshBvh, p: [f32; 3], band: f32, shell: f32) -> f32 {
    let d = bvh.nearest(p, band);
    let w = bvh.winding(p).abs();
    if (w - 0.5).abs() < 0.25 {
        d - shell
    } else if w > 0.5 {
        -d
    } else {
        d
    }
}

#[test]
fn gpu_queries_match_the_cpu_tree_across_concatenated_trees() {
    let trees = [
        MeshBvh::build(&[cube(8, None)]).unwrap(),
        MeshBvh::build(&[cube(4, Some((1, 1)))]).unwrap(),
    ];
    let (mut nodes, mut tris) = (Vec::new(), Vec::new());
    let mut bases = [0u32; 4];
    for (index, tree) in trees.iter().enumerate() {
        bases[2 * index] = (nodes.len() / 4) as u32;
        bases[2 * index + 1] = (tris.len() / 3) as u32;
        tree.gpu_nodes(&mut nodes);
        tree.gpu_triangles(&mut tris);
    }
    let (band, shell) = (0.3f32, 0.05f32);
    // A 9³ grid over the cube grown by a half, off every face plane.
    let mut points = Vec::new();
    for tree in 0..2 {
        for k in 0..9 {
            for j in 0..9 {
                for i in 0..9 {
                    let at = |n: usize| -0.5 + 0.0137 + n as f32 * 0.25;
                    points.push([at(i), at(j), at(k), tree as f32]);
                }
            }
        }
    }

    let device = manifold_gpu::testkit::test_device();
    let shader = format!("{LIBRARY}\n{HARNESS}");
    let pipeline = device.create_compute_pipeline(&shader, "main", "collider_bvh proof");
    let nodes_buffer = shared(&device, &nodes);
    let tris_buffer = shared(&device, &tris);
    let points_buffer = shared(&device, &points);
    let out = device.create_buffer((points.len() * 12) as u64, BufferUse::Upload);
    let mut params = [0u32; 8];
    params[..4].copy_from_slice(&bases);
    params[4] = band.to_bits();
    params[5] = shell.to_bits();
    let mut enc = device.create_encoder("collider_bvh proof");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::cast_slice(&params),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &nodes_buffer,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &tris_buffer,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &points_buffer,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &out,
                offset: 0,
            },
        ],
        [(points.len() as u32).div_ceil(64), 1, 1],
        "collider_bvh proof",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = out.mapped_ptr().expect("a shared buffer");
    // SAFETY: a shared buffer of 3 f32 per point; its GPU work has completed.
    let got: &[f32] =
        bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, points.len() * 12) });

    let mut shells = 0;
    for (index, point) in points.iter().enumerate() {
        let tree = &trees[point[3] as usize];
        let p = [point[0], point[1], point[2]];
        let (nearest, winding, phi) = (got[3 * index], got[3 * index + 1], got[3 * index + 2]);
        assert!(
            (nearest - tree.nearest(p, band)).abs() < 1e-5,
            "{point:?}: nearest {nearest}"
        );
        assert!(
            (winding - tree.winding(p)).abs() < 1e-3,
            "{point:?}: winding {winding}"
        );
        assert!(
            (phi - cpu_phi(tree, p, band, shell)).abs() < 1e-5,
            "{point:?}: phi {phi}"
        );
        shells += usize::from((tree.winding(p).abs() - 0.5).abs() < 0.25);
    }
    // The open box puts points on its shell side of the rule, so both
    // branches of C3 ran.
    assert!(shells > 0);
}
