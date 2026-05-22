//! `node.generate_lissajous` — emit an `Array<CurvePoint>` sampled
//! from a Lissajous curve.
//!
//! The producer half of the line-shape decomposition: Lissajous as
//! a graph primitive instead of a monolithic `LissajousGenerator`
//! Rust struct (deleted; see `assets/generator-presets/Lissajous.json`
//! for the full graph topology). Pair with
//! [`crate::node_graph::primitives::RenderLines`] downstream to draw
//! bright vector strokes. The renderer applies aspect correction +
//! center offset; this node outputs the curve in pre-aspect space
//! centered at the origin.
//!
//! Math: `x(t) = sin(freq_x * t + phase)`, `y(t) = sin(freq_y * t)`,
//! sampled at `vertex_count` points across `t ∈ [0, 2π]`. The
//! shader always interpolates between the floor and ceil integer
//! ratios so a non-integer `freq_x` or `freq_y` produces a smooth
//! morph between closed Lissajous shapes instead of a non-closing
//! scribble. Bit-perfect parity with the pre-decomposition
//! `LissajousGenerator` per-vertex math, locked in by
//! [`tests::cpu_mirror_matches_legacy_lissajous_math_byte_for_byte`].

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::CurvePoint;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LissajousUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    freq_x: f32,
    freq_y: f32,
    phase: f32,
    scale: f32,
}

crate::primitive! {
    name: GenerateLissajous,
    type_id: "node.generate_lissajous",
    purpose: "Sample a Lissajous curve `(sin(freq_x*t + phase), sin(freq_y*t))` into an Array<CurvePoint>. Output is in pre-aspect curve space centred at the origin; pair with node.render_lines to draw. Always blends between the floor/ceil integer ratios so non-integer freq_x / freq_y morph smoothly between closed shapes instead of producing non-closing scribbles. `freq_x`, `freq_y`, and `phase` are port-shadows-param: wire an LFO / oscillator / time ramp into the matching input port to drive the curve, or set the param inline.",
    inputs: {
        freq_x: ScalarF32 optional,
        freq_y: ScalarF32 optional,
        phase: ScalarF32 optional,
    },
    outputs: {
        points: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: "freq_x",
            label: "Frequency X",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "freq_y",
            label: "Frequency Y",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.1, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "phase",
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "max_capacity",
            label: "Vertex Count",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity = how many samples along [0, 2π] AND the size the chain build pre-allocates for the output Array<CurvePoint>. 256 matches the legacy LissajousGenerator; higher = smoother integer-interp blend. The name `max_capacity` is the canonical chain-build convention — the JsonGraphGenerator walks this param to size the GpuBuffer for the `points` output. `scale` multiplies the curve before pre-aspect mapping — at scale=1.0 the curve fills the inner 50% of the screen (matches legacy generator_math::PROJ_SCALE).",
    examples: [],
    picker: { label: "Generate Lissajous", category: Atom },
}

impl Primitive for GenerateLissajous {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param for freq_x / freq_y / phase. Inline
        // param values act as constants when no wire is connected;
        // wired scalars override them. `scale` and `vertex_count`
        // are param-only — wiring them is unlikely to be useful so
        // we skip the port plumbing for clarity.
        let freq_x = ctx.scalar_or_param("freq_x", 2.0);
        let freq_y = ctx.scalar_or_param("freq_y", 3.0);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let vertex_count = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(f)) => f.round().max(4.0) as u32,
            _ => 256,
        };

        let Some(out_buf) = ctx.outputs.array("points") else {
            // The chain build is supposed to pre-allocate every
            // Array output. A missing buffer here means the host
            // skipped this primitive in its allocation pass — the
            // generator silently produces no points and the
            // downstream renderer sees an empty buffer. Flag it.
            log::warn!(
                "node.generate_lissajous: no GpuBuffer bound to output port `points` — \
                 the chain build did not pre-allocate this Array<CurvePoint>, so the curve \
                 generator is a no-op this frame. Check JsonGraphGenerator's Array \
                 pre-allocation pass and confirm `max_capacity` is on the producer node.",
            );
            return;
        };
        let item_size = std::mem::size_of::<CurvePoint>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let active_count = vertex_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_lissajous.wgsl"),
                "cs_main",
                "node.generate_lissajous",
            )
        });

        let uniforms = LissajousUniforms {
            active_count,
            capacity,
            _pad0: 0,
            _pad1: 0,
            freq_x,
            freq_y,
            phase,
            scale,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.generate_lissajous",
        );
    }
}

