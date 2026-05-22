//! `node.integrate_particles_attractor` — RK2 ODE integration of
//! particles through one of five strange-attractor formulas
//! (Lorenz / Rössler / Aizawa / Thomas / Halvorsen).
//!
//! Wraps `generators/shaders/strange_attractor_simulate.wgsl` via
//! `include_str!`. Two pipelines share the WGSL source:
//!   - `cs_simulate` — per-frame, 8 RK2 sub-steps + 3D→2D projection.
//!   - `cs_seed` — init pass; hash-based seed near attractor centre,
//!     ~50-step warmup + 0..N stagger so the first rendered frame
//!     shows the full attractor structure.
//!
//! Per-attractor intrinsic constants — the domain centre, the scale
//! of that domain, and the base integration timestep — live inside
//! this primitive, keyed off the `attractor_type` enum. They're not
//! free user knobs: Lorenz's `[0,0,25] / 25.0 / 0.003` is part of
//! *being* Lorenz. Live performance speed lives on `dt_multiplier`
//! (multiplies the per-type base dt); aesthetic camera + chaos
//! controls are port-shadowed scalars.
//!
//! State (`AttractorState::last_attractor_type`) tracks which
//! attractor was integrated last frame. On a change — or on the
//! first frame after a state-store reset (layer resume, seek,
//! project load) — `cs_seed` runs before `cs_simulate` so particles
//! land on the new manifold instantly instead of dwelling as a
//! uniform cloud while the integrator catches up.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;

pub const ATTRACTOR_TYPES: &[&str] =
    &["Lorenz", "Rössler", "Aizawa", "Thomas", "Halvorsen"];

/// Number of attractor variants. Used by the JSON preset and any
/// ClipTriggerCycle wiring that wants `trigger_count % N`.
pub const ATTRACTOR_COUNT: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AttractorUniforms {
    attractor_type: u32,
    particle_count: u32,
    frame_count: u32,
    _pad0: u32,
    chaos: f32,
    cam_angle: f32,
    cam_tilt: f32,
    aspect: f32,
    diffusion: f32,
    attractor_dt: f32,
    uv_scale: f32,
    attractor_scale: f32,
    attractor_center: [f32; 3],
    _pad1: f32,
}

/// Per-attractor intrinsic domain centre. Particles converge onto
/// the manifold around this point; the projection step subtracts it
/// before camera transforms.
fn attractor_center(atype: u32) -> [f32; 3] {
    match atype {
        0 => [0.0, 0.0, 25.0], // Lorenz
        1 => [0.0, 0.0, 2.0],  // Rössler
        2 => [0.0, 0.0, 0.5],  // Aizawa
        3 => [0.0, 0.0, 0.0],  // Thomas
        _ => [0.0, 0.0, 0.0],  // Halvorsen
    }
}

/// Per-attractor characteristic scale of the trajectory's domain.
/// Used as both the projection denominator (so each attractor fits
/// the screen) and the escape-detection radius (`scale * 100`).
fn attractor_scale(atype: u32) -> f32 {
    match atype {
        0 => 25.0,
        1 => 10.0,
        2 => 1.2,
        3 => 4.0,
        _ => 12.0,
    }
}

/// Per-attractor base integration timestep. Each ODE family has a
/// characteristic stiffness — Thomas tolerates 10× the step Lorenz
/// does. The outer-card "Speed" slider feeds the `dt_multiplier`
/// port (default 1.0) which scales the base value.
fn attractor_base_dt(atype: u32) -> f32 {
    match atype {
        0 => 0.003,
        1 => 0.008,
        2 => 0.008,
        3 => 0.03,
        _ => 0.004,
    }
}

