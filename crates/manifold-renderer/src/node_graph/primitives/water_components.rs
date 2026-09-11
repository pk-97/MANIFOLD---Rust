//! Yu–Turk connected components as seed, atomic union, and immutable roots.
//! The union pass aliases its parent input; graph ordering places roots after
//! every union invocation has completed. No CPU convergence readback is used.
use crate::node_graph::effect_node::{EffectNodeContext, NodeRequires};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

pub fn seed_shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<WaterComponentSeed>()
        .expect("water component seed codegen")
}
pub fn roots_shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<WaterComponentRoots>()
        .expect("water component roots codegen")
}
pub fn union_shader_source() -> &'static str {
    include_str!("shaders/water_component_union.wgsl")
}
pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(
        &seed_shader_source(),
        "cs_main",
        "node.water_component_seed",
    );
    let _ = device.create_compute_pipeline(
        union_shader_source(),
        "cs_main",
        "node.water_component_union",
    );
    let _ = device.create_compute_pipeline(
        &roots_shader_source(),
        "cs_main",
        "node.water_component_roots",
    );
}

crate::primitive! {
    name: WaterComponentSeed,
    type_id: "node.water_component_seed",
    purpose: "Seed identity parents for live water particles; inactive or invalid bin-domain records receive u32::MAX.",
    inputs: { particles: Array(WaterParticle) required },
    outputs: { parents: Array(u32) },
    params: [],
    depth_rule: Terminal,
    composition_notes: "First component dispatch. Parents must be reseeded from original particles each reconstruction frame before union; it is not persistent connectivity state.",
    examples: [],
    picker: { label: "Water Component Seed", category: Atom },
    summary: "Seed identity parents for live water particles; inactive or invalid bin-domain records receive u32::MAX.",
    category: Particles3D,
    role: Filter,
    aliases: ["water component seed"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/water_component_seed_body.wgsl"),
    input_access: [BufferGather],
    extra_fields: { source: String = seed_shader_source(), },
}
impl Primitive for WaterComponentSeed {
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: false,
            gpu_encoder: true,
        }
    }
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "parents")
            .then(|| {
                capacities
                    .iter()
                    .find(|(name, _)| *name == "particles")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(input), Some(output)) =
            (ctx.inputs.array("particles"), ctx.outputs.array("parents"))
        else {
            return;
        };
        let count = (input.size / 96) as u32;
        if output.size < u64::from(count) * 4 {
            ctx.error("node.water_component_seed: insufficient output capacity");
            return;
        }
        if count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.water_component_seed")
        });
        let uniform = [count, 0, 0, 0];
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: output,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_component_seed",
        );
    }
}

crate::primitive! {
    name: WaterComponentRoots,
    type_id: "node.water_component_roots",
    purpose: "Resolve each strictly descending parent chain to its smallest-index canonical component label.",
    inputs: { parents: Array(u32) required },
    outputs: { components: Array(u32) },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Wire the completed water_component_union out to parents, then components to water_surface_fit. This immutable gather emits u32::MAX for inactive or invalid parent chains.",
    examples: [],
    picker: { label: "Water Component Roots", category: Atom },
    summary: "Resolve each strictly descending parent chain to its smallest-index canonical component label.",
    category: Particles3D,
    role: Filter,
    aliases: ["water component roots"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/water_component_roots_body.wgsl"),
    input_access: [BufferGather],
    extra_fields: { source: String = roots_shader_source(), },
}
impl Primitive for WaterComponentRoots {
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: false,
            gpu_encoder: true,
        }
    }
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "components")
            .then(|| {
                capacities
                    .iter()
                    .find(|(name, _)| *name == "parents")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(input), Some(output)) =
            (ctx.inputs.array("parents"), ctx.outputs.array("components"))
        else {
            return;
        };
        let count = (input.size / 4) as u32;
        if output.size < u64::from(count) * 4 {
            ctx.error("node.water_component_roots: insufficient output capacity");
            return;
        }
        if count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                "cs_main",
                "node.water_component_roots",
            )
        });
        let uniform = [count, 0, 0, 0];
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: output,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_component_roots",
        );
    }
}

