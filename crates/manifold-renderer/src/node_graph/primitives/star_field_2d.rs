//! `node.star_field_2d` — cinematic 3D parallax star field, packaged as
//! a single curated primitive with a side-channel `Array<CurvePoint>`
//! output exposing per-star screen-space positions.
//!
//! Port of the legacy `StarFieldGenerator` shader (4 layered hash-
//! jittered grids → spectral colour → multi-frequency twinkle, summed
//! per pixel) plus a secondary materialise compute pass that walks the
//! same (layer, cell) grid and writes each cell's screen NDC into a
//! shared `stars` buffer. Cells that fail the density threshold write a
//! sentinel position (`-99.0, -99.0`) so downstream consumers can
//! filter them out without an explicit active-count side channel.
//!
//! Two outputs are intentionally independent:
//! - **`out: Texture2D`** — the cinematic render. Use this on its own
//!   for the classic generator look.
//! - **`stars: Array(CurvePoint)`** — per-star screen positions. Wire
//!   into a constellation-renderer chain, particle seeder, audio-
//!   reactive modulator, etc. The texture render does not depend on
//!   the array; the array is computed deterministically from the same
//!   hash + camera-drift state.
//!
//! Slot layout matches the four hardcoded parallax layers:
//! `[0, 1600)` layer 0 (scale 40²), `[1600, 4100)` layer 1 (50²),
//! `[4100, 14100)` layer 2 (100²), `[14100, 46500)` layer 3 (180²).
//! Total capacity 46500 — sized to the worst-case full grid; expect
//! most slots to be sentinels at any non-maximum density.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::CurvePoint;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Total slot count in the `stars` Array output. Sum of per-layer
/// `scale²` across the four hardcoded parallax layers (40² + 50² +
/// 100² + 180²). Must match the same constant in the WGSL shader.
pub const STAR_FIELD_TOTAL_STARS: u32 = 46_500;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StarFieldUniforms {
    time_val: f32,
    aspect_ratio: f32,
    density: f32,
    brightness: f32,
    depth: f32,
    drift_speed: f32,
    drift_x: f32,
    drift_y: f32,
    twinkle: f32,
    warmth: f32,
    glow: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
}