crate::primitive! {
    name: IntegrateParticlesAttractor,
    type_id: "node.integrate_particles_attractor",
    purpose: "Integrate an Array<Particle> through one of five strange-attractor ODEs (Lorenz / Rössler / Aizawa / Thomas / Halvorsen) using RK2 sub-stepping with 3D→2D perspective projection. Per-attractor intrinsic constants — domain centre, scale, and base integration timestep — live inside the primitive; the user picks the attractor enum and the math is correct. `dt_multiplier` scales the per-type base dt for live speed control. Every numeric param (chaos, cam_angle, cam_tilt, aspect, diffusion, dt_multiplier, uv_scale, particle_count) is port-shadowed. On `attractor_type` change — or on a state-store reset — the primitive automatically dispatches `cs_seed` so particles land on the new manifold immediately. Pair with `node.array_feedback` upstream to persist particle state across frames, and feed into `node.scatter_particles` (boundary = Discard, because perspective projection puts particles outside [0,1]²) → `node.resolve_accumulator` → `node.reinhard_tone_map` for the full attractor visualisation.",
    inputs: {
        in: Array(Particle) required,
        chaos: ScalarF32 optional,
        cam_angle: ScalarF32 optional,
        cam_tilt: ScalarF32 optional,
        aspect: ScalarF32 optional,
        diffusion: ScalarF32 optional,
        dt_multiplier: ScalarF32 optional,
        uv_scale: ScalarF32 optional,
        particle_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "attractor_type",
            label: "Attractor",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: ATTRACTOR_TYPES,
        },
        ParamDef {
            name: "particle_count",
            label: "Particle Count",
            ty: ParamType::Int,
            default: ParamValue::Float(500_000.0),
            range: Some((1024.0, 8_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "chaos",
            label: "Chaos",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_angle",
            label: "Camera Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_tilt",
            label: "Camera Tilt",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
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
        ParamDef {
            name: "diffusion",
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "dt_multiplier",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "uv_scale",
            label: "UV Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 4.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Authoring shape: `node.seed_particles → node.array_feedback → node.integrate_particles_attractor → node.scatter_particles (boundary=Discard) → node.resolve_accumulator → tone-map`. `dt_multiplier` is the live-performance speed knob — multiplies the per-type base dt (Lorenz 0.003, Rössler/Aizawa 0.008, Thomas 0.03, Halvorsen 0.004). For clip-trigger-driven attractor cycling, wire `system.generator_input.trigger_count` through a ClipTriggerCycle-aware path (see `node.wireframe_shape` for the pattern) into `attractor_type` — auto-seed handles the rest. The first frame after any state-store reset (layer resume, seek, project load) dispatches `cs_seed` so particles land on the manifold instantly.",
    examples: [],
    picker: { label: "Integrate Particles Attractor", category: Atom },
    extra_fields: {
        seed_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

/// Persistent state — the attractor_type we integrated last frame.
/// Drives auto-seed: a change here (or the absence of state on a
/// fresh layer / reset) dispatches `cs_seed` this frame.
struct AttractorState {
    last_attractor_type: u32,
}

impl NodeState for AttractorState {}

impl IntegrateParticlesAttractor {
    fn ensure_simulate_pipeline(
        &mut self,
        device: &manifold_gpu::GpuDevice,
    ) -> &manifold_gpu::GpuComputePipeline {
        self.pipeline.get_or_insert_with(|| {
            device.create_compute_pipeline(
                include_str!("../../generators/shaders/strange_attractor_simulate.wgsl"),
                "cs_simulate",
                "node.integrate_particles_attractor.simulate",
            )
        })
    }

    fn ensure_seed_pipeline(
        &mut self,
        device: &manifold_gpu::GpuDevice,
    ) -> &manifold_gpu::GpuComputePipeline {
        self.seed_pipeline.get_or_insert_with(|| {
            device.create_compute_pipeline(
                include_str!("../../generators/shaders/strange_attractor_simulate.wgsl"),
                "cs_seed",
                "node.integrate_particles_attractor.seed",
            )
        })
    }
}

impl Primitive for IntegrateParticlesAttractor {
    /// Output `out` shares the input `in`'s capacity — the simulate
    /// kernel works in place on the producer's buffer; the chain
    /// build aliases both slots to the same MTLBuffer.
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
        let attractor_type = match ctx.params.get("attractor_type") {
            Some(ParamValue::Enum(n)) => (*n).min(ATTRACTOR_COUNT - 1),
            _ => 0,
        };
        let particle_count_request = ctx.scalar_or_param("particle_count", 500_000.0);
        let particle_count_request = particle_count_request.round().max(0.0) as u32;
        let chaos = ctx.scalar_or_param("chaos", 0.3);
        let cam_angle = ctx.scalar_or_param("cam_angle", 0.0);
        let cam_tilt = ctx.scalar_or_param("cam_tilt", 0.3);
        let aspect = ctx.scalar_or_param("aspect", 1.777);
        let diffusion = ctx.scalar_or_param("diffusion", 0.0);
        let dt_multiplier = ctx.scalar_or_param("dt_multiplier", 1.0);
        let uv_scale = ctx.scalar_or_param("uv_scale", 1.0);

        let attractor_center = attractor_center(attractor_type);
        let attractor_scale = attractor_scale(attractor_type);
        let attractor_dt = attractor_base_dt(attractor_type) * dt_multiplier;

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out_buf;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let particle_count = particle_count_request.min(capacity);
        if particle_count == 0 {
            return;
        }

        let frame_count = ctx.time.frame_count as u32;

        let uniforms = AttractorUniforms {
            attractor_type,
            particle_count,
            frame_count,
            _pad0: 0,
            chaos,
            cam_angle,
            cam_tilt,
            aspect,
            diffusion,
            attractor_dt,
            uv_scale,
            attractor_scale,
            attractor_center,
            _pad1: 0.0,
        };

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        // Split-borrow disjoint fields on ctx so gpu + state are both
        // mutable at once. Mirrors `temporal::Feedback` and
        // `array_feedback`.
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("IntegrateParticlesAttractor::run requires a GpuEncoder");
        let mut state = ctx.state.as_deref_mut();

        // Decide whether this frame's first dispatch is a seed pass.
        // Two triggers:
        //   1. No state for this (node, owner) — fresh primitive or
        //      post-cleanup. Seeds on the first frame.
        //   2. The recorded attractor_type doesn't match this frame's
        //      — the user (or a clip trigger) just switched
        //      attractors.
        // If state is unavailable (test harness running through
        // `execute_frame_with_gpu`), fall back to seeding every frame
        // — the test will assert the seed dispatch happened.
        let needs_seed = match state.as_deref_mut() {
            Some(store) => match store.get::<AttractorState>(node_id, owner_key) {
                Some(s) => s.last_attractor_type != attractor_type,
                None => true,
            },
            None => true,
        };

        if needs_seed {
            let seed_pipeline = self.ensure_seed_pipeline(gpu.device);
            gpu.native_enc.dispatch_compute(
                seed_pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: in_buf,
                        offset: 0,
                    },
                ],
                [particle_count.div_ceil(256), 1, 1],
                "node.integrate_particles_attractor.seed",
            );
        }

        let simulate_pipeline = self.ensure_simulate_pipeline(gpu.device);
        gpu.native_enc.dispatch_compute(
            simulate_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
            ],
            [particle_count.div_ceil(256), 1, 1],
            "node.integrate_particles_attractor.simulate",
        );

        // Record the type we just integrated so the next frame only
        // re-seeds on an actual change.
        if let Some(store) = state {
            store.insert(
                node_id,
                owner_key,
                AttractorState {
                    last_attractor_type: attractor_type,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn integrate_attractor_declares_particle_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<Particle>();
        assert_eq!(
            IntegrateParticlesAttractor::TYPE_ID,
            "node.integrate_particles_attractor"
        );
        let in_port = IntegrateParticlesAttractor::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .expect("`in` port");
        assert_eq!(in_port.ty, PortType::Array(layout));
        assert_eq!(IntegrateParticlesAttractor::OUTPUTS.len(), 1);
        assert_eq!(
            IntegrateParticlesAttractor::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn integrate_attractor_has_five_attractor_options() {
        let p = IntegrateParticlesAttractor::PARAMS
            .iter()
            .find(|p| p.name == "attractor_type")
            .unwrap();
        assert_eq!(p.ty, ParamType::Enum);
        assert_eq!(p.enum_values.len(), 5);
        assert_eq!(p.enum_values, ATTRACTOR_TYPES);
    }

    #[test]
    fn every_numeric_param_has_a_port_shadow() {
        // Per §6.2: every numeric param ships port-shadowed by
        // default. Mode selectors (`attractor_type`) are the
        // documented exception — wiring an enum doesn't compose.
        let numeric_params = [
            "chaos",
            "cam_angle",
            "cam_tilt",
            "aspect",
            "diffusion",
            "dt_multiplier",
            "uv_scale",
            "particle_count",
        ];
        for name in numeric_params {
            assert!(
                IntegrateParticlesAttractor::INPUTS
                    .iter()
                    .any(|p| p.name == name),
                "numeric param `{name}` must have a same-named scalar input port",
            );
        }
    }

    #[test]
    fn seed_now_param_is_gone() {
        // Auto-seed via StateStore replaced the manual toggle.
        // If this assertion ever fails because someone reintroduced
        // `seed_now`, audit the auto-seed path before deleting the
        // assertion — the user-facing knob shouldn't come back.
        assert!(
            !IntegrateParticlesAttractor::PARAMS
                .iter()
                .any(|p| p.name == "seed_now"),
            "seed_now should be auto-managed via StateStore — not exposed as a user param",
        );
    }

    #[test]
    fn attractor_scale_dt_center_are_not_user_params() {
        // §6.4: per-type intrinsic constants live inside the primitive.
        for name in ["attractor_scale", "attractor_dt", "attractor_center"] {
            assert!(
                !IntegrateParticlesAttractor::PARAMS
                    .iter()
                    .any(|p| p.name == name),
                "{name} is intrinsic to the attractor formula — must not be a user param",
            );
        }
    }

    #[test]
    fn per_type_tables_pin_legacy_values() {
        // Pin the Unity-derived tables so a typo here can't drift
        // every shape silently. If you intentionally retune one,
        // update both the function and this assertion.
        assert_eq!(attractor_center(0), [0.0, 0.0, 25.0]);
        assert_eq!(attractor_center(1), [0.0, 0.0, 2.0]);
        assert_eq!(attractor_center(2), [0.0, 0.0, 0.5]);
        assert_eq!(attractor_center(3), [0.0, 0.0, 0.0]);
        assert_eq!(attractor_center(4), [0.0, 0.0, 0.0]);

        assert_eq!(attractor_scale(0), 25.0);
        assert_eq!(attractor_scale(1), 10.0);
        assert_eq!(attractor_scale(2), 1.2);
        assert_eq!(attractor_scale(3), 4.0);
        assert_eq!(attractor_scale(4), 12.0);

        assert_eq!(attractor_base_dt(0), 0.003);
        assert_eq!(attractor_base_dt(1), 0.008);
        assert_eq!(attractor_base_dt(2), 0.008);
        assert_eq!(attractor_base_dt(3), 0.03);
        assert_eq!(attractor_base_dt(4), 0.004);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = IntegrateParticlesAttractor::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.integrate_particles_attractor");
    }
}

#[cfg(test)]
mod gpu_tests {
    //! GPU exercise tests for the rewritten primitive. We can't
    //! bit-parity check against the legacy generator here because the
    //! legacy generator is the same shader source — both call into
    //! the bundled `cs_simulate` / `cs_seed` pipelines. The
    //! interesting per-primitive thing to lock in is the auto-seed
    //! behaviour (the state-tracking we added) and the per-type
    //! lookup wiring (no Lorenz-on-Halvorsen-table bugs).
    //!
    //! Strategy: seed a particle buffer, run one frame with
    //! state-store, read back. Each particle's `velocity` field
    //! carries the integrated 3D state. We assert it landed in the
    //! attractor's domain (within the per-type escape radius) — not
    //! at the seed_particles uniform-random output. If auto-seed
    //! didn't dispatch, integration from random-in-[0,1] particles
    //! never converges in one frame and the test fails.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::compute_common::Particle;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
    };
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };
    use crate::node_graph::state_store::StateStore;
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };

    use super::{attractor_center, attractor_scale, IntegrateParticlesAttractor};

    /// Test-only source for `Array<Particle>`. The caller writes the
    /// initial particle layout via `mapped_ptr` then pre-binds it as
    /// this node's `out` resource — `run` is a no-op.
    struct ParticleSource {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl ParticleSource {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.particle_source_attractor"),
                inputs: vec![],
                outputs: vec![NodePort {
                    name: "out",
                    ty: PortType::Array(ArrayType::of_known::<Particle>()),
                    kind: PortKind::Output,
                    required: false,
                }],
            }
        }
    }

    impl EffectNode for ParticleSource {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        /// Capacity matches what the test pre-binds; downstream
        /// transform primitives (integrate, scatter) inherit this
        /// via `input_capacities`, so their `out` slots get sized
        /// correctly instead of starving to 0.
        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(TEST_PARTICLE_COUNT)
        }
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn resource_for(
        plan: &ExecutionPlan,
        node: NodeInstanceId,
        port: &str,
        is_input: bool,
    ) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                let pool = if is_input { &step.inputs } else { &step.outputs };
                for &(name, id) in pool {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!(
            "no {} port `{port}` on node {node:?}",
            if is_input { "input" } else { "output" }
        );
    }

    const TEST_PARTICLE_COUNT: u32 = 1024;

    /// Build a single-attractor graph: ParticleSource → integrate.
    /// Returns the final particles (velocity = 3D attractor state).
    fn run_attractor(attractor_type: u32) -> Vec<Particle> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(ParticleSource::new()));
        let int_node = g.add_node(Box::new(IntegrateParticlesAttractor::new()));
        g.connect((src, "out"), (int_node, "in")).unwrap();
        g.set_param(
            int_node,
            "attractor_type",
            ParamValue::Enum(attractor_type),
        )
        .unwrap();
        g.set_param(
            int_node,
            "particle_count",
            ParamValue::Float(TEST_PARTICLE_COUNT as f32),
        )
        .unwrap();
        let plan = compile(&g).unwrap();

        // Pre-bind both `in` and `out`. The simulate kernel writes
        // in-place through `in_buf`; the `out` buffer is unused but
        // the run() guard early-returns if the runtime hasn't pinned
        // a buffer for it.
        let r_in = resource_for(&plan, src, "out", false);
        let r_out = resource_for(&plan, int_node, "out", false);

        let particle_bytes =
            (TEST_PARTICLE_COUNT as u64) * std::mem::size_of::<Particle>() as u64;
        let in_buf = device.create_buffer_shared(particle_bytes);
        let out_buf = device.create_buffer_shared(particle_bytes);

        // CPU-write all-zero particles (life = 0 — dead by default).
        // `cs_seed` overwrites every slot with a manifold-seeded
        // particle; if auto-seed *didn't* fire, the buffer stays
        // zeroed and the assertion fails.
        unsafe {
            let zeros = vec![0u8; particle_bytes as usize];
            in_buf.write(0, &zeros);
        }

        let mut backend = MetalBackend::new(&device, 1, 1, format);
        let in_slot = backend.pre_bind_array(r_in, in_buf);
        let _out_slot = backend.pre_bind_array(r_out, out_buf);

        let mut native_enc = device.create_encoder("integrate-attractor-test");
        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                0,
            );
        }
        native_enc.commit_and_wait_completed();

        let buf = exec.backend().array_buffer(in_slot).expect("input retained");
        let ptr = buf.mapped_ptr().expect("shared input buffer");
        let bytes =
            unsafe { std::slice::from_raw_parts(ptr as *const u8, particle_bytes as usize) };
        bytemuck::cast_slice::<u8, Particle>(bytes).to_vec()
    }

    /// For every attractor type: after one frame, each integrated
    /// particle's 3D state (`velocity`) lives inside the per-type
    /// escape radius (scale × 100) around the per-type centre. If
    /// auto-seed didn't run, every particle's velocity stays
    /// (0,0,0) → fails for centres far from origin (Lorenz @ z=25).
    /// If integration is using the wrong attractor's tables, the
    /// state diverges past the escape radius.
    #[test]
    fn every_attractor_type_seeds_and_integrates_into_its_domain() {
        for atype in 0..super::ATTRACTOR_COUNT {
            let particles = run_attractor(atype);
            let centre = attractor_center(atype);
            let scale = attractor_scale(atype);
            let escape = scale * 100.0;

            // Every particle must have life = 1 (seed sets life=1)
            // and its 3D state within the escape radius of centre.
            for (i, p) in particles.iter().enumerate() {
                assert_eq!(
                    p.life, 1.0,
                    "atype={atype} particle {i} life={} after frame — auto-seed should have set life=1",
                    p.life,
                );
                let dx = p.velocity[0] - centre[0];
                let dy = p.velocity[1] - centre[1];
                let dz = p.velocity[2] - centre[2];
                let dist_sq = dx * dx + dy * dy + dz * dz;
                assert!(
                    dist_sq <= escape * escape,
                    "atype={atype} particle {i} 3D state {:?} escaped centre {:?} \
                     (dist² {} > escape² {})",
                    p.velocity,
                    centre,
                    dist_sq,
                    escape * escape,
                );
            }
        }
    }

    /// The first dispatch with a fresh StateStore must produce
    /// non-zero positions — proves `cs_seed` actually ran on frame 1.
    /// Without auto-seed, the input buffer's all-zero contents would
    /// integrate to non-zero too (the ODE pushes from origin), but
    /// the projection step would put every particle at the same UV
    /// (no hash variation). Auto-seed's hash-based init guarantees
    /// distinct UV positions for each particle.
    #[test]
    fn first_frame_after_reset_produces_distinct_per_particle_positions() {
        // Lorenz puts the centre at z=25, so an un-seeded buffer
        // (life=0, velocity=[0,0,0]) projects every particle to the
        // same UV — a clear failure signature if auto-seed didn't
        // run.
        let particles = run_attractor(0);

        // Collect UV positions of the first 32 particles (the seed
        // hash is per-id, so positions should differ).
        let positions: Vec<(f32, f32)> = particles
            .iter()
            .take(32)
            .map(|p| (p.position[0], p.position[1]))
            .collect();

        let unique: std::collections::HashSet<_> = positions
            .iter()
            .map(|(x, y)| (x.to_bits(), y.to_bits()))
            .collect();
        assert!(
            unique.len() >= 16,
            "expected at least 16 distinct UV positions among the first 32 particles \
             (per-id seed hash should spread them); got {} — auto-seed likely didn't run",
            unique.len(),
        );
    }
}
