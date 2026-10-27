//! `node.foam_cover` — per node of the display's world tiles, the fraction of
//! the water surface foam covers, made from the foam particles. Each live
//! particle is a disc of area pi r^2 on the surface; independent discs cover
//! `1 - exp(-tau)` of it, `tau` being the kernel-smoothed disc area per unit
//! area at the node.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::{TilePool, tile_pool};

use super::PARTICLE_SPLAT;

// The read side and the shared splat shape, then the kernel.
static SHADER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{PARTICLE_SPLAT}\n{}",
        include_str!("shaders/foam_cover.wgsl")
    )
});
const THREADS: u32 = 64;

/// Field order identical to the shader's `Params`; two 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CoverUniforms {
    h: f32,
    strength: f32,
    pool_tiles: u32,
    particle_capacity: u32,
    elements: u32,
    radius: f32,
    pad_b: u32,
    pad_c: u32,
}

/// The shader's constants for `pool`: the kernel's smoothing length is the
/// display cell size.
fn cover_uniforms(
    pool: TilePool,
    strength: f32,
    radius: f32,
    particle_capacity: u32,
) -> Result<CoverUniforms, String> {
    if !(strength.is_finite() && strength >= 0.0) {
        return Err("Foam Cover: strength must be finite and at least 0".into());
    }
    if !(radius.is_finite() && radius >= 0.0) {
        return Err("Foam Cover: radius must be finite and at least 0".into());
    }
    let elements = u32::try_from(pool.elements()).map_err(|_| {
        format!(
            "Foam Cover: a pool of {} tiles has more elements than an index can address",
            pool.pool_tiles
        )
    })?;
    Ok(CoverUniforms {
        h: pool.cell_size,
        strength,
        pool_tiles: pool.pool_tiles,
        particle_capacity,
        elements,
        radius,
        pad_b: 0,
        pad_c: 0,
    })
}

