//! `node.fluid_gradient_rotate` — fused gradient + 2D rotation pass
//! for the FluidSim family. Bit-exact extract from
//! `generators/shaders/fluid_gradient_rotate_compute.wgsl`.
//!
//! Reads a scalar density Texture2D, computes the toroidal-wrapped
//! central-difference gradient, scales by `slope_strength`, and
//! rotates by `rotation_angle` (radians) — all in one dispatch with
//! no intermediate fp16 storage write. Output: vec2 force field
//! packed as vec4 (xy = force, z = 0, w = 1).
//!
//! The pass is intentionally fused for bit-exact FluidSim parity.
//! Splitting gradient and rotation into separate primitives would
//! introduce an Rgba16Float storage write between them, breaking
//! the parity guarantee. If you want pure gradient or pure rotation
//! for non-FluidSim contexts, those should be separate primitives
//! authored from scratch (not extracted from this one).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientRotateUniforms {
    texel_x: f32,
    texel_y: f32,
    slope_strength: f32,
    rot_cos: f32,
    rot_sin: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: FluidGradientRotate,
    type_id: "node.fluid_gradient_rotate",
    purpose: "Fused gradient + 2D rotation for the FluidSim family. Computes central-difference gradient with toroidal wrap on a scalar density texture, scales by slope_strength, rotates by rotation_angle, writes a vec2 force field. Fused for bit-exact FluidSim parity — gradient and rotation share one dispatch with no intermediate storage write.",
    inputs: {
        density: Texture2D required,
    },
    outputs: {
        force: Texture2D,
    },
    params: [
        ParamDef {
            name: "slope_strength",
            label: "Slope Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(-500.0),
            range: Some((-5000.0, 5000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rotation_angle",
            label: "Rotation Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    composition_notes: "slope_strength negative = repulsion (default), positive = attraction. Texel size is read from the input texture's dimensions — no need to pass it as a param. The rotation is applied to the gradient *after* slope scaling. Output Z = 0, alpha = 1.",
    examples: [],
    picker: { label: "Fluid Gradient Rotate", category: Atom },
}

impl Primitive for FluidGradientRotate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let slope_strength = match ctx.params.get("slope_strength") {
            Some(ParamValue::Float(f)) => *f,
            _ => -500.0,
        };
        let rotation_angle = match ctx.params.get("rotation_angle") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(density) = ctx.inputs.texture_2d("density") else {
            return;
        };
        let Some(force) = ctx.outputs.texture_2d("force") else {
            return;
        };
        let width = density.width;
        let height = density.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_gradient_rotate_compute.wgsl"),
                "cs_main",
                "node.fluid_gradient_rotate",
            )
        });

        let uniforms = GradientRotateUniforms {
            texel_x: 1.0 / width as f32,
            texel_y: 1.0 / height as f32,
            slope_strength,
            rot_cos: rotation_angle.cos(),
            rot_sin: rotation_angle.sin(),
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: density,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: force,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.fluid_gradient_rotate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_gradient_rotate_declares_texture_in_and_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(FluidGradientRotate::TYPE_ID, "node.fluid_gradient_rotate");
        assert_eq!(FluidGradientRotate::INPUTS.len(), 1);
        assert_eq!(FluidGradientRotate::INPUTS[0].name, "density");
        assert!(FluidGradientRotate::INPUTS[0].required);
        assert_eq!(FluidGradientRotate::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(FluidGradientRotate::OUTPUTS.len(), 1);
        assert_eq!(FluidGradientRotate::OUTPUTS[0].name, "force");
        assert_eq!(FluidGradientRotate::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn fluid_gradient_rotate_has_slope_and_rotation_params() {
        let names: Vec<&str> = FluidGradientRotate::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["slope_strength", "rotation_angle"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidGradientRotate::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_gradient_rotate");
    }
}
