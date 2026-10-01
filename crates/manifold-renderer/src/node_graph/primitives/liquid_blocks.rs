//! `node.liquid_blocks` — the block occupancy map
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.10 (block occupancy map)).
//! A per-element gather on the codegen path, one thread per 4³ block.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::liquid::blocks::{LIQUID_BLOCKS_WGSL, block_total};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{cell_total, grid_cells, grid_nodes, refinement};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlocksUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    level_nodes_x: f32,
    level_nodes_y: f32,
    level_nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
}

/// Blocks over the solid lattice the params name, for output sizing.
fn param_blocks(params: &ParamValues) -> Option<u32> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => 71,
    });
    u32::try_from(block_total(grid_cells(nodes)?)).ok()
}

crate::primitive! {
    name: LiquidBlocks,
    type_id: "node.liquid_blocks",
    purpose: "The block occupancy map of a liquid: for each 4×4×4 block of the domain's cells (the solid lattice read as nodes − 1 cells a side; edge blocks partial), one u32 with bit 0 LIQUID (some cell's water > 0), bit 1 SURFACE (the refined level set has a node < 0 and a node >= 0 in the block's closed footprint) and bit 2 SOLID (some solid node < 0 in the block's closed footprint). A clear bit guarantees absence; a set bit means the block may hold it.",
    inputs: {
        water: Array(f32) required,
        level_set: Array(f32) required,
        solid: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        level_nodes_x: ScalarF32 optional, level_nodes_y: ScalarF32 optional, level_nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("level_nodes_x", "Level Nodes X", 211.0, 2.0, 16385.0),
        float_param!("level_nodes_y", "Level Nodes Y", 211.0, 2.0, 16385.0),
        float_param!("level_nodes_z", "Level Nodes Z", 211.0, 2.0, 16385.0),
    ],
    depth_rule: Terminal,
    composition_notes: "In the Liquid Surface group: water from node.cells_with_particles over the group's sort (bins must be the domain's cells), level_set and level_nodes_x/y/z from the group's level set, solid and nodes_x/y/z from the particle frame. Feed out to node.surface_crossings' blocks.",
    examples: [],
    picker: { label: "Liquid Blocks", category: Atom },
    summary: "Marks which blocks of a liquid's grid hold water, surface or walls, so later steps can skip the empty ones.",
    category: Particles3D,
    role: Filter,
    aliases: ["occupancy map", "block map", "sparse blocks"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/liquid_blocks_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_BLOCKS_WGSL],
}

impl Primitive for LiquidBlocks {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        if port != "out" {
            return None;
        }
        // The lattice may arrive on wires the planner cannot read; a block
        // per solid node covers any lattice the solid array holds.
        if let Some(&(_, nodes)) = inputs.iter().find(|(name, _)| *name == "solid") {
            return Some(nodes);
        }
        param_blocks(params)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let levels = ["level_nodes_x", "level_nodes_y", "level_nodes_z"].map(|name| ctx.scalar_or_param(name, 211.0).round().max(0.0) as u32);
        if let Err(message) = refinement(nodes, levels) {
            ctx.error(format!("Liquid Blocks: {message}"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(water), Some(level_set), Some(solid), Some(out)) =
            (ctx.inputs.array("water"), ctx.inputs.array("level_set"), ctx.inputs.array("solid"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = nodes.map(|n| n - 1);
        let count = block_total(cells);
        if cell_total(cells) * 4 > water.size {
            ctx.error(format!("Liquid Blocks: water holds fewer than the {cells:?} cells; bin its sort by the domain's cells"));
            return;
        }
        if count * 4 > out.size || cell_total(nodes) * 4 > solid.size || cell_total(levels) * 4 > level_set.size {
            ctx.error(format!("Liquid Blocks: a {nodes:?}-node grid with a {levels:?} level set is larger than its arrays"));
            return;
        }
        let uniforms = BlocksUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            level_nodes_x: levels[0] as f32,
            level_nodes_y: levels[1] as f32,
            level_nodes_z: levels[2] as f32,
            dispatch_count: count as u32,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: level_set, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.liquid_blocks",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::liquid::lattice::LiquidLattice;
    use crate::node_graph::fluid::domain_layout;
    use crate::node_graph::whitewater::MAX_REFINEMENT;
    use crate::node_graph::primitives::particle_volume::refined_nodes;

    /// At 64 the 70³ cells make 18³ blocks; the output, sized from the solid
    /// array or the params, holds them, and every read of the last block
    /// lands inside water, solid and the level set at every refinement.
    #[test]
    fn liquid_block_extents_at_64() {
        let nodes = LiquidLattice::from_layout(&domain_layout(None, 4.0, 64).expect("layout")).nodes();
        let cells = grid_cells(nodes).expect("cells");
        let blocks = block_total(cells);
        assert_eq!(blocks, 18 * 18 * 18);
        let mut params = ParamValues::default();
        for (name, n) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().zip(nodes) {
            params.insert(name.into(), ParamValue::Float(n as f32));
        }
        let by_params = LiquidBlocks::new().array_output_capacity("out", &params, &[]).expect("capacity");
        let by_solid = LiquidBlocks::new().array_output_capacity("out", &params, &[("solid", cell_total(nodes) as u32)]).expect("capacity");
        assert_eq!(u64::from(by_params), blocks);
        assert!(u64::from(by_solid) >= blocks);
        // The last block's far closed corner is the lattice's last node.
        let last_cell = cells.map(|n| u64::from(n - 1));
        assert!(last_cell[0] + u64::from(cells[0]) * (last_cell[1] + u64::from(cells[1]) * last_cell[2]) < cell_total(cells));
        for s in 1..=MAX_REFINEMENT {
            let level = refined_nodes(nodes.map(|n| n as f32), s);
            assert_eq!(refinement(nodes, level), Ok(s));
            let far = cells.map(|n| u64::from(n * s));
            let n = level.map(u64::from);
            assert!(far[0] + n[0] * (far[1] + n[1] * far[2]) < cell_total(level), "Surface Detail {s}");
        }
        assert_eq!(blocks.div_ceil(256), 23, "workgroups per map");
    }
}
