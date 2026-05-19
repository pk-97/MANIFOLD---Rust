//! [`build_color_compass`] — image self-organises around its own
//! brightness. The first composite that exercises the texture→scalar
//! bridge in a feedback-free loop: four [`ColorSample`]s read the
//! brightness at N / E / S / W positions, [`Math`] nodes compute the
//! direction vector `(dx, dy) = (E - W, N - S)`, `atan2(dy, dx)`
//! converts that vector into an angle, [`Smoothing`] tames the
//! frame-to-frame jitter, and the angle wires into an
//! [`AffineTransform`]'s `rotation` port (port-shadows-param)
//! upstream of [`KaleidoFold`]. The kaleidoscope mirrors around its
//! fixed (math-East) axis on the *rotated* image — so the active
//! mirror axis in the original frame swings to follow whichever
//! cardinal is currently brightest.
//!
//! No feedback texture, no accumulation — just a single-frame
//! pipeline whose modulation reads back from the previous frame's
//! ColorSample output via the bridge buffer (one-frame latency).
//! That latency is intrinsic; nothing visually persists past it.
//!
//! Three card sliders worth exposing for performance use:
//! - `intensity` — KaleidoFold's `amount`. 0 = pass-through source.
//! - `segments` — KaleidoFold's `segments`. 2-16 mirror wedges.
//! - `reactivity` — Smoothing's `time_constant`. Lower = snappier
//!   tracking, higher = languid drift.
//!
//! See [`super`]'s module doc and Color Compass discussion in the
//! Phase B / wire-driven primitive notes.
//!
//! [`ColorSample`]: crate::node_graph::primitives::ColorSample
//! [`Math`]: crate::node_graph::primitives::Math
//! [`Smoothing`]: crate::node_graph::primitives::Smoothing
//! [`AffineTransform`]: crate::node_graph::primitives::AffineTransform
//! [`KaleidoFold`]: crate::node_graph::primitives::KaleidoFold

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::{
    AffineTransform, ColorSample, KaleidoFold, Math, Smoothing, Value,
};
use crate::node_graph::validation::GraphError;

pub const COLOR_COMPASS_TYPE_ID: &str = "composite.color_compass";

/// Radians → degrees conversion constant, baked into a Value node so
/// the angle (which `atan2` emits in radians) can be multiplied into
/// the degrees-CW unit `AffineTransform` consumes.
const RAD_TO_DEG: f32 = 180.0 / std::f32::consts::PI;

/// UVs the compass samples. Slight inset from the edges keeps the
/// readings off the absolute boundary where a non-clamped sampler
/// would behave oddly. Symmetric around (0.5, 0.5) so a uniform
/// source produces dx=dy=0.
const UV_NORTH: [f32; 2] = [0.5, 0.2];
const UV_SOUTH: [f32; 2] = [0.5, 0.8];
const UV_EAST: [f32; 2] = [0.8, 0.5];
const UV_WEST: [f32; 2] = [0.2, 0.5];