crate::primitive! {
    name: WaterComponentUnion,
    type_id: "node.water_component_union",
    purpose: "Join original-position water neighbors at distance <= connection_radius using monotone atomic minimum union-find.",
    inputs: {
        particles: Array(WaterParticle) required,
        heads: Array(u32) required,
        next: Array(u32) required,
        parents: Array(u32) required,
        connection_radius: ScalarF32 optional,
    },
    outputs: { out: Array(u32) },
    params: [ParamDef {
        name: Cow::Borrowed("connection_radius"), label: "Connection Radius",
        ty: ParamType::Float, default: ParamValue::Float(0.03125),
        range: Some((0.001, 1.0)), enum_values: &[],
    }],
    depth_rule: Terminal,
    composition_notes: "Second component dispatch, after water_component_seed. Uses water_particle_bins' original-position 32³ domain. out aliases parents; never read the seed wire concurrently with this update. Follow with water_component_roots. This atomic scatter is outside pure per-element fusion; every root link decreases, and union retries descend without a fixed pass budget. Yu–Turk 2013 uses average particle spacing as connection radius.",
    examples: [],
    picker: { label: "Water Component Union", category: Atom },
    summary: "Connects neighboring particles into fluid components.",
    category: Particles3D,
    role: Filter,
    aliases: ["water component union"],
    boundary_reason: Blocked,
}
impl Primitive for WaterComponentUnion {
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: false,
            gpu_encoder: true,
        }
    }
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("parents", "out")]
    }
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                capacities
                    .iter()
                    .find(|(name, _)| *name == "parents")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(particles), Some(heads), Some(next), Some(parents), Some(_output)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("heads"),
            ctx.inputs.array("next"),
            ctx.inputs.array("parents"),
            ctx.outputs.array("out"),
        ) else {
            ctx.mark_gpu_accessed();
            return;
        };
        let count = (particles.size / 96) as u32;
        let radius = ctx.scalar_or_param("connection_radius", 0.03125);
        if !radius.is_finite()
            || radius <= 0.0
            || heads.size < 32768 * 4
            || next.size < u64::from(count) * 4
            || parents.size != u64::from(count) * 4
        {
            ctx.mark_gpu_accessed();
            ctx.error("node.water_component_union: invalid radius or parent/bin capacity");
            return;
        }
        if count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                union_shader_source(),
                "cs_main",
                "node.water_component_union",
            )
        });
        let uniform = [radius.to_bits(), count, 0, 0];
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: heads,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: next,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: parents,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_component_union",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    #[test]
    fn water_components_generated_seed_roots_and_atomic_union_validate() {
        for source in [
            seed_shader_source(),
            roots_shader_source(),
            union_shader_source().to_owned(),
        ] {
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .expect("component WGSL");
        }
    }
    #[test]
    fn water_components_capacity_alias_and_shadow_contract() {
        let params = Default::default();
        assert_eq!(
            WaterComponentSeed::new().array_output_capacity(
                "parents",
                &params,
                &[("particles", 513)]
            ),
            Some(513)
        );
        assert_eq!(
            WaterComponentRoots::new().array_output_capacity(
                "components",
                &params,
                &[("parents", 513)]
            ),
            Some(513)
        );
        assert_eq!(
            WaterComponentUnion::new().array_output_capacity("out", &params, &[("parents", 513)]),
            Some(513)
        );
        assert_eq!(
            WaterComponentUnion::new().aliased_array_io(),
            &[("parents", "out")]
        );
        assert!(
            WaterComponentUnion::INPUTS
                .iter()
                .any(|p| p.name == "connection_radius")
        );
    }
}
