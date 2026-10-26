//! `node.carry_liquid_into_solids` — the water field's last solid step when
//! the drawn surface should meet a wall flush. Inside a drawn solid the field
//! is carried in from the wall's surface for `carry_cells` field cells (air
//! beyond that); inside any other solid it is clipped to max(value, 0), the
//! same clip applies: max(value, 0) inside the solid. With `carry_cells` 0 the
//! output is exactly that clip.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::field_build::{FIELD_REDISTANCE_CELLS, SOLID_CARRY_CELLS};
use manifold_water_liquid::float_param;

const BODY: &str = include_str!("shaders/carry_liquid_into_solids.wgsl");
const THREADS: u32 = 64;

/// Field order identical to the shader's `CarryParams`; every row is 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CarryUniforms {
    lattice_min: [f32; 3],
    cell: f32,
    spacing: [f32; 3],
    carry_depth: f32,
    nodes: [u32; 3],
    node_count: u32,
    solid_spacing: [f32; 3],
    air: f32,
    solid_nodes: [u32; 3],
    drawn_len: u32,
}

fn whole_nodes(name: &str, nodes: [f32; 3]) -> Result<[u32; 3], String> {
    if nodes.iter().any(|n| !(n.is_finite() && n.round() >= 2.0)) {
        return Err(format!(
            "Carry Liquid Into Solids: a {}×{}×{} {name} lattice has fewer than 2 nodes on an axis",
            nodes[0], nodes[1], nodes[2]
        ));
    }
    Ok(nodes.map(|n| n.round() as u32))
}

/// The shader's constants from the node's inputs. The field cell is the level
/// set's smallest node spacing; the carry depth and the air value are
/// multiples of it.
fn carry_uniforms(
    center: [f32; 3],
    size: [f32; 3],
    nodes: [f32; 3],
    solid_nodes: [f32; 3],
    carry_cells: f32,
    drawn_len: u32,
) -> Result<CarryUniforms, String> {
    let nodes = whole_nodes("level set", nodes)?;
    let solid_nodes = whole_nodes("solid", solid_nodes)?;
    if center.iter().any(|v| !v.is_finite()) || size.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
        return Err(
            "Carry Liquid Into Solids: the lattice box must be finite with a positive size".into(),
        );
    }
    if !carry_cells.is_finite() {
        return Err("Carry Liquid Into Solids: carry_cells must be finite".into());
    }
    let node_count = nodes
        .iter()
        .try_fold(1u32, |total, &n| total.checked_mul(n))
        .ok_or_else(|| {
            format!(
                "Carry Liquid Into Solids: a {}×{}×{} lattice has more nodes than an index can address",
                nodes[0], nodes[1], nodes[2]
            )
        })?;
    let spacing: [f32; 3] = std::array::from_fn(|a| size[a] / (nodes[a] - 1) as f32);
    let cell = spacing.into_iter().fold(f32::INFINITY, f32::min);
    Ok(CarryUniforms {
        lattice_min: std::array::from_fn(|a| center[a] - 0.5 * size[a]),
        cell,
        spacing,
        carry_depth: carry_cells.max(0.0) * cell,
        nodes,
        node_count,
        solid_spacing: std::array::from_fn(|a| size[a] / (solid_nodes[a] - 1) as f32),
        air: FIELD_REDISTANCE_CELLS * cell,
        solid_nodes,
        drawn_len,
    })
}

