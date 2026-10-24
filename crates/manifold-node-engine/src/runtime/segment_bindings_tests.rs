use super::segment_bindings::resolve_segment_bindings;
use super::*;
use crate::freeze::install::{self, SegmentLookup, SegmentView};
use crate::param_binding::{ParamBinding, ParamConvert, ParamId, ParamTarget};
use std::borrow::Cow;
use std::sync::Arc;

fn binding(id: &'static str, node: &str) -> ParamBinding {
    ParamBinding {
        id: ParamId::from(id),
        label: Cow::Borrowed("Param"),
        default_value: 0.0,
        target: ParamTarget::Node {
            node_id: NodeId::new(node),
            param: Cow::Borrowed("gain"),
        },
        convert: ParamConvert::Float,
        scale: 1.0,
        offset: 0.0,
        min: 0.0,
        max: 1.0,
        curve: manifold_core::macro_bank::MacroCurve::Linear,
        invert: false,
        default_mirrors_node_param: false,
    }
}

fn passthrough_def(name: &str) -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3, "name": name,
        "nodes": [
            {"id": 0, "nodeId": "source", "typeId": "system.source"},
            {"id": 1, "nodeId": "final_output", "typeId": "system.final_output"}
        ],
        "wires": [{"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in"}]
    }))
    .expect("passthrough def parses")
}

fn view_of(def: &EffectGraphDef, type_id: &'static str) -> Arc<LoadedPresetView> {
    Arc::new(LoadedPresetView {
        type_id: PresetTypeId::new(type_id),
        canonical_def: Arc::new(def.clone()),
        bindings: vec![binding("gain", "map")],
        fused_retarget: Default::default(),
        mesh_rules: Default::default(),
    })
}

/// A segment member whose binding has no node in the fused splice refuses
/// the whole segment, naming the effect, and the next lookup of that
/// segment is Refused, so the build splices every member on its own with
/// its own bindings. A neighbour can never leave a card's params dead.
#[test]
fn unresolvable_member_binding_refuses_the_segment() {
    let members = [
        PresetInstance::new(PresetTypeId::new("SegA")),
        PresetInstance::new(PresetTypeId::new("SegB")),
    ];
    let member_refs: Vec<&PresetInstance> = members.iter().collect();
    let graph = Graph::new();
    let node_map = vec![(NodeId::new("c0.map"), NodeInstanceId(0))];
    let mut view = SegmentView {
        def: passthrough_def("segment"),
        card_bindings: vec![
            vec![binding("gain", "c0.map")],
            vec![binding("gain", "c1.map")],
        ],
        retarget: Default::default(),
        mesh_rules: Default::default(),
    };

    let err = resolve_segment_bindings(&view, &member_refs, &graph, &node_map)
        .expect_err("a member binding with no node must refuse the segment");
    assert!(
        err.contains(members[1].id.as_str()),
        "refusal names the member: {err}"
    );
    assert!(
        !err.contains(members[0].id.as_str()),
        "only the broken member is named: {err}"
    );

    view.card_bindings[1] = vec![binding("gain", "c0.map")];
    let resolved = resolve_segment_bindings(&view, &member_refs, &graph, &node_map)
        .expect("every binding resolves");
    assert_eq!(
        resolved.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![1, 1]
    );

    let defs = [passthrough_def("refused-a"), passthrough_def("refused-b")];
    let cards: Vec<(EffectGraphDef, Arc<LoadedPresetView>)> = defs
        .iter()
        .zip(["SegA", "SegB"])
        .map(|(d, t)| (d.clone(), view_of(d, t)))
        .collect();
    let refs: Vec<_> = cards.iter().map(|(d, v)| (d, v.as_ref())).collect();
    install::refuse_segment(install::segment_key(&refs));
    assert!(
        matches!(
            install::fused_segment_view_for(&cards),
            SegmentLookup::Refused
        ),
        "a refused segment must build its cards one by one",
    );
}

/// The segment and fused-effect cache keys cover the bindings a compiled
/// view carries: the same graph with a different binding list is a
/// different entry, so no card inherits another card's bindings.
#[test]
fn fused_cache_keys_cover_bindings() {
    let def = passthrough_def("keyed");
    let a = view_of(&def, "KeyA");
    let b = view_of(&def, "KeyA");
    assert_eq!(
        install::fused_effect_cache_key(&def, &a),
        install::fused_effect_cache_key(&def, &b),
        "equal inputs share an entry",
    );
    let mut scaled = view_of(&def, "KeyA");
    Arc::get_mut(&mut scaled).expect("unique").bindings[0].scale = 2.0;
    assert_ne!(
        install::fused_effect_cache_key(&def, &a),
        install::fused_effect_cache_key(&def, &scaled),
    );
    assert_ne!(
        install::fused_effect_cache_key(&def, &a),
        install::fused_effect_cache_key(&def, &view_of(&def, "KeyC")),
    );
    assert_ne!(
        install::segment_key(&[(&def, a.as_ref()), (&def, a.as_ref())]),
        install::segment_key(&[(&def, a.as_ref()), (&def, scaled.as_ref())]),
    );
}
