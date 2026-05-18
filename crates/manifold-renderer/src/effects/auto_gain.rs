// Auto Gain — visual dynamics compressor.
//
// Measures frame luminance on GPU (sparse 16×16 sample, parallel reduction),
// runs a program-dependent envelope follower on CPU (one-frame latency),
// and applies gain with optional analog-style character coloration.
//
// Architecture:
//   Dispatch 1 (measure): single workgroup reads source, writes avg luminance
//                          to a shared CPU/GPU buffer.
//   CPU envelope:          reads previous frame's measurement, computes gain.
//   Dispatch 2 (apply):    standard ComputeBlitHelper with gain in uniforms.
//
// Character modes (Clean/Warm/Film/Vivid/Grit) are specialized via function
// constants — Metal compiler dead-code eliminates inactive branches.

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::AUTO_GAIN,
        display_name: "Auto Gain",
        category: "Stylize",
        available: true,
        osc_prefix: "autoGain",
        legacy_discriminant: Some(41),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::continuous("ratio", "Ratio", 0.0, 1.0, 0.5, "F2", "Ratio"),
            ParamSpec::continuous("punch", "Punch", 0.0, 1.0, 0.5, "F2", "Punch"),
            ParamSpec::continuous("target", "Target", 0.0, 1.0, 0.5, "F2", "Target"),
            ParamSpec::continuous("hdr_retention", "HDR Retention", 0.0, 1.0, 0.5, "F2", "HdrRetention"),
            ParamSpec::continuous("color", "Color", -1.0, 1.0, 0.0, "F2", "ColorPush"),
            ParamSpec::whole_labels("character", "Character", 0.0, 4.0, 0.0, &["Clean", "Warm", "Film", "Vivid", "Grit"], "Character"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::AUTO_GAIN,
        create: |device| Box::new(AutoGainFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::AUTO_GAIN,
        aliases: &[
            ("char", Some("character")),
            ("hdr_ret", Some("hdr_retention")),
        ],
    }
}

// ── Uniforms for the apply pass ────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AutoGainUniforms {
    gain: f32,
    character: u32, // 0=clean, 1=warm, 2=film, 3=vivid, 4=grit
    color_push: f32,
    hdr_retention: f32,
    gain_delta: f32, // gain - 1.0, for coloration intensity
    amount: f32,     // wet/dry mix (parallel compression)
    _pad0: f32,
    _pad1: f32,
}

// ── Per-owner CPU-side envelope state ──────────────────────────────────

struct AutoGainOwnerState {
    /// Shared GPU buffer — GPU writes measured luminance, CPU reads it next frame.
    measure_buffer: manifold_gpu::GpuBuffer,
    /// Smoothed log-luminance envelope (fast EMA — the "compressor needle").
    envelope_log: f32,
    /// Long-term average log-luminance (slow EMA — for program-dependent behavior).
    long_term_log: f32,
    /// Frames since creation (for initialization).
    frame_count: u32,
    /// Last computed gain reduction in dB (for future UI metering).
    pub gain_reduction_db: f32,
}

