//! `node.nested_cubes_geometry` — gap-face cube field with EMA-smoothed
//! per-instance Y rotation, per-face scatter, and per-face envelope-driven
//! rotation kick. Renders directly to a `Texture2D` via a two-pass pipeline
//! (solid fill + line edges) under an isometric orthographic camera.
//!
//! The curated NestedCubes primitive — ports the legacy generator's shader
//! verbatim into the node graph runtime. Target rotation angles come from
//! the `target_angles` Array<f32> input (the cycler / accumulator chooses
//! between pose snapping and continuous accumulation upstream); the
//! envelope kick is internal state driven by `trigger_count` edges.
//!
//! Hardcoded for 5 instances — the legacy's `INSTANCE_COUNT = 5` is
//! load-bearing for the visual look (ramp 1.0 → 2.0, hand-tuned camera
//! framing). The `target_angles` input is required to carry 5 values;
//! shorter buffers leave the trailing instances at zero, longer buffers
//! are truncated.

use std::borrow::Cow;

use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul, ortho_rh};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Five instances. Matches the legacy generator's hardcoded
/// `INSTANCE_COUNT`. The shader's uniform packs five sizes / angles
/// into `sizes_0_3` + `extra.x` (size 4) and `angles_0_3` + `extra.y`
/// (angle 4); changing the count would break that packing.
pub const NESTED_CUBES_INSTANCE_COUNT: u32 = 5;

const TRI_VERTEX_COUNT: u32 = 36; // 6 faces × 2 triangles × 3 vertices
const EDGE_VERTEX_COUNT: u32 = 48; // 6 faces × 4 edges × 2 endpoints

/// Linear size ramp 1.0 → 2.0 across the 5 instances.
const INSTANCE_SIZES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// Uniform layout matching the shader's `Uniforms` struct.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NestedCubesUniforms {
    view_proj: [[f32; 4]; 4],
    sizes_0_3: [f32; 4],
    angles_0_3: [f32; 4],
    /// x: size[4], y: angle[4], z: color (0 = black, 1 = white), w: scatter
    extra: [f32; 4],
    /// x: time, y: clip_trigger_envelope
    extra2: [f32; 4],
}

crate::primitive! {
    name: NestedCubesGeometry,
    type_id: "node.nested_cubes_geometry",
    purpose: "Render a 5-instance gap-face cube field with EMA-smoothed per-instance Y rotation, per-face scatter, and a per-face envelope-driven kick on each trigger. Isometric orthographic camera. Target angles arrive on a port (Array<f32> length 5) so an upstream cycler or accumulator chooses the rotation behaviour; the kick envelope is internal state.",
    inputs: {
        target_angles: Array(f32) required,
        time: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("filter"),
            label: "Filter",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scatter"),
            label: "Scatter",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("decay_rate"),
            label: "Kick Decay",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.1, 50.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "target_angles is required — drive it from node.cycle_table_row (pose mode) or node.sum_into_bins (envelope mode). Trigger source is conventional `system.generator_input.trigger_count`; on each new trigger the kick envelope snaps to 1.0 and decays exponentially at `decay_rate` per second, multiplied into each face's random-axis 45° rotation. State is fresh on rebuild (per the graph-editor-is-authoring-not-perform rule).",
    examples: ["NestedCubes"],
    picker: { label: "Nested Cubes Geometry", category: Atom },
    summary: "Renders a field of nested, rotating cubes with per-face scatter and a beat-driven kick. A self-contained generator, still to be broken into atoms.",
    category: Geometry3D,
    role: Source,
    aliases: ["nested cubes", "cubes", "geometry"],
    boundary_reason: FusedBundle,
    extra_fields: {
        fill_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        edge_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil_write: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_stencil_read: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_dims: (u32, u32) = (0, 0),
        current_angles: [f32; 5] = [0.0; 5],
        initialized: bool = false,
        envelope: f32 = 0.0,
        last_trigger_count: Option<u32> = None,
    },
}

impl NestedCubesGeometry {
    fn ensure_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.fill_pipeline.is_none() {
            self.fill_pipeline = Some(device.create_render_pipeline_depth(
                include_str!("shaders/nested_cubes_geometry.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.nested_cubes_geometry::fill",
            ));
        }
        if self.edge_pipeline.is_none() {
            self.edge_pipeline = Some(device.create_render_pipeline_depth(
                include_str!("shaders/nested_cubes_geometry.wgsl"),
                "vs_edges",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.nested_cubes_geometry::edges",
            ));
        }
        if self.depth_stencil_write.is_none() {
            self.depth_stencil_write =
                Some(device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::LessEqual,
                    write_enabled: true,
                }));
        }
        if self.depth_stencil_read.is_none() {
            self.depth_stencil_read =
                Some(device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::LessEqual,
                    write_enabled: false,
                }));
        }
    }

    fn ensure_depth_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.depth_dims == (width, height) && self.depth_texture.is_some() {
            return;
        }
        self.depth_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "node.nested_cubes_geometry::depth",
            mip_levels: 1,
        }));
        self.depth_dims = (width, height);
    }
}

