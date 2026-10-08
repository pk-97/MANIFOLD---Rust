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

use manifold_node_engine::scene::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
use manifold_node_engine::scene::depth_rule::DepthRule;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::PortType;

/// Handle/id-space prefix for every node the relight template mints. Also
/// doubles as the idempotence guard: [`relight_augment`] refuses to run on a
/// def that already carries one.
const RL_PREFIX: &str = "rl_";

inventory::submit! {
    manifold_node_engine::load::augmentation::RelightAugmentation {
        augment: relight_augment,
        targets: relight_field_targets,
    }
}


fn is_texture(ty: PortType) -> bool {
    matches!(ty, PortType::Texture2D | PortType::Texture2DTyped(_))
}

fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}

fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}

use manifold_node_engine::load::augmentation::RelightTarget;

pub fn relight_field_targets(field: RelightField) -> &'static [RelightTarget] {
    static LIGHT_X: [RelightTarget; 2] = [
            RelightTarget { node_handle: "rl_lambert", param_name: "light_x", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "light_x", scale: 1.0 },
        ];
    static LIGHT_Y: [RelightTarget; 2] = [
            RelightTarget { node_handle: "rl_lambert", param_name: "light_y", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "light_y", scale: 1.0 },
        ];
    static RELIEF: [RelightTarget; 3] = [
            RelightTarget { node_handle: "rl_normal", param_name: "z_scale", scale: 12.0 },
            RelightTarget { node_handle: "rl_ao", param_name: "relief", scale: 1.0 },
            RelightTarget { node_handle: "rl_shadow", param_name: "relief", scale: 1.0 },
        ];
    static AO_INTENSITY: [RelightTarget; 1] = [RelightTarget {
            node_handle: "rl_ao",
            param_name: "intensity",
            scale: 1.0,
        }];
    static SHADOW_SOFTNESS: [RelightTarget; 1] = [RelightTarget {
            node_handle: "rl_shadow",
            param_name: "softness",
            scale: 1.0,
        }];
    static GAIN: [RelightTarget; 1] = [RelightTarget {
            node_handle: "rl_exposure",
            param_name: "gain",
            scale: 1.0,
        }];
    match field {
        RelightField::LightX => &LIGHT_X,
        RelightField::LightY => &LIGHT_Y,
        RelightField::Relief => &RELIEF,
        RelightField::AoIntensity => &AO_INTENSITY,
        RelightField::ShadowSoftness => &SHADOW_SOFTNESS,
        RelightField::Gain => &GAIN,
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
mod augmentation_source_tests {
    use super::*;
    #[test]
    fn relight_registration_preserves_augmentation_and_targets() {
        let registry = PrimitiveRegistry::with_builtin();
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            "nodes": [{"id": 0, "typeId": "system.source"}, {"id": 1, "typeId": "system.final_output"}],
            "wires": [{"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in"}]
        })).unwrap();
        let params = RelightParams::default();
        assert!(relight_augment(&def, &registry, &params).nodes.len() > def.nodes.len());
        assert_eq!(serde_json::to_value(relight_augment(&def, &registry, &params)).unwrap(),
            serde_json::to_value(manifold_node_engine::load::augmentation::relight_augment(&def, &registry, &params)).unwrap());
        for field in [RelightField::LightX, RelightField::LightY, RelightField::Relief,
            RelightField::AoIntensity, RelightField::ShadowSoftness, RelightField::Gain] {
            let direct = relight_field_targets(field);
            let registered = manifold_node_engine::load::augmentation::relight_field_targets(field);
            assert!(std::ptr::eq(direct, registered));
        }
    }
}

#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    use super::*;

    pub const RL_PREFIX: &str = super::RL_PREFIX;

    pub fn find_height_source(
        def: &EffectGraphDef,
        registry: &PrimitiveRegistry,
        final_output_id: u32,
    ) -> Option<(u32, String)> {
        super::find_height_source(def, registry, final_output_id)
    }

    pub fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        super::wire(from_node, from_port, to_node, to_port)
    }
}
