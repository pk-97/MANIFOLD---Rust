//! `node.scatter_particles` — atomic-add splat of particles into a
//! `u32` fixed-point accumulator.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. Reads particle positions from
//! an Array(Particle) input and writes to an Array(u32) accumulator
//! buffer sized `width × height`. Each live particle adds the
//! configured `scaled_energy` to its nearest texel via `atomicAdd`.
//!
//! The accumulator is cleared at dispatch time (one full-grid
//! `atomicStore(0)` pass) before the splat — so the consumer
//! reads a fresh frame each tick. Pair with
//! [`crate::node_graph::primitives::ResolveAccumulator`] to lift
//! the u32 grid into a float texture for downstream texture ops.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Out-of-bounds policy labels for the `boundary` enum.
/// `0 = Wrap` (toroidal); `1 = Discard` (skip the particle).
pub const SCATTER_BOUNDARY_MODES: &[&str] = &["Wrap", "Discard"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScatterUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
    boundary: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ScatterParticles,
    type_id: "node.scatter_particles",
    purpose: "Atomic-add splat of particles into a u32 fixed-point accumulator buffer sized to the host's canvas. Each live particle contributes `scaled_energy` to its nearest texel; the buffer is cleared at the start of each dispatch. `boundary` selects the out-of-bounds policy: Wrap (toroidal — seamless tiling, FluidSim style) or Discard (drop the particle — avoids the edge seam when projecting from 3D where particles legitimately fall outside [0,1]², StrangeAttractor style). `active_count` and `scaled_energy` are port-shadows-param so they can be driven by runtime wires (e.g. a `node.math` chain for brightness normalisation by particle count). The accumulator dimensions track the canvas automatically — declared via `canvas_sized_array_outputs()` — so a layer renders correctly at any host resolution without hardcoded width/height params. Pair with `node.resolve_accumulator` to read the result as a float texture.",
    inputs: {
        particles: Array(Particle) required,
        active_count: ScalarF32 optional,
        scaled_energy: ScalarF32 optional,
    },
    outputs: {
        accum: Array(u32),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scaled_energy",
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "boundary",
            label: "Boundary",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: SCATTER_BOUNDARY_MODES,
        },
    ],
    composition_notes: "Output accumulator buffer is u32 fixed-point sized to the host canvas (width × height u32s) — re-allocated on `Generator::resize` so the splat coords always span the full output texture. `scaled_energy = 4096` ≈ 1.0 in float after Resolve divides by FIXED_POINT_SCALE — matching the FluidSim convention. `boundary = Wrap` (default) keeps the FluidSim toroidal behaviour; `boundary = Discard` is for particle systems that project from 3D space (Strange Attractor, BlackHole) where wrapping creates a visible edge seam.",
    examples: [],
    picker: { label: "Scatter Particles", category: Atom },
    extra_fields: {
        splat_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for ScatterParticles {
    /// Accumulator dimensions track the host canvas — declared
    /// `canvas_sized_array_outputs()` below. The chain builder
    /// allocates `canvas_w × canvas_h × 4` bytes from the backend's
    /// canvas dims, bypassing this method for the `accum` port.
    fn canvas_sized_array_outputs(&self) -> &'static [&'static str] {
        &["accum"]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;
        // Canvas dims from the executor — set on the context before
        // every evaluate. Matches what `canvas_sized_array_outputs`
        // told the chain builder to allocate, so the dispatch grid
        // spans every cell of the accumulator buffer.
        let width = ctx.canvas_width.max(1);
        let height = ctx.canvas_height.max(1);
        let scaled_energy = ctx
            .scalar_or_param("scaled_energy", 4096.0)
            .round()
            .max(0.0) as u32;
        let boundary = match ctx.params.get("boundary") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("accum") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(particle_capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline_clear = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/scatter_particles.wgsl"),
                "clear_main",
                "node.scatter_particles.clear",
            )
        });

        let uniforms = ScatterUniforms {
            active_count,
            width,
            height,
            scaled_energy,
            boundary,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Pass 1: zero the accumulator. 16×16 workgroups cover the grid.
        gpu.native_enc.dispatch_compute(
            pipeline_clear,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.scatter_particles.clear",
        );

        let pipeline_splat = self.splat_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/scatter_particles.wgsl"),
                "splat_main",
                "node.scatter_particles.splat",
            )
        });

        // Pass 2: atomic-add splat. 256-particle workgroups along x.
        gpu.native_enc.dispatch_compute(
            pipeline_splat,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.scatter_particles.splat",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_particles_declares_array_in_and_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let u32_layout = ArrayType::of_known::<u32>();

        assert_eq!(ScatterParticles::TYPE_ID, "node.scatter_particles");

        let particles_in = ScatterParticles::INPUTS
            .iter()
            .find(|p| p.name == "particles")
            .expect("particles input");
        assert_eq!(particles_in.ty, PortType::Array(particle_layout));
        assert!(particles_in.required);

        // Port-shadow inputs: active_count and scaled_energy. width
        // and height stay param-only because the accumulator buffer
        // is built to those dims and can't safely resize per-frame.
        for name in ["active_count", "scaled_energy"] {
            let port = ScatterParticles::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("missing port-shadow input `{name}`"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }

        assert_eq!(ScatterParticles::OUTPUTS.len(), 1);
        assert_eq!(ScatterParticles::OUTPUTS[0].name, "accum");
        assert_eq!(ScatterParticles::OUTPUTS[0].ty, PortType::Array(u32_layout));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scatter_particles");
    }

    #[test]
    fn boundary_param_is_enum_with_wrap_and_discard() {
        let p = ScatterParticles::PARAMS
            .iter()
            .find(|p| p.name == "boundary")
            .expect("boundary param must exist");
        assert_eq!(p.ty, ParamType::Enum);
        assert_eq!(p.enum_values, &["Wrap", "Discard"]);
        // Default must be Wrap (== 0) so existing presets keep the
        // legacy toroidal behaviour without any JSON change.
        assert!(matches!(p.default, ParamValue::Enum(0)));
    }
}