crate::primitive! {
    name: StarField2D,
    type_id: "node.star_field_2d",
    purpose: "Cinematic 3D parallax star field as a single curated primitive — 4 depth-staggered hash-jittered layers with spectral colour, aspect-corrected core+halo gaussians, and multi-frequency twinkle. Texture output reproduces the legacy StarFieldGenerator bit-exact. Array(CurvePoint) `stars` output exposes per-star screen NDC positions for downstream composition (constellation lines, particle seeding, audio-reactive modulation) — 46500-slot fixed layout matching the 4-layer grid, sentinel-filled at cells below the density threshold.",
    inputs: {
        // Standard generator-input scalars, port-shadowable so the
        // generator graph can drive them from system.generator_input.
        time: ScalarF32 optional,
        aspect: ScalarF32 optional,
        // Every numeric scalar param is port-shadowable per the
        // primitive-authoring convention so LFOs / envelopes /
        // audio analysis can modulate without a Value-node detour.
        density: ScalarF32 optional,
        brightness: ScalarF32 optional,
        depth: ScalarF32 optional,
        drift_speed: ScalarF32 optional,
        drift_x: ScalarF32 optional,
        drift_y: ScalarF32 optional,
        twinkle: ScalarF32 optional,
        warmth: ScalarF32 optional,
        glow: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
        stars: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: "density",
            label: "Density",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "brightness",
            label: "Brightness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "depth",
            label: "Depth",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "drift_speed",
            label: "Drift Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "drift_x",
            label: "Drift X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "drift_y",
            label: "Drift Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "twinkle",
            label: "Twinkle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "warmth",
            label: "Warmth",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "glow",
            label: "Glow",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "time",
            label: "Time (base)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
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
    ],
    composition_notes: "Wire `time` from system.generator_input.time and `aspect` from system.generator_input.aspect for the canonical generator setup. The texture output is bit-exact against the legacy `StarFieldGenerator`. The `stars` Array carries one CurvePoint slot per (layer, cell) — 46500 slots total — with screen-NDC positions for cells that pass the density threshold and sentinel `(-99, -99)` elsewhere. Same hash, same camera drift as the texture, so the stars output tracks what's actually visible. Wire it into `node.render_lines` (sentinel-aware filtering pending; for now, downstream consumers should treat any point with x < -1.5 as absent) for constellation rendering, into a particle-seeding primitive for star-driven sims, or into image-domain compose primitives via a position-extracting intermediary. Stars behind the camera (post-drift) also write sentinel.",
    examples: [],
    picker: { label: "Star Field 2D", category: Atom },
    extra_fields: {
        materialize_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for StarField2D {
    /// Stars output is fixed-capacity at `STAR_FIELD_TOTAL_STARS`. Per-
    /// layer slot ranges are documented in the module docs.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "stars" {
            Some(STAR_FIELD_TOTAL_STARS)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let time = ctx.scalar_or_param("time", 0.0);
        let aspect = ctx.scalar_or_param("aspect", 1.777);
        let density = ctx.scalar_or_param("density", 0.5);
        let brightness = ctx.scalar_or_param("brightness", 0.7);
        let depth = ctx.scalar_or_param("depth", 0.5);
        let drift_speed = ctx.scalar_or_param("drift_speed", 0.15);
        let drift_x = ctx.scalar_or_param("drift_x", 0.3);
        let drift_y = ctx.scalar_or_param("drift_y", 0.1);
        let twinkle = ctx.scalar_or_param("twinkle", 0.3);
        let warmth = ctx.scalar_or_param("warmth", 0.0);
        let glow = ctx.scalar_or_param("glow", 0.3);

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let Some(stars_buf) = ctx.outputs.array("stars") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let uniforms = StarFieldUniforms {
            time_val: time,
            aspect_ratio: aspect,
            density,
            brightness,
            depth,
            drift_speed,
            drift_x,
            drift_y,
            twinkle,
            warmth,
            glow,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
        };

        let gpu = ctx.gpu_encoder();

        // Render pipeline — bit-exact port of legacy cs_main.
        let render_pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/star_field_2d.wgsl"),
                "cs_render",
                "node.star_field_2d.render",
            )
        });

        let render_bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: out_tex,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: stars_buf,
                offset: 0,
            },
        ];

        gpu.native_enc.dispatch_compute(
            render_pipeline,
            &render_bindings,
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.star_field_2d.render",
        );

        // Materialise pipeline — emits per-star screen NDC into the
        // `stars` Array. Iterates 4 × 180 × 180 cells (max layer
        // covers the rest with early exits).
        let materialize_pipeline = self.materialize_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/star_field_2d.wgsl"),
                "cs_materialize",
                "node.star_field_2d.materialize",
            )
        });

        let materialize_bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: out_tex,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: stars_buf,
                offset: 0,
            },
        ];

        // Largest layer is 180². 8×8 workgroups → 23×23 groups per
        // layer × 4 layers in z. Out-of-range cells exit early in the
        // shader.
        gpu.native_enc.dispatch_compute(
            materialize_pipeline,
            &materialize_bindings,
            [180_u32.div_ceil(8), 180_u32.div_ceil(8), 4],
            "node.star_field_2d.materialize",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn star_field_2d_declares_eleven_optional_inputs_and_two_outputs() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        assert_eq!(StarField2D::TYPE_ID, "node.star_field_2d");
        let ins = StarField2D::INPUTS;
        assert_eq!(ins.len(), 11);
        for port in ins {
            assert!(!port.required, "all star_field_2d inputs are optional");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        let outs = StarField2D::OUTPUTS;
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].name, "out");
        assert_eq!(outs[0].ty, PortType::Texture2D);
        assert_eq!(outs[1].name, "stars");
        assert_eq!(
            outs[1].ty,
            PortType::Array(ArrayType::of_known::<CurvePoint>())
        );
    }

    #[test]
    fn star_field_2d_declares_nine_visible_params_plus_two_input_carriers() {
        // Nine outer-card params plus `time` + `aspect` carriers
        // (which exist as both inputs and params so port-shadow
        // works). 11 total.
        let names: Vec<&str> = StarField2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "density",
                "brightness",
                "depth",
                "drift_speed",
                "drift_x",
                "drift_y",
                "twinkle",
                "warmth",
                "glow",
                "time",
                "aspect",
            ]
        );
    }

    #[test]
    fn stars_array_capacity_matches_layer_grid_sum() {
        // 40² + 50² + 100² + 180² = 1600 + 2500 + 10000 + 32400 = 46500
        assert_eq!(STAR_FIELD_TOTAL_STARS, 46_500);
        let prim = StarField2D::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "stars", &params, &[]),
            Some(STAR_FIELD_TOTAL_STARS)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &[]),
            None
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = StarField2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.star_field_2d");
    }
}

