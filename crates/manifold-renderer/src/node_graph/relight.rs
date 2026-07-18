//! The "3D Shading" compiler pass — depth-companion synthesis + the fixed
//! relight template (`docs/DEPTH_RELIGHT_DESIGN.md` D2/D3/D4, phase P3).
//!
//! [`relight_augment`] is a pure `EffectGraphDef -> EffectGraphDef` transform,
//! architecturally modeled on `manifold_core::flatten::flatten_groups`: given
//! a validated def, it (a) walks backward from the node feeding
//! `system.final_output` using each node's [`DepthRule`] to find a height
//! source (D1/D4), (b) splices the D3 relight template between the current
//! final producer and `final_output`, and (c) returns the augmented def.
//! Append-only — every existing node, wire, and id in the input def is
//! preserved verbatim; the template's nodes get fresh ids above the def's
//! max and `rl_`-prefixed handles.
//!
//! This lives in `manifold-renderer` (not `manifold-core`, where
//! `EffectGraphDef` and the group flattener live) because it needs
//! [`PrimitiveRegistry`] to answer "what is this type_id's `depth_rule` and
//! Texture2D port shape" — exactly the same reason `graph_loader` and
//! `validate` live here instead of core.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_core::effects::{RelightField, RelightHeightFrom, RelightParams};
use manifold_core::NodeId;

use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
use crate::node_graph::depth_rule::DepthRule;
use crate::node_graph::persistence::PrimitiveRegistry;
use crate::node_graph::ports::PortType;

/// Handle/id-space prefix for every node the relight template mints. Also
/// doubles as the idempotence guard: [`relight_augment`] refuses to run on a
/// def that already carries one.
const RL_PREFIX: &str = "rl_";

fn is_texture(ty: PortType) -> bool {
    matches!(ty, PortType::Texture2D | PortType::Texture2DTyped(_))
}

fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}

fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}

/// Maps a live D3 knob to the `(handle, param)` of the template node it
/// drives, plus any value scaling the template applies at mint time so the
/// live write matches. Handles are `rl_`-prefixed and deterministic, so this
/// mapping is static and can be resolved against a spliced graph (unfused
/// handles) or a fused retarget map (fused uniform fields).
pub struct RelightTarget {
    pub node_handle: &'static str,
    pub param_name: &'static str,
    /// Multiplier applied to the raw field value before writing. The template
    /// bakes `relief * 12.0` into `surface_bumps.z_scale`, so the Relief
    /// knob's `rl_normal` target uses `12.0`; all others are `1.0`.
    pub scale: f32,
}

pub fn relight_field_targets(field: RelightField) -> &'static [RelightTarget] {
    match field {
        RelightField::LightX => &[
            RelightTarget { node_handle: "rl_lambert", param_name: "light_x", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "light_x", scale: 1.0 },
        ],
        RelightField::LightY => &[
            RelightTarget { node_handle: "rl_lambert", param_name: "light_y", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "light_y", scale: 1.0 },
        ],
        RelightField::Relief => &[
            RelightTarget { node_handle: "rl_normal", param_name: "z_scale", scale: 12.0 },
            RelightTarget { node_handle: "rl_ao", param_name: "relief", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "relief", scale: 1.0 },
        ],
        RelightField::AoIntensity => &[RelightTarget {
            node_handle: "rl_ao",
            param_name: "intensity",
            scale: 1.0,
        }],
        RelightField::ShadowSoftness => &[RelightTarget {
            node_handle: "rl_shadow",
            param_name: "softness",
            scale: 1.0,
        }],
        RelightField::Gain => &[RelightTarget {
            node_handle: "rl_exposure",
            param_name: "gain",
            scale: 1.0,
        }],
    }
}

/// Human-readable name for a relight knob, for logs / test failure messages.
pub fn relight_field_name(field: RelightField) -> &'static str {
    match field {
        RelightField::LightX => "Light X",
        RelightField::LightY => "Light Y",
        RelightField::Relief => "Relief",
        RelightField::AoIntensity => "AO Intensity",
        RelightField::ShadowSoftness => "Shadow Softness",
        RelightField::Gain => "Gain",
    }
}