impl Primitive for NestedCubesGeometry {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(target_angles_buf) = ctx.inputs.array("target_angles") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        // Read target_angles via the mapped CPU pointer. Both upstream
        // sources (cycle_table_row, scalar_array_accumulator) CPU-write,
        // so the same-frame read is safe.
        let mut target_angles = [0.0_f32; NESTED_CUBES_INSTANCE_COUNT as usize];
        if let Some(ptr) = target_angles_buf.mapped_ptr() {
            let f32_size = std::mem::size_of::<f32>();
            let available = (target_angles_buf.size as usize) / f32_size;
            let read_count = available.min(target_angles.len());
            // Safety: ptr is valid for `available` f32 elements (buffer
            // allocation policy); we read `read_count` ≤ both available
            // and target_angles.len(); content thread serial-executes
            // primitives so no concurrent writer races this read.
            let src = unsafe { std::slice::from_raw_parts(ptr as *const f32, read_count) };
            target_angles[..read_count].copy_from_slice(src);
        }

        // First-frame init: snap current_angles to target so we don't
        // ease in from zero at preset load.
        if !self.initialized {
            self.current_angles = target_angles;
            self.initialized = true;
        }

        let time = ctx
            .inputs
            .scalar("time")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let aspect = if height > 0 {
            width as f32 / height as f32
        } else {
            1.0
        };
        let dt = ctx.time.delta.0 as f32;

        // Detect trigger edge to kick envelope. First frame establishes
        // baseline without kicking.
        let raw_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let trigger_count = raw_count.round().max(0.0) as u32;
        let should_kick = match self.last_trigger_count {
            None => false,
            Some(last) => trigger_count != last,
        };
        self.last_trigger_count = Some(trigger_count);
        if should_kick {
            self.envelope = 1.0;
        }

        let decay_rate = ctx
            .params
            .get("decay_rate")
            .and_then(|v| v.as_scalar())
            .unwrap_or(10.0);
        if self.envelope > 0.001 {
            self.envelope *= (-decay_rate * dt).exp();
        } else {
            self.envelope = 0.0;
        }

