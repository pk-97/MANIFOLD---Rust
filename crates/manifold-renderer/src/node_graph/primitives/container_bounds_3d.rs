//! `node.container_bounds_3d` — post-integration hard containment for 3D
//! particles. The position-bounds policy atom: toroidal wrap (None) or
//! SDF reflect + clamp (Cube / Sphere / Torus).
//!
//! The 3D sibling of `node.wrap_particles_torus` (which only does the
//! torus case). Bit-exact (position-wise) with the containment step of
//! the legacy fused `node.fluid_simulate_3d`. The legacy kernel also
//! wrote a reflected `velocity` on bounce, but nothing in the fluid sim
//! reads particle velocity — that write was dead state and is dropped.
//!
//! Pair downstream of `node.euler_step_particles_3d`; the soft
//! pre-integration cushion is the separate `node.container_repel_force_3d`.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

use super::container_repel_force_3d::CONTAINER_3D_MODES;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoundsUniforms {
    active_count: u32,
    container: u32,
    ctr_scale: f32,
    _pad0: u32,
}

crate::primitive! {
    name: ContainerBounds3D,
    type_id: "node.container_bounds_3d",
    purpose: "Post-integration hard containment for 3D particles: toroidal wrap (container = None) or SDF reflect + clamp (Cube/Sphere/Torus). For None: position = fract(position + 1). For an SDF container: when a particle escapes (d > 0) it's pushed back inside along the surface normal, then clamped to [0.001, 0.999]. The 3D sibling of node.wrap_particles_torus (torus-only); decomposed from the containment step of the fused node.fluid_simulate_3d. Particle velocity is not touched (the legacy velocity-bounce write was dead state).",
    inputs: {
        in: Array(Particle) required,
        ctr_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "container",
            label: "Container",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: CONTAINER_3D_MODES,
        },
        ParamDef {
            name: "ctr_scale",
            label: "Container Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.2, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `container` is a mode enum (0 None / 1 Cube / 2 Sphere / 3 Torus); None is the default toroidal-wrap [0,1]^3 policy (the 3D wrap_particles_torus). `ctr_scale` is port-shadow. Wire downstream of node.euler_step_particles_3d; the soft pre-integration boundary cushion is node.container_repel_force_3d. For alternative policies, swap for a future boundary_death / wall_bounce sibling.",
    examples: ["FluidSimulation3D"],
    picker: { label: "Container Bounds 3D", category: Atom },
}

impl Primitive for ContainerBounds3D {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let container = match ctx.params.get("container") {
            Some(ParamValue::Enum(n)) => *n,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };
        let ctr_scale = ctx.scalar_or_param("ctr_scale", 0.8);
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/container_bounds_3d.wgsl"),
                "cs_main",
                "node.container_bounds_3d",
            )
        });

        let uniforms = BoundsUniforms {
            active_count,
            container,
            ctr_scale,
            _pad0: 0,
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
                    buffer: particles,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.container_bounds_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(ContainerBounds3D::TYPE_ID, "node.container_bounds_3d");
        let names: Vec<&str> = ContainerBounds3D::INPUTS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["in", "ctr_scale", "active_count"]);
        assert_eq!(
            ContainerBounds3D::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ContainerBounds3D::INPUTS[0].required);

        assert_eq!(ContainerBounds3D::OUTPUTS.len(), 1);
        assert_eq!(
            ContainerBounds3D::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = ContainerBounds3D::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ContainerBounds3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.container_bounds_3d");
    }
}