#[cfg(test)]
mod gpu_tests {
    //! GPU parity tests for the `boundary` mode added 2026-05-23.
    //! Wrap mode is the legacy FluidSim behaviour (toroidal); Discard
    //! is the new path for projected-from-3D particle systems
    //! (StrangeAttractor) where out-of-bounds particles should drop
    //! instead of wrap to avoid an edge seam.
    //!
    //! Pattern matches `project_4d::gpu_tests` — a test-only
    //! `ParticleSource` node satisfies the chain validator, the
    //! caller CPU-writes a known particle layout into the shared
    //! input buffer, and the test reads the u32 accumulator back via
    //! `mapped_ptr` for element-wise assertions.
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
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };

    use super::ScatterParticles;

    /// Test-only source for `Array<Particle>`. CPU-write the input
    /// buffer via `mapped_ptr`, then pre-bind it as this node's `out`
    /// resource. `run` is a no-op — the data already lives in the
    /// buffer when the executor reaches the downstream node.
    struct ParticleSource {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl ParticleSource {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.particle_source"),
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

        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            // pre_bind_array bypasses the executor's pre-allocator,
            // so any non-zero placeholder works here.
            Some(0)
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

    /// Build a particle with `position = (x, y, 0)` and `life = 1`.
    fn alive(x: f32, y: f32) -> Particle {
        Particle {
            position: [x, y, 0.0],
            _pad0: 0.0,
            velocity: [0.0; 3],
            life: 1.0,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        }
    }

    /// Run scatter_particles with the given input particles, a
    /// 16×1 accumulator, and the given boundary mode. Returns the
    /// 16 u32 cells of the accumulator.
    fn run_scatter(particles: &[Particle], boundary: u32) -> [u32; 16] {
        const WIDTH: u32 = 16;
        const HEIGHT: u32 = 1;
        const ENERGY: u32 = 4096;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(ParticleSource::new()));
        let scatter = g.add_node(Box::new(ScatterParticles::new()));
        g.connect((src, "out"), (scatter, "particles")).unwrap();
        g.set_param(
            scatter,
            "active_count",
            ParamValue::Float(particles.len() as f32),
        )
        .unwrap();
        g.set_param(scatter, "scaled_energy", ParamValue::Float(ENERGY as f32))
            .unwrap();
        g.set_param(scatter, "boundary", ParamValue::Enum(boundary)).unwrap();
        let plan = compile(&g).unwrap();

        let r_in = resource_for(&plan, src, "out", false);
        let r_accum = resource_for(&plan, scatter, "accum", false);

        let particle_bytes = std::mem::size_of_val(particles) as u64;
        let accum_bytes = (WIDTH as u64) * (HEIGHT as u64) * 4;
        let in_buf = device.create_buffer_shared(particle_bytes);
        let accum_buf = device.create_buffer_shared(accum_bytes);

        // CPU-write the input particles into the shared buffer.
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(particles));
        }

        // Backend canvas dims drive scatter's dispatch (width/height
        // are read from `ctx.canvas_width/height`, not params). 16×1
        // matches the test's expected accumulator layout — set the
        // backend to those dims so the executor passes them through.
        let mut backend = MetalBackend::new(&device, WIDTH, HEIGHT, format);
        let _in_slot = backend.pre_bind_array(r_in, in_buf);
        let accum_slot = backend.pre_bind_array(r_accum, accum_buf);

        let mut native_enc = device.create_encoder("scatter-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let accum_buf = exec
            .backend()
            .array_buffer(accum_slot)
            .expect("accumulator buffer retained");
        let ptr = accum_buf.mapped_ptr().expect("shared accumulator buffer");
        let bytes =
            unsafe { std::slice::from_raw_parts(ptr as *const u8, accum_bytes as usize) };
        let mut out = [0u32; 16];
        out.copy_from_slice(bytemuck::cast_slice::<u8, u32>(bytes));
        out
    }

    /// Wrap mode: an OOB particle's column index wraps via `% width`
    /// and lands at a valid texel — colliding with whatever particle
    /// already lives at that texel. This is the FluidSim default.
    #[test]
    fn wrap_mode_collides_oob_particle_into_wrapped_column() {
        // x=0.1 → 0.1*16=1.6 → col 1
        // x=0.3 → 0.3*16=4.8 → col 4
        // x=0.5 → 0.5*16=8.0 → col 8
        // x=0.9 → 0.9*16=14.4 → col 14
        // x=1.1 → 1.1*16=17.6 → u32(17.6)=17, %16=1 → col 1 (collision with 0.1)
        let particles = [
            alive(0.1, 0.5),
            alive(0.3, 0.5),
            alive(0.5, 0.5),
            alive(0.9, 0.5),
            alive(1.1, 0.5),
        ];
        let accum = run_scatter(&particles, 0);

        const E: u32 = 4096;
        let mut expected = [0u32; 16];
        expected[1] = 2 * E; // 0.1 + 1.1 wrap collision
        expected[4] = E;
        expected[8] = E;
        expected[14] = E;
        assert_eq!(accum, expected, "wrap mode accumulator mismatch");
    }

    /// Discard mode: the OOB particle drops; only in-bounds particles
    /// contribute. The collision at col 1 disappears. This is what
    /// StrangeAttractor needs — projecting from 3D, particles legitimately
    /// fall outside [0,1]² and wrapping creates a hard seam at the edge.
    #[test]
    fn discard_mode_drops_oob_particle_without_wrapping() {
        let particles = [
            alive(0.1, 0.5),
            alive(0.3, 0.5),
            alive(0.5, 0.5),
            alive(0.9, 0.5),
            alive(1.1, 0.5), // OOB right — discarded
        ];
        let accum = run_scatter(&particles, 1);

        const E: u32 = 4096;
        let mut expected = [0u32; 16];
        expected[1] = E; // no collision — 1.1 was discarded
        expected[4] = E;
        expected[8] = E;
        expected[14] = E;
        assert_eq!(accum, expected, "discard mode accumulator mismatch");
    }

    /// Dead particles (life <= 0) are skipped under both boundary
    /// modes — the boundary param only affects how OOB *live*
    /// particles are handled.
    #[test]
    fn dead_particles_skipped_in_both_modes() {
        let dead = Particle {
            position: [0.5, 0.5, 0.0],
            _pad0: 0.0,
            velocity: [0.0; 3],
            life: 0.0,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        };
        for mode in [0u32, 1u32] {
            let accum = run_scatter(&[dead], mode);
            assert_eq!(accum, [0u32; 16], "boundary={mode}: dead particle leaked");
        }
    }
}

