//! Real import/serialization fixture for the clean-cut scene verification.
use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;

#[path = "common/scene_modifier.rs"]
mod common;

#[test]
fn clean_cut_import_roundtrip_and_render_fixture() {
    let model = std::env::var_os("MANIFOLD_CUT_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/gltf/cc0___mushroom.glb")
        });
    let (host, _) = assemble_import_graph(&model).expect("import cut fixture");
    let registry = PrimitiveRegistry::with_builtin();
    for (preset, cutter) in [
        ("OrderedRecon", "node.cut_mesh_bands"),
        ("OrderedReconHit", "node.cut_mesh_bands"),
        ("SurfacePeel", "node.cut_mesh_cells"),
        ("SurfacePeelHit", "node.cut_mesh_cells"),
        ("MaskedPeel", "node.cut_mesh_cells"),
        ("VortexFragments", "node.cut_mesh_cells"),
    ] {
        let owner = common::attach(&host, preset, "clean_cut_fixture");
        let saved = serde_json::to_string_pretty(&owner).expect("serialize fixture");
        let reloaded: EffectGraphDef = serde_json::from_str(&saved).expect("reload fixture");
        let prepared = prepare_scene_modifiers(&reloaded, &registry).expect("prepare clean cuts");
        assert!(
            prepared.def.nodes.iter().any(|node| node.type_id == cutter),
            "{preset}"
        );
        assert!(
            prepared
                .def
                .wires
                .iter()
                .any(|wire| wire.to_port == "topology"),
            "{preset} topology revision"
        );
        assert_eq!(
            serde_json::to_string_pretty(&reloaded).unwrap(),
            saved,
            "cut preparation must not mutate the saved graph"
        );
        if preset == "OrderedRecon"
            && let Some(output) = std::env::var_os("MANIFOLD_CUT_FIXTURE_OUT")
        {
            std::fs::write(output, saved).expect("write requested render fixture");
        }
    }
}

fn fixture() -> EffectGraphDef {
    assemble_import_graph(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/cc0___mushroom.glb"),
    )
    .unwrap()
    .0
}

fn set_leaf(
    nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
    type_id: &str,
    param: &str,
    value: f32,
) -> bool {
    for node in nodes {
        if node.type_id == type_id {
            node.params.insert(
                param.into(),
                manifold_core::effect_graph_def::SerializedParamValue::Float { value },
            );
            return true;
        }
        if let Some(group) = node.group.as_deref_mut()
            && set_leaf(&mut group.nodes, type_id, param, value)
        {
            return true;
        }
    }
    false
}

#[test]
fn cut_partition_controls_update_motion_and_map_without_reprepare() {
    use manifold_renderer::node_graph::EffectGraphDefExt;
    use manifold_renderer::node_graph::ParamValue;
    use manifold_renderer::node_graph::scene_modifier_expand::PreparedGraphValueWrites;
    let registry = PrimitiveRegistry::with_builtin();
    for (preset, motion, cutter, controls) in [
        (
            "OrderedRecon",
            "node.ordered_recon_mesh",
            "node.cut_mesh_bands",
            vec![("bands", 17.0), ("direction_x", 0.7), ("direction_y", 0.3)],
        ),
        (
            "SurfacePeel",
            "node.transform_mesh_patches",
            "node.cut_mesh_cells",
            vec![("cell_size", 0.13)],
        ),
    ] {
        let mut owner = common::attach(&fixture(), preset, "live_cuts");
        if preset == "OrderedRecon" {
            // Expose an actual host macro before asserting its generated fan-out.
            let partition = "bands";
            let local = owner.scene_modifiers[0]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap();
            let mut binding = local
                .bindings
                .iter()
                .find(|binding| binding.id == partition)
                .unwrap()
                .clone();
            let mut definition = local
                .params
                .iter()
                .find(|param| param.id == partition)
                .unwrap()
                .clone();
            binding.id = "cut_control".into();
            definition.id = binding.id.clone();
            binding.convert = manifold_core::effects::ParamConvert::Float;
            binding.target = manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                modifier_id: owner.scene_modifiers[0].id.clone(),
                param_id: partition.into(),
            };
            let host_metadata = owner.preset_metadata.as_mut().unwrap();
            host_metadata.params.push(definition);
            host_metadata.bindings.push(binding);
        }
        let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
        let mut graph = prepared.def.clone().into_graph(&registry).unwrap();
        let writes = PreparedGraphValueWrites::prepare(
            &owner,
            &prepared.routes,
            &graph,
            &Default::default(),
        )
        .unwrap();
        for (param, value) in controls {
            assert!(set_leaf(
                &mut owner.scene_modifiers[0].graph.nodes,
                motion,
                param,
                value
            ));
            writes.apply(&owner, &mut graph).unwrap();
            for type_id in [motion, cutter] {
                let nodes: Vec<_> = graph
                    .nodes()
                    .filter(|node| node.node.type_id().as_str() == type_id)
                    .collect();
                assert!(!nodes.is_empty(), "{type_id}");
                for node in nodes {
                    assert_eq!(
                        node.params.get(param),
                        Some(&ParamValue::Float(value)),
                        "{type_id}.{param}"
                    );
                }
            }
        }
        if preset != "OrderedRecon" {
            continue;
        }
        let metadata = prepared.def.preset_metadata.as_ref().unwrap();
        for cutter_node in prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == cutter)
        {
            assert!(metadata.bindings.iter().any(|binding| matches!(
                &binding.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                    if *node_id == cutter_node.node_id && (param == "bands" || param == "cell_size")
            )), "outer automation binding reaches cutter");
        }
    }
}

#[test]
fn mixed_fragment_wave_and_mask_stack_shares_cut_maps() {
    use manifold_renderer::node_graph::EffectGraphDefExt;
    let registry = PrimitiveRegistry::with_builtin();
    let mut owner = fixture();
    for preset in [
        "OrderedRecon",
        "SurfacePeel",
        "SurfaceWaves",
        "ElasticSculpture",
    ] {
        owner = common::attach(&owner, preset, preset);
    }
    let prepared = prepare_scene_modifiers(&owner, &registry).expect("mixed stack prepares");
    prepared
        .def
        .clone()
        .into_graph(&registry)
        .expect("mixed stack has valid typed wires");
    let incoming = |id, port: &str| {
        prepared
            .def
            .wires
            .iter()
            .find(|wire| wire.to_node == id && wire.to_port == port)
            .unwrap()
    };
    for fragment in prepared.def.nodes.iter().filter(|node| {
        matches!(
            node.type_id.as_str(),
            "node.ordered_recon_mesh" | "node.transform_mesh_patches"
        )
    }) {
        let current = incoming(fragment.id, "in").from_node;
        let reference = incoming(fragment.id, "reference").from_node;
        assert_eq!(
            incoming(current, "map").from_node,
            incoming(reference, "map").from_node,
            "current and reference must use identical barycentric maps"
        );
    }
    assert!(
        prepared
            .def
            .nodes
            .iter()
            .any(|node| node.type_id == "node.remap_cut_weights"),
        "unmodified mask weights must expand with cut geometry"
    );
    assert!(
        prepared
            .def
            .wires
            .iter()
            .any(|wire| wire.to_port == "topology")
    );
}