/// (Re)allocate `slot` when its size changes; never otherwise.
fn ensure_buffer(
    slot: &mut Option<GpuBuffer>,
    device: &manifold_gpu::GpuDevice,
    bytes: u64,
) -> Result<GpuBuffer, String> {
    let bytes = bytes.max(16);
    if slot.as_ref().is_none_or(|buffer| buffer.size != bytes) {
        *slot = Some(
            device
                .try_create_buffer_shared(bytes)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(slot.as_ref().expect("buffer was just allocated").clone())
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: FoamCover,
    type_id: "node.foam_cover",
    purpose: "Per node of the display's world tiles, the fraction of the surface foam covers, 0 to 1, from the foam particles. Each live particle (a slot with fade 0 is empty) is a disc of foam of radius r, the Foam Radius times the particle's fade (its w, 0 to 1), and area pi r^2 lying on the surface; discs placed independently cover 1 - exp(-tau) of it, with tau the expected disc area per unit area at the node: strength times the sum over particles of pi r^2 W(distance), W the 2D cubic-spline kernel normalised over a plane, with smoothing length the display cell size, reaching two cells. Node p is at (p - 1/2) cells; a node in a tile the table does not hold takes nothing, and the canonical element reads 0.",
    inputs: {
        particles: Array(FluidParticle) required,
        tiles: Array(u32) required,
        strength: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        cover: Array(f32),
    },
    params: [
        float_param!("strength", "Foam", 1.0, 0.0, 4.0),
        // A foam particle at full fade stands for a disc of foam 5 mm in radius.
        float_param!("radius", "Foam Radius", 0.005, 0.0, 0.1),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Takes the displayed foam particles (node.interpolate_particle_frames of the frame's foam) and tiles, pool_tiles and cell_size from node.surface_tiles, the table those foam particles helped build; its cover feeds the water object's field_foam.",
    examples: [],
    picker: { label: "Foam Cover", category: Atom },
    summary: "Turns the foam particles into how much of the water surface foam covers.",
    category: Particles3D,
    role: Filter,
    aliases: ["foam cover", "foam coverage", "whitewater cover"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        clear_pipeline: Option<GpuComputePipeline> = None,
        scatter_pipeline: Option<GpuComputePipeline> = None,
        resolve_pipeline: Option<GpuComputePipeline> = None,
        accum: Option<GpuBuffer> = None,
        cover: Option<GpuBuffer> = None,
    },
}

impl Primitive for FoamCover {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "cover"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "cover" => self.cover.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the pool at run time.
        (port == "cover").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let gpu = ctx.gpu_encoder();
            for (slot, entry) in [
                (&mut self.clear_pipeline, "cs_clear"),
                (&mut self.scatter_pipeline, "cs_scatter"),
                (&mut self.resolve_pipeline, "cs_resolve"),
            ] {
                if slot.is_none() {
                    *slot = Some(gpu.device.create_compute_pipeline(
                        SHADER.as_str(),
                        entry,
                        "node.foam_cover",
                    ));
                }
            }
        }
        let pool = match tile_pool(ctx, "Foam Cover") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(tiles)) =
            (ctx.inputs.array("particles"), ctx.inputs.array("tiles"))
        else {
            ctx.error("Foam Cover: particles and tiles must be wired");
            return;
        };
        let capacity = u32::try_from(particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .unwrap_or(u32::MAX);
        let uniforms = match cover_uniforms(
            pool,
            ctx.scalar_or_param("strength", 1.0),
            ctx.scalar_or_param("radius", 0.005),
            capacity,
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let element_bytes = u64::from(uniforms.elements) * 4;

        let buffers = {
            let gpu = ctx.gpu_encoder();
            let accum = ensure_buffer(&mut self.accum, gpu.device, element_bytes);
            let cover = ensure_buffer(&mut self.cover, gpu.device, element_bytes);
            accum.and_then(|a| cover.map(|c| (a, c)))
        };
        let (accum, cover) = match buffers {
            Ok(buffers) => buffers,
            Err(error) => {
                ctx.error(format!("Foam Cover: output allocation failed: {error}"));
                return;
            }
        };

        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            buffer(1, particles),
            buffer(2, tiles),
            buffer(3, &accum),
            buffer(4, &cover),
        ];
        let element_groups = [uniforms.elements.div_ceil(THREADS), 1, 1];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.clear_pipeline
                .as_ref()
                .expect("clear pipeline prepared"),
            &bindings,
            element_groups,
            "node.foam_cover.clear",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        if capacity > 0 {
            gpu.native_enc.dispatch_compute(
                self.scatter_pipeline
                    .as_ref()
                    .expect("scatter pipeline prepared"),
                &bindings,
                [capacity.div_ceil(THREADS), 1, 1],
                "node.foam_cover.scatter",
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
        gpu.native_enc.dispatch_compute(
            self.resolve_pipeline
                .as_ref()
                .expect("resolve pipeline prepared"),
            &bindings,
            element_groups,
            "node.foam_cover.resolve",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("foam cover shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("foam cover shader validates");
        for entry in ["cs_clear", "cs_scatter", "cs_resolve"] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<CoverUniforms>(), 32);
    }

    #[test]
    fn the_smoothing_length_is_the_display_cell_over_a_pool_of_any_size() {
        let pool = TilePool {
            pool_tiles: 13,
            cell_size: 0.0625,
        };
        let u = cover_uniforms(pool, 2.0, 0.004, 100).unwrap();
        assert_eq!(u.h, 0.0625);
        assert_eq!(u.elements, 13 * 512 + 1);
        assert_eq!(u.pool_tiles, 13);
        assert_eq!(u.particle_capacity, 100);
        assert_eq!(u.strength, 2.0);
        assert_eq!(u.radius, 0.004);
    }

    #[test]
    fn a_bad_strength_or_radius_is_an_error() {
        let pool = TilePool {
            pool_tiles: 1,
            cell_size: 0.1,
        };
        assert!(cover_uniforms(pool, -1.0, 0.004, 10).is_err());
        assert!(cover_uniforms(pool, 1.0, -1.0, 10).is_err());
        assert!(cover_uniforms(pool, f32::NAN, 0.004, 10).is_err());
        assert!(cover_uniforms(pool, 1.0, f32::NAN, 10).is_err());
        assert!(cover_uniforms(pool, f32::INFINITY, 0.004, 10).is_err());
        assert!(cover_uniforms(pool, 1.0, f32::INFINITY, 10).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
