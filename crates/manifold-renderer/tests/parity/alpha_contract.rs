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

use std::panic::{self, AssertUnwindSafe};

use half::f16;

use manifold_renderer::node_graph::{Category, PrimitiveRegistry, descriptor_for};

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
const OPAQUE_BY_DESIGN: &[&str] = &[];

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
