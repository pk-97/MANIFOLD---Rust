//! Alpha-contract sweep — the oracle for the alpha-standardisation pass.
//!
//! Manifold's compositor blends **premultiplied alpha**, but most shaders
//! were authored writing `vec4(rgb, 1.0)` — they hardcode opaque output and
//! discard the input's alpha. On a keyable layer that paints an opaque box
//! over whatever is below (the text-generator bug that kicked this off).
//!
//! This test enumerates every texture→texture effect in the registry, feeds
//! it a **fully transparent** input (alpha 0 everywhere), and asserts the
//! output stays transparent. An effect handed nothing must output nothing;
//! anything that forces alpha to 1.0 manufactures opacity and fails here.
//!
//! It is both the discovery tool (run it → the failures ARE the worklist)
//! and the permanent regression guard. Genuine exceptions — effects that are
//! opaque by design — go in [`OPAQUE_BY_DESIGN`]; everything else must be
//! fixed to carry the input's alpha.
//!
//! ## Coverage and known gaps (2026-06-22)
//!
//! Covered: every texture→texture effect. The only display violator found
//! was `node.gradient_map` (now fixed).
//!
//! The "could not probe" nodes need non-texture inputs (Channels arrays /
//! mesh / camera / material) the transparent probe can't synthesise. They
//! were verified alpha-correct by *reading* the shaders, not by this probe:
//!   - `draw_dots/markers/ticks/connections/gauge` write `(src.rgb + …, src.a)`
//!     — they preserve input alpha.
//!   - `render_filled_rects` outputs premultiplied `(color*a, 0)` with an
//!     additive blend that keeps the destination alpha.
//!   - `render_3d_mesh` / `…instanced…` clear the colour target to
//!     transparent `(0,0,0,0)` and draw opaque geometry over it → keyable.
//!   - `downsample` is a resize (size mismatch defeats the probe, not alpha);
//!     `feedback` is stateful (handles alpha per-mode in its shader).
//!
//! Not covered here: GENERATORS (no texture input, so out of this sweep).
//! `render_text` is guarded by its own gpu_test and by
//! [`render_text_respects_premultiplied_alpha_producer_contract`] below;
//! `basic_shape` writes coverage to alpha correctly (verified by reading). A
//! generator-keying probe for the rest of the sparse-generator surface is the
//! natural next extension if that surface grows.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use half::f16;

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::primitives::RenderText;
use manifold_renderer::node_graph::{
    Backend, Category, Executor, FinalOutput, FrameTime, Graph, MetalBackend, ParamValue, PrimitiveRegistry,
    Slot, compile, descriptor_for,
};

use crate::harness::{self, port_is_texture};

/// Display-effect categories whose `Texture2D` output is a finished image
/// that reaches the compositor. The alpha contract — transparent in →
/// transparent out — is *enforced* (hard-fail) only here.
///
/// Everything else that outputs a texture (`FieldsAndCoordinates`,
/// `MathAndConvert`, `MaterialsAndLighting`, `Routing`, `Geometry3D`,
/// `DetectionAndSampling`, `Noise`, `Mask`, particles) carries DATA in
/// texture channels — a coordinate field, a normal map, a depth map, a
/// mask. There `alpha = 1` is filler that is never composited, so those
/// are *reported* but not failed.
fn is_display_category(c: Category) -> bool {
    matches!(
        c,
        Category::ColorAndTone
            | Category::BlurAndSharpen
            | Category::DistortAndWarp
            | Category::Stylize
            | Category::Composite
    )
}

/// Display effects that legitimately produce opaque output from a fully
/// transparent input (a fill, a pattern source with an unused texture
/// input, …). Empty until triage proves a real exception.
const OPAQUE_BY_DESIGN: &[&str] = &[
    // Forcing alpha IS this node's one job — the explicit display-stage
    // opacity decision for generator termini whose blend chains have
    // consumed the alpha channel.
    // It is the composable form of the alpha=1 that resolve_scatter /
    // resolve_accumulator bake in-kernel. Never place it inside an effect.
    "node.set_alpha",
];

/// Output alpha above this counts as opaque. The bug forces 1.0 and legit
/// effects keep ~0, so the exact threshold is not delicate.
const ALPHA_EPS: f32 = 0.01;

