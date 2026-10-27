//! `node.face_sample_component` — one axis of GPU FLIP's face lattice as the
//! seam's face array. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use crate::fluid_particles::FaceSample;
use crate::grid::face_len;
use crate::int_param;
use crate::lattice::{FlipSolverGrid, LiquidLattice};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};

pub const AXES: &[&str] = &["X (u)", "Y (v)", "Z (w)"];

/// The `axis` param: 0, 1 or 2.
pub fn axis_param(params: &ParamValues) -> Option<usize> {
    match params.get("axis") {
        Some(ParamValue::Enum(a)) => Some(*a as usize),
        None => Some(0),
        _ => None,
    }
    .filter(|&a| a < 3)
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ComponentUniforms {
    axis: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    pool_tiles: f32,
    cell_origin_x: i32,
    cell_origin_y: i32,
    cell_origin_z: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

impl ComponentUniforms {
    /// The uniform for `count` faces of `axis` on a padded lattice of `nodes`
    /// whose solver grid starts at world cell `cell_origin`, and a pool of
    /// `pool_tiles`; the standalone run and the substep history both bind
    /// this one layout.
    pub(crate) fn new(
        axis: usize,
        nodes: [u32; 3],
        pool_tiles: u32,
        cell_origin: [i32; 3],
        count: u64,
    ) -> Self {
        Self {
            axis: axis as u32,
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            pool_tiles: pool_tiles as f32,
            cell_origin_x: cell_origin[0],
            cell_origin_y: cell_origin[1],
            cell_origin_z: cell_origin[2],
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        }
    }
}

/// The world cell of a solver grid's first cell, from the grid's first node
/// (its minimum corner) and cell size. Cell p is centred on p·h (the lattice
/// origin is -h/2), so the first cell's centre, half a cell in from the first
/// node, is p·h. The grid a domain publishes sits on whole world cells; the
/// rounding only absorbs f32 noise.
pub fn solver_origin_cell(first_node: [f32; 3], cell_size: f32) -> [i32; 3] {
    first_node.map(|x| (x / cell_size + 0.5).round() as i32)
}

/// The `pool_tiles` wire, else its param: the pool's capacity in tiles.
pub fn pool_tiles_wire(read: impl Fn(&str, f32) -> f32) -> Result<u32, String> {
    let value = read("pool_tiles", 1.0);
    if value.fract() == 0.0 && (1.0..=MAX_POOL_TILES).contains(&value) {
        Ok(value as u32)
    } else {
        Err(format!(
            "pool_tiles is {value}; it must be a whole tile count from 1 to {MAX_POOL_TILES}"
        ))
    }
}

const MAX_POOL_TILES: f32 = 16_777_216.0;

/// Bytes the pooled faces and the world-tile table must hold for a pool of
/// `pool_tiles` slots.
pub fn pooled_bytes(pool_tiles: u32) -> (u64, u64) {
    (
        pool_len(pool_tiles) * size_of::<FaceSample>() as u64,
        table_words(pool_tiles) * 4,
    )
}

manifold_node_engine::primitive! {
    name: FaceSampleComponent,
    type_id: "node.face_sample_component",
    purpose: "Copy one axis of a face grid (node.gpu_flip_step's faces, pooled by its world-tile table: tiles and pool_tiles, one FaceSample per world cell) into the liquid seam's face array for that axis: (cells + 1) along the axis by cells on the other two, x fastest, velocity in m/s. nodes_x/y/z are the padded lattice's (node.gpu_flip_domain's), so GPU FLIP has nodes − 4 solver cells per axis. These wires retain the authored lattice contract; use the domain nodes, not frame grid_nodes. cell_origin_x/y/z are the world cell of the solver grid's first cell (the domain's cell_origin_x/y/z): face f of the array is the pooled record of world cell cell_origin + f, so a face in a tile the table does not hold reads the canonical record. Faces with weight 0 read 0. The storage holds one float per face grid record, a little more than the axis has faces; readers size the axis from the lattice.",
    inputs: {
        faces: Array(FaceSample) required,
        tiles: Array(u32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        cell_origin_x: ScalarF32 optional, cell_origin_y: ScalarF32 optional, cell_origin_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("axis"), label: "Axis", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: AXES },
        float_param!("nodes_x", "Nodes X", 71.0, 8.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 71.0, 8.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 71.0, 8.0, 1024.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        int_param!("cell_origin_x", "Cell Origin X", -33.0, -16_777_216.0, 16_777_216.0),
        int_param!("cell_origin_y", "Cell Origin Y", -1.0, -16_777_216.0, 16_777_216.0),
        int_param!("cell_origin_z", "Cell Origin Z", -33.0, -16_777_216.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Three of them, one per axis, on node.gpu_flip_step's pooled faces, tiles and pool_tiles publish GPU FLIP's face grid as three axis arrays (face_u, face_v, face_w) for consumers of the liquid seam that read axes; whitewater reads the packed faces directly. Wire tiles and pool_tiles from the step beside faces, and nodes_x/y/z and cell_origin_x/y/z from the domain, so the arrays follow Resolution and the water box: their storage follows the nodes, their place in the world the cell origin.",
    examples: [],
    picker: { label: "Face Grid Component", category: Atom },
    summary: "Hands one direction of the water's velocity grid to effects that follow the water.",
    category: Particles3D,
    role: Filter,
    aliases: ["face grid", "velocity component", "mac faces"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/face_sample_component_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::MultipleOf { input: "faces", factor: 1 },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for FaceSampleComponent {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port != "out" {
            return None;
        }
        // One float per record of the dense face grid the nodes name; the
        // pooled faces input is not that size.
        let read = |name: &str, default: f32| match params.get(name) {
            Some(ParamValue::Float(v)) => *v,
            _ => default,
        };
        let lattice = LiquidLattice::from_scalars(read).ok()?;
        let cells = FlipSolverGrid::from_lattice(lattice).cells();
        Some(cells.iter().map(|&n| n + 1).product())
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(axis) = axis_param(ctx.params) else {
            ctx.error("Face Grid Component: the axis must be X, Y or Z".to_string());
            return;
        };
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Face Grid Component") else {
            return;
        };
        let cells = FlipSolverGrid::from_lattice(lattice).cells();
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(tiles), Some(out)) = (
            ctx.inputs.array("faces"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let pool_tiles = match pool_tiles_wire(|name, default| ctx.scalar_or_param(name, default)) {
            Ok(pool_tiles) => pool_tiles,
            Err(refusal) => {
                ctx.error(format!("Face Grid Component: {refusal}"));
                return;
            }
        };
        let count = face_len(cells, axis);
        let (face_bytes, table_bytes) = pooled_bytes(pool_tiles);
        if count * 4 > out.size || face_bytes > faces.size || table_bytes > tiles.size {
            ctx.error(format!(
                "Face Grid Component: a {cells:?} cell lattice with {pool_tiles} pool tiles is larger than its arrays"
            ));
            return;
        }
        let cell_origin = ["cell_origin_x", "cell_origin_y", "cell_origin_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0).round() as i32);
        let uniforms =
            ComponentUniforms::new(axis, lattice.nodes(), pool_tiles, cell_origin, count);
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: faces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.face_sample_component",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_origin_cell_inverts_the_snapped_grid() {
        // The first node of a grid whose first cell is world cell p sits half a
        // cell below p·h, whatever the cell size and the sign of p.
        for h in [0.0625f32, 0.05, 0.003, 1.0] {
            for p in [[-33, -1, 7], [0, 0, 0], [-4000, 12, 250]] {
                let first_node =
                    p.map(|c| (f64::from(c) * f64::from(h) - 0.5 * f64::from(h)) as f32);
                assert_eq!(solver_origin_cell(first_node, h), p, "h {h}");
            }
        }
    }

    /// The component's uniform is its params in order, then the count, padded
    /// to four words.
    #[test]
    fn component_uniform_follows_the_params() {
        let words: [u32; 12] =
            bytemuck::cast(ComponentUniforms::new(1, [71, 72, 73], 9, [-5, 6, -7], 33));
        let float = |v: f32| v.to_bits();
        assert_eq!(
            words,
            [
                1,
                float(71.0),
                float(72.0),
                float(73.0),
                float(9.0),
                (-5i32) as u32,
                6,
                (-7i32) as u32,
                33,
                0,
                0,
                0
            ]
        );
        let names: Vec<_> =
            <FaceSampleComponent as manifold_node_engine::primitive::PrimitiveSpec>::PARAMS
                .iter()
                .map(|p| p.name.as_ref())
                .collect();
        assert_eq!(
            names,
            [
                "axis",
                "nodes_x",
                "nodes_y",
                "nodes_z",
                "pool_tiles",
                "cell_origin_x",
                "cell_origin_y",
                "cell_origin_z"
            ]
        );
    }
}
