//! BUG-115 half-day spike evidence — NOT a regression test, NOT wired into
//! any real primitive. Ignored by default; run explicitly with:
//!   cargo test -p manifold-renderer --test bug115_dynamic_arity_spike -- --ignored --nocapture
//!
//! Question: can `node.multi_blend`'s dynamic `num_inputs` (currently a
//! runtime-synthesized `shader_for(k)` kernel, a fusion boundary) be
//! converted to the freeze codegen path by declaring a FIXED max-arity
//! (MAX_INPUTS=8) set of statically-present `optional` `Coincident` texture
//! inputs, with per-input `use_<name>` flags gating the contribution — the
//! same mechanism `node.pack_rgba` (pack_channels.rs) already ships in
//! production for its 4 optional r/g/b/a inputs?
//!
//! This builds a throwaway 8-input MultiInputCoincident spec directly
//! against `generate_standalone` (bypassing the `primitive!` macro, so no
//! fake node registers in the global palette) and inspects the generated
//! WGSL to answer: (a) does codegen accept it at all, (b) what does the
//! generated kernel actually cost per dispatch regardless of how many of
//! the 8 slots are wired.

use manifold_renderer::node_graph::freeze::classify::{FusionKind, InputAccess};
use manifold_renderer::node_graph::freeze::codegen::{generate_standalone, StandaloneKernelSpec};
use manifold_renderer::node_graph::{ParamDef, ParamType, ParamValue};
use manifold_renderer::node_graph::ports::{NodeOutput, NodePort, PortKind, PortType};

const MAX_INPUTS: usize = 8;

fn eight_optional_coincident_inputs() -> Vec<NodePort> {
    (0..MAX_INPUTS)
        .map(|i| NodePort {
            name: std::borrow::Cow::Owned(format!("in_{i}")),
            ty: PortType::Texture2D,
            kind: PortKind::Input,
            required: false,
        })
        .collect()
}

fn outputs() -> Vec<NodeOutput> {
    vec![NodePort {
        name: std::borrow::Cow::Borrowed("out"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    }]
}

fn params() -> Vec<ParamDef> {
    vec![ParamDef {
        name: std::borrow::Cow::Borrowed("divisor"),
        label: "Divisor",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 100.0)),
        enum_values: &[],
    }]
}

/// Body fragment mirroring pack_channels_body.wgsl's shape: every input is
/// pre-read unconditionally by the wrapper (c_in_0 .. c_in_7), the body
/// itself only ADDS a term when its use_<name> flag is set. This is exactly
/// the "0u use-flag folding" mechanism named in the BUG-115 fix-shape hint.
fn eight_input_sum_body() -> String {
    let mut s = String::new();
    s.push_str("fn body(\n");
    for i in 0..MAX_INPUTS {
        s.push_str(&format!("    c_in_{i}: vec4<f32>,\n"));
    }
    s.push_str("    uv: vec2<f32>, dims: vec2<f32>, divisor: f32,\n");
    for i in 0..MAX_INPUTS {
        let sep = if i + 1 == MAX_INPUTS { "" } else { "," };
        s.push_str(&format!("    use_in_{i}: u32{sep}\n"));
    }
    s.push_str(") -> vec4<f32> {\n");
    s.push_str("    var sum = vec4<f32>(0.0, 0.0, 0.0, 0.0);\n");
    for i in 0..MAX_INPUTS {
        s.push_str(&format!(
            "    if use_in_{i} != 0u {{ sum = sum + c_in_{i}; }}\n"
        ));
    }
    s.push_str("    var outc = vec4<f32>(0.0, 0.0, 0.0, 0.0);\n");
    s.push_str("    if abs(divisor) > 1e-6 { outc = sum / divisor; }\n");
    s.push_str("    return outc;\n");
    s.push_str("}\n");
    s
}