        let filter_width = ctx
            .params
            .get("filter")
            .and_then(|v| v.as_scalar())
            .unwrap_or(2.0);
        let scale = ctx
            .params
            .get("scale")
            .and_then(|v| v.as_scalar())
            .unwrap_or(1.0);
        let scatter = ctx
            .params
            .get("scatter")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);

        // EMA smooth current_angles toward target_angles.
        let alpha = 1.0 - (-dt * filter_width).exp();
        for (current, target) in self.current_angles.iter_mut().zip(target_angles.iter()) {
            *current += alpha * (target - *current);
        }

        let sizes: [f32; 5] = std::array::from_fn(|i| INSTANCE_SIZES[i] * scale);

        // Isometric orthographic camera — ortho width 3.41, aspect-corrected
        // height. eye = ~5 * normalize(1,1,1). Matches legacy.
        let half_w = 1.705_f32;
        let half_h = half_w / aspect;
        let view = look_at_rh([2.887, 2.887, 2.887], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let proj = ortho_rh(-half_w, half_w, -half_h, half_h, 0.1, 20.0);
        let view_proj = mat4_mul(proj, view);

        let fill_uniforms = NestedCubesUniforms {
            view_proj,
            sizes_0_3: [sizes[0], sizes[1], sizes[2], sizes[3]],
            angles_0_3: [
                self.current_angles[0],
                self.current_angles[1],
                self.current_angles[2],
                self.current_angles[3],
            ],
            extra: [sizes[4], self.current_angles[4], 0.0, scatter],
            extra2: [time, self.envelope, 0.0, 0.0],
        };

        let gpu = ctx.gpu_encoder();
        self.ensure_pipelines(gpu.device);
        self.ensure_depth_texture(gpu.device, width, height);
        let fill_pipeline = self.fill_pipeline.as_ref().expect("fill pipeline init");
        let edge_pipeline = self.edge_pipeline.as_ref().expect("edge pipeline init");
        let depth_write = self
            .depth_stencil_write
            .as_ref()
            .expect("depth_write state init");
        let depth_read = self
            .depth_stencil_read
            .as_ref()
            .expect("depth_read state init");
        let depth_tex = self.depth_texture.as_ref().expect("depth texture init");

        gpu.native_enc.draw_instanced_depth_ex(
            fill_pipeline,
            target,
            depth_tex,
            depth_write,
            &[manifold_gpu::GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&fill_uniforms),
            }],
            TRI_VERTEX_COUNT,
            NESTED_CUBES_INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Clear,
            manifold_gpu::GpuTriangleFillMode::Fill,
            manifold_gpu::GpuPrimitiveType::Triangle,
            Some((1.0, 1.0, 0.0)),
            "node.nested_cubes_geometry::fill",
        );

        let edge_uniforms = NestedCubesUniforms {
            extra: [sizes[4], self.current_angles[4], 1.0, scatter],
            ..fill_uniforms
        };
        gpu.native_enc.draw_instanced_depth_ex(
            edge_pipeline,
            target,
            depth_tex,
            depth_read,
            &[manifold_gpu::GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&edge_uniforms),
            }],
            EDGE_VERTEX_COUNT,
            NESTED_CUBES_INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Load,
            manifold_gpu::GpuTriangleFillMode::Fill,
            manifold_gpu::GpuPrimitiveType::Line,
            None,
            "node.nested_cubes_geometry::edges",
        );
    }

    fn clear_state(&mut self) {
        self.current_angles = [0.0; 5];
        self.initialized = false;
        self.envelope = 0.0;
        self.last_trigger_count = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_required_target_angles_optional_time_and_trigger_count() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let inputs = NestedCubesGeometry::INPUTS;
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name, "target_angles");
        assert!(inputs[0].required);
        assert_eq!(
            inputs[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        assert_eq!(inputs[1].name, "time");
        assert!(!inputs[1].required);
        assert_eq!(inputs[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(inputs[2].name, "trigger_count");
        assert!(!inputs[2].required);
    }

    #[test]
    fn declares_texture_output_named_out() {
        use crate::node_graph::ports::PortType;
        let outputs = NestedCubesGeometry::OUTPUTS;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "out");
        assert_eq!(outputs[0].ty, PortType::Texture2D);
    }

    #[test]
    fn declares_four_params() {
        let names: Vec<_> = NestedCubesGeometry::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, ["filter", "scale", "scatter", "decay_rate"]);
    }

    #[test]
    fn instance_count_constant_is_five() {
        assert_eq!(NESTED_CUBES_INSTANCE_COUNT, 5);
    }

    #[test]
    fn primitive_registers_as_palette_generator() {
        use crate::node_graph::palette::{PaletteCategory, palette_atoms};
        let atoms = palette_atoms();
        let entry = atoms
            .iter()
            .find(|e| e.type_id == NestedCubesGeometry::TYPE_ID)
            .expect("nested_cubes_geometry should be registered as a palette atom");
        assert_eq!(entry.label, "Nested Cubes Geometry");
        assert!(matches!(entry.category, PaletteCategory::Atom));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Smoke tests on the real GPU. Verifies the new primitive produces
    //! non-trivial output (cubes visible, not a black frame) and that
    //! repeated runs with identical inputs produce identical pixels.
    //!
    //! Bit-exact parity vs the legacy generator's shader is a follow-up
    //! (see commit message): the WGSL is copied verbatim from
    //! `generators/shaders/nested_cubes.wgsl` (retained as the parity
    //! reference), and the Rust uniform construction mirrors the legacy
    //! generator 1:1 so a formal `Vec<u16>` diff harness is achievable
    //! but non-trivial render-pipeline test scaffolding.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::parameters::TableData;
    use crate::node_graph::primitives::CycleTableRow;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue, compile,
    };
    use crate::render_target::RenderTarget;

    use super::NestedCubesGeometry;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(
        plan: &crate::node_graph::ExecutionPlan,
        node: NodeInstanceId,
        port: &str,
    ) -> ResourceId {
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

    /// Build a minimal graph: cycle_table_row(1×5 table) → nested_cubes_geometry,
    /// run one frame, read back the rendered texture as `u16` pixels.
    fn run_geometry(w: u32, h: u32, angles: [f32; 5]) -> Vec<u16> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let cycler = g.add_node(Box::new(CycleTableRow::new()));
        let table = std::sync::Arc::new(
            TableData::new(vec![vec![angles[0], angles[1], angles[2], angles[3], angles[4]]])
                .expect("1×5 table"),
        );
        g.set_param(cycler, "table", ParamValue::Table(table)).unwrap();

        let geom = g.add_node(Box::new(NestedCubesGeometry::new()));
        g.connect((cycler, "row"), (geom, "target_angles")).unwrap();
        // FinalOutput sink: as of d84ae560 the planner skips outputs
        // with no downstream consumer, so wire `geom.out` to something
        // that keeps the resource alive for readback.
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.connect((geom, "out"), (sink, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_out = output_resource(&plan, geom, "out");
        let r_row = output_resource(&plan, cycler, "row");

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let out_target = RenderTarget::new(&device, w, h, format, "nested-cubes-geometry-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);
        // Pre-allocate the intermediate Array<f32> wire (cycler → geom).
        // Mirror of JsonGraphGenerator::pre_allocate_array_buffers but
        // local to this test (the generator path runs that walk
        // automatically; the bare Graph + Executor path here doesn't).
        let row_buf = device.create_buffer_shared((5 * std::mem::size_of::<f32>()) as u64);
        backend.pre_bind_array(r_row, row_buf);

        let mut native_enc = device.create_encoder("nested-cubes-geometry-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("nested-cubes-geometry-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize)
        };
        halves.to_vec()
    }

    /// Sanity: at the canonical initial angles + default params, the
    /// centre of the frame should be non-black — at least one of the
    /// white edge lines crosses the centre region of the isometric
    /// camera. If the whole frame is black, the dispatch is broken
    /// (depth state, vertex emission, transform composition, etc.).
    #[test]
    fn renders_non_black_output_at_initial_pose() {
        let w = 128;
        let h = 128;
        // First pose from POSES table — same as legacy initial angles.
        let pixels = run_geometry(w, h, [0.0, 90.0, 180.0, 270.0, 360.0]);
        let mut nonzero = 0usize;
        for chunk in pixels.chunks_exact(4) {
            // Any of R/G/B > 0 (fp16 0x0000) counts as a lit pixel.
            if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
                nonzero += 1;
            }
        }
        assert!(
            nonzero > 100,
            "expected >100 non-black pixels at the initial pose, got {nonzero}"
        );
    }

    /// Determinism: same input → same output. Confirms the dispatch is
    /// not picking up uninitialised state or time-dependent jitter when
    /// time = 0 / no trigger.
    #[test]
    fn deterministic_across_runs_with_same_input() {
        let w = 64;
        let h = 64;
        let angles = [0.0, 45.0, 90.0, 135.0, 180.0];
        let a = run_geometry(w, h, angles);
        let b = run_geometry(w, h, angles);
        assert_eq!(a.len(), b.len());
        for (i, (&n, &m)) in a.iter().zip(b.iter()).enumerate() {
            if n != m {
                panic!("pixel {i} diverged: {n:#06x} vs {m:#06x}");
            }
        }
    }
}
