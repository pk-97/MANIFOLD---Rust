//! `node.voronoi_2d` — 2D Worley/Voronoi cellular noise.
//!
//! Pure generator. Each integer cell holds one jittered feature
//! point. The shader returns F1 (distance to nearest), F2
//! (second-nearest), F2 - F1 (cell-edge factor), and a per-cell
//! stable random hash.
//!
//! Output:
//! - R = F1 (cell-center proximity)
//! - G = F2 (second-nearest distance)
//! - B = F2 - F1 (edge factor: high at cell boundaries)
//! - A = cell_hash (per-cell stable random in [0, 1] — same value
//!   across every pixel inside one cell, uncorrelated between cells.
//!   The foundation for per-cell variation: density threshold,
//!   per-cell colour, per-cell timing, twinkle frequency, etc.)

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    jitter: f32,
    out_scale: f32,
    write_out: u32,
    write_cell_id: u32,
    _pad2: f32,
}

crate::primitive! {
    name: Voronoi2D,
    type_id: "node.voronoi_2d",
    purpose: "Pure generator. 2D Worley / Voronoi cellular noise. `out` packs F1 in R (distance to nearest feature point), F2 in G (second-nearest), F2-F1 in B (cell-edge factor — high at boundaries), and a per-cell stable random hash in A (same value across every pixel inside a cell, uncorrelated between cells — drives per-cell variation: density threshold, twinkle frequency, per-cell colour, per-cell size). `cell_id` carries the F1-winning cell's integer coordinate in RG (constant within a Voronoi region) — feed RG + a seed into node.hash_field_by_seed for beat-reseeded per-cell composites (Voronoi Prism). Both outputs are independently optional: read only what you need and the other slot isn't allocated. Foundation for cellular patterns, cracked-glass, stained-glass, stars (sparse jitter + per-star twinkle), foam, fire embers, procedural tiles.",
    inputs: {
        // Every numeric param is port-shadowable so drift / animated
        // density / time-varying cell scale can come from upstream
        // scalar wires (LFOs, value-from-clip-trigger math chains,
        // generator-input.time multiplied by per-axis drift values).
        scale: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
        jitter: ScalarF32 optional,
        out_scale: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
        cell_id: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("jitter"),
            label: "Jitter",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("out_scale"),
            label: "Output Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "For star fields: chain into node.wrap → node.power (high exponent ~16) to spike F1 into points. Read A (cell_hash) to threshold which cells are stars (density slider via node.filter or node.smoothstep against A), and to derive per-star twinkle (math chain: A → frequency range → multiply by time → sin_term → multiply with the core). For cracked-glass / cell edges: read the B (F2-F1) channel via node.channel_mixer. For watercolor patches: read R, threshold via node.threshold. For per-cell colour variation (foam, pebbles, tiles): feed A into node.gradient_map or node.lut1d. Setting jitter to 0 gives a perfect grid; 1 gives full random cells. The cell_hash on A uses an independent hash mix from the jitter offsets, so each cell's hash is stable as jitter is animated (only the F1-winner can change at cell boundaries). The `cell_id` output (RG = F1-winner cell coordinate) is read via textureLoad downstream (node.hash_field_by_seed), so keep field and consumer at the same resolution; it carries integer cell coords (exact in fp16) for beat-reseeded per-cell offset/visibility (Voronoi Prism). Reading only `cell_id` (not `out`) is fine — the F1/F2 slot isn't allocated.",
    examples: [],
    picker: { label: "Voronoi 2D", category: Atom },
    summary: "Cellular noise that gives each cell a distance and a stable random value. Good for tiles, foam, cracked glass and starfields.",
    category: Noise,
    role: Source,
    aliases: ["cellular", "worley", "cells", "mosaic"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/voronoi_2d_body.wgsl"),
}

impl Primitive for Voronoi2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param on every numeric input: a wired scalar
        // overrides the inline param, the param's default fires when
        // unwired.
        let scale = ctx.scalar_or_param("scale", 8.0);
        let offset_x = ctx.scalar_or_param("offset_x", 0.0);
        let offset_y = ctx.scalar_or_param("offset_y", 0.0);
        let jitter = ctx.scalar_or_param("jitter", 1.0);
        let out_scale = ctx.scalar_or_param("out_scale", 1.0);

        // Both outputs are independently optional — the executor only
        // allocates a slot for an output that has a downstream consumer.
        // Gate each store on whether its slot exists, and bind whichever
        // slot IS live to both bindings (the gated-off store never
        // touches the placeholder, so aliasing is harmless).
        let out_slot = ctx.outputs.texture_2d("out");
        let cell_slot = ctx.outputs.texture_2d("cell_id");
        let Some(primary) = out_slot.or(cell_slot) else {
            return; // nothing consumes either output
        };
        let (w, h) = (primary.width, primary.height);
        if w == 0 || h == 0 {
            return;
        }
        let write_out = u32::from(out_slot.is_some());
        let write_cell_id = u32::from(cell_slot.is_some());
        let out_tex = out_slot.unwrap_or(primary);
        let cell_tex = cell_slot.unwrap_or(primary);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Multi-output Source: the generated kernel binds uniform(0)/dst_out(1)/
            // dst_cell_id(2), the body returns both in a BodyOutputs struct, and the
            // wrapper gates each store on the injected write_out/write_cell_id flags
            // (which sit at the same offsets as the hand uniform's, so VoronoiUniforms
            // packs the generated layout unchanged). voronoi_2d.wgsl is the oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.voronoi_2d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.voronoi_2d",
            )
        });

        let uniforms = VoronoiUniforms {
            scale,
            offset_x,
            offset_y,
            jitter,
            out_scale,
            write_out,
            write_cell_id,
            _pad2: 0.0,
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
                    texture: out_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: cell_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.voronoi_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn voronoi_2d_declares_five_optional_scalar_inputs_and_two_texture_outputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(Voronoi2D::TYPE_ID, "node.voronoi_2d");
        let ins = Voronoi2D::INPUTS;
        let names: Vec<&str> = ins.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["scale", "offset_x", "offset_y", "jitter", "out_scale"]
        );
        for port in ins {
            assert!(!port.required, "all voronoi_2d inputs are optional");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        let out_names: Vec<&str> = Voronoi2D::OUTPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(out_names, vec!["out", "cell_id"]);
        for port in Voronoi2D::OUTPUTS {
            assert_eq!(port.ty, PortType::Texture2D);
        }
    }

    #[test]
    fn voronoi_2d_has_expected_params() {
        let names: Vec<&str> = Voronoi2D::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["scale", "offset_x", "offset_y", "jitter", "out_scale"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Voronoi2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.voronoi_2d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Hardware tests for the cell_hash A-channel contract:
    //! (1) per-cell stable — pixels deep inside the same cell share A,
    //! (2) decorrelated across cells — distinct cells produce distinct A,
    //! (3) range — A ∈ [0, 1].
    //!
    //! These are the load-bearing properties downstream consumers rely
    //! on (density threshold via filter / smoothstep, per-cell colour
    //! ramp, per-star twinkle frequency, etc.). Run at jitter=0 so the
    //! F1 winner for each pixel is the cell the pixel falls in — makes
    //! "deep inside cell X" pixel coordinates deterministic.
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::Voronoi2D;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::{Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId};
    use crate::render_target::RenderTarget;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
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

    /// Render one Voronoi2D frame at the given params; return raw fp16
    /// pixels in row-major rgba order.
    fn run_voronoi(scale: f32, jitter: f32, w: u32, h: u32) -> Vec<u16> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let node = g.add_node(Box::new(Voronoi2D::new()));
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "scale", ParamValue::Float(scale)).unwrap();
        g.set_param(node, "jitter", ParamValue::Float(jitter)).unwrap();
        g.connect((node, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let r_out = output_resource(&plan, node, "out");

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let target = RenderTarget::new(&device, w, h, format, "voronoi-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, target);

        let mut native_enc = device.create_encoder("voronoi-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec.backend().texture_2d(out_slot).expect("retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("voronoi-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let slice: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        slice.to_vec()
    }

    fn alpha_at(pixels: &[u16], x: u32, y: u32, w: u32) -> f32 {
        let i = (y * w + x) as usize;
        f16::from_bits(pixels[i * 4 + 3]).to_f32()
    }

    /// At jitter=0 every feature point sits at the cell centre, so the
    /// F1 winner for a pixel is the cell the pixel falls in. Two
    /// pixels well inside the same cell must report identical A.
    #[test]
    fn cell_hash_is_stable_within_one_cell() {
        // scale=4 over 16-pixel canvas → cells span 4 pixels each.
        // Pixels (1,1) and (2,2) are deep inside cell (0,0).
        let w = 16;
        let h = 16;
        let pixels = run_voronoi(4.0, 0.0, w, h);

        let a_at_1_1 = alpha_at(&pixels, 1, 1, w);
        let a_at_2_2 = alpha_at(&pixels, 2, 2, w);
        assert_eq!(
            a_at_1_1, a_at_2_2,
            "two pixels in the same cell (0,0) returned different cell_hash"
        );

        // Pixels (9,9), (10,10) are deep inside cell (2,2). Same cell,
        // expect same hash.
        let a_at_9_9 = alpha_at(&pixels, 9, 9, w);
        let a_at_10_10 = alpha_at(&pixels, 10, 10, w);
        assert_eq!(
            a_at_9_9, a_at_10_10,
            "two pixels in the same cell (2,2) returned different cell_hash"
        );
    }

    /// Distinct cells must produce distinct (with very high probability)
    /// hashes. Survey several non-adjacent cells; require that not all
    /// reported alphas collapse to a single value.
    #[test]
    fn cell_hash_decorrelates_across_cells() {
        let w = 16;
        let h = 16;
        let pixels = run_voronoi(4.0, 0.0, w, h);

        // One sample deep inside each of cells (0,0), (1,0), (2,0),
        // (3,0), (0,1), (0,2), (0,3) — 7 different cells.
        let samples = [
            alpha_at(&pixels, 1, 1, w),    // cell (0,0)
            alpha_at(&pixels, 5, 1, w),    // cell (1,0)
            alpha_at(&pixels, 9, 1, w),    // cell (2,0)
            alpha_at(&pixels, 13, 1, w),   // cell (3,0)
            alpha_at(&pixels, 1, 5, w),    // cell (0,1)
            alpha_at(&pixels, 1, 9, w),    // cell (0,2)
            alpha_at(&pixels, 1, 13, w),   // cell (0,3)
        ];
        let unique: std::collections::HashSet<u32> =
            samples.iter().map(|f| f.to_bits()).collect();
        assert!(
            unique.len() >= 5,
            "expected ≥5 distinct cell hashes across 7 cells, got {unique:?} from {samples:?}",
        );
    }

    /// Every pixel's A must land in [0, 1].
    #[test]
    fn cell_hash_in_unit_range() {
        let w = 16;
        let h = 16;
        let pixels = run_voronoi(4.0, 1.0, w, h);
        for y in 0..h {
            for x in 0..w {
                let a = alpha_at(&pixels, x, y, w);
                assert!(
                    (0.0..=1.0).contains(&a),
                    "cell_hash out of range at ({x},{y}): {a}",
                );
            }
        }
    }
}
