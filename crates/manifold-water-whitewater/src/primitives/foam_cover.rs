//! `node.foam_cover` — per field-lattice node, the fraction of the water
//! surface foam covers, made from the foam particles. Each live particle is a
//! disc of area pi r^2 on the surface; independent discs cover
//! `1 - exp(-tau)` of it, `tau` being the kernel-smoothed disc area per unit
//! area at the node.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_storage::TILE_GRID;

// The kernel after the grid helpers it indexes through.
static SHADER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("{TILE_GRID}\n{}", include_str!("shaders/foam_cover.wgsl"))
});
const THREADS: u32 = 64;

/// Field order identical to the shader's `Params`; every row is 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CoverUniforms {
    lattice_min: [f32; 3],
    h: f32,
    spacing: [f32; 3],
    strength: f32,
    nodes: [u32; 3],
    particle_capacity: u32,
    node_count: u32,
    pad_a: u32,
    pad_b: u32,
    pad_c: u32,
}

/// The shader's constants from the node's inputs. `nodes` are the field
/// lattice's node counts as wired (whole numbers, at least 2 per axis); the
/// kernel's smoothing length is the smallest node spacing.
fn cover_uniforms(
    center: [f32; 3],
    size: [f32; 3],
    nodes: [f32; 3],
    strength: f32,
    particle_capacity: u32,
) -> Result<CoverUniforms, String> {
    if nodes.iter().any(|n| !(n.is_finite() && n.round() >= 2.0)) {
        return Err(format!(
            "Foam Cover: a {}×{}×{} field lattice has fewer than 2 nodes on an axis. Wire nodes_x/y/z from the water field's lattice.",
            nodes[0], nodes[1], nodes[2]
        ));
    }
    let nodes = nodes.map(|n| n.round() as u32);
    if center.iter().any(|v| !v.is_finite()) || size.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
        return Err("Foam Cover: the lattice box must be finite with a positive size".into());
    }
    if !(strength.is_finite() && strength >= 0.0) {
        return Err("Foam Cover: strength must be finite and at least 0".into());
    }
    let node_count = nodes
        .iter()
        .try_fold(1u32, |total, &n| total.checked_mul(n))
        .ok_or_else(|| {
            format!(
                "Foam Cover: a {}×{}×{} field lattice has more nodes than an index can address",
                nodes[0], nodes[1], nodes[2]
            )
        })?;
    let spacing: [f32; 3] = std::array::from_fn(|a| size[a] / (nodes[a] - 1) as f32);
    let h = spacing.into_iter().fold(f32::INFINITY, f32::min);
    Ok(CoverUniforms {
        lattice_min: std::array::from_fn(|a| center[a] - 0.5 * size[a]),
        h,
        spacing,
        strength,
        nodes,
        particle_capacity,
        node_count,
        pad_a: 0,
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

manifold_node_engine::primitive! {
    name: FoamCover,
    type_id: "node.foam_cover",
    purpose: "Per node of the water's field lattice, the fraction of the surface foam covers, 0 to 1, from the foam particles. Each live particle (a slot with radius 0 is empty) is a disc of foam of area pi r^2 lying on the surface; discs placed independently cover 1 - exp(-tau) of it, with tau the expected disc area per unit area at the node: strength times the sum over particles of pi r^2 W(distance), W the 2D cubic-spline kernel normalised over a plane, with smoothing length the lattice's smallest node spacing, reaching two spacings.",
    inputs: {
        particles: Array(FluidParticle) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        strength: ScalarF32 optional,
    },
    outputs: {
        cover: Array(f32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Field Nodes X", 2.0, 2.0, 16384.0),
        float_param!("nodes_y", "Field Nodes Y", 2.0, 2.0, 16384.0),
        float_param!("nodes_z", "Field Nodes Z", 2.0, 2.0, 16384.0),
        float_param!("strength", "Foam", 1.0, 0.0, 4.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Takes the displayed foam particles (node.interpolate_particle_frames of the frame's foam) and the water field's lattice; its cover feeds the water object's field_foam.",
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
        // Provided storage: a one-record hint, sized to the lattice at run time.
        (port == "cover").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let gpu = ctx.gpu_encoder();
            if self.clear_pipeline.is_none() {
                self.clear_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER.as_str(),
                    "cs_clear",
                    "node.foam_cover",
                ));
            }
            if self.scatter_pipeline.is_none() {
                self.scatter_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER.as_str(),
                    "cs_scatter",
                    "node.foam_cover",
                ));
            }
            if self.resolve_pipeline.is_none() {
                self.resolve_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER.as_str(),
                    "cs_resolve",
                    "node.foam_cover",
                ));
            }
        }
        let Some(particles) = ctx.inputs.array("particles") else {
            ctx.error("Foam Cover: particles must be wired");
            return;
        };
        let center =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0));
        let capacity = u32::try_from(particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .unwrap_or(u32::MAX);
        let uniforms = match cover_uniforms(
            center,
            size,
            nodes,
            ctx.scalar_or_param("strength", 1.0),
            capacity,
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let node_count = u64::from(uniforms.node_count);

        let buffers = {
            let gpu = ctx.gpu_encoder();
            let accum = ensure_buffer(&mut self.accum, gpu.device, node_count * 4);
            let cover = ensure_buffer(&mut self.cover, gpu.device, node_count * 4);
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
            GpuBinding::Buffer {
                binding: 1,
                buffer: particles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &cover,
                offset: 0,
            },
        ];
        let node_groups = [uniforms.node_count.div_ceil(THREADS), 1, 1];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.clear_pipeline
                .as_ref()
                .expect("clear pipeline prepared"),
            &bindings,
            node_groups,
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
            node_groups,
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
        assert_eq!(std::mem::size_of::<CoverUniforms>(), 64);
    }

    #[test]
    fn smoothing_length_is_the_smallest_spacing_of_a_non_cubic_lattice() {
        let u = cover_uniforms(
            [0.5, -1.0, 2.0],
            [2.0, 1.5, 2.5],
            [33.0, 25.0, 41.0],
            2.0,
            100,
        )
        .unwrap();
        assert_eq!(u.h, 0.0625);
        assert_eq!(u.lattice_min, [-0.5, -1.75, 0.75]);
        assert_eq!(u.spacing, [0.0625, 0.0625, 0.0625]);
        assert_eq!(u.node_count, 33 * 25 * 41);
        assert_eq!(u.strength, 2.0);
    }

    #[test]
    fn a_bad_lattice_or_strength_is_an_error() {
        assert!(cover_uniforms([0.0; 3], [2.0; 3], [33.0, 1.0, 41.0], 1.0, 10).is_err());
        assert!(cover_uniforms([0.0; 3], [2.0, 0.0, 2.0], [33.0; 3], 1.0, 10).is_err());
        assert!(cover_uniforms([0.0; 3], [2.0; 3], [33.0; 3], -1.0, 10).is_err());
        assert!(cover_uniforms([0.0; 3], [2.0; 3], [33.0; 3], f32::NAN, 10).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
