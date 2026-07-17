//! `node.coc_dilate` — fixed 3x3 neighborhood-max dilation of a CoC (circle-
//! of-confusion) texture. Standalone atom fixing BUG-137
//! (`docs/BUG_BACKLOG.md`): `node.variable_blur` picks its per-pixel gather
//! radius from *only the center pixel's own* CoC, so a heavily-blurred pixel
//! never borrows a wider radius from a neighboring high-CoC pixel and a sharp
//! pixel is never bled into by an adjacent blurred one — producing a hard
//! seam at depth discontinuities instead of a soft transition. Spreading the
//! max CoC found in a small neighborhood outward, before the gather
//! consumes it, is the standard real-time-DoF fix for exactly this.
//!
//! **Scoping decision, committed 2026-07-13 (`docs/BUG_BACKLOG.md` BUG-137):
//! a standalone atom, NOT folded into `node.coc_from_depth`** — folding a
//! neighborhood read into that atom would change its Pointwise fusion
//! classification (a `CoincidentTexel` producer) and cost its fusability.
//! `coc_dilate` is its own dispatch: `coc_from_depth` → `coc_dilate` →
//! `variable_blur` (H/V) — three dispatches, not a fused monolith, matching
//! the no-fused-monolith rule the way `coc_from_depth` + `variable_blur`
//! already do (`docs/CINEMATIC_POST_DESIGN.md` D1).
//!
//! §2.5 audit (`docs/DECOMPOSING_GENERATORS.md`): `rg 'purpose: "'` over
//! `primitives/` found no existing dilation / neighborhood-max atom for a
//! CoC-shaped (or any single-channel mask) texture — `neighbor_smooth.rs`
//! operates on `Array<InstanceTransform>` (a buffer of particle transforms),
//! not a texture, so it is not a match. Genuinely new.
//!
//! Algorithm (fixed, not parameterized — quality plumbing, not a performer
//! knob, per D8's `bilateral_blur` precedent of "no new cards"): for each
//! output texel, `out.r = max` over a 3x3 neighborhood (self + 8 neighbors)
//! of the input's R channel. `node.coc_from_depth`'s output convention is
//! R == G == B == coc_px / max_radius (a `[0,1]` fraction), alpha == 1.0 —
//! this atom preserves that convention exactly: broadcast the max to RGB,
//! alpha == 1.0 (matching the center's own alpha, which is always 1.0 under
//! that convention, so this is simultaneously "pass-through" and "constant").
//! Dilating a uniform (flat) CoC field returns the same flat field unchanged
//! (max of N identical values is that value) — the cheap no-op sanity gate.
//!
//! Single `Texture2D` input, sampler-`Gather` access (stencil-fetch ABI,
//! `fetch_in(uv)`) — matches `separable_gaussian.rs`'s single-input Gather
//! shape (the closer structural precedent vs. `gaussian_blur_variable_width.
//! rs`'s two-input `MultiInputCoincident`, since this atom has one input).
//! No params, no derived uniforms — codegen binds no uniform buffer at all
//! (`freeze/codegen.rs`'s paramless-atom rule, precedent `abs_texture.rs`):
//! bindings are `tex(0)`, `samp(1)`, `dst(2)`.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: CocDilate,
    type_id: "node.coc_dilate",
    purpose: "Fixed 3x3 neighborhood-max dilation of a single-channel mask-shaped texture (R == G == B convention, e.g. node.coc_from_depth's output): out.r = max over the 3x3 neighborhood (self + 8 neighbors) of in.r, broadcast to RGB, alpha = 1.0. Fixes BUG-137 (docs/BUG_BACKLOG.md): node.variable_blur reads its per-pixel gather radius from only the center pixel's own CoC, so a heavily-blurred pixel never borrows a wider radius from a neighboring high-CoC pixel — this atom spreads the max CoC outward before the gather consumes it, softening the hard seam at depth discontinuities. Wire coc_from_depth.out -> coc_dilate.in -> variable_blur(H/V).width. No params: the 3x3 radius is fixed, not a performer knob. A flat (uniform) input passes through unchanged (max of identical values is that value).",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: Inherit,
    composition_notes: "Insert between node.coc_from_depth and the two node.variable_blur (H then V) nodes that consume its `width` input: coc_from_depth.out -> coc_dilate.in, coc_dilate.out -> variable_blur_h.width AND -> variable_blur_v.width (both H and V read the SAME dilated CoC texture, matching the existing convention where both blur passes read the same undilated coc_from_depth.out today). Preserves coc_from_depth's output convention exactly (R==G==B in [0,1], alpha=1.0), so no downstream unit change is needed — variable_blur's width contract (step_size = width_sample * max_radius + 1.0) is unaffected other than reading a spatially-widened value. Also feeds node.bokeh_gather (the CINEMATIC_POST P4 upgrade) equally — dilation is upstream of whichever gather consumes the CoC.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "CoC Dilate", category: Atom },
    summary: "Spreads the maximum blur amount from a depth-of-field mask into its neighboring pixels, so the transition from sharp to blurry looks soft instead of having a hard visible edge.",
    category: Mask,
    role: Map,
    aliases: ["coc dilate", "dilate", "circle of confusion dilation", "depth of field", "dof", "neighborhood max", "max filter"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/coc_dilate_body.wgsl"),
    input_access: [Gather],
    stencil_fetch: true,
}

