//! Audio visualizer source atoms.
//!
//! The sources read the content-thread-owned audio visual histories through
//! `GpuEncoder::audio_visuals`. They deliberately keep all output storage
//! bounded: the waveform is a fixed 512-element shared array and the
//! spectrogram is a fixed 512×256 texture. The spectrogram uploads into a
//! three-entry persistent CPU-visible staging ring and blits the selected
//! texture into the graph output, since pooled graph targets are not
//! CPU-uploadable. The ring matches ContentPipeline's three-surface fence.

use std::borrow::Cow;

use half::f16;
use manifold_core::AudioSendId;
use manifold_gpu::{GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const WAVEFORM_CAPACITY: u32 = 512;
const SPECTRUM_WIDTH: u32 = 512;
const SPECTRUM_HEIGHT: u32 = 256;
const SPECTRUM_PIXELS: usize = (SPECTRUM_WIDTH * SPECTRUM_HEIGHT) as usize;
/// ContentPipeline rotates three surfaces and waits for the selected surface
/// signal before reuse. Mirror that bounded in-flight depth for CPU uploads.
const STAGING_ROTATION: usize = 3;

fn read_send(ctx: &EffectNodeContext<'_, '_>, cached: &mut Option<AudioSendId>) {
    let value = match ctx.params.get("send") {
        Some(ParamValue::String(value)) if !value.is_empty() => Some(value.as_str()),
        _ => None,
    };
    if cached.as_ref().map(AudioSendId::as_str) != value {
        *cached = value.map(AudioSendId::new);
    }
}

crate::primitive! {
    name: AudioWaveform,
    type_id: "node.audio_waveform",
    purpose: "Read one live audio send's recent waveform into a fixed 512-sample Array<f32>. `send` selects a send by stable AudioSendId; empty selects the first configured send. `window_ms` chooses a 5–100 ms view and `trigger` aligns it to a rising zero crossing when possible. Numeric inputs shadow same-named params when wired.",
    inputs: {
        window_ms: ScalarF32 optional,
        trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("send"),
            label: "Audio Send",
            ty: ParamType::String,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("window_ms"),
            label: "Window (ms)",
            ty: ParamType::Float,
            default: ParamValue::Float(25.0),
            range: Some((5.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("trigger"),
            label: "Trigger",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is always 512. Wire `out` into node.combine_xy after creating a matching x range, then into node.draw_lines. `send` is a String binding; an empty value selects the first registry history. `window_ms` and `trigger` have optional scalar input shadows. The source is an IO boundary and remains live even when its params are unchanged.",
    examples: ["preset.effect.oscilloscope"],
    picker: { label: "Audio Waveform", category: Atom },
    summary: "Provides a live audio waveform as 512 curve samples for oscilloscope and line-based graphs.",
    category: Generate,
    role: Source,
    aliases: ["audio waveform", "waveform", "oscilloscope source"],
    boundary_reason: IoBridge,
    extra_fields: {
        send_id: Option<AudioSendId> = None,
    },
}

impl Primitive for AudioWaveform {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then_some(WAVEFORM_CAPACITY)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        read_send(ctx, &mut self.send_id);
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let capacity = (out.size / std::mem::size_of::<f32>() as u64) as usize;
        let count = capacity.min(WAVEFORM_CAPACITY as usize);
        if count == 0 {
            return;
        }

        // The registry is borrowed through the GPU context because that is
        // the per-frame service boundary shared by generators and effects.
        let history = ctx
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.audio_visuals)
            .and_then(|registry| registry.get(self.send_id.as_ref()));
        let window_ms = ctx.scalar_or_param("window_ms", 25.0);
        let trigger = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(value)) => value >= 0.5,
            _ => matches!(
                ctx.params.get("trigger"),
                Some(ParamValue::Bool(true)) | None
            ),
        };
        let mut values = [0.0_f32; WAVEFORM_CAPACITY as usize];
        if let Some(history) = history {
            history.waveform_into(&mut values[..count], window_ms, trigger);
        }
        // The chain allocates exactly 512 elements, but clamp the write for
        // standalone tests and malformed plans.
        unsafe {
            out.write(0, bytemuck::cast_slice(&values[..count]));
        }
    }
}

