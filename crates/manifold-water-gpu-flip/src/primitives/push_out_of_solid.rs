//! `node.push_out_of_solid` — move liquid particles out of a negative solid
//! distance field on the display's world tiles. The particle frame stays a
//! generated buffer atom: each particle reads the solid through the table and
//! writes one `FluidParticle`.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

/// Codegen uniform layout: the two params in `PARAMS`, then the injected
/// element count and padding to a 16-byte multiple.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PushUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

const _: () = assert!(std::mem::size_of::<PushUniforms>() == 16);

manifold_node_engine::primitive! {
    name: PushOutOfSolid,
    type_id: "node.push_out_of_solid",
    purpose: "Push each liquid particle out of a negative solid distance field on the display's world tiles. The solid is a node array, node p at (p - 1/2) cells, sampled manually and trilinearly at the particle position through the table; a central-difference gradient supplies the outward normal, and a particle with phi < 0 receives up to four fixed normalized-gradient projections by -phi. A node in a tile the table does not hold reads the canonical solid, 1e30, no solid. Position is the only changed field: radius, velocity and id pass through unchanged.",
    inputs: {
        particles: Array(FluidParticle) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the liquid solver's particle output and before surface extraction or any collider-sensitive particle consumer. Wire solid from node.liquid_solid_distance and tiles, pool_tiles and cell_size from the same node.surface_tiles. The field values must be signed distance in scene metres (negative inside). Up to four fixed normalized-gradient projections reduce curved-field and interpolation error. A negative sample with a zero or non-finite central-difference gradient is left unchanged: without a normal there is no safe direction to push, so callers should provide a valid signed-distance field rather than relying on this atom to invent one.",
    examples: [],
    picker: { label: "Push Out Of Solid", category: Atom },
    summary: "Moves liquid particles out of solid regions while keeping their size, velocity and identity.",
    category: Particles3D,
    role: Filter,
    aliases: ["push particles out", "solid projection", "particle collision", "remove penetration"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/push_out_of_solid_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for PushOutOfSolid {
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
                    .find(|(name, _)| *name == "particles")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Push Out Of Solid") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(solid), Some(tiles), Some(out)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count =
            (particles.size.min(out.size) / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        if solid.size < pool.elements() * 4 {
            ctx.error(format!(
                "Push Out Of Solid: a pool of {} tiles needs {} solid values; solid holds {}",
                pool.pool_tiles,
                pool.elements(),
                solid.size / 4
            ));
            return;
        }
        let uniforms = PushUniforms {
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.push_out_of_solid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_wgsl_has_gathered_solid_and_particle_output() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<PushOutOfSolid>()
            .expect("push_out_of_solid codegen");
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
