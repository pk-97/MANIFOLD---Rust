//! Free-slip ghost velocities for particle sampling at stationary box walls.
//! Open pressure faces are preserved exactly; only fully blocked faces are
//! extended. Reflection is even tangentially and odd along the wall normal.
use super::mac_box_fractions::{
    DEFAULT_BASIN_MAX, DEFAULT_BASIN_MIN, DEFAULT_BOX_MAX, DEFAULT_BOX_MIN, MacBoxFractionsUniforms,
};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;
pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacBoxSampleExtension>()
        .expect("MAC ghost extension codegen")
}
crate::primitive! {
name:MacBoxSampleExtension,type_id:"node.mac_box_sample_extension",
purpose:"Extend stationary box wall ghost velocities for trilinear particle sampling, preserving pressure faces with nonzero open area. Reflect tangential velocity evenly and normal velocity oddly across the closest wall.",
inputs:{grid:Channels["mac_velocity":Vec4F,"mac_valid":Vec4F] required,geometry:Channels["mac_open":Vec4F] required,

        basin_min_x: ScalarF32 optional,
        basin_min_y: ScalarF32 optional,
        basin_min_z: ScalarF32 optional,
        basin_max_x: ScalarF32 optional,
        basin_max_y: ScalarF32 optional,
        basin_max_z: ScalarF32 optional,
        box_min_x: ScalarF32 optional,
        box_min_y: ScalarF32 optional,
        box_min_z: ScalarF32 optional,
        box_max_x: ScalarF32 optional,
        box_max_y: ScalarF32 optional,
        box_max_z: ScalarF32 optional,
},
outputs:{out:Channels["mac_velocity":Vec4F,"mac_valid":Vec4F],},
params:[
        ParamDef {
            name: Cow::Borrowed("basin_min_x"),
            label: "Basin Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MIN[0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_y"),
            label: "Basin Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MIN[1]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_min_z"),
            label: "Basin Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MIN[2]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_x"),
            label: "Basin Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MAX[0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_y"),
            label: "Basin Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MAX[1]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("basin_max_z"),
            label: "Basin Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BASIN_MAX[2]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_min_x"),
            label: "Box Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MIN[0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_min_y"),
            label: "Box Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MIN[1]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_min_z"),
            label: "Box Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MIN[2]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_max_x"),
            label: "Box Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MAX[0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_max_y"),
            label: "Box Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MAX[1]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("box_max_z"),
            label: "Box Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_BOX_MAX[2]),
            range: None,
            enum_values: &[],
        },
],depth_rule:Terminal,
composition_notes:"Compose after pressure gradient and five extrapolation layers, before MAC gather. Bounds must match the stationary fraction and particle collision nodes. Open face velocities remain byte-identical. Fully blocked ghost faces sample a reflected point with odd normal and even tangent symmetry; unavailable samples stay invalid.",
examples:[],picker:{label:"MAC Wall Samples",category:Atom},summary:"Supply free-slip wall ghost velocities to particle interpolation.",category:Particles3D,role:Filter,aliases:[],fusion_kind:Source,
wgsl_body:include_str!("shaders/mac_box_sample_extension_body.wgsl"),input_access:[BufferGather,BufferGather],wgsl_includes:[include_str!("shaders/water_common.wgsl"),include_str!("shaders/mac_index.wgsl")],extra_fields:{source:String=shader_source(),},
}
impl Primitive for MacBoxSampleExtension {
    fn array_output_capacity(
        &self,
        p: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (p == "out").then_some(274625)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(grid) = ctx.inputs.array("grid") else {
            return;
        };
        let Some(geometry) = ctx.inputs.array("geometry") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if grid.size < 274625 * 32 || geometry.size < 274625 * 16 || out.size < 274625 * 32 {
            ctx.error("node.mac_box_sample_extension: incomplete MAC lattice");
            return;
        }
        let u = MacBoxFractionsUniforms {
            basin_min: [
                ctx.scalar_or_param("basin_min_x", DEFAULT_BASIN_MIN[0]),
                ctx.scalar_or_param("basin_min_y", DEFAULT_BASIN_MIN[1]),
                ctx.scalar_or_param("basin_min_z", DEFAULT_BASIN_MIN[2]),
            ],
            basin_max: [
                ctx.scalar_or_param("basin_max_x", DEFAULT_BASIN_MAX[0]),
                ctx.scalar_or_param("basin_max_y", DEFAULT_BASIN_MAX[1]),
                ctx.scalar_or_param("basin_max_z", DEFAULT_BASIN_MAX[2]),
            ],
            box_min: [
                ctx.scalar_or_param("box_min_x", DEFAULT_BOX_MIN[0]),
                ctx.scalar_or_param("box_min_y", DEFAULT_BOX_MIN[1]),
                ctx.scalar_or_param("box_min_z", DEFAULT_BOX_MIN[2]),
            ],
            box_max: [
                ctx.scalar_or_param("box_max_x", DEFAULT_BOX_MAX[0]),
                ctx.scalar_or_param("box_max_y", DEFAULT_BOX_MAX[1]),
                ctx.scalar_or_param("box_max_z", DEFAULT_BOX_MAX[2]),
            ],
            dispatch_count: 274625,
            padding: [0; 3],
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                "cs_main",
                "node.mac_box_sample_extension",
            )
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: geometry,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [274625u32.div_ceil(256), 1, 1],
            "node.mac_box_sample_extension",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mac_wall_samples_uniform_abi_and_scalar_shadows() {
        assert_eq!(std::mem::size_of::<MacBoxFractionsUniforms>(), 64);
        assert_eq!(MacBoxSampleExtension::PARAMS.len(), 12);
        for (input, param) in MacBoxSampleExtension::INPUTS[2..]
            .iter()
            .zip(MacBoxSampleExtension::PARAMS)
        {
            assert_eq!(input.name, param.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
            assert!(!input.required);
        }
        let PortType::Array(output) = &MacBoxSampleExtension::OUTPUTS[0].ty else {
            panic!("MAC output must be channels");
        };
        assert_eq!(output.item_size, 32);
    }

    #[test]
    fn mac_wall_samples_generated_shader_validates() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("valid wall sample shader");
    }
}