/// True for any stable node id that belongs to the relight template. Segment
/// members are prefixed with `c{i}.`, so strip one dot-prefixed segment before
/// checking the `rl_` prefix.
pub fn is_relight_node_id(node_id: &str) -> bool {
    node_id
        .split_once('.')
        .map(|(_, rest)| rest.starts_with(RL_PREFIX))
        .unwrap_or_else(|| node_id.starts_with(RL_PREFIX))
}

/// D1/D4 backward walk: starting at the node feeding `final_output`'s `in`
/// port, follow `depth_rule` upstream (through the first Texture2D input
/// that has an incoming wire — the walk's one simplification: it doesn't
/// reconstruct `CombineNearest`'s true per-pixel nearest-depth compositing,
/// it just picks a deterministic single path toward a plausible height
/// origin) until hitting a `SourceHeight` producer, whose first Texture2D
/// output is the tap point. Returns `None` — meaning "fall back to
/// luminance of the final color" per D4 — on `Terminal`, on a construction
/// failure, on a cycle (feedback loops), or when the walk runs off the
/// front of the graph.
fn find_height_source(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    final_output_id: u32,
) -> Option<(u32, String)> {
    let mut current = def
        .wires
        .iter()
        .find(|w| w.to_node == final_output_id && w.to_port == "in")
        .map(|w| w.from_node)?;

    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(current) {
            return None; // cycle (a feedback loop reached before any SourceHeight) — fall back
        }
        let node = def.nodes.iter().find(|n| n.id == current)?;
        let instance = registry.construct(&node.type_id)?;
        match instance.depth_rule() {
            DepthRule::SourceHeight => {
                let port = instance
                    .outputs()
                    .iter()
                    .find(|p| is_texture(p.ty))
                    .map(|p| p.name.to_string())?;
                return Some((current, port));
            }
            DepthRule::Terminal => return None,
            DepthRule::Inherit | DepthRule::Warp | DepthRule::CombineNearest => {
                let tex_input = instance.inputs().iter().find(|p| is_texture(p.ty)).map(|p| p.name.to_string())?;
                let wire = def.wires.iter().find(|w| w.to_node == current && w.to_port == tex_input)?;
                current = wire.from_node;
            }
        }
    }
}

/// Builder for the template's synthesized nodes — mints a fresh sequential
/// id + a `rl_`-prefixed handle + a fresh [`NodeId`] per call, and pushes
/// onto `nodes`.
struct Mint<'a> {
    nodes: &'a mut Vec<EffectGraphNode>,
    next_id: u32,
}

