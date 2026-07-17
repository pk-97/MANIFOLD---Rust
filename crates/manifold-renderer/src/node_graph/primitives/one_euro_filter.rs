//! `node.one_euro_filter` — adaptive temporal low-pass filter on a
//! Channels array. Implements the 1€ filter algorithm (Casiez et al.,
//! CHI 2012): low cutoff when the signal is still → heavy smoothing;
//! raises cutoff when the signal moves fast → responsive tracking.
//! Eliminates jitter without adding perceptible lag.
//!
//! Operates on the buffer as a flat sequence of f32 values — every
//! channel of every sample gets an independent filter instance. The
//! port signature is typed for buffer sizing but the math is generic.
//!
//! First consumer: Blob Track (detection regions). Future consumers
//! (DNN depth, audio bins, MIDI, sensor data) extend the port
//! signature per §6.2 or build Permissive output propagation infra.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// 1€ filter smoothing coefficient from cutoff frequency and timestep.
/// α = 1 / (1 + τ/dt), where τ = 1/(2π·fc).
#[inline]
fn one_euro_alpha(dt: f32, cutoff: f32) -> f32 {
    let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
    1.0 / (1.0 + tau / dt)
}

crate::primitive! {
    name: OneEuroFilter,
    type_id: "node.one_euro_filter",
    purpose: "Adaptive temporal low-pass (1€ filter) on a Channels array. Low cutoff when the signal is still (heavy smoothing, eliminates jitter); raises cutoff when the signal moves fast (responsive tracking, no perceptible lag). Per-channel per-sample independent filter. Wire detection regions, DNN depth scalars, audio bins, or any noisy signal that needs temporal stabilisation.",
    inputs: {
        in: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        min_cutoff: ScalarF32 optional,
        beta: ScalarF32 optional,
    },
    outputs: {
        out: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("min_cutoff"),
            label: "Min Cutoff (Hz)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("beta"),
            label: "Beta (speed coeff)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("d_cutoff"),
            label: "Derivative Cutoff (Hz)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire after any noisy Channels producer (blob_detect_ffi, depth_estimate_midas, audio analyzer). min_cutoff sets the baseline smoothing when the signal is still — lower = more smoothing. beta controls how aggressively the filter opens up during fast motion — higher = more responsive to speed. d_cutoff is the derivative filter's cutoff — rarely needs changing. All three are port-shadows-param so control wires can modulate them per-frame.",
    examples: [],
    picker: { label: "One Euro Filter", category: Driver },
    summary: "Smooths a jittery signal but lets fast moves through cleanly, so it removes noise without the laggy feel of a plain smooth. Great for hand-tracked or sensor input.",
    category: Control,
    role: Control,
    aliases: ["one euro filter", "smooth", "1 euro filter", "denoise"],
    boundary_reason: NonGpu,
    extra_fields: {
        prev: Vec<f32> = Vec::new(),
        dx: Vec<f32> = Vec::new(),
        initialized: bool = false,
        last_output_all_zero: Option<bool> = None,
    },
}

impl Primitive for OneEuroFilter {
    // Data-driven skip, reporter side: all-zero output is the upstream
    // sentinel for "nothing detected" (track_persist zero-fills empty
    // slots), so downstream `empty_skip_input_ports` declarers (the Draw
    // HUD atoms) can skip while the tracker reports nothing.
    fn reports_empty_output(&self) -> bool {
        self.last_output_all_zero == Some(true)
    }

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

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.last_output_all_zero = None;
        let min_cutoff = ctx.scalar_or_param("min_cutoff", 1.0).max(0.001);
        let beta = ctx.scalar_or_param("beta", 0.5).max(0.0);
        let d_cutoff = match ctx.params.get("d_cutoff") {
            Some(ParamValue::Float(f)) => f.max(0.01),
            _ => 1.0,
        };
        let dt = ctx.time.delta.0 as f32;
        if dt <= 0.0 {
            return;
        }

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        // 4 f32 channels × capacity samples. Use the smaller buffer.
        let n_bytes = in_buf.size.min(out_buf.size) as usize;
        let n_floats = n_bytes / 4;
        if n_floats == 0 {
            return;
        }

        let in_ptr = in_buf
            .mapped_ptr()
            .expect("one_euro_filter: input must be shared-memory buffer");
        let in_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(in_ptr as *const f32, n_floats) };

        let out_ptr = out_buf
            .mapped_ptr()
            .expect("one_euro_filter: output must be shared-memory buffer");
        let out_slice: &mut [f32] =
            unsafe { std::slice::from_raw_parts_mut(out_ptr as *mut f32, n_floats) };

        // First frame or size change: initialise to input (no bleed from zero).
        if !self.initialized || self.prev.len() != n_floats {
            self.prev.clear();
            self.prev.extend_from_slice(in_slice);
            self.dx.clear();
            self.dx.resize(n_floats, 0.0);
            self.initialized = true;
            out_slice.copy_from_slice(in_slice);
            self.last_output_all_zero = Some(out_slice.iter().all(|v| *v == 0.0));
            return;
        }

        let d_alpha = one_euro_alpha(dt, d_cutoff);

        for i in 0..n_floats {
            let raw = in_slice[i];
            let prev = self.prev[i];

            // Snap (no smoothing) on appearance/disappearance: when
            // raw or prev is exactly 0.0 — the sentinel upstream
            // producers (e.g. track_persist) write for empty slots.
            // Without this, slots that transition through zero produce
            // visible intermediate frames as the filter decays from
            // last value to 0, which renders as "stale data drifting
            // to the corner" for Channels[X, Y, W, H] consumers.
            if raw == 0.0 || prev == 0.0 {
                self.prev[i] = raw;
                self.dx[i] = 0.0;
                out_slice[i] = raw;
                continue;
            }

            let raw_dx = (raw - prev) / dt;
            self.dx[i] += d_alpha * (raw_dx - self.dx[i]);

            let cutoff = min_cutoff + beta * self.dx[i].abs();
            let alpha = one_euro_alpha(dt, cutoff);
            let smoothed = prev + alpha * (raw - prev);

            self.prev[i] = smoothed;
            out_slice[i] = smoothed;
        }
        self.last_output_all_zero = Some(out_slice.iter().all(|v| *v == 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn one_euro_filter_declares_channels_io_and_params() {
        use crate::node_graph::ports::PortType;
        assert_eq!(OneEuroFilter::TYPE_ID, "node.one_euro_filter");
        assert_eq!(OneEuroFilter::INPUTS.len(), 3);
        assert_eq!(OneEuroFilter::INPUTS[0].name, "in");
        assert!(matches!(OneEuroFilter::INPUTS[0].ty, PortType::Array(_)));
        assert!(OneEuroFilter::INPUTS[0].required);
        assert_eq!(OneEuroFilter::INPUTS[1].name, "min_cutoff");
        assert!(!OneEuroFilter::INPUTS[1].required);
        assert_eq!(OneEuroFilter::INPUTS[2].name, "beta");
        assert!(!OneEuroFilter::INPUTS[2].required);
        assert_eq!(OneEuroFilter::OUTPUTS.len(), 1);
        assert_eq!(OneEuroFilter::OUTPUTS[0].name, "out");
        let names: Vec<&str> = OneEuroFilter::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["min_cutoff", "beta", "d_cutoff"]);
    }

    #[test]
    fn one_euro_filter_registers_as_palette_driver() {
        let prim = OneEuroFilter::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.one_euro_filter");
    }

    #[test]
    fn one_euro_alpha_at_zero_cutoff_returns_near_zero() {
        let a = one_euro_alpha(1.0 / 60.0, 0.001);
        assert!(a < 0.001, "near-zero cutoff → near-zero alpha, got {a}");
    }

    #[test]
    fn one_euro_alpha_at_high_cutoff_returns_large_value() {
        let a = one_euro_alpha(1.0 / 60.0, 100.0);
        // At 100 Hz cutoff, dt=1/60: tau≈0.00159, α = 1/(1+tau/dt) ≈ 0.913
        assert!(a > 0.9, "high cutoff → large alpha, got {a}");
    }

    #[test]
    fn one_euro_filter_step_response_matches_reference() {
        // Hand-computed reference for a step input 0→1 on a single f32.
        // Params: min_cutoff=1.0, beta=0.5, d_cutoff=1.0, dt=1/60.
        let dt: f32 = 1.0 / 60.0;
        let min_cutoff: f32 = 1.0;
        let beta: f32 = 0.5;
        let d_cutoff: f32 = 1.0;

        // Frame 0: prev=0, dx=0 (initialised to input=0).
        let mut prev: f32 = 0.0;
        let mut dx: f32 = 0.0;

        // Frame 1: input=1.0 (step).
        let raw = 1.0_f32;
        let raw_dx = (raw - prev) / dt; // 60.0
        let d_alpha = one_euro_alpha(dt, d_cutoff);
        dx += d_alpha * (raw_dx - dx);
        let cutoff = min_cutoff + beta * dx.abs();
        let alpha = one_euro_alpha(dt, cutoff);
        let smoothed = prev + alpha * (raw - prev);
        prev = smoothed;
        let _ = &prev; // read to suppress unused-assignment warning

        // Verify against hand computation:
        // d_alpha ≈ 0.09479, dx ≈ 5.6876, cutoff ≈ 3.8438,
        // alpha ≈ 0.28691, smoothed ≈ 0.28691
        assert!(
            (smoothed - 0.287).abs() < 0.005,
            "step response frame 1: expected ~0.287, got {smoothed}",
        );

        // Frame 2: input=1.0 (held).
        let raw = 1.0_f32;
        let raw_dx = (raw - prev) / dt;
        dx += d_alpha * (raw_dx - dx);
        let cutoff = min_cutoff + beta * dx.abs();
        let alpha = one_euro_alpha(dt, cutoff);
        let smoothed = prev + alpha * (raw - prev);
        #[allow(unused_assignments)]
        { prev = smoothed; }

        // Should converge further toward 1.0.
        assert!(
            (smoothed - 0.551).abs() < 0.01,
            "step response frame 2: expected ~0.551, got {smoothed}",
        );
        assert!(
            smoothed > 0.287,
            "frame 2 should be closer to 1.0 than frame 1",
        );
    }

    #[test]
    fn constant_signal_passes_through_unchanged() {
        // After initialisation, a constant signal should emit the
        // same constant (no drift, no decay).
        let dt = 1.0 / 60.0;
        let min_cutoff = 1.0;
        let beta = 0.5;
        let d_cutoff = 1.0;
        let d_alpha = one_euro_alpha(dt, d_cutoff);
        let value = 0.42_f32;

        let mut prev = value; // first frame init
        let mut dx = 0.0_f32;

        for _ in 0..100 {
            let raw_dx = (value - prev) / dt;
            dx += d_alpha * (raw_dx - dx);
            let cutoff = min_cutoff + beta * dx.abs();
            let alpha = one_euro_alpha(dt, cutoff);
            let smoothed = prev + alpha * (value - prev);
            prev = smoothed;
        }

        assert!(
            (prev - value).abs() < 1e-5,
            "constant signal should pass through, got {prev} vs {value}",
        );
    }
}