/// (1)+(2) — does the always-8-input static-optional-Coincident shape even
/// generate valid WGSL through the real codegen entry point?
#[test]
#[ignore = "BUG-115 spike evidence, not a regression test — see docs/BUG_BACKLOG.md BUG-115"]
fn eight_input_static_optional_coincident_codegen_succeeds() {
    let inputs = eight_optional_coincident_inputs();
    let body = eight_input_sum_body();
    let access = vec![InputAccess::Coincident; MAX_INPUTS];

    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::MultiInputCoincident,
        body: &body,
        inputs: &inputs,
        params: &params(),
        input_access: &access,
        derived_uniforms: &[],
        outputs: &outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("codegen accepts 8 statically-declared optional Coincident texture inputs");

    // (3) Cost measurement: the wrapper pre-reads EVERY declared texture
    // input unconditionally (region.rs / codegen.rs's own doc comments:
    // "the pre-read c_* is harmlessly discarded" for an unwired optional).
    // Count actual textureSampleLevel calls in the generated wrapper.
    let sample_count = generated.matches("textureSampleLevel").count();
    eprintln!("generated kernel textureSampleLevel calls: {sample_count}");
    assert_eq!(
        sample_count, MAX_INPUTS,
        "the always-8-input shape samples all 8 textures on EVERY dispatch \
         regardless of how many are actually wired — this is the perf \
         tradeoff BUG-115's spike is measuring, not a bug in this test"
    );

    // Binding count: uniform + sampler + 8 textures + output = 11 bindings,
    // vs. today's dynamic shader_for(k) which binds exactly 2 + k + 1
    // (e.g. k=2 wired -> 5 bindings, k=8 wired -> 11 bindings).
    let binding_count = generated.matches("@binding(").count();
    eprintln!("generated kernel @binding declarations: {binding_count}");
    assert_eq!(
        binding_count,
        1 /* uniform */ + 1 /* sampler */ + MAX_INPUTS + 1, /* output */
        "always-8 binds 11 slots regardless of wired count \
         (today's dynamic k=2 case binds only 5)"
    );

    eprintln!(
        "generated kernel source size: {} bytes (8-wired case)",
        generated.len()
    );

    // naga parse sanity (the same gate region.rs's classify uses before
    // admitting a candidate atom) — belt-and-braces on top of codegen's own
    // Result, since a codegen success doesn't guarantee naga accepts the
    // WGSL syntax it emitted.
    let module = naga::front::wgsl::parse_str(&generated)
        .expect("naga must parse the generated always-8-input kernel");
    assert!(
        !module.entry_points.is_empty(),
        "generated kernel must have an entry point"
    );
}

/// Sanity comparison: what the CURRENT dynamic-arity approach costs for a
/// typical 2-wired-of-8 case (multi_blend.rs's `shader_for(k)`), to make the
/// always-8 tradeoff concrete rather than asserted. Hand-mirrors
/// `MultiBlend::shader_for` (private to that module) rather than reaching
/// into it, since this spike must not touch production code paths.
#[test]
#[ignore = "BUG-115 spike evidence, not a regression test — see docs/BUG_BACKLOG.md BUG-115"]
fn dynamic_two_input_kernel_costs_less_than_static_eight() {
    fn shader_for_k(k: usize) -> String {
        let mut s = String::new();
        for i in 0..k {
            s.push_str(&format!(
                "    sum = sum + textureSampleLevel(t{i}, samp, uv, 0.0);\n"
            ));
        }
        s
    }
    let k2 = shader_for_k(2);
    let sample_count_k2 = k2.matches("textureSampleLevel").count();
    assert_eq!(sample_count_k2, 2, "today's dynamic kernel for a 2-wired multi_blend samples exactly 2 textures");
    // The always-8 static shape (measured above) samples 8 regardless — a 4x
    // texture-sample-count increase for the common small-N case, with no
    // possibility of naga/backend DCE removing the extra samples since the
    // use-flag gate is a RUNTIME uniform value, not a compile-time constant,
    // so the sample call itself can never be proven dead.
}