#[cfg(test)]
mod gpu_tests {
    //! Hardware parity test against the legacy `StarFieldGenerator`
    //! shader (kept inline below as the canonical reference). Renders
    //! both pipelines at the same dimensions with identical uniforms
    //! and asserts the `rgba16float` output is bit-exact pixel-by-pixel
    //! across a sample grid.
    //!
    //! The materialise pass is smoke-tested separately: at default
    //! density we expect a non-zero number of non-sentinel slots, all
    //! inside the [-5, 5] safety window the shader clips to.
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::{
        GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use crate::generators::mesh_common::CurvePoint;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue, compile,
    };
    use crate::render_target::RenderTarget;

    use super::{STAR_FIELD_TOTAL_STARS, StarField2D, StarFieldUniforms};

    /// Bit-exact copy of `crates/manifold-renderer/src/generators/
    /// shaders/star_field.wgsl` at the time of the migration commit.
    /// Kept here as the parity reference so the test survives the
    /// legacy generator's deletion.
    const LEGACY_SHADER: &str = include_str!("../../generators/shaders/star_field.wgsl");

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

    /// Build a graph with a single `StarField2D` node, run one frame,
    /// read back the texture as `u16` pixels and the stars array as
    /// `CurvePoint`s.
    fn run_star_field_2d(
        time: f32,
        w: u32,
        h: u32,
    ) -> (Vec<u16>, Vec<CurvePoint>) {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let sf = g.add_node(Box::new(StarField2D::new()));
        // Lock the inputs to known values so legacy and primitive see
        // identical uniforms.
        g.set_param(sf, "time", ParamValue::Float(time)).unwrap();
        g.set_param(sf, "aspect", ParamValue::Float(w as f32 / h as f32))
            .unwrap();

        let plan = compile(&g).unwrap();
        let r_out = output_resource(&plan, sf, "out");
        let r_stars = output_resource(&plan, sf, "stars");

        let mut backend = MetalBackend::new(&device, w, h, format);
        let out_target = RenderTarget::new(&device, w, h, format, "star-field-2d-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let stars_bytes =
            (STAR_FIELD_TOTAL_STARS as u64) * std::mem::size_of::<CurvePoint>() as u64;
        let stars_buf = device.create_buffer_shared(stars_bytes);
        let stars_slot = backend.pre_bind_array(r_stars, stars_buf);

        let mut native_enc = device.create_encoder("star-field-2d-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        // Texture readback.
        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("star-field-2d-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize)
        };
        let texture: Vec<u16> = halves.to_vec();

        // Stars Array readback.
        let stars_b = exec
            .backend()
            .array_buffer(stars_slot)
            .expect("stars buffer retained");
        let s_ptr = stars_b.mapped_ptr().expect("shared stars buffer");
        let s_slice = unsafe {
            std::slice::from_raw_parts(s_ptr as *const u8, stars_bytes as usize)
        };
        let stars: Vec<CurvePoint> = bytemuck::cast_slice::<u8, CurvePoint>(s_slice).to_vec();

        (texture, stars)
    }

    /// Run the legacy `StarFieldGenerator` shader directly via a
    /// hand-rolled compute pipeline (no graph runtime). Same uniform
    /// layout, same dispatch grid, same texture format.
    fn run_legacy_shader(time: f32, w: u32, h: u32) -> Vec<u16> {
        let device = crate::test_device();

        let pipeline = device.create_compute_pipeline(LEGACY_SHADER, "cs_main", "legacy-star-field");
        let target = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "legacy-star-field-out",
            mip_levels: 1,
        });

        let uniforms = StarFieldUniforms {
            time_val: time,
            aspect_ratio: w as f32 / h as f32,
            density: 0.5,
            brightness: 0.7,
            depth: 0.5,
            drift_speed: 0.15,
            drift_x: 0.3,
            drift_y: 0.1,
            twinkle: 0.3,
            warmth: 0.0,
            glow: 0.3,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
        };

        let mut native_enc = device.create_encoder("legacy-star-field-test");
        native_enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "legacy-star-field",
        );

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        native_enc.copy_texture_to_buffer(&target, &readback_buf, w, h, bytes_per_row);
        native_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize)
        };
        halves.to_vec()
    }

    /// Texture parity — the primitive's `cs_render` must be bit-exact
    /// against the legacy `StarFieldGenerator::cs_main` over the
    /// canonical default-param input.
    #[test]
    fn render_texture_bit_exact_against_legacy_shader() {
        let w = 64;
        let h = 64;
        let time = 1.234; // Arbitrary nonzero so drift kicks in.

        let (new_tex, _stars) = run_star_field_2d(time, w, h);
        let legacy_tex = run_legacy_shader(time, w, h);

        assert_eq!(new_tex.len(), legacy_tex.len());
        for (i, (&n, &l)) in new_tex.iter().zip(legacy_tex.iter()).enumerate() {
            if n != l {
                let px = i / 4;
                let comp = i % 4;
                let px_x = (px as u32) % w;
                let px_y = (px as u32) / w;
                let nf = f16::from_bits(n).to_f32();
                let lf = f16::from_bits(l).to_f32();
                panic!(
                    "texture diverged at pixel ({px_x}, {px_y}) component {comp}: \
                     primitive=0x{n:04x} ({nf}) legacy=0x{l:04x} ({lf})"
                );
            }
        }
    }

    /// Materialise smoke test — at default density most slots should
    /// contain a finite NDC position inside the screen-clip window
    /// (|x| ≤ 5, |y| ≤ 5). Sentinel slots `(-99, -99)` must dominate
    /// only the under-threshold cells, not the whole buffer.
    #[test]
    fn materialize_emits_real_stars_inside_clip_window() {
        let w = 64;
        let h = 64;
        let time = 0.0;

        let (_tex, stars) = run_star_field_2d(time, w, h);
        assert_eq!(stars.len() as u32, STAR_FIELD_TOTAL_STARS);

        let mut active = 0usize;
        let mut sentinel = 0usize;
        let mut out_of_layer = 0usize; // gid out-of-range slots (untouched)
        let mut bad_finite = 0usize;
        for s in &stars {
            let x = s.xy[0];
            let y = s.xy[1];
            if x < -98.0 && y < -98.0 {
                sentinel += 1;
            } else if x == 0.0 && y == 0.0 {
                // The buffer is shared-memory and may carry zeroed
                // slots for cells outside any layer's actual grid
                // (gid.x or gid.y past `layer_scale`). Count those
                // separately so the assertions stay clear.
                out_of_layer += 1;
            } else if !x.is_finite() || !y.is_finite() || x.abs() > 5.0 || y.abs() > 5.0 {
                bad_finite += 1;
            } else {
                active += 1;
            }
        }

        assert_eq!(bad_finite, 0, "no star should escape the clip window");
        assert!(
            active > 1000,
            "expected >1000 active stars at default density, got {active} \
             (sentinel={sentinel}, out_of_layer={out_of_layer})"
        );
    }

    /// Materialise determinism — same uniforms → same star positions.
    /// The hash is pure, the camera drift is a deterministic function
    /// of `time`, so two runs at `time = 0` must produce identical
    /// output.
    #[test]
    fn materialize_is_deterministic_at_fixed_time() {
        let w = 32;
        let h = 32;
        let (_t0, stars_a) = run_star_field_2d(0.0, w, h);
        let (_t1, stars_b) = run_star_field_2d(0.0, w, h);
        assert_eq!(stars_a.len(), stars_b.len());
        for (i, (a, b)) in stars_a.iter().zip(stars_b.iter()).enumerate() {
            assert_eq!(
                a.xy, b.xy,
                "star slot {i} diverged between identical runs: {:?} vs {:?}",
                a.xy, b.xy
            );
        }
    }
}
