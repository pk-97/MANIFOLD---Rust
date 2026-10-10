//! `node.count_surface_edges` — positive-axis marching-cubes crossings per
//! lattice node. The inclusive scan of these counts gives each shared surface
//! edge one compact vertex slot.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::count_surface_triangles::MARCHING_CUBES_COMMON;
use crate::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

pub(crate) const SURFACE_EDGE_OWNERSHIP: &str = include_str!("shaders/surface_edge_ownership.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CountUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: CountSurfaceEdges,
    type_id: "node.count_surface_edges",
    purpose: "At each lattice node, count its positive-axis marching-cubes edges whose level-set endpoints change sign. The three bits are x, y and z, so each node contributes 0 to 3 shared surface vertices and the inclusive scan gives compact edge indices.",
    inputs: {
        levelset: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        counts: Array(u32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset and nodes_x/y/z from the same node.clamp_liquid_to_solids or node.smooth_lattice chain used by node.count_surface_triangles. Feed counts to node.running_total and wire that edge scan into node.volume_surface_mesh or node.relax_surface_mesh as edge_scan; each lattice edge is counted once at its lower endpoint, including the padded zero tail.",
    examples: [],
    picker: { label: "Count Surface Edges", category: Atom },
    summary: "Counts shared marching-cubes surface edges at lattice nodes so a later scan can index one vertex per crossing.",
    category: Geometry3D,
    role: Filter,
    aliases: ["marching cubes edges", "surface edge count", "edge crossings", "isosurface edge scan"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/count_surface_edges_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [MARCHING_CUBES_COMMON, SURFACE_EDGE_OWNERSHIP],
}

impl Primitive for CountSurfaceEdges {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "counts")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let (Some(levelset), Some(counts)) =
            (ctx.inputs.array("levelset"), ctx.outputs.array("counts"))
        else {
            return;
        };
        let Some(node_total) = nodes.iter().try_fold(1u64, |total, node| {
            node.is_finite().then(|| total.checked_mul(node.max(0.0) as u64)).flatten()
        }) else {
            ctx.error("Count Surface Edges: invalid lattice dimensions");
            return;
        };
        if nodes.iter().all(|&node| node >= 2.0) && (node_total > levelset.size / 4 || node_total > counts.size / 4) {
            ctx.error(format!(
                "Count Surface Edges: a {}×{}×{} lattice is larger than its level set",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let capacity = counts.size / 4;
        let Ok(dispatch_count) = u32::try_from(capacity) else {
            ctx.error("Count Surface Edges: level set capacity exceeds one dispatch");
            return;
        };
        if dispatch_count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = CountUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            dispatch_count,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: levelset,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: counts,
                    offset: 0,
                },
            ],
            [dispatch_count.div_ceil(256), 1, 1],
            "node.count_surface_edges",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    const CORNERS: [[u32; 3]; 8] = [
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

    fn upstream_triangle_table() -> Vec<[i32; 16]> {
        let source =
            include_str!("../../../manifold-fluids/native/flip_engine/polygonizer3d.cpp");
        let start = source
            .find("_triTable[256][16] = {")
            .expect("upstream triangle table");
        source[start..]
            .lines()
            .skip(1)
            .filter(|line| line.trim_start().starts_with('{'))
            .take(256)
            .map(|line| {
                let values: Vec<i32> = line
                    .trim()
                    .trim_start_matches('{')
                    .trim_end_matches([',', '}', ';', ' '])
                    .split(',')
                    .map(|value| value.trim().parse().expect("table entry"))
                    .collect();
                values.try_into().expect("sixteen entries")
            })
            .collect()
    }

    fn node_index(p: [u32; 3], nodes: [u32; 3]) -> usize {
        (p[0] + nodes[0] * (p[1] + nodes[1] * p[2])) as usize
    }

    fn edge_lower(cell: [u32; 3], edge: usize) -> [u32; 3] {
        let a: [u32; 3] = std::array::from_fn(|axis| cell[axis] + CORNERS[EDGES[edge].0][axis]);
        let b: [u32; 3] = std::array::from_fn(|axis| cell[axis] + CORNERS[EDGES[edge].1][axis]);
        std::array::from_fn(|axis| a[axis].min(b[axis]))
    }

    fn edge_axis(edge: usize) -> usize {
        if edge >= 8 {
            1
        } else if edge.is_multiple_of(2) {
            0
        } else {
            2
        }
    }

    fn edge_mask(p: [u32; 3], nodes: [u32; 3], values: &[f32]) -> u32 {
        let here = values[node_index(p, nodes)] < 0.0;
        let mut mask = 0;
        for axis in 0..3 {
            if p[axis] + 1 < nodes[axis] {
                let mut next = p;
                next[axis] += 1;
                if here != (values[node_index(next, nodes)] < 0.0) {
                    mask |= 1 << axis;
                }
            }
        }
        mask
    }

    fn edge_owner(cell: [u32; 3], edge: usize, nodes: [u32; 3]) -> bool {
        if nodes.iter().any(|&n| n < 2) {
            return false;
        }
        let lower = edge_lower(cell, edge);
        let last = nodes.map(|n| n - 2);
        cell == std::array::from_fn(|axis| lower[axis].min(last[axis]))
    }

    #[test]
    fn output_capacity_is_the_levelset_capacity() {
        let primitive = CountSurfaceEdges::new();
        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&primitive, "counts", &params, &[("levelset", 4096)]),
            Some(4096)
        );
        assert_eq!(
            Primitive::array_output_capacity(&primitive, "counts", &params, &[]),
            None
        );
    }

    #[test]
    fn every_triangle_table_case_matches_positive_axis_crossings() {
        let table = upstream_triangle_table();
        assert_eq!(table.len(), 256);
        let nodes = [2, 2, 2];
        for (case_index, references) in table.iter().enumerate() {
            let values: Vec<f32> = (0..8)
                .map(|node| {
                    let p = [
                        node as u32 & 1,
                        (node as u32 >> 1) & 1,
                        (node as u32 >> 2) & 1,
                    ];
                    let corner = CORNERS.iter().position(|&corner| corner == p).unwrap();
                    if case_index & (1 << corner) != 0 {
                        -1.0
                    } else {
                        1.0
                    }
                })
                .collect();
            let mut expected = HashSet::new();
            for &edge in references.iter().take_while(|&&edge| edge >= 0) {
                expected.insert(edge as usize);
            }
            let mut actual = HashSet::new();
            for z in 0..nodes[2] {
                for y in 0..nodes[1] {
                    for x in 0..nodes[0] {
                        let p = [x, y, z];
                        let mask = edge_mask(p, nodes, &values);
                        for axis in 0..3 {
                            if mask & (1 << axis) != 0 {
                                actual.insert((p, axis));
                            }
                        }
                    }
                }
            }
            let expected_positions: HashSet<([u32; 3], usize)> = expected
                .iter()
                .map(|&edge| (edge_lower([0, 0, 0], edge), edge_axis(edge)))
                .collect();
            assert_eq!(actual, expected_positions, "case {case_index}");
            assert_eq!(actual.len(), expected.len(), "case {case_index} edge count");
        }
    }

    #[test]
    fn shared_edges_have_adjacent_references_and_one_owner_at_boundaries() {
        let nodes = [4, 4, 4];
        let cells = nodes.map(|n| n - 1);
        type Edge = ([u32; 3], usize);
        let mut references: HashMap<Edge, Vec<Edge>> = HashMap::new();
        for z in 0..cells[2] {
            for y in 0..cells[1] {
                for x in 0..cells[0] {
                    let cell = [x, y, z];
                    for edge in 0..12 {
                        let lower = edge_lower(cell, edge);
                        let axis = edge_axis(edge);
                        let key = (lower, axis);
                        references.entry(key).or_default().push((cell, edge));
                    }
                }
            }
        }
        assert!(references.values().any(|entries| entries.len() > 1));
        for ((lower, axis), entries) in references {
            let expected = (0..3)
                .filter(|&other| other != axis)
                .map(|other| usize::from(lower[other] > 0 && lower[other] < nodes[other] - 1) + 1)
                .product::<usize>();
            assert_eq!(entries.len(), expected, "edge {lower:?} axis {axis}");
            let owners = entries
                .iter()
                .filter(|&&(cell, edge)| edge_owner(cell, edge, nodes))
                .count();
            assert_eq!(owners, 1, "edge {lower:?} axis {axis}");
        }
    }

    #[test]
    fn generated_count_surface_edges_shader_validates() {
        let source = manifold_node_engine::freeze::codegen::standalone_for_spec::<CountSurfaceEdges>()
            .expect("count_surface_edges standalone codegen");
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("count_surface_edges WGSL validation");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::freeze::codegen::{standalone_for_spec, ENTRY};

    fn node_index(p: [u32; 3], nodes: [u32; 3]) -> usize {
        (p[0] + nodes[0] * (p[1] + nodes[1] * p[2])) as usize
    }

    #[test]
    fn generated_count_values_match_the_cpu_fixture() {
        let nodes = [8u32; 3];
        let total = (nodes[0] * nodes[1] * nodes[2]) as usize;
        let values: Vec<f32> = (0..total)
            .map(|index| {
                let p = [
                    index as u32 % nodes[0],
                    (index as u32 / nodes[0]) % nodes[1],
                    index as u32 / (nodes[0] * nodes[1]),
                ];
                let q = [p[0] as f32 - 3.4, p[1] as f32 - 3.1, p[2] as f32 - 3.6];
                q.iter().map(|v| v * v).sum::<f32>().sqrt() - 2.7
            })
            .collect();
        let expected: Vec<u32> = (0..total)
            .map(|index| {
                let p = [
                    index as u32 % nodes[0],
                    (index as u32 / nodes[0]) % nodes[1],
                    index as u32 / (nodes[0] * nodes[1]),
                ];
                let here = values[index] < 0.0;
                (0..3)
                    .filter(|&axis| {
                        if p[axis] + 1 >= nodes[axis] {
                            return false;
                        }
                        let mut next = p;
                        next[axis] += 1;
                        here != (values[node_index(next, nodes)] < 0.0)
                    })
                    .count() as u32
            })
            .collect();

        let device = manifold_gpu::testkit::test_device();
        let in_buf = device.create_buffer_shared((total * 4) as u64);
        let out_buf = device.create_buffer_shared((total * 4) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(&values));
        }
        out_buf.zero_fill();
        let wgsl = standalone_for_spec::<CountSurfaceEdges>().expect("count surface edges codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, ENTRY, "count-surface-edges-proof");
        let uniforms = CountUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: total as u32,
        };
        let mut encoder = device.create_encoder("count surface edges proof");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &out_buf,
                    offset: 0,
                },
            ],
            [u32::try_from(total).unwrap().div_ceil(256), 1, 1],
            "count surface edges proof",
        );
        encoder.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared count buffer");
        let actual = unsafe { std::slice::from_raw_parts(ptr as *const u32, total) };
        assert_eq!(actual, expected.as_slice());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