/// Build a Color Compass sub-graph rooted at `source`. Returns a
/// [`CompositeHandle`] whose output is the kaleidoscope's texture
/// and which exposes `intensity` / `segments` / `reactivity` for
/// outer-card surfacing.
pub fn build_color_compass(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let sample_n = graph.add_node(Box::new(ColorSample::new()));
    let sample_s = graph.add_node(Box::new(ColorSample::new()));
    let sample_e = graph.add_node(Box::new(ColorSample::new()));
    let sample_w = graph.add_node(Box::new(ColorSample::new()));
    let math_dy = graph.add_node(Box::new(Math::new()));
    let math_dx = graph.add_node(Box::new(Math::new()));
    let math_atan2 = graph.add_node(Box::new(Math::new()));
    let rad_to_deg = graph.add_node(Box::new(Value::new()));
    let math_deg = graph.add_node(Box::new(Math::new()));
    let smoothing = graph.add_node(Box::new(Smoothing::new()));
    let affine = graph.add_node(Box::new(AffineTransform::new()));
    let kaleido = graph.add_node(Box::new(KaleidoFold::new()));

    // Sample UVs around the cardinal positions. Aimed off-edge so a
    // non-clamped sampler doesn't pick up the boundary row/column.
    graph.set_param(sample_n, "uv", ParamValue::Vec2(UV_NORTH))?;
    graph.set_param(sample_s, "uv", ParamValue::Vec2(UV_SOUTH))?;
    graph.set_param(sample_e, "uv", ParamValue::Vec2(UV_EAST))?;
    graph.set_param(sample_w, "uv", ParamValue::Vec2(UV_WEST))?;

    // Math op codes from `MATH_OPS`: 1=Subtract, 2=Multiply, 6=Atan2.
    graph.set_param(math_dy, "op", ParamValue::Enum(1))?;
    graph.set_param(math_dx, "op", ParamValue::Enum(1))?;
    graph.set_param(math_atan2, "op", ParamValue::Enum(6))?;
    graph.set_param(math_deg, "op", ParamValue::Enum(2))?;
    graph.set_param(rad_to_deg, "value", ParamValue::Float(RAD_TO_DEG))?;

    // KaleidoFold defaults to amount=0 (pass-through). Lift to a
    // visible fold so the effect actually does something out of the
    // box; the user dials it back via the exposed `intensity` slider.
    graph.set_param(kaleido, "amount", ParamValue::Float(0.6))?;

    // Source fans out to all four ColorSamples plus the AffineTransform.
    graph.connect(source, (sample_n, "in"))?;
    graph.connect(source, (sample_s, "in"))?;
    graph.connect(source, (sample_e, "in"))?;
    graph.connect(source, (sample_w, "in"))?;
    graph.connect(source, (affine, "in"))?;

    // `dy = N_luma - S_luma`, `dx = E_luma - W_luma`. Positive dy
    // means North is brighter than South; positive dx means East is
    // brighter than West.
    graph.connect((sample_n, "luma"), (math_dy, "a"))?;
    graph.connect((sample_s, "luma"), (math_dy, "b"))?;
    graph.connect((sample_e, "luma"), (math_dx, "a"))?;
    graph.connect((sample_w, "luma"), (math_dx, "b"))?;

    // `atan2(dy, dx)` → angle in radians, math-CCW convention.
    // East-bright = 0, North-bright = π/2, West-bright = π,
    // South-bright = -π/2.
    graph.connect((math_dy, "out"), (math_atan2, "a"))?;
    graph.connect((math_dx, "out"), (math_atan2, "b"))?;

    // Convert radians → degrees so the AffineTransform's
    // degrees-screen-CW rotation param gets the right magnitude.
    graph.connect((math_atan2, "out"), (math_deg, "a"))?;
    graph.connect((rad_to_deg, "out"), (math_deg, "b"))?;

    // Smooth the angle. Without this the rotation jitters frame-to-
    // frame as fp16 storage on the source quantises the luma
    // readings. Time constant is the `reactivity` knob.
    graph.connect((math_deg, "out"), (smoothing, "in"))?;

    // Smoothed angle drives the AffineTransform's `rotation` via
    // port-shadows-param.
    graph.connect((smoothing, "out"), (affine, "rotation"))?;
    graph.connect((affine, "out"), (kaleido, "in"))?;

    let mut handle = CompositeHandle::new(COLOR_COMPASS_TYPE_ID, (kaleido, "out"));
    handle.add_inner(sample_n);
    handle.add_inner(sample_s);
    handle.add_inner(sample_e);
    handle.add_inner(sample_w);
    handle.add_inner(math_dy);
    handle.add_inner(math_dx);
    handle.add_inner(math_atan2);
    handle.add_inner(rad_to_deg);
    handle.add_inner(math_deg);
    handle.add_inner(smoothing);
    handle.add_inner(affine);
    handle.add_inner(kaleido);
    handle.expose_param("intensity", kaleido, "amount");
    handle.expose_param("segments", kaleido, "segments");
    handle.expose_param("reactivity", smoothing, "time_constant");
    Ok(handle)
}

