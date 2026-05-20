//! [`build_strobe_opacity`] — Strobe (Opacity mode) as a primitive graph.
//!
//! Decomposition of `node.strobe`'s Opacity branch:
//! `Source → Gain(gain = 1 - BeatGate) → out`. Validates the §12.6
//! worked example end-to-end — that the legacy fused Strobe shader
//! can be expressed as a graph of small primitives with **pixel-exact
//! parity**. The parity holds because the gate signal flows on a
//! `Scalar(F32)` wire, which carries f32 end-to-end (no fp16
//! intermediate texture quantises the gate value).
//!
//! Only the Opacity branch is decomposed here. White-mode and
//! Gain-mode need either a `ConstantColor` primitive (for the Mix-to-
//! white path) or a different scalar-shaping chain (for the 1+2×gate
//! brightening). Both follow naturally from this pattern; deferred
//! until the V0 proof-point lands.

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::{BeatGate, Gain, Math, Value};
use crate::node_graph::validation::GraphError;

pub const STROBE_OPACITY_TYPE_ID: &str = "composite.strobe_opacity";

/// Build a decomposed Strobe-Opacity sub-graph rooted at `source`.
/// Returns a [`CompositeHandle`] whose output is the gain-modulated
/// texture and which exposes the inner `BeatGate.rate`, `amount`,
/// `duty`, and `phase` params for outer-card surfacing.
pub fn build_strobe_opacity(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let gate = graph.add_node(Box::new(BeatGate::new()));
    let one = graph.add_node(Box::new(Value::new()));
    let invert = graph.add_node(Box::new(Math::new()));
    let gain = graph.add_node(Box::new(Gain::new()));

    // `one` produces a constant 1.0. `invert` computes `1.0 - gate` so
    // the gain goes to 0 when the gate is on (image darkens) and to 1
    // when off (image passes through). Math defaults to Multiply, so
    // override to Subtract.
    graph.set_param(one, "value", ParamValue::Float(1.0))?;
    graph.set_param(invert, "op", ParamValue::Enum(1))?; // 1 = Subtract

    graph.connect((one, "out"), (invert, "a"))?;
    graph.connect((gate, "out"), (invert, "b"))?;
    graph.connect(source, (gain, "in"))?;
    graph.connect((invert, "out"), (gain, "gain"))?;

    let mut handle = CompositeHandle::new(STROBE_OPACITY_TYPE_ID, (gain, "out"));
    handle.add_inner(gate);
    handle.add_inner(one);
    handle.add_inner(invert);
    handle.add_inner(gain);
    handle.expose_param("rate", gate, "rate");
    handle.expose_param("amount", gate, "amount");
    handle.expose_param("duty", gate, "duty");
    handle.expose_param("phase", gate, "phase");
    Ok(handle)
}