manifold_node_engine::primitive! {
    name: CarryLiquidIntoSolids,
    type_id: "node.carry_liquid_into_solids",
    purpose: "Clip the liquid level set against its solid lattice, carrying the water into the solids the scene draws so the drawn surface meets a wall flush. At each level-set node with solid distance sd (negative inside, trilinear on the solid lattice): outside solids the value is kept. Inside a carried solid (drawn_solid negative at the node, or every solid when drawn_solid is unwired) with sd above -carry_cells field cells, the value is the level set sampled trilinearly at the solid's surface point along the solid's normal plus half a field cell into the water; deeper it is air, FIELD_REDISTANCE_CELLS field cells. Inside any other solid, and inside every solid when carry_cells is 0, the value is max(value, 0), the clip rule. The field cell is the level set's smallest node spacing; the normal is the normalised central-difference gradient of solid at half a solid spacing (a zero gradient clips).",
    inputs: {
        levelset: Array(f32) required,
        solid: Array(f32) required,
        drawn_solid: Array(f32) optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        solid_nodes_x: ScalarF32 optional, solid_nodes_y: ScalarF32 optional, solid_nodes_z: ScalarF32 optional,
        carry_cells: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_x", "Solid Nodes X", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_y", "Solid Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("solid_nodes_z", "Solid Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("carry_cells", "Carry Cells", SOLID_CARRY_CELLS, 0.0, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the smoothing chain. Wire levelset from the last smoothing pass, nodes_x/y/z from node.particle_volume's volume_nodes_x/y/z, and solid, solid_nodes_x/y/z and center/size from the same wires node.particle_volume takes. drawn_solid is optional and lives on the solid lattice (same nodes, same box): negative inside the solids the scene draws. Unwired, every solid carries water; wired, only drawn solids do and the rest (the lattice padding, invisible colliders) still clip. carry_cells 0 gives the plain clamp. levelset must hold the level-set lattice, solid the solid lattice, and drawn_solid exactly as many values as solid, or nothing runs, a named error. It reads the level set at arbitrary nodes, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Carry Liquid Into Solids", category: Atom },
    summary: "Keeps the liquid surface out of walls and bodies, and lets the water reach the walls the scene draws so the surface meets them flush.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid carry", "wall flush", "clamp liquid to solids", "level set carry"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        carry_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for CarryLiquidIntoSolids {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if self.carry_pipeline.is_none() {
            let gpu = ctx.gpu_encoder();
            self.carry_pipeline = Some(gpu.device.create_compute_pipeline(
                BODY,
                "cs_main",
                "node.carry_liquid_into_solids",
            ));
        }
        let (Some(levelset), Some(solid), Some(out)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("solid"),
            ctx.outputs.array("out"),
        ) else {
            ctx.error("Carry Liquid Into Solids: levelset and solid must be wired and the output allocated");
            return;
        };
        let drawn = ctx.inputs.array("drawn_solid");
        let drawn_len = drawn.map_or(0, |buffer| (buffer.size / 4) as u32);
        let center =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0));
        let solid_nodes = ["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0));
        let uniforms = match carry_uniforms(
            center,
            size,
            nodes,
            solid_nodes,
            ctx.scalar_or_param("carry_cells", SOLID_CARRY_CELLS),
            drawn_len,
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let node_count = u64::from(uniforms.node_count);
        if levelset.size / 4 < node_count || out.size / 4 < node_count {
            ctx.error(format!(
                "Carry Liquid Into Solids: levelset holds {} values and the output {}; a {}×{}×{} lattice needs {node_count}. Wire nodes_x/y/z from the same producer as levelset.",
                levelset.size / 4,
                out.size / 4,
                uniforms.nodes[0],
                uniforms.nodes[1],
                uniforms.nodes[2]
            ));
            return;
        }
        let solid_total = uniforms
            .solid_nodes
            .iter()
            .map(|&n| u64::from(n))
            .product::<u64>();
        if solid.size / 4 < solid_total {
            ctx.error(format!(
                "Carry Liquid Into Solids: solid holds {} values; a {}×{}×{} solid lattice needs {solid_total}. Wire solid_nodes_x/y/z from the same producer as solid.",
                solid.size / 4,
                uniforms.solid_nodes[0],
                uniforms.solid_nodes[1],
                uniforms.solid_nodes[2]
            ));
            return;
        }
        if drawn.is_some() && u64::from(drawn_len) != solid.size / 4 {
            ctx.error(format!(
                "Carry Liquid Into Solids: drawn_solid holds {drawn_len} values but solid holds {}; it must be on the solid lattice.",
                solid.size / 4
            ));
            return;
        }
        let bindings = [
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
                buffer: solid,
                offset: 0,
            },
            // Unwired: the solid stands in, never read because drawn_len is 0.
            GpuBinding::Buffer {
                binding: 3,
                buffer: drawn.unwrap_or(solid),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: out,
                offset: 0,
            },
        ];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.carry_pipeline
                .as_ref()
                .expect("carry pipeline prepared"),
            &bindings,
            [uniforms.node_count.div_ceil(THREADS), 1, 1],
            "node.carry_liquid_into_solids",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(BODY).expect("carry shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("carry shader validates");
        assert!(
            module
                .entry_points
                .iter()
                .any(|point| point.name == "cs_main")
        );
        assert_eq!(std::mem::size_of::<CarryUniforms>(), 80);
    }

    #[test]
    fn distances_follow_the_smallest_node_spacing_of_a_non_cubic_lattice() {
        // Spacings 0.125, 0.125 and 0.1: the cell is the 0.1.
        let u = carry_uniforms(
            [0.5, -1.0, 2.0],
            [2.0, 1.5, 2.5],
            [17.0, 13.0, 26.0],
            [9.0, 7.0, 11.0],
            2.0,
            0,
        )
        .unwrap();
        assert!((u.cell - 0.1).abs() < 1e-6, "{}", u.cell);
        assert!((u.carry_depth - 0.2).abs() < 1e-6);
        assert!((u.air - FIELD_REDISTANCE_CELLS * 0.1).abs() < 1e-6);
        assert_eq!(u.lattice_min, [-0.5, -1.75, 0.75]);
        assert_eq!(u.nodes, [17, 13, 26]);
        assert_eq!(u.node_count, 17 * 13 * 26);
        assert_eq!(u.solid_nodes, [9, 7, 11]);
        assert!((u.solid_spacing[0] - 0.25).abs() < 1e-6);
        assert_eq!(
            carry_uniforms([0.0; 3], [2.0; 3], [17.0; 3], [9.0; 3], 0.0, 5)
                .unwrap()
                .carry_depth,
            0.0
        );
    }

    #[test]
    fn a_lattice_with_one_node_on_an_axis_is_an_error() {
        assert!(carry_uniforms([0.0; 3], [2.0; 3], [17.0, 1.0, 9.0], [9.0; 3], 2.0, 0).is_err());
        assert!(carry_uniforms([0.0; 3], [2.0; 3], [17.0; 3], [9.0, 9.0, 1.0], 2.0, 0).is_err());
        assert!(carry_uniforms([0.0; 3], [0.0, 2.0, 2.0], [17.0; 3], [9.0; 3], 2.0, 0).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "carry_liquid_into_solids_gpu_tests.rs"]
mod gpu_tests;
