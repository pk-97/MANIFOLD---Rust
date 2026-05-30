//! `node.container_repel_force_3d` — soft container-boundary repulsion
//! added in place to an `Array<[f32; 3]>` force buffer.
//!
//! The pre-integration "cushion" half of the FluidSim3D container
//! behaviour: when a particle is within a small margin of the container
//! wall, a gentle inward force pushes it back along the SDF's outward
//! normal, preventing pile-up at the boundary. Bit-exact with the soft
//! boundary repulsion in the legacy fused `node.fluid_simulate_3d`.
//!
//! Distinct from `node.container_bounds_3d`, which applies the
//! *post-integration* hard containment (toroidal wrap or SDF reflect +
//! clamp). Both read the same `container` enum (None / Cube / Sphere /
//! Torus); compose this one before `node.euler_step_particles_3d` and
//! the bounds atom after it.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Container SDF modes — shared label set with `node.container_bounds_3d`
/// and the legacy fluid_simulate_3d / fluid_seed_3d.
pub const CONTAINER_3D_MODES: &[&str] = &["None", "Cube", "Sphere", "Torus"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RepelUniforms {
    active_count: u32,
    container: u32,
    ctr_scale: f32,
    _pad0: u32,
}

crate::primitive! {
    name: ContainerRepelForce3D,
    type_id: "node.container_repel_force_3d",
    purpose: "Soft container-boundary repulsion added in-place to an Array<[f32; 3]> force buffer. When a particle is within a 0.1 margin of the container SDF (Cube/Sphere/Torus), a gentle inward force `n * (t*t*0.15)` cushions it back along the outward normal, preventing wall pile-up. container = None disables it. The pre-integration cushion half of the FluidSim3D container behaviour (the post-integration hard wall is node.container_bounds_3d). Decomposed from the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        ctr_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
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
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place subtract). `container` is a mode enum (0 None / 1 Cube / 2 Sphere / 3 Torus) — None makes the dispatch a no-op so the atom is inert at the default. `ctr_scale` is port-shadow (sizes the SDF). Wire upstream of node.euler_step_particles_3d so the cushion is integrated through speed*dt, and pair with node.container_bounds_3d (post-integration hard wall) downstream.",
    examples: ["FluidSimulation3D"],
    picker: { label: "Container Repel Force 3D", category: Atom },
}

impl Primitive for ContainerRepelForce3D {
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
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;

        let Some(in_forces) = ctx.inputs.array("in") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 12) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/container_repel_force_3d.wgsl"),
                "cs_main",
                "node.container_repel_force_3d",
            )
        });

        let uniforms = RepelUniforms {
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: in_forces,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.container_repel_force_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec3_in_out_required_particles() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            ContainerRepelForce3D::TYPE_ID,
            "node.container_repel_force_3d"
        );
        let names: Vec<&str> = ContainerRepelForce3D::INPUTS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["in", "particles", "ctr_scale", "active_count"]);
        assert_eq!(
            ContainerRepelForce3D::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(ContainerRepelForce3D::INPUTS[0].required);

        assert_eq!(ContainerRepelForce3D::OUTPUTS.len(), 1);
        assert_eq!(
            ContainerRepelForce3D::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = ContainerRepelForce3D::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn has_four_container_modes() {
        assert_eq!(CONTAINER_3D_MODES, &["None", "Cube", "Sphere", "Torus"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ContainerRepelForce3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.container_repel_force_3d");
    }
}