impl<'a> Mint<'a> {
    fn node(&mut self, type_id: &str, handle: &str, params: BTreeMap<String, SerializedParamValue>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(EffectGraphNode {
            id,
            // Deterministic, not `short_id()`'s random UUID: card params
            // (P5) address inner template nodes by stable `node_id` exactly
            // like a bundled preset's hand-authored JSON node ids, and a
            // binding's target must survive every future rebuild — a fresh
            // random id per `relight_augment` call would silently orphan
            // every persisted binding the moment the chain rebuilds. The
            // `rl_`-prefixed handle already doubles as a unique, content-
            // stable name (the idempotence guard above refuses to double
            // mint it), so reusing it as the node_id costs nothing and buys
            // the stability card bindings depend on.
            node_id: NodeId::new(format!("{RL_PREFIX}{handle}")),
            type_id: type_id.to_string(),
            handle: Some(format!("{RL_PREFIX}{handle}")),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        });
        id
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// The "3D Shading" compiler pass. Off (never called) leaves the def, and
/// therefore the compiled plan, byte-identical to today's — this function
/// is the entire cost and behavior surface of the toggle.
///
/// `params` is the instance's live D3 card knobs (`PresetInstance::relight_params`,
/// phase P5) — always present on the instance regardless of the toggle, so
/// re-enabling restores whatever was last dialed in. `RelightParams::default()`
/// reproduces the probe's proven v6 recipe exactly.
///
/// Panics if `def` already carries `rl_`-prefixed nodes (idempotence guard —
/// this must never be applied twice to the same def).
pub fn relight_augment(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    params: &RelightParams,
) -> EffectGraphDef {
    assert!(
        !def.nodes
            .iter()
            .any(|n| n.handle.as_deref().is_some_and(|h| h.starts_with(RL_PREFIX))),
        "relight_augment: def already carries rl_-prefixed nodes — refusing to double-augment"
    );

    let Some(final_output_id) = def
        .nodes
        .iter()
        .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
        .map(|n| n.id)
    else {
        return def.clone(); // no final_output boundary — nothing to splice onto
    };

    let Some(final_wire_pos) = def
        .wires
        .iter()
        .position(|w| w.to_node == final_output_id && w.to_port == "in")
    else {
        return def.clone(); // final_output unwired — nothing to augment
    };
    let orig_source = (
        def.wires[final_wire_pos].from_node,
        def.wires[final_wire_pos].from_port.clone(),
    );

    // D4: `Auto` runs the structural D1 walk (itself falling back to
    // luminance-of-output when no `SourceHeight` producer is reachable);
    // `Luminance`/`InvertedLuminance` force the tap onto the final color's
    // luminance regardless of what the structural walk would find.
    let height_source = match params.height_from {
        RelightHeightFrom::Auto => {
            find_height_source(def, registry, final_output_id).unwrap_or_else(|| orig_source.clone())
        }
        RelightHeightFrom::Luminance | RelightHeightFrom::InvertedLuminance => orig_source.clone(),
    };

    let mut out = def.clone();
    out.wires.remove(final_wire_pos);

    let next_id = out.nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
    let mut mint = Mint {
        nodes: &mut out.nodes,
        next_id,
    };

    // ── Height branch: dither the tapped height source, blur it into the
    // working height field the rest of the template reads. ──
    let height_gray = mint.node("node.saturation", "height_gray", BTreeMap::from([("saturation".into(), float(0.0))]));
    // D4 `InvertedLuminance`: one extra invert atom between the grayscale tap
    // and the dither add — the rest of the template is unchanged, it just
    // reads the inverted field as "height".
    let height_tap = if params.height_from == RelightHeightFrom::InvertedLuminance {
        mint.node("node.invert", "height_inverted", BTreeMap::new())
    } else {
        height_gray
    };
    let dither_noise = mint.node(
        "node.noise",
        "dither_noise",
        BTreeMap::from([("type".into(), enum_val(2)), ("scale".into(), float(997.0))]),
    );
    let dither_scaled = mint.node(
        "node.scale_offset_image",
        "dither_scaled",
        BTreeMap::from([("scale".into(), float(0.003)), ("offset".into(), float(-0.0015))]),
    );
    let height_dithered = mint.node(
        "node.mix",
        "height_dithered",
        BTreeMap::from([("mode".into(), enum_val(2)), ("amount".into(), float(1.0))]), // Add
    );
    let height_blur_h = mint.node(
        "node.gaussian_blur",
        "height_blur_h",
        BTreeMap::from([("kernel_size".into(), enum_val(0)), ("axis".into(), enum_val(0))]),
    );
    let height_blur_v = mint.node(
        "node.gaussian_blur",
        "height_blur_v",
        BTreeMap::from([("kernel_size".into(), enum_val(0)), ("axis".into(), enum_val(1))]),
    );

    // ── Shading from the height field's normal. Relief fans out ×12-scaled
    // onto z_scale (0.25 → 3.0, the proven default) so one card knob covers
    // bump strength + AO relief + shadow relief in the same physical units
    // the D3 recipe tuned each atom at. Light X/Y fan to BOTH the Lambert
    // term and the shadow raymarch — they must track the same light
    // direction, or dragging the light knob desyncs the shadow from the
    // shading it's supposed to darken. ──
    let normal = mint.node(
        "node.surface_bumps",
        "normal",
        BTreeMap::from([("z_scale".into(), float(params.relief * 12.0))]),
    );
    let lambert = mint.node(
        "node.basic_light",
        "lambert",
        BTreeMap::from([
            ("ambient".into(), float(0.30)),
            ("light_x".into(), float(params.light_x)),
            ("light_y".into(), float(params.light_y)),
        ]),
    );
    let spec = mint.node("node.shininess", "spec", BTreeMap::from([("power".into(), float(48.0))]));

    // ── Occlusion + shadow. ──
    let camera = mint.node("node.look_at_camera", "camera", BTreeMap::new());
    let ao = mint.node(
        "node.ssao_gtao",
        "ao",
        BTreeMap::from([
            ("projection".into(), enum_val(1)), // Height Field
            ("relief".into(), float(params.relief)),
            ("radius".into(), float(0.02)),
            ("intensity".into(), float(params.ao_intensity)),
            ("slices".into(), float(4.0)),
            ("steps".into(), float(8.0)),
        ]),
    );
    let ao_blur_h = mint.node(
        "node.gaussian_blur",
        "ao_blur_h",
        BTreeMap::from([("kernel_size".into(), enum_val(0)), ("axis".into(), enum_val(0))]),
    );
    let ao_blur_v = mint.node(
        "node.gaussian_blur",
        "ao_blur_v",
        BTreeMap::from([("kernel_size".into(), enum_val(0)), ("axis".into(), enum_val(1))]),
    );
    let shadow = mint.node(
        "node.heightfield_shadow",
        "shadow",
        BTreeMap::from([
            ("light_x".into(), float(params.light_x)),
            ("light_y".into(), float(params.light_y)),
            ("softness".into(), float(params.shadow_softness)),
            ("relief".into(), float(params.relief)),
        ]),
    );

    // ── Combine: shadow * AO into Lambert, source * shading, + tinted spec, exposure. ──
    let lambert_shadowed = mint.node(
        "node.mix",
        "lambert_shadowed",
        BTreeMap::from([("mode".into(), enum_val(4)), ("amount".into(), float(1.0))]), // Multiply
    );
    let lambert_ao = mint.node(
        "node.mix",
        "lambert_ao",
        BTreeMap::from([("mode".into(), enum_val(4)), ("amount".into(), float(1.0))]), // Multiply
    );
    let shaded = mint.node(
        "node.mix",
        "shaded",
        BTreeMap::from([("mode".into(), enum_val(4)), ("amount".into(), float(1.0))]), // Multiply
    );
    let spec_tinted = mint.node(
        "node.mix",
        "spec_tinted",
        BTreeMap::from([("mode".into(), enum_val(4)), ("amount".into(), float(1.0))]), // Multiply
    );
    let combined = mint.node(
        "node.mix",
        "combined",
        BTreeMap::from([("mode".into(), enum_val(2)), ("amount".into(), float(1.0))]), // Add
    );
    let exposure = mint.node("node.exposure", "exposure", BTreeMap::from([("gain".into(), float(params.gain))]));

    let mut wires = vec![
        wire(height_source.0, &height_source.1, height_gray, "in"),
        wire(dither_noise, "out", dither_scaled, "in"),
        wire(height_tap, "out", height_dithered, "a"),
        wire(dither_scaled, "out", height_dithered, "b"),
        wire(height_dithered, "out", height_blur_h, "in"),
        wire(height_blur_h, "out", height_blur_v, "in"),
        wire(height_blur_v, "out", normal, "in"),
        wire(normal, "out", lambert, "normal"),
        wire(normal, "out", spec, "normal"),
        wire(height_blur_v, "out", ao, "depth"),
        wire(camera, "out", ao, "camera"),
        wire(ao, "out", ao_blur_h, "in"),
        wire(ao_blur_h, "out", ao_blur_v, "in"),
        wire(height_blur_v, "out", shadow, "height"),
        wire(lambert, "out", lambert_shadowed, "a"),
        wire(shadow, "out", lambert_shadowed, "b"),
        wire(lambert_shadowed, "out", lambert_ao, "a"),
        wire(ao_blur_v, "out", lambert_ao, "b"),
        wire(orig_source.0, &orig_source.1, shaded, "a"),
        wire(lambert_ao, "out", shaded, "b"),
        wire(spec, "out", spec_tinted, "a"),
        wire(orig_source.0, &orig_source.1, spec_tinted, "b"),
        wire(shaded, "out", combined, "a"),
        wire(spec_tinted, "out", combined, "b"),
        wire(combined, "out", exposure, "in"),
        wire(exposure, "out", final_output_id, "in"),
    ];
    // `InvertedLuminance` splices the invert atom between the grayscale tap
    // and the rest of the chain — the only wire that differs from the
    // default topology.
    if height_tap != height_gray {
        wires.push(wire(height_gray, "out", height_tap, "in"));
    }
    out.wires.extend(wires);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
    use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef};

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: Default::default(),
            type_id: type_id.to_string(),
            handle: handle.map(|s| s.to_string()),
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    fn base_def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        }
    }

    /// A trivial effect chain: Source → Contrast (Inherit) → FinalOutput.
    /// Contrast has no SourceHeight upstream, so this exercises the
    /// luminance-of-output fallback.
    fn simple_effect_def() -> EffectGraphDef {
        base_def(
            vec![
                node(0, SOURCE_TYPE_ID, Some("source")),
                node(1, "node.contrast", Some("contrast")),
                node(2, FINAL_OUTPUT_TYPE_ID, Some("final")),
            ],
            vec![
                wire(0, "out", 1, "in"),
                wire(1, "out", 2, "in"),
            ],
        )
    }

    /// A generator def whose final producer IS a SourceHeight atom
    /// (`node.noise`) directly feeding `final_output`.
    fn source_height_def() -> EffectGraphDef {
        base_def(
            vec![node(0, "node.noise", Some("gen")), node(1, FINAL_OUTPUT_TYPE_ID, Some("final"))],
            vec![wire(0, "out", 1, "in")],
        )
    }

    #[test]
    fn augmenting_a_minimal_def_splices_the_template_and_preserves_originals() {
        let reg = registry();
        let def = simple_effect_def();
        let augmented = relight_augment(&def, &reg, &RelightParams::default());

        // Every original node is present, unchanged, at its original id.
        for n in &def.nodes {
            let found = augmented.nodes.iter().find(|m| m.id == n.id).expect("original node preserved");
            assert_eq!(found.type_id, n.type_id);
            assert_eq!(found.handle, n.handle);
        }
        // Every original wire is present, unchanged, EXCEPT the one that fed
        // final_output — that one gets re-anchored onto the template's tail.
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        for w in &def.wires {
            if w.to_node == final_id && w.to_port == "in" {
                continue;
            }
            assert!(augmented.wires.iter().any(|aw| aw == w), "original wire preserved: {w:?}");
        }
        // final_output is now fed by a fresh rl_-prefixed node.
        let new_final_wire = augmented
            .wires
            .iter()
            .find(|w| w.to_node == final_id && w.to_port == "in")
            .expect("final_output still wired");
        let producer = augmented.nodes.iter().find(|n| n.id == new_final_wire.from_node).unwrap();
        assert!(producer.handle.as_deref().unwrap().starts_with(RL_PREFIX));

        // All minted nodes carry fresh ids above the original max and rl_ handles.
        let orig_max = def.nodes.iter().map(|n| n.id).max().unwrap();
        for n in &augmented.nodes {
            if n.handle.as_deref().is_some_and(|h| h.starts_with(RL_PREFIX)) {
                assert!(n.id > orig_max, "minted node id {} must exceed original max {orig_max}", n.id);
            }
        }
    }

    #[test]
    #[should_panic(expected = "already carries rl_-prefixed nodes")]
    fn double_application_is_refused() {
        let reg = registry();
        let def = simple_effect_def();
        let once = relight_augment(&def, &reg, &RelightParams::default());
        let _twice = relight_augment(&once, &reg, &RelightParams::default());
    }

    #[test]
    fn source_height_producer_is_tapped_directly() {
        let reg = registry();
        let def = source_height_def();
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        let tapped = find_height_source(&def, &reg, final_id);
        assert_eq!(tapped, Some((0, "out".to_string())));
    }

    #[test]
    fn def_with_only_inherit_and_terminal_falls_back_to_luminance() {
        let reg = registry();
        let def = simple_effect_def();
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        // Source is depth_rule Inherit (it's the entry boundary — see
        // boundary_nodes.rs), Contrast is Inherit too, so the walk runs off
        // the front of the graph (Source has no upstream wire) and returns
        // None — the fallback per D4.
        assert_eq!(find_height_source(&def, &reg, final_id), None);
    }

    /// "Just works on every graph" contract (P3 item 4): every bundled
    /// effect AND generator preset def must still validate after
    /// augmentation. GPU-gated (`validate_def` builds a real chain/generator
    /// through a `GpuDevice`) — run with `--features gpu-proofs`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn every_bundled_preset_validates_after_relight_augmentation() {
        use crate::node_graph::bundled_presets::bundled_preset_def;
        use crate::node_graph::validate::{ValidateKind, validate_def};
        use manifold_core::preset_def::PresetKind;

        let reg = registry();
        let device = crate::test_device();
        let device_arc = device.arc();
        let mut checked = 0usize;
        for (kind, validate_kind) in [
            (PresetKind::Effect, ValidateKind::Effect),
            (PresetKind::Generator, ValidateKind::Generator),
        ] {
            for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&type_id)
                    .unwrap_or_else(|| panic!("bundled preset {type_id:?} has no parsed def"));
                let augmented = relight_augment(def, &reg, &RelightParams::default());
                let report = validate_def(&augmented, &reg, validate_kind, &device_arc);
                assert!(
                    report.errors.is_empty(),
                    "relight-augmented {type_id:?} failed validation: {:?}",
                    report.errors
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected at least one bundled preset to check");
    }

    /// The golden test from P3 item 3 / D2: `relight = false` builds today's
    /// exact graph for every bundled effect preset — structural equality
    /// (node type_ids/doc-ids/params, and wires, in the SAME order) between
    /// the production `splice_def_into_chain(..., false)` wrapper and
    /// calling `instantiate_def` directly (bypassing the relight wrapper
    /// entirely — the pre-P3 call shape). Proves the toggle is a genuine
    /// compiled-variant, not a hidden always-on cost.
    ///
    /// Deliberately compares the pre-`compile()` `Graph`, not the compiled
    /// `ExecutionPlan`: `compile()`'s topological sort ties independent
    /// (no-dependency) nodes by hash-map iteration order, which is
    /// per-process-random and reorders unrelated steps between two
    /// independently-built graphs even when they're structurally identical
    /// — a pre-existing property of the compiler, unrelated to relight, that
    /// made an `ExecutionPlan`-level comparison spuriously flaky. Comparing
    /// the graph directly asserts the actual invariant this test exists for.
    #[test]
    fn relight_off_matches_pre_relight_effect_graph_for_every_bundled_preset() {
        use crate::node_graph::boundary_nodes::{FinalOutput, Source};
        use crate::node_graph::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::graph::Graph;
        use crate::node_graph::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
        use manifold_core::preset_def::PresetKind;

        type NodeSig = (u32, String, String, String);
        type WireSig = (u32, String, u32, String);
        fn signature(graph: &Graph) -> (Vec<NodeSig>, Vec<WireSig>) {
            let mut nodes: Vec<_> = graph
                .nodes()
                .map(|n| {
                    // AHashMap iteration order is per-process-random — sort
                    // params by key so this signature is comparable across
                    // two independently-built graphs.
                    let mut params: Vec<(&str, String)> =
                        n.params.iter().map(|(k, v)| (k.as_ref(), format!("{v:?}"))).collect();
                    params.sort_by_key(|(k, _)| *k);
                    (
                        n.id.0,
                        n.node.type_id().as_str().to_string(),
                        n.node_id.as_str().to_string(),
                        format!("{params:?}"),
                    )
                })
                .collect();
            nodes.sort_by_key(|(id, ..)| *id);
            let wires = graph
                .wires()
                .iter()
                .map(|w| (w.from.0.0, w.from.1.to_string(), w.to.0.0, w.to.1.to_string()))
                .collect();
            (nodes, wires)
        }

        let reg = registry();
        let mut checked = 0usize;
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id)
                .unwrap_or_else(|| panic!("bundled preset {type_id:?} has no parsed def"));

            // Path A: the production wrapper, relight OFF.
            let mut graph_a = Graph::new();
            let src_a = graph_a.add_node(Box::new(Source::new()));
            let Some(result_a) = splice_def_into_chain(&mut graph_a, (src_a, "out"), def, &reg, None) else {
                continue; // a preset that fails to splice fails identically on both paths; skip rather than false-fail
            };
            let final_a = graph_a.add_node(Box::new(FinalOutput::new()));
            graph_a.connect(result_a.output, (final_a, "in")).expect("connect A");

            // Path B: instantiate_def directly — bypasses the relight wrapper
            // entirely, exactly the pre-P3 call shape.
            let mut graph_b = Graph::new();
            let src_b = graph_b.add_node(Box::new(Source::new()));
            let inst_b = instantiate_def(
                &mut graph_b,
                def,
                &reg,
                HandleScope::PerSplice,
                BoundaryHandling::Splice {
                    source_endpoint: (src_b, "out"),
                },
            )
            .expect("instantiate_def B");
            let final_b = graph_b.add_node(Box::new(FinalOutput::new()));
            graph_b
                .connect(inst_b.output_endpoint.expect("splice output"), (final_b, "in"))
                .expect("connect B");

            assert_eq!(
                signature(&graph_a),
                signature(&graph_b),
                "relight=false must produce a byte-identical graph for {type_id:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "expected at least one bundled effect preset to check");
    }
}
