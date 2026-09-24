//! Structural contracts for the Blob Tracking V2 source and mask presets.
//!
//! These checks deliberately go through the bundled catalog and the normal
//! graph loaders. They catch a preset that parses as JSON while silently
//! dropping a binding, exposing controls for an inactive source, or placing
//! the mask validity gate before inversion.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::preset_def::PresetKind;
use manifold_renderer::node_graph::{
    EffectGraphDefExt, PrimitiveRegistry, bundled_preset_def, bundled_preset_type_ids, compile,
};
use manifold_renderer::preset_runtime::PresetRuntime;

const SOURCE_VARIANTS: &[(&str, &[&str])] = &[
    (
        "BlobTrackingV2",
        &[
            "amount",
            "detection_mode",
            "threshold",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "connect",
        ],
    ),
    (
        "BlobTrackingV2Colour",
        &[
            "amount",
            "target_red",
            "target_green",
            "target_blue",
            "tolerance",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "connect",
        ],
    ),
    (
        "BlobTrackingV2Motion",
        &[
            "amount",
            "motion_threshold",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "connect",
        ],
    ),
];

const MASK_VARIANTS: &[(&str, &[&str])] = &[
    (
        "MaskBlob",
        &[
            "threshold",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "selection",
            "expand",
            "feather",
            "invert",
            "amount",
        ],
    ),
    (
        "MaskBlobColour",
        &[
            "target_red",
            "target_green",
            "target_blue",
            "tolerance",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "selection",
            "expand",
            "feather",
            "invert",
            "amount",
        ],
    ),
    (
        "MaskBlobMotion",
        &[
            "motion_threshold",
            "denoise",
            "min_area",
            "max_area",
            "max_box_area",
            "max_blobs",
            "smoothing",
            "retention",
            "selection",
            "expand",
            "feather",
            "invert",
            "amount",
        ],
    ),
];

fn def_for(id: &str) -> EffectGraphDef {
    let type_id = PresetTypeId::from_string(id.to_owned());
    bundled_preset_def(&type_id)
        .unwrap_or_else(|| panic!("{id}: bundled preset is missing"))
        .clone()
}

fn node_map(nodes: &[EffectGraphNode]) -> BTreeMap<String, &EffectGraphNode> {
    let mut result = BTreeMap::new();
    for node in nodes {
        result.insert(node.node_id.to_string(), node);
        if let Some(group) = &node.group {
            result.extend(node_map(&group.nodes));
        }
    }
    result
}

fn type_ids(nodes: &[EffectGraphNode], result: &mut BTreeSet<String>) {
    for node in nodes {
        result.insert(node.type_id.clone());
        if let Some(group) = &node.group {
            type_ids(&group.nodes, result);
        }
    }
}

fn assert_binding_targets_are_active(id: &str, def: &EffectGraphDef) {
    let metadata = def
        .preset_metadata
        .as_ref()
        .unwrap_or_else(|| panic!("{id}: preset metadata is required"));
    let nodes = node_map(&def.nodes);
    let param_ids: BTreeSet<&str> = metadata
        .params
        .iter()
        .map(|param| param.id.as_str())
        .collect();

    for binding in &metadata.bindings {
        assert!(
            param_ids.contains(binding.id.as_str()),
            "{id}: binding {} has no active card parameter",
            binding.id
        );
        let BindingTarget::Node { node_id, param } = &binding.target else {
            panic!("{id}: binding {} must target a node parameter", binding.id);
        };
        let node = nodes.get(&node_id.to_string()).unwrap_or_else(|| {
            panic!(
                "{id}: binding {} targets missing node {}",
                binding.id, node_id
            )
        });
        assert!(
            node.params.contains_key(param),
            "{id}: binding {} targets inactive parameter {}.{}",
            binding.id,
            node_id,
            param
        );
    }
}

fn assert_graph_loads_validates_and_compiles(id: &str, def: &EffectGraphDef) {
    let registry = PrimitiveRegistry::with_builtin();
    if def
        .nodes
        .iter()
        .any(|node| node.type_id == "system.generator_input")
    {
        // Generator-shaped variants require the generator runtime's boundary
        // checks; an effect-shaped graph can use the direct graph compiler.
        PresetRuntime::from_def(def.clone(), &registry, None)
            .unwrap_or_else(|error| panic!("{id}: generator graph failed to compile: {error}"));
        return;
    }

    let graph = def
        .clone()
        .into_graph(
            &registry,
            &manifold_renderer::node_graph::mesh_change::PreparedMeshRules::default(),
        )
        .unwrap_or_else(|error| panic!("{id}: graph load failed: {error}"));
    // `compile` performs the graph validation pass before building the plan.
    compile(&graph).unwrap_or_else(|error| panic!("{id}: graph compile failed: {error:?}"));
}

