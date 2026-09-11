//! Exact stationary axis-aligned basin/obstacle fractions on a staggered MAC grid.
//! xyz are open face area fractions; w is the open cell volume fraction.
use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const MAC_BOX_ENTRIES: u32 = 65 * 65 * 65;
pub const DEFAULT_BASIN_MIN: [f32; 3] = [-1.65, 0.25, -1.125];
pub const DEFAULT_BASIN_MAX: [f32; 3] = [1.65, 3.875, 1.125];
pub const DEFAULT_BOX_MIN: [f32; 3] = [0.33, 0.25, -0.02];
pub const DEFAULT_BOX_MAX: [f32; 3] = [0.57, 0.95, 0.58];

/// Twelve scalar params in declaration order, followed by codegen's count
/// and three padding words. Rust arrays here are tightly packed, not vec3s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacBoxFractionsUniforms {
    pub basin_min: [f32; 3],
    pub basin_max: [f32; 3],
    pub box_min: [f32; 3],
    pub box_max: [f32; 3],
    pub dispatch_count: u32,
    pub padding: [u32; 3],
}

/// Host ABI witness: a single channel is a bare vec4 in generated WGSL.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacBoxCell {
    pub mac_open: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<MacBoxFractionsUniforms>() == 64);
const _: () = assert!(std::mem::size_of::<MacBoxCell>() == 16);

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacBoxFractions>()
        .expect("node.mac_box_fractions codegen")
}

crate::primitive! {
    name: MacBoxFractions,
    type_id: "node.mac_box_fractions",
    purpose: "Compute exact open face-area and cell-volume fractions for a stationary axis-aligned basin minus its intersection with a stationary box obstacle. The padded 65-cubed output stores x/y/z staggered face fractions and the cell fraction in w. World-boundary faces and unused padding are closed. Nonfinite or non-increasing bounds emit NaNs for downstream rejection.",
    inputs: {
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
    outputs: { out: Channels["mac_open": Vec4F] },
    params: [
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
    ],
    depth_rule: Terminal,
    composition_notes: "Fixed 64-cubed world grid with h=0.0625 and origin [-2,0,-2]. Faces lie at integer coordinates along their component axis and half-cell centers tangentially. xyz is (face intersect basin minus face intersect basin intersect obstacle)/h^2; w is the corresponding cell volume/h^3. The obstacle may extend outside the basin. Bounds are scalar-port shadowable, but this atom models stationary axis-aligned solids only; moving-wall velocities and arbitrary transforms are not represented.",
    examples: [],
    picker: { label: "MAC Box Fractions", category: Atom },
    summary: "Measures the open water faces and cell volumes around a box obstacle.",
    category: Particles3D,
    role: Source,
    aliases: ["mac geometry", "open face fraction", "box obstacle"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/mac_box_fractions_body.wgsl"),
    input_access: [],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
    extra_fields: { source: String = shader_source(), },
}

impl Primitive for MacBoxFractions {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(MAC_BOX_ENTRIES)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let uniforms = MacBoxFractionsUniforms {
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
            dispatch_count: MAC_BOX_ENTRIES,
            padding: [0; 3],
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if out.size < u64::from(MAC_BOX_ENTRIES) * 16 {
            ctx.error("node.mac_box_fractions requires a full padded 65-cubed output");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mac_box_fractions",
            )
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: out,
                    offset: 0,
                },
            ],
            [MAC_BOX_ENTRIES.div_ceil(256), 1, 1],
            "node.mac_box_fractions",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mac_box_geometry_params_are_port_shadowed() {
        assert_eq!(MacBoxFractions::INPUTS.len(), 12);
        assert_eq!(MacBoxFractions::PARAMS.len(), 12);
        for (input, param) in MacBoxFractions::INPUTS.iter().zip(MacBoxFractions::PARAMS) {
            assert_eq!(input.name, param.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
            assert!(!input.required);
        }
        let PortType::Array(output) = &MacBoxFractions::OUTPUTS[0].ty else {
            panic!("geometry output must be channels");
        };
        assert_eq!(output.item_size, 16);
        assert_eq!(
            MacBoxFractions::new().array_output_capacity("out", &Default::default(), &[]),
            Some(MAC_BOX_ENTRIES)
        );
    }

    #[test]
    fn mac_box_geometry_generated_shader_validates() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("valid MAC box geometry shader");
    }
}
