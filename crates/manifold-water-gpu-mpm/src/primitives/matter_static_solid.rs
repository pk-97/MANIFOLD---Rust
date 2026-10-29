//! `node.matter_static_solid` — the static collider meshes' distance and
//! normal at every node of Matter's held world tiles, cached. The mesh search
//! keeps a deep stack per thread, which starves any kernel that carries it,
//! so the per-substep passes read this cache instead: it is written for the
//! tiles a table build hands out, and for every held tile when the static
//! meshes change or move.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer};

use super::matter_common::{COLLIDER_BVH, MATTER_TREE};
use crate::matter::MATTER_REACH;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::Vec4Vertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};
use manifold_water_liquid::bodies::{LIQUID_POSE, LiquidBody, LiquidShape};
use manifold_water_liquid::world_tiles::fresh_args_offset;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StaticSolidUniforms {
    cell_size: f32,
    pool_tiles: i32,
    body_count: i32,
    static_key: i32,
    built: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: MatterStaticSolid,
    type_id: "node.matter_static_solid",
    purpose: "Cache the static collider meshes for Matter's grid: at every node of a held world tile, the nearest static mesh's signed distance, outward normal and body. Written for the tiles a table build hands out (built = 1), and for every held tile when static_key, the shapes or the trees change.",
    inputs: {
        held: Array(u32) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        collider_nodes: Array(Vec4Vertex) optional,
        collider_triangles: Array(Vec4Vertex) optional,
        tiles: Array(u32) required,
        fresh: Array(u32) required,
        held_args: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        body_count: ScalarF32 optional,
        static_key: ScalarF32 optional,
        built: ScalarF32 optional,
    },
    outputs: {
        solid: Array(Vec4Vertex),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("static_key"), label: "Static Key", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("built"), label: "Table Built", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.matter_to_grid (tiles, held, fresh, held_args, built) and node.matter_move_bodies (bodies), before node.matter_grid_update, node.matter_body_reaction and node.grid_to_matter, which read solid. shapes, collider_nodes, collider_triangles, body_count and static_key come from node.matter_domain. solid holds one element per pooled node (512 per tile slot and a canonical node last): (n·(1 + row), φ) with φ the nearest static mesh's signed distance (band four cells), n its outward unit normal and row its body, or (0, 0, 0, φ) with no mesh near. The node owns solid; a new pool clears it and rewrites every held tile.",
    examples: ["SandColumn"],
    picker: { label: "Matter Static Solid", category: Atom },
    summary: "Caches how far each grid node is from the scene's fixed meshes, so the sand can collide with them cheaply.",
    category: Particles3D,
    role: Filter,
    aliases: ["static collider cache", "matter colliders"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/matter_static_solid_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, LIQUID_POSE, COLLIDER_BVH, MATTER_TREE],
    buffer_index: "mss_node_of",
    extra_fields: {
        solid: Option<GpuBuffer> = None,
        // static_key, shapes, trees, body_count and pool the cache was last
        // written for over every held tile.
        written: Option<(i32, usize, u64, usize, i32, u32)> = None,
    },
}

impl Primitive for MatterStaticSolid {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "solid"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "solid").then_some(self.solid.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage, sized to the tile pool at run time.
        (port_name == "solid").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        let static_key = ctx.scalar_or_param("static_key", 0.0).round() as i32;
        let built = ctx.scalar_or_param("built", 0.0) > 0.5;
        let held = ctx.inputs.array("held");
        let fresh = ctx.inputs.array("fresh");
        let held_args = ctx.inputs.array("held_args");
        let tiles = ctx.inputs.array("tiles");
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
        );
        let gpu = ctx.gpu_encoder();
        let (Some(held), Some(fresh), Some(held_args), Some(tiles)) =
            (held, fresh, held_args, tiles)
        else {
            return;
        };
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error(format!(
                "Matter Static Solid: cell_size is {cell_size}; it must be a finite number above 0"
            ));
            return;
        }
        let Ok(nodes) = u32::try_from(pool_len(pool_tiles)) else {
            ctx.error(format!(
                "Matter Static Solid: a pool of {pool_tiles} tiles has more nodes than a dispatch can count"
            ));
            return;
        };
        if tiles.size < table_words(pool_tiles) * 4 {
            ctx.error(format!(
                "Matter Static Solid: tiles is smaller than a pool of {pool_tiles} tiles needs"
            ));
            return;
        }
        let bytes = u64::from(nodes) * std::mem::size_of::<Vec4Vertex>() as u64;
        if self.solid.as_ref().is_none_or(|s| s.size != bytes) {
            self.solid = None;
            self.written = None;
            match gpu.device.try_create_buffer(bytes, BufferUse::Gpu) {
                Ok(solid) => {
                    // Zero reads as no mesh at the node.
                    gpu.native_enc.clear_buffer(&solid);
                    self.solid = Some(solid);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Matter Static Solid: a pool of {pool_tiles} tiles needs storage the device cannot give: {error}. Raise Cell Size."
                    ));
                    return;
                }
            }
        }
        let solid = self.solid.as_ref().expect("allocated above");
        // Without all four collider arrays there is no static mesh: the
        // cache stays clear.
        let (bodies, shapes, collider_nodes, collider_triangles) = match colliders {
            (Some(bodies), Some(shapes), Some(nodes), Some(triangles)) if body_count > 0 => {
                (bodies, shapes, nodes, triangles)
            }
            _ => {
                if self.written.is_some_and(|w| w.4 > 0) {
                    gpu.native_enc.clear_buffer(solid);
                }
                self.written = Some((static_key, 0, 0, 0, 0, pool_tiles));
                return;
            }
        };
        let body_count =
            body_count.min((bodies.size / std::mem::size_of::<LiquidBody>() as u64) as i32);
        let key = (
            static_key,
            shapes.identity_key(),
            shapes.size,
            collider_nodes.identity_key(),
            body_count,
            pool_tiles,
        );
        let every_held = self.written != Some(key);
        if !every_held && !built {
            return;
        }
        self.written = Some(key);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = StaticSolidUniforms {
            cell_size,
            pool_tiles: pool_tiles as i32,
            body_count,
            static_key,
            built: i32::from(built),
            dispatch_count: nodes,
            _pad0: 0,
            _pad1: 0,
        };
        // Every held tile after a change, else the build's fresh tiles: the
        // list bound as `held` and the triple it is dispatched by.
        let (list, offset) = if every_held {
            (held, super::matter_to_grid::held_args_offset())
        } else {
            (fresh, fresh_args_offset(MATTER_REACH.ring_max))
        };
        let buffers = [
            list,
            bodies,
            shapes,
            collider_nodes,
            collider_triangles,
            tiles,
            fresh,
            held_args,
            solid,
        ];
        let mut bindings = vec![GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(&uniforms),
        }];
        bindings.extend(
            buffers
                .into_iter()
                .enumerate()
                .map(|(i, buffer)| GpuBinding::Buffer {
                    binding: i as u32 + 1,
                    buffer,
                    offset: 0,
                }),
        );
        gpu.native_enc.dispatch_compute_indirect(
            pipeline,
            &bindings,
            held_args,
            offset,
            "node.matter_static_solid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_static_solid_generates_a_valid_kernel() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterStaticSolid>()
                .expect("matter_static_solid codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