fn assert_exact_params(id: &str, def: &EffectGraphDef, expected: &[&str]) {
    let actual: BTreeSet<&str> = def
        .preset_metadata
        .as_ref()
        .unwrap_or_else(|| panic!("{id}: preset metadata is required"))
        .params
        .iter()
        .map(|param| param.id.as_str())
        .collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    assert_eq!(actual, expected, "{id}: exposed controls drifted");
}

fn path_exists(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], from: &str, to: &str) -> bool {
    let ids: BTreeMap<String, u32> = nodes
        .iter()
        .map(|node| (node.node_id.to_string(), node.id))
        .collect();
    let Some(&from_id) = ids.get(from) else {
        return false;
    };
    let Some(&to_id) = ids.get(to) else {
        return false;
    };
    let mut outgoing: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for wire in wires {
        outgoing
            .entry(wire.from_node)
            .or_default()
            .push(wire.to_node);
    }
    let mut pending = VecDeque::from([from_id]);
    let mut visited = BTreeSet::new();
    while let Some(node) = pending.pop_front() {
        if !visited.insert(node) {
            continue;
        }
        if node == to_id {
            return true;
        }
        if let Some(next) = outgoing.get(&node) {
            pending.extend(next.iter().copied());
        }
    }
    false
}

fn float_param(node: &EffectGraphNode, name: &str) -> f32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Float { value }) => *value,
        Some(value) => panic!("{}.{} must be Float, got {value:?}", node.node_id, name),
        None => panic!("{}.{} is missing", node.node_id, name),
    }
}

#[test]
fn blob_v2_preset_graphs_and_bindings() {
    let mut all_variants = Vec::new();
    all_variants.extend_from_slice(SOURCE_VARIANTS);
    all_variants.extend_from_slice(MASK_VARIANTS);

    for (id, expected_params) in all_variants {
        let def = def_for(id);
        let metadata = def.preset_metadata.as_ref().expect("preset metadata");
        assert_eq!(metadata.id.as_str(), id);
        assert_eq!(metadata.available, id.starts_with("BlobTrackingV2"));
        assert_exact_params(id, &def, expected_params);
        assert_binding_targets_are_active(id, &def);
        assert_graph_loads_validates_and_compiles(id, &def);

        if id.starts_with("BlobTrackingV2") {
            let amount = def
                .nodes
                .iter()
                .find(|node| node.node_id.as_str() == "amount_value")
                .expect("HUD amount control");
            assert_eq!(
                float_param(amount, "a"),
                1.0,
                "{id}: HUD amount source must be one"
            );
        }

        let mut types = BTreeSet::new();
        type_ids(&def.nodes, &mut types);
        assert!(
            types.contains("node.detect_regions"),
            "{id}: detector core missing"
        );
        assert!(
            types.contains("node.track_regions"),
            "{id}: tracker core missing"
        );
    }
}

#[test]
fn blob_v2_legacy_preset_contract() {
    let def = def_for("BlobTracking");
    let metadata = def.preset_metadata.as_ref().expect("legacy metadata");
    assert_eq!(metadata.id.as_str(), "BlobTracking");
    assert!(
        metadata.available,
        "legacy BlobTracking must remain available"
    );
    assert_exact_params(
        "BlobTracking",
        &def,
        &["amount", "threshold", "sensitivity", "smoothing", "connect"],
    );
    let bindings: BTreeSet<&str> = metadata
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect();
    assert_eq!(
        bindings,
        BTreeSet::from(["amount", "threshold", "sensitivity", "smoothing", "connect"]),
    );
    assert_graph_loads_validates_and_compiles("BlobTracking", &def);
}

#[test]
fn blob_v2_invalid_inverted_mask_is_zero() {
    let def = def_for("MaskBlob");
    assert!(
        path_exists(&def.nodes, &def.wires, "invert", "valid_gate"),
        "MaskBlob validity must be applied after inversion"
    );
    assert!(
        path_exists(&def.nodes, &def.wires, "valid_gate", "final_output"),
        "MaskBlob validity gate must feed the final output"
    );
    let valid_gate = def
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "valid_gate")
        .expect("MaskBlob validity gate");
    assert_eq!(float_param(valid_gate, "scale"), 0.0);
}

#[test]
fn blob_v2_catalog_contains_effect_and_mask_ids() {
    let effect_ids: BTreeSet<String> = bundled_preset_type_ids(PresetKind::Effect)
        .map(|id| id.as_str().to_string())
        .collect();
    let generator_ids: BTreeSet<String> = bundled_preset_type_ids(PresetKind::Generator)
        .map(|id| id.as_str().to_string())
        .collect();
    let ids: BTreeSet<_> = effect_ids.union(&generator_ids).cloned().collect();
    for (id, _) in SOURCE_VARIANTS.iter().chain(MASK_VARIANTS) {
        assert!(
            ids.contains(*id),
            "{id}: preset is absent from bundled catalogs"
        );
    }
}