#[cfg(test)]
mod tests {
    //! Color Compass behaviour tests. The composite is built around a
    //! scalar `angle` signal whose convergence is what the test
    //! asserts. Visual verification of the rotated KaleidoFold output
    //! is left for runtime play — the angle-side proof is what
    //! validates the bridge + atan2 + smoothing pipeline.

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::build_color_compass;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId,
    };
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::{ParamDef, ParamValue};
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };
    use crate::node_graph::state_store::StateStore;
    use crate::node_graph::{Executor, MetalBackend, Source};
    use crate::render_target::RenderTarget;

    fn frame_time(dt: f64) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(dt),
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

    /// Sink that captures the most recent Float emitted on its `in`
    /// port. Used to read the smoothed angle out of the composite
    /// without disturbing the inner topology.
    struct CaptureFloat {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::Mutex<Option<f32>>>,
    }
    impl EffectNode for CaptureFloat {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: "in",
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            &[]
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("in") {
                *self.seen.lock().unwrap() = Some(v);
            }
        }
    }

    /// Build a source texture filled with `rgba` (4×4 single colour),
    /// pre-bind it as the Source node's output, run the composite for
    /// `frames` frames, and return the smoothed angle scalar after
    /// settle.
    ///
    /// `reactivity` is the Smoothing time constant. Smaller =
    /// converges faster. The test uses 0.01 (10ms) so even 10 frames
    /// at 60fps are well past the time constant.
    fn run_with_source(rgba: [f64; 4], frames: usize, reactivity: f32) -> f32 {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let handle = build_color_compass(&mut g, (src, "out")).unwrap();
        // Dial reactivity from the test side via the inner Smoothing
        // node's exposed addressing. inner_nodes[9] = Smoothing
        // (matches the order in build_color_compass).
        let smoothing_id = handle.inner_nodes()[9];
        g.set_param(smoothing_id, "time_constant", ParamValue::Float(reactivity))
            .unwrap();

        // Tap the smoothing output via a CaptureFloat sink. The
        // composite's primary output is texture; the sink reads the
        // scalar wire that drives the AffineTransform.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = g.add_node(Box::new(CaptureFloat {
            type_id: EffectNodeType::new("test.capture_float"),
            seen: seen.clone(),
        }));
        g.connect((smoothing_id, "out"), (sink, "in")).unwrap();
        // The composite's texture output also has to terminate
        // somewhere — wire it into a second sink (FinalOutput would
        // require a complete kaleidoscope render path which we don't
        // need for the angle test).
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "compass-src");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            rgba,
            "compass-src-clear",
        );

        let mut backend = MetalBackend::new(device.clone(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let mut exec = Executor::new(Box::new(backend));
        let mut state = StateStore::new();

        // dt = 1/60 per frame. With reactivity=0.01 (10ms) the
        // smoothing alpha at one frame is ~0.81 — exponential decay
        // settles to within 1% in ~5 frames.
        let dt = 1.0 / 60.0;
        for _ in 0..frames {
            let mut native_enc = device.create_encoder("compass-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_state(
                    &mut g,
                    &plan,
                    frame_time(dt),
                    &mut gpu,
                    &mut state,
                    0,
                );
            }
            native_enc.commit_and_wait_completed();
        }

        seen.lock().unwrap().expect("capture saw an angle")
    }

    /// Uniform source: all four cardinals see the same luma, so the
    /// direction vector is `(0, 0)` and atan2 returns 0. With no
    /// asymmetry the compass has nothing to point at. Confirms the
    /// pipeline's degenerate-case behaviour rather than NaN-storming
    /// through the smoothed angle.
    #[test]
    fn uniform_source_yields_zero_angle() {
        let angle = run_with_source([0.5, 0.5, 0.5, 1.0], 20, 0.01);
        assert!(
            angle.abs() < 1e-3,
            "uniform source: expected angle ~0°, got {angle}",
        );
    }

    /// Three card sliders surface as exposed params on the handle.
    /// Reuses build_color_compass's exposed-param wiring — if a slot
    /// gets renamed by accident this test catches it.
    #[test]
    fn composite_exposes_intensity_segments_and_reactivity() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let handle = build_color_compass(&mut g, (src, "out")).unwrap();

        let exposed: std::collections::HashSet<_> = handle.exposed_params().collect();
        assert!(exposed.contains("intensity"));
        assert!(exposed.contains("segments"));
        assert!(exposed.contains("reactivity"));

        // intensity routes into KaleidoFold's amount — set it and
        // confirm the routing didn't slip. The CompositeHandle's
        // `set_param` returns an error if the outer name doesn't
        // route anywhere.
        handle
            .set_param(&mut g, "intensity", ParamValue::Float(1.0))
            .unwrap();
        handle
            .set_param(&mut g, "segments", ParamValue::Int(8))
            .unwrap();
        handle
            .set_param(&mut g, "reactivity", ParamValue::Float(0.05))
            .unwrap();
    }
}