impl AutoGainOwnerState {
    fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let measure_buffer = device.create_buffer_shared(16); // 1 f32 + padding
        // Initialize buffer to 0.5 luminance so first frame isn't garbage.
        if let Some(ptr) = measure_buffer.mapped_ptr() {
            unsafe {
                std::ptr::write(ptr as *mut f32, 0.5);
            }
        }
        Self {
            measure_buffer,
            envelope_log: 0.0, // will be set on first read
            long_term_log: 0.0,
            frame_count: 0,
            gain_reduction_db: 0.0,
        }
    }

    /// Read the GPU-written luminance measurement from the shared buffer.
    fn read_measured_luminance(&self) -> f32 {
        if let Some(ptr) = self.measure_buffer.mapped_ptr() {
            let val = unsafe { std::ptr::read(ptr as *const f32) };
            // Sanity clamp — GPU might write NaN or garbage on first frame.
            if val.is_finite() && val >= 0.0 {
                val
            } else {
                0.5
            }
        } else {
            0.5
        }
    }

    /// Update the envelope follower and compute the gain factor.
    fn update_and_compute_gain(
        &mut self,
        measured_lum: f32,
        ratio_param: f32,
        punch_param: f32,
        target_param: f32,
        dt: f32,
    ) -> f32 {
        let epsilon = 0.0001;
        let log_lum = (measured_lum + epsilon).log2();

        // First frame: initialize envelopes.
        if self.frame_count == 0 {
            self.envelope_log = log_lum;
            self.long_term_log = log_lum;
            self.frame_count = 1;
            return 1.0; // no correction on first frame
        }
        self.frame_count += 1;

        // Long-term EMA (~6s time constant) for program-dependent detection.
        let long_term_tc = 6.0_f32;
        let long_term_alpha = 1.0 - (-dt / long_term_tc).exp();
        self.long_term_log += (log_lum - self.long_term_log) * long_term_alpha;

        // Program-dependent: detect transient vs sustained content.
        let deviation = (log_lum - self.long_term_log).abs();
        let is_transient = deviation > 0.5; // ~0.5 stops = transient threshold

        // Base attack/release from Punch parameter (visual-rate timings).
        // At 60fps: 50ms ≈ 3 frames, 250ms ≈ 15 frames, 1000ms ≈ 60 frames.
        let base_attack = lerp(0.050, 0.250, punch_param);
        let base_release = lerp(1.000, 0.100, punch_param);

        // Program-dependent adjustment: widen attack on transients, tighten release.
        let attack = if is_transient {
            base_attack * 2.0
        } else {
            base_attack
        };
        let release = if is_transient {
            base_release * 0.5
        } else {
            base_release
        };

        // Choose attack or release based on direction.
        let time_constant = if log_lum > self.envelope_log {
            attack
        } else {
            release
        };
        let alpha = 1.0 - (-dt / time_constant.max(0.001)).exp();

        self.envelope_log += (log_lum - self.envelope_log) * alpha;

        // Gain computation: compress deviation from target.
        let target_log = (target_param.max(0.001)).log2();
        let env_deviation = self.envelope_log - target_log;
        let ratio = 1.0 + ratio_param * 9.0; // 1:1 to 10:1
        let compressed_deviation = env_deviation / ratio;
        let desired_log = target_log + compressed_deviation;
        let gain = 2.0_f32.powf(desired_log - self.envelope_log);

        // Gain reduction metering (for future UI display).
        self.gain_reduction_db = 20.0 * gain.log10();

        gain.clamp(0.1, 10.0) // safety clamp
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

// ── Shader sources ─────────────────────────────────────────────────────

const MEASURE_WGSL: &str = include_str!("shaders/auto_gain_measure.wgsl");
const APPLY_WGSL: &str = include_str!("shaders/auto_gain_apply.wgsl");

// ── AutoGainFX ─────────────────────────────────────────────────────────

pub struct AutoGainFX {
    /// Pipeline for the luminance measurement dispatch (single workgroup).
    measure_pipeline: manifold_gpu::GpuComputePipeline,
    /// 5 specialized apply pipelines (one per character mode).
    apply_clean: ComputeBlitHelper,
    apply_warm: ComputeBlitHelper,
    apply_film: ComputeBlitHelper,
    apply_vivid: ComputeBlitHelper,
    apply_grit: ComputeBlitHelper,
    /// Per-owner envelope state + measurement buffer.
    states: AHashMap<i64, AutoGainOwnerState>,
}

impl AutoGainFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let measure_pipeline =
            device.create_compute_pipeline(MEASURE_WGSL, "cs_main", "Auto Gain Measure");

        // Specialized apply pipelines — character mode baked in.
        let spec = |mode: &str, label: &str| -> ComputeBlitHelper {
            let source = APPLY_WGSL.replace("uniforms.character", mode);
            ComputeBlitHelper::new(device, &source, label)
        };

        Self {
            measure_pipeline,
            apply_clean: spec("0u", "Auto Gain Clean"),
            apply_warm: spec("1u", "Auto Gain Warm"),
            apply_film: spec("2u", "Auto Gain Film"),
            apply_vivid: spec("3u", "Auto Gain Vivid"),
            apply_grit: spec("4u", "Auto Gain Grit"),
            states: AHashMap::new(),
        }
    }

    fn ensure_state(&mut self, device: &manifold_gpu::GpuDevice, owner_key: i64) {
        if !self.states.contains_key(&owner_key) {
            self.states
                .insert(owner_key, AutoGainOwnerState::new(device));
        }
    }
}

impl PostProcessEffect for AutoGainFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::AUTO_GAIN
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let p = &fx.param_values;
        let amount = p.first().map(|pv| pv.value).unwrap_or(0.5);
        let ratio_param = p.get(1).map(|pv| pv.value).unwrap_or(0.5);
        let punch_param = p.get(2).map(|pv| pv.value).unwrap_or(0.5);
        let target_param = p.get(3).map(|pv| pv.value).unwrap_or(0.5);
        let hdr_retention = p.get(4).map(|pv| pv.value).unwrap_or(0.5);
        let color_push = p.get(5).map(|pv| pv.value).unwrap_or(0.0);
        let character = p.get(6).map(|pv| pv.value).unwrap_or(0.0).round() as u32;

        self.ensure_state(gpu.device, ctx.owner_key);
        let state = self.states.get_mut(&ctx.owner_key).unwrap();

        // ── CPU envelope: read previous frame's measurement, compute gain ──
        let measured_lum = state.read_measured_luminance();
        let gain = state.update_and_compute_gain(
            measured_lum,
            ratio_param,
            punch_param,
            target_param,
            ctx.dt,
        );

        // ── Dispatch 1: measure current frame's luminance (for next frame) ──
        gpu.native_enc.dispatch_compute(
            &self.measure_pipeline,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &state.measure_buffer,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "Auto Gain Measure",
        );

        // ── Dispatch 2: apply gain with character coloration ──
        let uniforms = AutoGainUniforms {
            gain,
            character: character.min(4),
            color_push,
            hdr_retention,
            gain_delta: gain - 1.0,
            amount,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        let helper = match character.min(4) {
            0 => &self.apply_clean,
            1 => &self.apply_warm,
            2 => &self.apply_film,
            3 => &self.apply_vivid,
            _ => &self.apply_grit,
        };
        helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Auto Gain Apply",
            ctx.width,
            ctx.height,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Measurement buffers are resolution-independent (just a single float).
        // Envelope state carries across resolution changes — no need to clear.
    }

}
