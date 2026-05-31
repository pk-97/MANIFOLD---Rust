//! `node.fbm_2d` — octave-summed Perlin (fractional Brownian motion).
//!
//! Pure generator. Sums `octaves` octaves of 2D Perlin with
//! frequency multiplied by `lacunarity` and amplitude by
//! `persistence` per octave. Richer, more natural detail than
//! single-octave noise. Output normalized to [0, 1] and broadcast
//! to RGB (A = 1).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FbmUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    octaves: i32,
    lacunarity: f32,
    persistence: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: Fbm2D,
    type_id: "node.fbm_2d",
    purpose: "Pure generator. Octave-summed Perlin noise (fractional Brownian motion). Each octave doubles frequency (or `lacunarity`× it) and halves amplitude (or `persistence`× it). Output remapped to [0, 1], broadcast to RGB (A = 1). Workhorse for terrain, clouds, organic textures with multi-scale detail.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_x",
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset_y",
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "octaves",
            label: "Octaves",
            ty: ParamType::Int,
            default: ParamValue::Float(4.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "lacunarity",
            label: "Lacunarity",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "persistence",
            label: "Persistence",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Octaves clamped to [1, 8] in-shader (8 octaves is more than enough perceptually and bounds the inner loop for the WGSL compiler). lacunarity = 2.0 + persistence = 0.5 is the classic 'pink-spectrum' fBM. Drop persistence to ~0.3 for cleaner large shapes; raise toward 1.0 for spiky noise.",
    examples: [],
    picker: { label: "fBM 2D", category: Atom },
    summary: "Layered noise that stacks several octaves for rich, detailed texture, the fractal look behind clouds, terrain, and smoke.",
    category: Noise,
    role: Source,
    aliases: ["fbm", "fractal noise", "octaves", "turbulence"],
}

impl Primitive for Fbm2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let offset_x = match ctx.params.get("offset_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let offset_y = match ctx.params.get("offset_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let octaves = match ctx.params.get("octaves") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => 4,
        };
        let lacunarity = match ctx.params.get("lacunarity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
        };
        let persistence = match ctx.params.get("persistence") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/fbm_2d.wgsl"),
                "cs_main",
                "node.fbm_2d",
            )
        });

        let uniforms = FbmUniforms {
            scale,
            offset_x,
            offset_y,
            octaves,
            lacunarity,
            persistence,
            _pad0: 0.0,
            _pad1: 0.0,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.fbm_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fbm_2d_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Fbm2D::TYPE_ID, "node.fbm_2d");
        assert!(Fbm2D::INPUTS.is_empty());
        assert_eq!(Fbm2D::OUTPUTS.len(), 1);
        assert_eq!(Fbm2D::OUTPUTS[0].name, "out");
        assert_eq!(Fbm2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn fbm_2d_has_expected_params() {
        let names: Vec<&str> = Fbm2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "scale",
                "offset_x",
                "offset_y",
                "octaves",
                "lacunarity",
                "persistence"
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Fbm2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fbm_2d");
    }
}