crate::primitive! {
    name: AudioSpectrum,
    type_id: "node.audio_spectrum",
    purpose: "Read one live audio send's recent spectrum into a fixed 512×256 Texture2D. The CPU history is laid out oldest-left/newest-right and high-frequency-top; this source reuses a three-entry CPU-upload staging ring per node and copies it into the graph output each frame. The seconds input shadows the same-named param when wired.",
    inputs: {
        seconds: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("send"),
            label: "Audio Send",
            ty: ParamType::String,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seconds"),
            label: "History (s)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.1, 2.5)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Output is always 512×256 and contains raw linear magnitudes in RGB with alpha 1.0. Follow with node.magnitude_db, node.scale_offset_image (scale 1/60, offset 1), node.clamp, and node.gradient/node.color_lut to map -60..0 dB into a palette. `send` is a String binding; empty selects the first registry history. `seconds` has an optional scalar input shadow.",
    examples: ["preset.effect.spectrogram"],
    picker: { label: "Audio Spectrum", category: Atom },
    summary: "Provides a live scrolling spectrum texture for spectrogram graphs.",
    category: Generate,
    role: Source,
    aliases: ["audio spectrum", "spectrogram source", "FFT texture"],
    boundary_reason: IoBridge,
    extra_fields: {
        send_id: Option<AudioSendId> = None,
        staging: [Option<GpuTexture>; STAGING_ROTATION] = [None, None, None],
        staging_frame: usize = 0,
        // Half-float bit patterns are kept as u16 because half::f16 does not
        // opt into bytemuck::Pod in this dependency configuration.
        pixels: Vec<u16> = vec![0; SPECTRUM_PIXELS * 4],
        magnitudes: Vec<f32> = vec![0.0; SPECTRUM_PIXELS],
    },
}

impl Primitive for AudioSpectrum {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "out").then_some((SPECTRUM_WIDTH, SPECTRUM_HEIGHT))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        read_send(ctx, &mut self.send_id);
        let seconds = ctx.scalar_or_param("seconds", 0.5);
        let out = ctx.outputs.texture_2d("out");
        let Some(out) = out else { return };
        if out.width == 0 || out.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let history = gpu
            .audio_visuals
            .and_then(|registry| registry.get(self.send_id.as_ref()));
        if let Some(history) = history {
            history.spectrum_into(
                &mut self.magnitudes,
                SPECTRUM_WIDTH as usize,
                SPECTRUM_HEIGHT as usize,
                seconds,
            );
        } else {
            self.magnitudes.fill(0.0);
        }

        for (pixel, &magnitude) in self.magnitudes.iter().enumerate() {
            let value = f16::from_f32(if magnitude.is_finite() {
                magnitude.max(0.0)
            } else {
                0.0
            })
            .to_bits();
            let dst = pixel * 4;
            self.pixels[dst] = value;
            self.pixels[dst + 1] = value;
            self.pixels[dst + 2] = value;
            self.pixels[dst + 3] = f16::from_f32(1.0).to_bits();
        }

        let staging_slot = self.staging_frame % STAGING_ROTATION;
        self.staging_frame = self.staging_frame.wrapping_add(1);
        if self.staging[staging_slot].is_none() {
            self.staging[staging_slot] = Some(gpu.device.create_texture(&GpuTextureDesc {
                width: SPECTRUM_WIDTH,
                height: SPECTRUM_HEIGHT,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
                label: "node.audio_spectrum staging",
                mip_levels: 1,
            }));
        }
        // ContentPipeline's three-surface fence makes this slot safe to
        // overwrite: it waits for the selected surface before starting the
        // frame, so a prior command buffer no longer samples this texture.
        let staging = self.staging[staging_slot]
            .as_ref()
            .expect("audio spectrum staging");
        gpu.native_enc.upload_texture(
            staging,
            SPECTRUM_WIDTH,
            SPECTRUM_HEIGHT,
            1,
            bytemuck::cast_slice(&self.pixels),
        );
        let width = out.width.min(SPECTRUM_WIDTH);
        let height = out.height.min(SPECTRUM_HEIGHT);
        gpu.copy_texture_to_texture(staging, out, width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::{Primitive, PrimitiveSpec};

    #[test]
    fn waveform_has_fixed_capacity() {
        assert_eq!(
            Primitive::array_output_capacity(
                &AudioWaveform::new(),
                "out",
                &Default::default(),
                &[]
            ),
            Some(512)
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &AudioWaveform::new(),
                "other",
                &Default::default(),
                &[]
            ),
            None
        );
    }

    #[test]
    fn spectrum_has_fixed_dimensions() {
        assert_eq!(
            Primitive::output_dims(
                &AudioSpectrum::new(),
                "out",
                (1920, 1080),
                &[],
                &Default::default()
            ),
            Some((512, 256))
        );
        assert_eq!(
            Primitive::output_dims(
                &AudioSpectrum::new(),
                "other",
                (1920, 1080),
                &[],
                &Default::default()
            ),
            None
        );
    }

    #[test]
    fn live_numeric_controls_shadow_params() {
        for (node, names) in [
            (
                &AudioWaveform::INPUTS,
                ["window_ms", "trigger"] as [&str; 2],
            ),
            (&AudioSpectrum::INPUTS, ["seconds", ""] as [&str; 2]),
        ] {
            for name in names.into_iter().filter(|name| !name.is_empty()) {
                assert!(node.iter().any(|port| port.name == name));
            }
        }
        assert!(
            AudioWaveform::PARAMS
                .iter()
                .any(|param| param.name == "send")
        );
        assert!(
            AudioSpectrum::PARAMS
                .iter()
                .any(|param| param.name == "send")
        );
    }
}
