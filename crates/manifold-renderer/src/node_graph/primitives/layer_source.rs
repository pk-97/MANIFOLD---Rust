//! `node.layer_source` — emit another layer's composited output as a
//! `Texture2D` wire (SCENE_FX_DESIGN.md section 3.3 "layer skins"), so a
//! scene object's `emissive_map` / `base_color_map` can be driven by
//! whatever another layer is playing. This is the first primitive that
//! couples layers together; the coupling is unidirectional and one frame
//! delayed, which is what keeps it cheap.
//!
//! The texture comes from the compositor's [`LayerSkinRegistry`]: the
//! compositor publishes every layer's final post-effect texture at end of
//! frame, after ALL layer renders complete, and graph execution reads it
//! next frame — layer→layer loops are one-frame-delay feedback (a look,
//! not a bug), never a render-order hazard. A missing or deleted layer id
//! emits the registry's 1×1 transparent-black fallback; the `layer` param
//! is never cleared and nothing ever panics (section 4's
//! "layer_source never blocks render" invariant).
//!
//! No gating on `mark_outputs_unchanged`: the source content is live by
//! definition — the skin updates every frame the source layer renders.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use manifold_core::LayerId;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerSourceBlitUniforms {
    out_width: f32,
    out_height: f32,
}

crate::primitive! {
    name: LayerSource,
    type_id: "node.layer_source",
    purpose: "Emit another layer's composited output as a Texture2D wire, so a scene_object's emissive_map or base_color_map (or any texture input) can be skinned by whatever that layer is playing. Reads the previous frame's composite — the compositor publishes every layer's final texture after all renders complete, and graph execution reads it next frame, so layer-to-layer loops are one-frame feedback, never a render-order hazard. A missing or deleted layer id emits transparent black; the layer param is never cleared and nothing panics.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("layer"),
            label: "Layer",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
    ],
    // Canvas-sized texture producer with no inputs — the same dimensional
    // class as node.checkerboard: SourceHeight makes the slot allocator
    // resolve the out slot at canvas dims (Inherit would hand an inputless
    // node (0, 0) and abort inside Metal texture validation).
    depth_rule: SourceHeight,
    composition_notes: "Set `layer` to the source layer's id (the D8 Skin row renders it as a picker; in graph JSON it arrives via presetMetadata.stringBindings or def-baked directly on the node, same convention as node.hdri_source's `path`). Wire `out` into the target map port — node.scene_object's `emissive_map` or `base_color_map`; the wire target decides which map, there is no target_map param (D7). The emitted texture is always the PREVIOUS frame's composite: a layer skinned by itself produces a one-frame feedback smear (legal, a look), and two layers skinning each other converge through alternating frames. A missing or deleted source layer emits transparent black — loud on the Skin row via the D8 missing-layer chip, silent-black at the render level, never a panic.",
    examples: [],
    picker: { label: "Layer Source", category: Atom },
    summary: "Skins a scene object with another layer's output — wire it into emissive_map or base_color_map and pick the source layer; the model wears whatever that layer is playing, one frame behind.",
    category: Generate,
    role: Source,
    aliases: ["layer", "skin", "layer feed", "cross-layer"],
    boundary_reason: IoBridge,
}

impl Primitive for LayerSource {
    fn output_mipmapped(&self, port: &str) -> bool {
        // The skin is sampled under minification on 3D geometry — the out
        // slot carries a mip chain, regenerated every frame after the blit.
        port == "out"
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // Resolve the source: bound param → registry lookup (unknown id →
        // the registry's fallback texture); unbound param or no registry
        // (mock-backend tests, standalone validation) → None, which emits
        // transparent black directly below. Never pool leftovers, never a
        // panic, and the `layer` param is only ever read.
        let layer_id = match ctx.params.get("layer") {
            Some(ParamValue::String(s)) if !s.is_empty() => Some(LayerId::new(s.as_str())),
            _ => None,
        };
        let source = layer_id
            .as_ref()
            .and_then(|id| ctx.layer_skin_registry.map(|registry| registry.get(id)));

        let Some(source) = source else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 0.0);
            if out.mip_level_count() > 1 {
                gpu.native_enc.generate_mipmaps(out);
            }
            return;
        };

        let gpu = ctx.gpu_encoder();
        // COMPILE_CONTRACT P2: prewarmed at startup; the get_or_create stays
        // because test harnesses and headless tools never run the prewarm.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/layer_source_blit.wgsl"),
                "cs_main",
                "node.layer_source",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = LayerSourceBlitUniforms {
            out_width: w as f32,
            out_height: h as f32,
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
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.layer_source",
        );

        if out.mip_level_count() > 1 {
            gpu.native_enc.generate_mipmaps(out);
        }
    }
}

impl LayerSource {
    /// Compile the blit pipeline into `device`'s shared cache at startup,
    /// mirroring `HdriSource::prewarm_pipeline`. Called from
    /// `GeneratorRegistry::prewarm_all`.
    pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(
            include_str!("shaders/layer_source_blit.wgsl"),
            "cs_main",
            "node.layer_source",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// Unknown/missing layer id → the registry's fallback, no panic (the
    /// section-4 "never blocks render" invariant, registry side).
    #[test]
    fn unknown_layer_id_returns_fallback_no_panic() {
        let device = crate::test_device();
        let mut registry = crate::layer_skin::LayerSkinRegistry::new(
            &device,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
        );
        // Publish one layer so the map is non-empty.
        let published = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 8,
            height: 8,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "published",
            mip_levels: 1,
        });
        registry.publish(LayerId::new("layer-a"), published);
        // Unknown id → fallback (1×1), never a panic, param id untouched.
        let missing = registry.get(&LayerId::new("deleted-layer"));
        assert_eq!(missing.width, 1);
        assert_eq!(missing.height, 1);
        // The published entry is still there — a missing lookup must not
        // mutate the registry.
        assert_eq!(registry.get(&LayerId::new("layer-a")).width, 8);
    }
}
