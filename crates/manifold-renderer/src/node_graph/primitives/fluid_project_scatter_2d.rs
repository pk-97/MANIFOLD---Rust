//! `node.fluid_project_scatter_2d` — fused 3D-particle perspective/
//! orthographic projection + 2D scatter for FluidSim3D's display
//! path. Bit-exact wrap of `generators/shaders/fluid_scatter_3d.wgsl`'s
//! `splat_projected` entry via include_str.
//!
//! Reads 3D particle positions, projects them through a camera
//! (orthographic with toroidal wrap, or perspective with cull),
//! atomic-adds `scaled_energy` into a 2D u32 accumulator buffer
//! sized disp_w × disp_h. Pair with `node.resolve_accumulator`
//! downstream to lift the u32 grid into a float Texture2D for
//! display.
//!
//! Camera vectors are precomputed on CPU and passed as scalar params
//! (same as the legacy FluidSim3D Rust code). For free-camera
//! control surfaces, drive `cam_*` params via Math nodes.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const FLUID_PROJECT_MODES: &[&str] = &["Perspective", "Orthographic"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    ortho: u32,
    scaled_energy: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    _pad3: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad4: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    _pad5: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    _pad6: f32,
    aspect: f32,
    _pad7: f32,
    _pad8: f32,
    _pad9: f32,
}

crate::primitive! {
    name: FluidProjectScatter2D,
    type_id: "node.fluid_project_scatter_2d",
    purpose: "Fused 3D→2D camera projection + atomic-add scatter for FluidSim3D's display path. Each 3D particle projects through orthographic (with toroidal wrap) or perspective camera; resulting screen-space UV indexes a 2D u32 accumulator that receives scaled_energy via atomicAdd. Pair downstream with node.resolve_accumulator → texture for display.",
    inputs: {
        particles: Array(Particle) required,
    },
    outputs: {
        accum: Array(u32),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "disp_w",
            label: "Display Width",
            ty: ParamType::Int,
            default: ParamValue::Int(1920),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "disp_h",
            label: "Display Height",
            ty: ParamType::Int,
            default: ParamValue::Int(1080),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "mode",
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: FLUID_PROJECT_MODES,
        },
        ParamDef {
            name: "scaled_energy",
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Int(4096),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_pos_x",
            label: "Cam Pos X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_pos_y",
            label: "Cam Pos Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_pos_z",
            label: "Cam Pos Z",
            ty: ParamType::Float,
            default: ParamValue::Float(-2.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_fwd_x",
            label: "Cam Fwd X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_fwd_y",
            label: "Cam Fwd Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_fwd_z",
            label: "Cam Fwd Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_right_x",
            label: "Cam Right X",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_right_y",
            label: "Cam Right Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_right_z",
            label: "Cam Right Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_up_x",
            label: "Cam Up X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_up_y",
            label: "Cam Up Y",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_up_z",
            label: "Cam Up Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "aspect",
            label: "Aspect Ratio",
            ty: ParamType::Float,
            default: ParamValue::Float(1.777),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Camera vectors must form an orthonormal basis — FluidSim3D precomputes them on CPU from Euler angles. For graph use: either set them as constants and drive rotation through downstream primitives, or expose Math-driven angle params upstream that compute cam_fwd/right/up. Orthographic mode wraps toroidally (good for ambient volumetric textures); perspective culls behind-camera particles. Output accumulator buffer must be sized disp_w × disp_h × 4 bytes upstream.",
    examples: [],
    picker: { label: "Fluid Project Scatter 2D", category: Atom },
}

impl Primitive for FluidProjectScatter2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let disp_w = match ctx.params.get("disp_w") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 1920,
        };
        let disp_h = match ctx.params.get("disp_h") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 1080,
        };
        let ortho = match ctx.params.get("mode") {
            Some(ParamValue::Enum(n)) if *n == 1 => 1u32,
            _ => 0,
        };
        let scaled_energy = match ctx.params.get("scaled_energy") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 4096,
        };

        fn float_param(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        }

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("accum") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(particle_capacity);

        let uniforms = ProjectedUniforms {
            active_count,
            disp_w,
            disp_h,
            ortho,
            scaled_energy,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            cam_pos_x: float_param(ctx, "cam_pos_x", 0.0),
            cam_pos_y: float_param(ctx, "cam_pos_y", 0.0),
            cam_pos_z: float_param(ctx, "cam_pos_z", -2.0),
            _pad3: 0.0,
            cam_fwd_x: float_param(ctx, "cam_fwd_x", 0.0),
            cam_fwd_y: float_param(ctx, "cam_fwd_y", 0.0),
            cam_fwd_z: float_param(ctx, "cam_fwd_z", 1.0),
            _pad4: 0.0,
            cam_right_x: float_param(ctx, "cam_right_x", 1.0),
            cam_right_y: float_param(ctx, "cam_right_y", 0.0),
            cam_right_z: float_param(ctx, "cam_right_z", 0.0),
            _pad5: 0.0,
            cam_up_x: float_param(ctx, "cam_up_x", 0.0),
            cam_up_y: float_param(ctx, "cam_up_y", 1.0),
            cam_up_z: float_param(ctx, "cam_up_z", 0.0),
            _pad6: 0.0,
            aspect: float_param(ctx, "aspect", 1.777),
            _pad7: 0.0,
            _pad8: 0.0,
            _pad9: 0.0,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_scatter_3d.wgsl"),
                "splat_projected",
                "node.fluid_project_scatter_2d",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.fluid_project_scatter_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_project_scatter_2d_declares_particle_in_and_u32_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };
        let u32_layout = ArrayType {
            item_size: 4,
            item_align: 4,
        };

        assert_eq!(
            FluidProjectScatter2D::TYPE_ID,
            "node.fluid_project_scatter_2d"
        );
        assert_eq!(FluidProjectScatter2D::INPUTS.len(), 1);
        assert_eq!(FluidProjectScatter2D::INPUTS[0].name, "particles");
        assert_eq!(
            FluidProjectScatter2D::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(FluidProjectScatter2D::OUTPUTS.len(), 1);
        assert_eq!(FluidProjectScatter2D::OUTPUTS[0].name, "accum");
        assert_eq!(
            FluidProjectScatter2D::OUTPUTS[0].ty,
            PortType::Array(u32_layout)
        );
    }

    #[test]
    fn fluid_project_scatter_2d_uniform_struct_is_112_bytes() {
        assert_eq!(std::mem::size_of::<ProjectedUniforms>(), 112);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidProjectScatter2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_project_scatter_2d");
    }
}