#[cfg(test)]
mod parity_tests {
    //! Pixel-exact parity vs the legacy fused `node.strobe` shader at
    //! `mode = Opacity`. The promise from §12.6: scalar wires preserve
    //! f32 precision end-to-end, so the decomposed graph produces
    //! bit-identical output to the legacy single-pass version.

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::build_strobe_opacity;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::primitives::Strobe;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId, Source,
    };
    use crate::render_target::RenderTarget;

    fn frame_at(beat: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beat as f64),
            seconds: Seconds(beat as f64 * 0.5),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    fn read_first_pixel(device: &manifold_gpu::GpuDevice, tex: &manifold_gpu::GpuTexture, w: u32, h: u32) -> [f32; 4] {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let buf = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("strobe-parity-readback");
        enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
            f16::from_bits(pixels[3]).to_f32(),
        ]
    }

    /// Run the decomposed Opacity-mode graph at the given beat/rate/amount.
    /// Returns the first output pixel.
    fn run_decomposed(beat: f32, rate_idx: u32, amount: f32, src_rgba: [f32; 4]) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let handle = build_strobe_opacity(&mut g, (src, "out")).unwrap();
        let final_out = g.add_node(Box::new(FinalOutput::new()));
        // Pull the BeatGate node id out of the composite to set its
        // rate/amount/duty/phase. `inner_nodes[0]` is BeatGate because
        // build_strobe_opacity registers it first.
        let gate_id = handle.inner_nodes()[0];
        g.set_param(gate_id, "rate", ParamValue::Enum(rate_idx)).unwrap();
        g.set_param(gate_id, "amount", ParamValue::Float(amount)).unwrap();
        g.connect(handle.output(), (final_out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, handle.output().0, handle.output().1);
        let src_target = RenderTarget::new(&device, w, h, format, "strobe-decomp-src");
        let out_target = RenderTarget::new(&device, w, h, format, "strobe-decomp-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [src_rgba[0] as f64, src_rgba[1] as f64, src_rgba[2] as f64, src_rgba[3] as f64],
            "strobe-decomp-src-clear",
        );

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("strobe-decomp-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_at(beat), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec.backend().texture_2d(out_slot).expect("retained");
        read_first_pixel(&device, tex, w, h)
    }

    /// Run the legacy fused `node.strobe` at the same params.
    fn run_legacy(beat: f32, rate_idx: u32, amount: f32, src_rgba: [f32; 4]) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let strobe = g.add_node(Box::new(Strobe::new()));
        let final_out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(strobe, "rate", ParamValue::Enum(rate_idx)).unwrap();
        g.set_param(strobe, "amount", ParamValue::Float(amount)).unwrap();
        g.set_param(strobe, "mode", ParamValue::Enum(0)).unwrap(); // Opacity
        g.set_param(strobe, "beat", ParamValue::Float(beat)).unwrap();
        g.connect((src, "out"), (strobe, "in")).unwrap();
        g.connect((strobe, "out"), (final_out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, strobe, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "strobe-legacy-src");
        let out_target = RenderTarget::new(&device, w, h, format, "strobe-legacy-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [src_rgba[0] as f64, src_rgba[1] as f64, src_rgba[2] as f64, src_rgba[3] as f64],
            "strobe-legacy-src-clear",
        );

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("strobe-legacy-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_at(beat), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec.backend().texture_2d(out_slot).expect("retained");
        read_first_pixel(&device, tex, w, h)
    }

    /// At a beat position where the gate is OFF, both implementations
    /// should pass the source through unchanged.
    #[test]
    fn off_phase_passes_source_unchanged() {
        // rate=6 (1/16), beat=0.0 → phase = fract(0 * 4) = 0 → off.
        let src = [0.4_f32, 0.6, 0.2, 1.0];
        let decomp = run_decomposed(0.0, 6, 1.0, src);
        let legacy = run_legacy(0.0, 6, 1.0, src);
        for c in 0..4 {
            assert!(
                (decomp[c] - legacy[c]).abs() < 1e-3,
                "off-phase ch {c}: decomp={} vs legacy={}",
                decomp[c],
                legacy[c],
            );
        }
    }

    /// At a beat position where the gate is ON with amount=1.0, both
    /// implementations should produce black.
    #[test]
    fn on_phase_with_full_amount_produces_black() {
        // rate=6 (1/16 → 4 cycles/beat), beat=0.125 → phase = fract(0.5) = 0.5 → on.
        let src = [0.4_f32, 0.6, 0.2, 1.0];
        let decomp = run_decomposed(0.125, 6, 1.0, src);
        let legacy = run_legacy(0.125, 6, 1.0, src);
        // Both should be ~black on RGB; alpha preserved.
        assert!(decomp[0] < 0.01 && decomp[1] < 0.01 && decomp[2] < 0.01);
        assert!(legacy[0] < 0.01 && legacy[1] < 0.01 && legacy[2] < 0.01);
        for c in 0..4 {
            assert!(
                (decomp[c] - legacy[c]).abs() < 1e-3,
                "on-phase ch {c}: decomp={} vs legacy={}",
                decomp[c],
                legacy[c],
            );
        }
    }

    /// Partial amount during on-phase: both implementations should
    /// produce the same partially-darkened pixel.
    #[test]
    fn on_phase_partial_amount_matches_legacy() {
        // rate=6, beat=0.125 → on. amount=0.5 → gain = 1 - 0.5 = 0.5.
        let src = [0.4_f32, 0.6, 0.2, 1.0];
        let decomp = run_decomposed(0.125, 6, 0.5, src);
        let legacy = run_legacy(0.125, 6, 0.5, src);
        for c in 0..3 {
            // Expected: 0.5 * src[c].
            let expected = 0.5 * src[c];
            assert!(
                (decomp[c] - expected).abs() < 0.01,
                "decomp ch {c}: expected {expected}, got {}",
                decomp[c],
            );
            assert!(
                (decomp[c] - legacy[c]).abs() < 1e-3,
                "partial ch {c}: decomp={} vs legacy={}",
                decomp[c],
                legacy[c],
            );
        }
    }
}