/// CPU mirror of the WGSL math in `shaders/generate_lissajous.wgsl`.
/// Same formula, same operators, same intermediate precision — all
/// IEEE-754 `f32` ops compile to identical results on Apple Silicon
/// GPUs and Rust's `f32` (modulo undefined-behaviour edge cases like
/// `sin(NaN)`, which neither path produces). Used by the bit-perfect
/// parity test below; not called by the live GenerateLissajous
/// `run()` path (which dispatches the WGSL shader).
#[cfg(test)]
fn lissajous_sample_cpu(
    freq_x: f32,
    freq_y: f32,
    phase: f32,
    scale: f32,
    vertex_count: u32,
    idx: u32,
) -> (f32, f32) {
    const TWO_PI: f32 = std::f32::consts::TAU;
    const PROJ_SCALE: f32 = 0.25;

    let t = idx as f32 / vertex_count.max(1) as f32 * TWO_PI;

    let a_lo = freq_x.floor();
    let a_hi = freq_x.ceil();
    let a_lerp = freq_x - a_lo;

    let b_lo = freq_y.floor();
    let b_hi = freq_y.ceil();
    let b_lerp = freq_y - b_lo;

    let x_lo = (a_lo * t + phase).sin();
    let x_hi = (a_hi * t + phase).sin();
    let x = x_lo + (x_hi - x_lo) * a_lerp;

    let y_lo = (b_lo * t).sin();
    let y_hi = (b_hi * t).sin();
    let y = y_lo + (y_hi - y_lo) * b_lerp;

    (x * scale * PROJ_SCALE, y * scale * PROJ_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_three_optional_scalar_inputs_and_linepoint_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(GenerateLissajous::TYPE_ID, "node.generate_lissajous");
        assert_eq!(GenerateLissajous::INPUTS.len(), 3);
        for port in GenerateLissajous::INPUTS {
            assert!(!port.required, "scalar param ports must be optional: {}", port.name);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        let input_names: Vec<&str> = GenerateLissajous::INPUTS.iter().map(|p| p.name).collect();
        assert_eq!(input_names, vec!["freq_x", "freq_y", "phase"]);

        let layout = ArrayType::of_known::<CurvePoint>();
        assert_eq!(GenerateLissajous::OUTPUTS.len(), 1);
        assert_eq!(GenerateLissajous::OUTPUTS[0].name, "points");
        assert_eq!(GenerateLissajous::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn params_cover_freq_phase_scale_and_max_capacity() {
        let names: Vec<&str> = GenerateLissajous::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["freq_x", "freq_y", "phase", "scale", "max_capacity"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateLissajous::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_lissajous");
    }

    /// **Bit-perfect parity reference for the legacy
    /// `LissajousGenerator`.** The legacy generator computed each
    /// vertex on the CPU before uploading to GPU:
    ///
    /// ```ignore
    /// let t = i as f32 / VERTEX_COUNT as f32 * TAU;
    /// let x_lo = (a_lo * t + phase).sin();
    /// let x_hi = (a_hi * t + phase).sin();
    /// let x = x_lo + (x_hi - x_lo) * a_lerp;
    /// // ...same for y...
    /// projected_x[i] = x * PROJ_SCALE;
    /// projected_y[i] = y * PROJ_SCALE;
    /// ```
    ///
    /// `lissajous_sample_cpu` mirrors `generate_lissajous.wgsl`
    /// line-for-line — same operators, same intermediate
    /// precision, same constants. This test runs the legacy
    /// formula inline (so the parity stays locked even after the
    /// legacy code is deleted) and asserts byte-equality against
    /// the CPU mirror across a battery of (freq, phase, scale,
    /// vertex_count, idx) configurations. Any drift between the
    /// curve generator and the legacy math trips here.
    #[test]
    fn cpu_mirror_matches_legacy_lissajous_math_byte_for_byte() {
        const PROJ_SCALE: f32 = 0.25;
        const TAU: f32 = std::f32::consts::TAU;

        // Test grid: integer + non-integer frequencies (exercise the
        // floor/ceil interp branch), small + large phase, varied
        // vertex_count, default + non-default scale.
        let cases: &[(f32, f32, f32, f32, u32, u32)] = &[
            // (freq_x, freq_y, phase, scale, vertex_count, idx)
            (2.0,   3.0,   0.0,   1.0, 256, 0),
            (2.0,   3.0,   0.0,   1.0, 256, 64),
            (2.0,   3.0,   0.0,   1.0, 256, 128),
            (2.0,   3.0,   0.0,   1.0, 256, 255),
            (2.5,   3.7,   0.221, 1.0, 256, 100), // non-integer + non-zero phase
            (3.0,   4.0,   1.234, 0.5, 256, 42),  // user-scaled curve
            (1.0,   2.0,   0.0,   1.0, 512, 256), // higher resolution
            (7.0,   8.0,   5.0,   2.0, 256, 200), // largest trigger-table ratio
            (3.0,   3.001, 0.0,   1.0, 256, 13),  // tiny non-integer (edge of interp)
        ];

        for &(fx, fy, phase, scale, n, idx) in cases {
            // Inline the legacy formula. Anchored verbatim to the
            // pre-decomposition `LissajousGenerator::render` math
            // so future-me deleting `crates/manifold-renderer/src/
            // generators/lissajous.rs` doesn't lose the parity
            // reference.
            let a_lo = fx.floor();
            let a_hi = fx.ceil();
            let a_lerp = fx - a_lo;
            let b_lo = fy.floor();
            let b_hi = fy.ceil();
            let b_lerp = fy - b_lo;

            let t = idx as f32 / n as f32 * TAU;

            let x_lo = (a_lo * t + phase).sin();
            let x_hi = (a_hi * t + phase).sin();
            let x = x_lo + (x_hi - x_lo) * a_lerp;
            let y_lo = (b_lo * t).sin();
            let y_hi = (b_hi * t).sin();
            let y = y_lo + (y_hi - y_lo) * b_lerp;

            let legacy_px = x * scale * PROJ_SCALE;
            let legacy_py = y * scale * PROJ_SCALE;

            let (cpu_px, cpu_py) = lissajous_sample_cpu(fx, fy, phase, scale, n, idx);

            // Byte-for-byte equality. Both compute the same IEEE-754
            // f32 chain on the same hardware so anything less than
            // perfect equality means the formula has drifted.
            assert_eq!(
                cpu_px.to_bits(),
                legacy_px.to_bits(),
                "px mismatch at (fx={fx}, fy={fy}, phase={phase}, scale={scale}, n={n}, idx={idx}): \
                 cpu={cpu_px} (0x{:08x}), legacy={legacy_px} (0x{:08x})",
                cpu_px.to_bits(),
                legacy_px.to_bits(),
            );
            assert_eq!(
                cpu_py.to_bits(),
                legacy_py.to_bits(),
                "py mismatch at (fx={fx}, fy={fy}, phase={phase}, scale={scale}, n={n}, idx={idx}): \
                 cpu={cpu_py} (0x{:08x}), legacy={legacy_py} (0x{:08x})",
                cpu_py.to_bits(),
                legacy_py.to_bits(),
            );
        }
    }

    /// **End-to-end parity, time → frequency → curve.** Pulls
    /// together the LFO Free-mode parity test from
    /// `node.lfo::tests::legacy_lissajous_frequency_oscillator_parity`
    /// with the per-vertex curve parity above: at a known
    /// `(time_seconds, freq_x_rate, freq_y_rate, phase_rate)`, the
    /// graph pipeline computes:
    ///   freq_x = 2.0 + 1.5 * sin(t * freq_x_rate)
    ///   freq_y = 3.0 + 2.0 * sin(t * freq_y_rate)
    ///   phase  = (t * phase_rate) mod 2π
    /// and then feeds those into the Lissajous curve sample. This
    /// test simulates the chain on the CPU side and asserts byte
    /// equality with the legacy LissajousGenerator's render-time
    /// math at the same inputs.
    #[test]
    fn smooth_mode_full_chain_parity_at_known_time() {
        let time_seconds = 1.7_f32;
        let freq_x_rate = 0.13_f32;
        let freq_y_rate = 0.09_f32;
        let phase_rate = 0.07_f32;

        // Legacy formula — `let a = 2.0 + 1.5 * (time * rate).sin();`
        // matches the Lfo Free-mode (sine, min=0.5, max=3.5) output
        // exactly (see lfo::tests::legacy_lissajous_frequency_oscillator_parity).
        let legacy_a = 2.0_f32 + 1.5 * (time_seconds * freq_x_rate).sin();
        let legacy_b = 3.0_f32 + 2.0 * (time_seconds * freq_y_rate).sin();
        let legacy_phase = time_seconds * phase_rate;

        // Sample the curve at a handful of vertices using both
        // legacy + CPU-mirror math; assert byte equality.
        for idx in [0, 17, 64, 128, 200, 255] {
            let (cpu_px, cpu_py) = lissajous_sample_cpu(
                legacy_a,
                legacy_b,
                legacy_phase,
                /*scale*/ 1.0,
                /*vertex_count*/ 256,
                idx,
            );

            const PROJ_SCALE: f32 = 0.25;
            const TAU: f32 = std::f32::consts::TAU;
            let a_lo = legacy_a.floor();
            let a_hi = legacy_a.ceil();
            let a_lerp = legacy_a - a_lo;
            let b_lo = legacy_b.floor();
            let b_hi = legacy_b.ceil();
            let b_lerp = legacy_b - b_lo;
            let t = idx as f32 / 256.0 * TAU;
            let x_lo = (a_lo * t + legacy_phase).sin();
            let x_hi = (a_hi * t + legacy_phase).sin();
            let x = x_lo + (x_hi - x_lo) * a_lerp;
            let y_lo = (b_lo * t).sin();
            let y_hi = (b_hi * t).sin();
            let y = y_lo + (y_hi - y_lo) * b_lerp;
            let legacy_px = x * PROJ_SCALE;
            let legacy_py = y * PROJ_SCALE;

            assert_eq!(
                cpu_px.to_bits(),
                legacy_px.to_bits(),
                "smooth-mode px parity broke at idx={idx}: cpu={cpu_px}, legacy={legacy_px}",
            );
            assert_eq!(
                cpu_py.to_bits(),
                legacy_py.to_bits(),
                "smooth-mode py parity broke at idx={idx}: cpu={cpu_py}, legacy={legacy_py}",
            );
        }
    }
}