#[test]
fn effects_preserve_transparency() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();

    let mut type_ids: Vec<String> = registry
        .known_type_ids()
        .filter(|id| !id.starts_with("node.__")) // skip test fixtures
        .map(|s| s.to_string())
        .collect();
    type_ids.sort();

    let mut checked = 0usize;
    let mut not_effect = 0usize;
    // (id, category_label, max_alpha, opaque_frac)
    let mut display_violators: Vec<(String, &'static str, f32, f32)> = Vec::new();
    let mut data_writes: Vec<(String, &'static str, f32, f32)> = Vec::new();
    let mut errored: Vec<String> = Vec::new();

    // Silence panic backtraces from individual probes so the sweep's
    // VIOLATOR list stays readable; restored before the final assert.
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    for id in &type_ids {
        // Classify: only sweep texture→texture effects. Generators (no
        // texture input) and data nodes (no texture output) are out of scope.
        let Some(node) = registry.construct(id) else {
            continue;
        };
        let is_effect = node.inputs().iter().any(|p| port_is_texture(&p.ty))
            && node.outputs().iter().any(|p| port_is_texture(&p.ty));
        drop(node);
        if !is_effect {
            not_effect += 1;
            continue;
        }

        let Some(node) = registry.construct(id) else {
            continue;
        };
        let probe = panic::catch_unwind(AssertUnwindSafe(|| h.run_transparent_probe(node)));
        let bytes = match probe {
            Ok(Some(b)) => b,
            Ok(None) => {
                errored.push(format!("{id} (no bind / compile)"));
                continue;
            }
            Err(_) => {
                errored.push(format!("{id} (panic)"));
                continue;
            }
        };
        checked += 1;

        let px = (h.width * h.height) as usize;
        let mut max_a = 0.0f32;
        let mut opaque = 0usize;
        for i in 0..px {
            let o = i * 8 + 6; // 4th f16 (alpha) of an Rgba16Float pixel
            let a = f16::from_bits(u16::from_le_bytes([bytes[o], bytes[o + 1]])).to_f32();
            max_a = max_a.max(a);
            if a > 0.5 {
                opaque += 1;
            }
        }
        let frac = opaque as f32 / px as f32;
        if max_a > ALPHA_EPS && !OPAQUE_BY_DESIGN.contains(&id.as_str()) {
            let category = descriptor_for(id)
                .map(|d| d.category)
                .unwrap_or(Category::Uncategorized);
            let entry = (id.clone(), category.label(), max_a, frac);
            if is_display_category(category) {
                display_violators.push(entry);
            } else {
                data_writes.push(entry);
            }
        }
    }

    panic::set_hook(prev_hook);

    let by_frac =
        |a: &(String, &str, f32, f32), b: &(String, &str, f32, f32)| b.3.total_cmp(&a.3);
    display_violators.sort_by(by_frac);
    data_writes.sort_by(by_frac);

    eprintln!(
        "\n=== alpha-contract sweep ===\n\
         checked {checked} texture->texture effects \
         ({not_effect} non-effect nodes skipped, {} could not be probed)\n",
        errored.len(),
    );
    eprintln!(
        "{} DISPLAY VIOLATOR(S) — composited effects that force opacity on a transparent \
         input (THE WORKLIST — fix to carry input alpha):",
        display_violators.len(),
    );
    for (id, cat, max_a, frac) in &display_violators {
        eprintln!("  {id:<44} [{cat:<16}] max_alpha={max_a:.3}  opaque_frac={frac:.3}");
    }
    eprintln!(
        "\n{} data-texture write(s) — non-display nodes (fields / math / materials / masks) \
         that write alpha=1 as filler; NOT composited, reported for review only:",
        data_writes.len(),
    );
    for (id, cat, max_a, frac) in &data_writes {
        eprintln!("  {id:<44} [{cat:<16}] max_alpha={max_a:.3}  opaque_frac={frac:.3}");
    }
    if !errored.is_empty() {
        eprintln!(
            "\ncould not probe ({}) — sparse producers / stateful / needs non-texture inputs:",
            errored.len(),
        );
        for e in &errored {
            eprintln!("  {e}");
        }
    }
    eprintln!("=== end sweep ===\n");

    assert!(
        display_violators.is_empty(),
        "{} display effect(s) force opaque alpha on a transparent input (see DISPLAY VIOLATOR \
         list above). Add genuine opaque-by-design effects to OPAQUE_BY_DESIGN; fix the rest \
         to carry the input's alpha (premultiplied-alpha contract).",
        display_violators.len(),
    );
}

/// BUG-8us3 value-level proof: a genuinely transparent generator must emit
/// premultiplied alpha. We drive `node.render_text` headlessly because text on
/// a transparent background is the canonical case — large guaranteed-transparent
/// regions plus anti-aliased glyph edges that exercise the semi-transparent
/// case in a single render.
///
/// Contract under test:
/// 1. Every texel with alpha == 0 has rgb == 0 (exactly).
/// 2. Semi-transparent texels satisfy `rgb ≈ alpha * unmultiplied_colour`.
/// 3. Opaque-ish texels (alpha close to 1) are not all black — sanity that the
///    generator actually drew something.
#[test]
fn render_text_respects_premultiplied_alpha_producer_contract() {
    let h = harness::shared();
    let (w, h_dim) = (h.width, h.height);
    let format = GpuTextureFormat::Rgba16Float;

    // Unmultiplied fill colour. The shader writes `rgb = fill_cov * unmul * alpha`
    // and `a = fill_cov * alpha`, so the output ratio `rgb / a` must equal `unmul`.
    let unmul = [1.0f32, 0.5, 0.25];
    let fill = [unmul[0], unmul[1], unmul[2], 1.0f32];

    let mut g = Graph::new();
    let rt = g.add_node(Box::new(RenderText::new()));
    let out = g.add_node(Box::new(FinalOutput::new()));

    g.set_param(rt, "text", ParamValue::String(Arc::new("A".to_string())))
        .unwrap();
    g.set_param(
        rt,
        "fontFamily",
        ParamValue::String(Arc::new("Helvetica".to_string())),
    )
    .unwrap();
    g.set_param(rt, "fill_color", ParamValue::Color(fill)).unwrap();
    g.set_param(rt, "size", ParamValue::Float(0.4)).unwrap();
    g.connect((rt, "out"), (out, "in")).unwrap();

    let plan = compile(&g).unwrap();

    let backend = MetalBackend::new(Arc::clone(&h.device), w, h_dim, format);
    let out_slot = Slot(backend.slot_count());

    let frame_time = FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    };

    let mut native_enc = h.device.create_encoder("alpha-contract-render-text");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut native_enc, &h.device);
        exec.execute_frame_with_gpu(&mut g, &plan, frame_time, &mut gpu);
    }
    native_enc.commit_and_wait_completed();

    let out_tex = exec
        .backend()
        .texture_2d(out_slot)
        .expect("output texture retained");
    let bytes = h.readback(out_tex);

    let px = (w * h_dim) as usize;
    let mut transparent_count = 0usize;
    let mut max_transparent_rgb = 0.0f32;
    let mut max_alpha = 0.0f32;
    let mut opaque_count = 0usize;
    let mut max_opaque_rgb = [0.0f32; 3];
    let mut best_edge: Option<(f32, [f32; 3])> = None;

    for i in 0..px {
        let o = i * 8;
        let r = f16::from_le_bytes([bytes[o], bytes[o + 1]]).to_f32();
        let g_ = f16::from_le_bytes([bytes[o + 2], bytes[o + 3]]).to_f32();
        let b = f16::from_le_bytes([bytes[o + 4], bytes[o + 5]]).to_f32();
        let a = f16::from_le_bytes([bytes[o + 6], bytes[o + 7]]).to_f32();

        if a < 0.001 {
            transparent_count += 1;
            max_transparent_rgb = max_transparent_rgb.max(r.max(g_).max(b));
        } else {
            max_alpha = max_alpha.max(a);
            if a > 0.9 {
                opaque_count += 1;
                max_opaque_rgb[0] = max_opaque_rgb[0].max(r);
                max_opaque_rgb[1] = max_opaque_rgb[1].max(g_);
                max_opaque_rgb[2] = max_opaque_rgb[2].max(b);
            }
            if a > 0.1 && a < 0.9 {
                let closeness = (a - 0.5).abs();
                if best_edge.map(|(best_a, _)| (best_a - 0.5).abs() > closeness).unwrap_or(true) {
                    best_edge = Some((a, [r, g_, b]));
                }
            }
        }
    }

    eprintln!(
        "alpha-contract producer proof: transparent={transparent_count}, \
         max_transparent_rgb={max_transparent_rgb:.5}, max_alpha={max_alpha:.5}, \
         opaque_count={opaque_count}, best_edge={best_edge:?}"
    );

    assert!(transparent_count > 0, "expected a transparent background");
    assert!(
        max_transparent_rgb < 0.0001,
        "transparent texels must have rgb == 0; max |rgb| was {max_transparent_rgb}"
    );

    assert!(opaque_count > 0, "expected opaque-ish glyph centre pixels");
    let max_opaque = max_opaque_rgb[0].max(max_opaque_rgb[1]).max(max_opaque_rgb[2]);
    assert!(
        max_opaque > 0.5,
        "opaque texels must not be black; max rgb was {max_opaque}"
    );

    let Some((a, rgb)) = best_edge else {
        panic!("expected at least one semi-transparent anti-aliased edge pixel");
    };
    for c in 0..3 {
        let expected = unmul[c] * a;
        assert!(
            (rgb[c] - expected).abs() < 0.05,
            "semi-transparent edge contract violated: channel {c} got {} expected {} \
             (alpha={a}, unmul={})",
            rgb[c],
            expected,
            unmul[c]
        );
    }
}