impl Primitive for CocDilate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from `wgsl_body` (Gather,
            // stencil-fetch — paramless, so no uniform buffer; generated
            // bindings are tex(0)/samp(1)/dst(2)). coc_dilate.wgsl is the
            // parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.coc_dilate standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.coc_dilate",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: in_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.coc_dilate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_single_texture_input_and_output() {
        use crate::node_graph::ports::PortType;

        assert_eq!(CocDilate::TYPE_ID, "node.coc_dilate");
        let names: Vec<&str> = CocDilate::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in"]);
        assert_eq!(CocDilate::INPUTS[0].ty, PortType::Texture2D);
        assert!(CocDilate::INPUTS[0].required);

        assert_eq!(CocDilate::OUTPUTS.len(), 1);
        assert_eq!(CocDilate::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_no_params() {
        assert!(CocDilate::PARAMS.is_empty());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CocDilate::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.coc_dilate");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I1**: generated-vs-hand parity (`docs/ADDING_PRIMITIVES.md` "The
    //! codegen path is mandatory") — the standalone kernel `run()` actually
    //! dispatches (built via `standalone_for_spec::<CocDilate>()`) must
    //! reproduce `coc_dilate.wgsl` (the hand oracle) texel-for-texel on a
    //! synthetic CoC-shaped input containing a sharp step and an isolated
    //! spike, so a broken kernel can't hide behind a flat fill.
    //!
    //! Flat-field no-op sanity: dilating a uniform CoC field returns the
    //! same flat field unchanged (BUG-137's fix shape doesn't smear a
    //! constant field into something else).
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::CocDilate;
    use crate::render_target::RenderTarget;

    fn upload_rgba16f(device: &GpuDevice, w: u32, h: u32, label: &str, px: &[f16]) -> GpuTexture {
        assert_eq!(px.len(), (w * h * 4) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Synthetic CoC-shaped input: a sharp step at x == w/2 (0.1 CoC on the
    /// left half, 0.9 on the right — a depth-discontinuity silhouette, the
    /// exact shape BUG-137 names) PLUS a single isolated spike at (2, 2) set
    /// to 1.0 — so a broken (non-max, or wrong-radius) kernel can't hide
    /// behind either fixture alone. R==G==B, alpha=1.0 (coc_from_depth's
    /// output convention).
    fn coc_step_with_spike(w: u32, h: u32) -> Vec<f32> {
        let mut plane = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                plane[i] = if x < w / 2 { 0.1 } else { 0.9 };
            }
        }
        if w > 2 && h > 2 {
            plane[(2 * w + 2) as usize] = 1.0;
        }
        plane
    }

    fn plane_to_rgba16f_tex(device: &GpuDevice, w: u32, h: u32, plane: &[f32], label: &str) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let v = f16::from_f32(plane[i]);
            px[i * 4] = v;
            px[i * 4 + 1] = v;
            px[i * 4 + 2] = v;
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, label, &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("coc-dilate-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    /// CPU reference implementing the SAME committed algorithm as the WGSL
    /// (I1's "two implementations of the same spec" pattern): 3x3
    /// neighborhood max with clamp-to-edge addressing (matching the default
    /// sampler `run()` creates).
    fn cpu_dilate(plane: &[f32], w: u32, h: u32) -> Vec<f32> {
        let mut out = vec![0.0f32; (w * h) as usize];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut m = f32::MIN;
                for dy in -1..=1i32 {
                    for dx in -1..=1i32 {
                        let sx = (x + dx).clamp(0, w as i32 - 1) as u32;
                        let sy = (y + dy).clamp(0, h as i32 - 1) as u32;
                        let v = plane[(sy * w + sx) as usize];
                        m = m.max(v);
                    }
                }
                out[(y as u32 * w + x as u32) as usize] = m;
            }
        }
        out
    }

    fn dispatch_dilate(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        sampler: &manifold_gpu::GpuSampler,
        input: &GpuTexture,
        w: u32,
        h: u32,
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "coc-dilate-out");
        let mut enc = device.create_encoder("coc-dilate-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture { binding: 0, texture: input },
                GpuBinding::Sampler { binding: 1, sampler },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "coc-dilate-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// **I1**: hand oracle (`coc_dilate.wgsl`) vs the generated standalone
    /// kernel (`standalone_for_spec::<CocDilate>()`) — both dispatched on the
    /// same synthetic step+spike fixture — AND both cross-checked against a
    /// plain-Rust CPU reference implementing the same 3x3-max spec
    /// independently (the CPU-reference parity pattern this design doc's
    /// cluster uses everywhere, `docs/CINEMATIC_POST_DESIGN.md` I1).
    #[test]
    fn generated_dilate_matches_hand_kernel_and_cpu_reference() {
        let device = crate::test_device();
        let (w, h) = (16u32, 8u32);
        let plane = coc_step_with_spike(w, h);
        let input = plane_to_rgba16f_tex(&device, w, h, &plane, "coc-dilate-in");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_wgsl = include_str!("shaders/coc_dilate.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "coc-dilate-hand");
        let hand_out = dispatch_dilate(&device, &hand_pipeline, &sampler, &input, w, h);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CocDilate>()
            .expect("node.coc_dilate standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "coc-dilate-generated",
        );
        let gen_out = dispatch_dilate(&device, &gen_pipeline, &sampler, &input, w, h);

        assert_eq!(hand_out.len(), gen_out.len());
        for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
            for c in 0..3 {
                assert!(
                    (h_px[c] - g_px[c]).abs() < 1e-4,
                    "texel {i} channel {c}: hand={} gen={}",
                    h_px[c],
                    g_px[c]
                );
            }
        }

        let cpu = cpu_dilate(&plane, w, h);
        for (i, g_px) in gen_out.iter().enumerate() {
            assert!(
                (g_px[0] - cpu[i]).abs() < 1e-4,
                "texel {i}: gpu={} cpu_reference={}",
                g_px[0],
                cpu[i]
            );
            // R == G == B broadcast, alpha == 1.0 (coc_from_depth's convention).
            assert!((g_px[0] - g_px[1]).abs() < 1e-6, "texel {i}: R != G");
            assert!((g_px[0] - g_px[2]).abs() < 1e-6, "texel {i}: R != B");
            assert!((g_px[3] - 1.0).abs() < 1e-6, "texel {i}: alpha != 1.0");
        }
    }

    /// Flat-field no-op sanity: dilating a uniform CoC field returns the
    /// same flat field unchanged (max of N identical values is that value)
    /// — a cheap, dedicated check that a broken kernel (e.g. a wrong
    /// neighborhood offset that reads outside the intended radius, or a
    /// non-max reduction) can't silently pass the step+spike test above by
    /// coincidence.
    #[test]
    fn flat_field_dilate_is_a_no_op() {
        let device = crate::test_device();
        let (w, h) = (12u32, 12u32);
        let flat_value = 0.37f32;
        let plane = vec![flat_value; (w * h) as usize];
        let input = plane_to_rgba16f_tex(&device, w, h, &plane, "coc-dilate-flat-in");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CocDilate>()
            .expect("node.coc_dilate standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "coc-dilate-flat",
        );
        let out = dispatch_dilate(&device, &pipeline, &sampler, &input, w, h);

        for (i, px) in out.iter().enumerate() {
            // f16 round-trip slack (the flat value itself only round-trips
            // through Rgba16Float to ~1e-3, same tolerance class as
            // coc_from_depth's gpu_tests) — not algorithm slack.
            assert!(
                (px[0] - flat_value).abs() < 2e-3,
                "texel {i}: flat field must dilate to itself, got {} want {}",
                px[0],
                flat_value
            );
        }
    }
}
