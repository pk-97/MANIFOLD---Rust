/// Every bundled host preset, scene modifiers expanded and groups
/// flattened. Scene-modifier recipes prepare only on a host scene, so the
/// hosts are what the wiring guards walk.
fn flat_bundled_hosts() -> Vec<(String, manifold_core::effect_graph_def::EffectGraphDef)> {
    use manifold_core::preset_def::PresetKind;
    use manifold_nodes::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};

    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let mut hosts = Vec::new();
    let engine_scenes = manifold_nodes::engine_scenes::NAMES.map(manifold_core::PresetTypeId::new);
    for kind in [PresetKind::Effect, PresetKind::Generator] {
        let engine_scenes = if kind == PresetKind::Generator {
            engine_scenes.to_vec()
        } else {
            Vec::new()
        };
        for type_id in bundled_preset_type_ids(kind).chain(engine_scenes) {
            let def = bundled_preset_def(&type_id)
                .or_else(|| manifold_nodes::engine_scenes::scene_def(&type_id))
                .expect("bundled preset");
            let expanded =
                manifold_node_engine::load::expand::expand_scene_modifiers(def.as_ref(), &registry)
                    .unwrap_or_else(|error| panic!("{type_id}: {error}"));
            let flat = manifold_core::flatten::flatten_groups(&expanded)
                .unwrap_or_else(|error| panic!("{type_id}: {error}"));
            hosts.push((type_id.to_string(), flat));
        }
    }
    hosts
}

/// The one wire into `id.port`, as (from node, from port).
fn source<'a>(
    type_id: &str,
    flat: &'a manifold_core::effect_graph_def::EffectGraphDef,
    id: u32,
    port: &str,
) -> (u32, &'a str) {
    let mut wires = flat
        .wires
        .iter()
        .filter(|w| w.to_node == id && w.to_port == port);
    let wire = wires
        .next()
        .unwrap_or_else(|| panic!("{type_id}: {port} is not wired"));
    assert!(wires.next().is_none(), "{type_id}: {port} has two sources");
    (wire.from_node, wire.from_port.as_str())
}

fn type_of(flat: &manifold_core::effect_graph_def::EffectGraphDef, id: u32) -> Option<&str> {
    flat.nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.type_id.as_str())
}

/// Every bundled Liquid Surface builds on one display table: its level set
/// reads the tiles, pool and cell size of a node.surface_tiles, and its solid
/// is a node.liquid_solid_distance on that same table. Group interfaces may
/// pass them along; flattening makes every wire direct.
#[test]
fn liquid_surface_reads_one_display_table() {
    let mut checked = Vec::new();
    for (type_id, flat) in flat_bundled_hosts() {
        let source = |id: u32, port: &str| source(&type_id, &flat, id, port);
        for volume in flat
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.particle_volume")
        {
            let (table, _) = source(volume.id, "tiles");
            assert_eq!(
                type_of(&flat, table),
                Some("node.surface_tiles"),
                "{type_id}: display table"
            );
            for port in ["pool_tiles", "cell_size"] {
                assert_eq!(source(volume.id, port), (table, port), "{type_id}: {port}");
            }
            let (solid, port) = source(volume.id, "solid");
            assert_eq!(
                (type_of(&flat, solid), port),
                (Some("node.liquid_solid_distance"), "solid"),
                "{type_id}: display solid"
            );
            for port in ["tiles", "pool_tiles", "cell_size"] {
                assert_eq!(
                    source(solid, port),
                    (table, port),
                    "{type_id}: solid {port}"
                );
            }
            checked.push(type_id.clone());
        }
    }
    assert!(
        !checked.is_empty(),
        "no bundled preset meshes a liquid surface"
    );
}
