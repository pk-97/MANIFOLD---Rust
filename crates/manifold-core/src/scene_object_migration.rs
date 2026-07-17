//! `SceneObject` wire migration — SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D5.
//!
//! `node.render_scene` used to carry 21 parallel per-object port families
//! (`mesh_k`/`material_k`/17 maps/`transform_k`/`instances_k`); P2 deletes
//! all of them in favor of one `object_k: Object` port fed by a
//! `node.scene_object` node. Every def written before that landing still
//! carries the legacy wiring — this migration rewrites it in place,
//! structurally, with no version gate (idempotence is the gate itself, per
//! D5). Beside `flatten.rs`: defs are core vocabulary, and every def-to-Graph
//! conversion converges on `instantiate_def` (`manifold-renderer`'s
//! `graph_loader.rs`), the one choke point that calls this — project load,
//! bundled/reference preset load, user-library preset load, `graph_tool
//! migrate`, and the live glTF importer's freshly-built output all pass
//! through it, so this one rewrite covers all of them without a separate
//! call site per producer.

use std::collections::BTreeMap;

use crate::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID};
use crate::id::NodeId;
use crate::math::short_id;

const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";

/// `(legacy port prefix, scene_object input port name)`. No prefix here is a
/// prefix of another, so at most one entry ever matches a given port name —
/// order is cosmetic, kept in the same order `SceneObject`'s fields are
/// declared (`crates/manifold-renderer/src/node_graph/scene_object.rs`).
const LEGACY_OBJECT_PORT_FAMILIES: &[(&str, &str)] = &[
    ("mesh_", "vertices"),
    ("material_", "material"),
    ("base_color_map_", "base_color_map"),
    ("normal_map_", "normal_map"),
    ("mr_map_", "mr_map"),
    ("occlusion_map_", "occlusion_map"),
    ("emissive_map_", "emissive_map"),
    ("sheen_color_map_", "sheen_color_map"),
    ("sheen_roughness_map_", "sheen_roughness_map"),
    ("iridescence_map_", "iridescence_map"),
    ("iridescence_thickness_map_", "iridescence_thickness_map"),
    ("anisotropy_map_", "anisotropy_map"),
    ("clearcoat_map_", "clearcoat_map"),
    ("clearcoat_roughness_map_", "clearcoat_roughness_map"),
    ("clearcoat_normal_map_", "clearcoat_normal_map"),
    ("specular_map_", "specular_map"),
    ("specular_color_map_", "specular_color_map"),
    ("transmission_map_", "transmission_map"),
    ("volume_thickness_map_", "volume_thickness_map"),
    ("transform_", "transform"),
    ("instances_", "instances"),
];

/// Rewrite every `node.render_scene`'s legacy per-object wiring (at any
/// group depth) into `node.scene_object` nodes feeding `object_k` ports.
/// Returns `true` iff anything changed. A def with no legacy wires is a
/// pure no-op — the common case once every def has been migrated once,
/// matching `migrate_def_type_ids`'s "passes through unchanged" contract.
///
/// Forbidden (D5): dropping a triple this rule can't parse. Any wire whose
/// `to_port` doesn't match one of the 21 known families with a valid
/// trailing object index is left exactly as loaded — never touched, never
/// silently discarded.
pub fn migrate_scene_object_wires(def: &mut EffectGraphDef) -> bool {
    let mut next_id = max_node_id_recursive(&def.nodes) + 1;
    let mut changed = false;
    migrate_scope(&mut def.nodes, &mut def.wires, &mut next_id, &mut changed);
    changed
}

/// Same recursive max-id walk `gltf_import.rs`'s merge assembler uses
/// (`max_node_id_recursive`) — ids are unique across the WHOLE document,
/// group bodies included, so a fresh id must clear every scope's max, not
/// just the scope being minted into.
fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_recursive(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

/// Parse a legacy per-object port name into `(scene_object input name,
/// object index)`. `None` for anything that doesn't match — the
/// "unparseable" case the caller must leave untouched.
fn parse_legacy_object_port(port: &str) -> Option<(&'static str, u32)> {
    for (prefix, target) in LEGACY_OBJECT_PORT_FAMILIES {
        if let Some(suffix) = port.strip_prefix(prefix)
            && let Ok(k) = suffix.parse::<u32>()
        {
            return Some((target, k));
        }
    }
    None
}

/// One object about to get a minted `node.scene_object` — planned in a
/// first pass (below) so the handle-dedup set can be seeded correctly
/// before any minting happens (see `migrate_scope`'s comment on why this
/// needs two passes).
struct PlannedObject {
    render_scene_id: u32,
    k: u32,
    wire_indices: Vec<usize>,
    desired_handle: String,
    /// The node whose handle `desired_handle` was borrowed from (a group
    /// producer, D5), if any — excluded from the dedup seed so borrowing a
    /// still-present group's own name isn't treated as a collision with
    /// itself (the group disappears once flatten runs; `format!("Object
    /// {k}")` handles never have a donor).
    donor_id: Option<u32>,
    editor_pos: Option<(f32, f32)>,
}

/// One scope's worth of migration: `nodes`/`wires` are either a def's root
/// lists or one group body's — structurally identical, so the same walk
/// handles both ("at any group depth", D5). Wires never cross a scope
/// boundary in this schema, so a `node.render_scene`'s legacy per-object
/// wires always live in the exact same `wires` list passed in here — no
/// group-interface surgery is ever needed, only a same-scope re-point.
fn migrate_scope(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    next_id: &mut u32,
    changed: &mut bool,
) {
    let render_scene_ids: Vec<u32> =
        nodes.iter().filter(|n| n.type_id == RENDER_SCENE_TYPE_ID).map(|n| n.id).collect();

    // Pass 1: plan every mint in this scope WITHOUT mutating `nodes`/`wires`
    // yet — the handle-dedup seed (pass 2) needs to know every donor id
    // before it can correctly decide what counts as "already used".
    let mut planned: Vec<PlannedObject> = Vec::new();
    for render_scene_id in render_scene_ids {
        // Group this scope's legacy wires targeting `render_scene_id` by
        // object index k. `BTreeMap` keeps mint order deterministic.
        let mut by_index: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, w) in wires.iter().enumerate() {
            if w.to_node == render_scene_id && parse_legacy_object_port(&w.to_port).is_some() {
                let (_, k) = parse_legacy_object_port(&w.to_port).unwrap();
                by_index.entry(k).or_default().push(i);
            }
        }
        if by_index.is_empty() {
            continue;
        }

        let render_scene_pos =
            nodes.iter().find(|n| n.id == render_scene_id).and_then(|n| n.editor_pos);

        for (k, wire_indices) in by_index {
            // The mesh (`vertices`) producer, if this object wired one —
            // source of both the minted handle and the midpoint position.
            let mesh_wire_from = wire_indices.iter().find_map(|&i| {
                let w = &wires[i];
                let (target, _) = parse_legacy_object_port(&w.to_port)?;
                (target == "vertices").then_some(w.from_node)
            });
            let mesh_producer = mesh_wire_from.and_then(|id| nodes.iter().find(|n| n.id == id));
            // D5: "handle = the enclosing group's name of the mesh_k
            // producer when one exists" — the producer of a `vertices`
            // wire in the common (importer/hand-built) shape IS the group
            // instance itself (its declared output port), so a producer
            // whose OWN type_id is `group` supplies its handle as the
            // object's name. A producer that is NOT a group (an ordinary
            // leaf node — the shape an already-flattened def has, e.g. the
            // fusion diagnostics' internal `resolve_output_spaces` probe,
            // where no group survives to name the object) has no
            // "enclosing group" to borrow a name from; copying its OWN
            // handle would duplicate that handle onto a new sibling node.
            // Falls through to "Object {k}" in that case.
            let donor = mesh_producer.filter(|n| n.type_id == GROUP_TYPE_ID);
            let desired_handle =
                donor.and_then(|n| n.handle.clone()).unwrap_or_else(|| format!("Object {k}"));
            let editor_pos = match (mesh_producer.and_then(|n| n.editor_pos), render_scene_pos) {
                (Some((mx, my)), Some((rx, ry))) => Some(((mx + rx) / 2.0, (my + ry) / 2.0)),
                _ => None,
            };

            planned.push(PlannedObject {
                render_scene_id,
                k,
                wire_indices,
                desired_handle,
                donor_id: donor.map(|n| n.id),
                editor_pos,
            });
        }
    }

    if !planned.is_empty() {
        *changed = true;
    }

    // Pass 2: seed the used-handle set from every node already in this
    // scope EXCEPT this pass's donors (borrowing a still-present group's
    // own name isn't a collision with itself — the group disappears once
    // flatten runs). Real collisions observed in practice: an unrelated
    // node that already holds the exact string a producer group's handle
    // would lend (AntiqueCamera.glb: the view camera's own "camera" node
    // vs. a mesh producer group ALSO named "camera" — the antique camera
    // prop), and an already-flattened scope where the producer IS the node
    // whose handle would be borrowed (ApricotWeather.json's internal
    // fusion-diagnostics probe: "Blossom Cluster A/wind_out_1" duplicated
    // onto a new sibling). `unique_name` (shared with `group_edit`'s
    // inline-flatten dedup) appends `_2`/`_3`/… on a collision, matching
    // the panel's existing "duplicate name" UX.
    let donor_ids: std::collections::BTreeSet<u32> =
        planned.iter().filter_map(|p| p.donor_id).collect();
    let mut used_handles: std::collections::BTreeSet<String> = nodes
        .iter()
        .filter(|n| !donor_ids.contains(&n.id))
        .filter_map(|n| n.handle.clone())
        .collect();

    for p in planned {
        let handle = crate::group_edit::unique_name(&p.desired_handle, &mut used_handles);

        let scene_object_id = *next_id;
        *next_id += 1;
        nodes.push(EffectGraphNode {
            id: scene_object_id,
            node_id: NodeId::new(short_id()),
            type_id: SCENE_OBJECT_TYPE_ID.to_string(),
            handle: Some(handle),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: p.editor_pos,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });

        for i in p.wire_indices {
            let (target_port, _) = parse_legacy_object_port(&wires[i].to_port)
                .expect("indices collected above all parsed successfully");
            wires[i].to_node = scene_object_id;
            wires[i].to_port = target_port.to_string();
        }

        wires.push(EffectGraphWire {
            from_node: scene_object_id,
            from_port: "object".to_string(),
            to_node: p.render_scene_id,
            to_port: format!("object_{}", p.k),
        });
    }

    for n in nodes.iter_mut() {
        if let Some(group) = &mut n.group {
            migrate_scope(&mut group.nodes, &mut group.wires, next_id, changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::GroupDef;
    use std::collections::BTreeSet;

    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(format!("n{id}")),
            type_id: type_id.to_string(),
            handle: handle.map(|h| h.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
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

    /// A minimal two-object def in the pre-P2 shape: one mesh + one
    /// material node per object, wired directly to `mesh_k`/`material_k`
    /// (no group wrapping — the "flat" shape, as opposed to the importer's
    /// group-wrapped shape covered separately below).
    fn flat_two_object_def() -> EffectGraphDef {
        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mesh0 = node(1, "node.grid_mesh", Some("Floor Mesh"));
        let mat0 = node(2, "node.pbr_material", None);
        let mesh1 = node(3, "node.grid_mesh", Some("Cube Mesh"));
        let mat1 = node(4, "node.pbr_material", None);
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, mesh0, mat0, mesh1, mat1],
            wires: vec![
                wire(1, "out", 0, "mesh_0"),
                wire(2, "out", 0, "material_0"),
                wire(3, "out", 0, "mesh_1"),
                wire(4, "out", 0, "material_1"),
            ],
        }
    }

    #[test]
    fn mints_one_scene_object_per_index_and_repoints_wires() {
        let mut def = flat_two_object_def();
        let changed = migrate_scene_object_wires(&mut def);
        assert!(changed);

        let scene_objects: Vec<&EffectGraphNode> =
            def.nodes.iter().filter(|n| n.type_id == SCENE_OBJECT_TYPE_ID).collect();
        assert_eq!(scene_objects.len(), 2, "one scene_object per object index");

        // No legacy wire families survive; each render_scene object_k port
        // is fed by exactly one scene_object's `object` output.
        for k in 0..2u32 {
            assert!(
                !def.wires.iter().any(|w| w.to_node == 0
                    && parse_legacy_object_port(&w.to_port).is_some()),
                "no legacy-shaped port name should remain wired to render_scene"
            );
            let object_wire = def
                .wires
                .iter()
                .find(|w| w.to_node == 0 && w.to_port == format!("object_{k}"))
                .unwrap_or_else(|| panic!("object_{k} must be wired"));
            let producer = def.nodes.iter().find(|n| n.id == object_wire.from_node).unwrap();
            assert_eq!(producer.type_id, SCENE_OBJECT_TYPE_ID);
            assert_eq!(object_wire.from_port, "object");
        }

        // The mesh/material wires re-point onto the new scene_object's
        // inputs, keeping their original `from_node`. The producer here is
        // a plain `node.grid_mesh` leaf, not a group instance, so it has no
        // "enclosing group" to name the object after (D5) — the handle
        // falls through to "Object {k}", NOT the leaf's own handle (which
        // would duplicate that handle onto a new sibling node — the exact
        // collision a flattened importer-shaped preset hit in practice).
        let obj0 = def
            .nodes
            .iter()
            .find(|n| n.type_id == SCENE_OBJECT_TYPE_ID && n.handle.as_deref() == Some("Object 0"))
            .expect("falls through to \"Object {k}\" for a non-group producer");
        assert!(def.wires.iter().any(|w| w.from_node == 1
            && w.to_node == obj0.id
            && w.to_port == "vertices"));
        assert!(def.wires.iter().any(|w| w.from_node == 2
            && w.to_node == obj0.id
            && w.to_port == "material"));
    }

    #[test]
    fn no_legacy_wires_is_a_pure_no_op() {
        let mut def = flat_two_object_def();
        migrate_scene_object_wires(&mut def); // first pass migrates
        let migrated_snapshot = def.clone();
        let changed_again = migrate_scene_object_wires(&mut def);
        assert!(!changed_again, "a fully-migrated def must report no change");
        assert_eq!(def, migrated_snapshot, "and must be byte-identical to before the second pass");
    }

    #[test]
    fn idempotent_across_two_full_runs() {
        let mut def = flat_two_object_def();
        assert!(migrate_scene_object_wires(&mut def));
        let after_first = def.clone();
        assert!(!migrate_scene_object_wires(&mut def));
        assert_eq!(def, after_first);
    }

    #[test]
    fn unparseable_object_port_is_left_intact() {
        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mesh = node(1, "node.grid_mesh", None);
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, mesh],
            // Not a real legacy family (no trailing integer) — must survive
            // untouched, never dropped.
            wires: vec![wire(1, "out", 0, "mesh_abc")],
        };
        let before = def.clone();
        let changed = migrate_scene_object_wires(&mut def);
        assert!(!changed, "an unparseable wire alone must not trigger a mint");
        assert_eq!(def, before, "the unparseable wire must round-trip byte-identical");
    }

    #[test]
    fn unrelated_port_on_render_scene_is_left_intact() {
        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let cam = node(1, "node.orbit_camera", None);
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, cam],
            wires: vec![wire(1, "out", 0, "camera")],
        };
        let before = def.clone();
        assert!(!migrate_scene_object_wires(&mut def));
        assert_eq!(def, before);
    }

    /// The importer shape: mesh/material live INSIDE a group whose
    /// declared outward interface exports them; the group's own output
    /// ports (not the inner nodes) are what render_scene's legacy ports
    /// wire from at root scope. Migration must mint the scene_object at
    /// ROOT scope too (same scope as render_scene and the group wire),
    /// consuming from the group's declared outputs exactly as render_scene
    /// did — no group-interior surgery.
    #[test]
    fn group_wrapped_object_mints_scene_object_at_the_same_scope_as_render_scene() {
        use crate::effect_graph_def::{GroupInterface, InterfacePortDef};

        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mut group_node = node(1, "group", Some("Floor"));
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![
                    InterfacePortDef { name: "vertices".to_string(), port_type: "Array".to_string() },
                    InterfacePortDef {
                        name: "material".to_string(),
                        port_type: "Material".to_string(),
                    },
                ],
                params: vec![],
            },
            nodes: vec![],
            wires: vec![],
            tint: None,
        }));
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, group_node],
            wires: vec![wire(1, "vertices", 0, "mesh_0"), wire(1, "material", 0, "material_0")],
        };

        assert!(migrate_scene_object_wires(&mut def));
        let scene_object = def
            .nodes
            .iter()
            .find(|n| n.type_id == SCENE_OBJECT_TYPE_ID)
            .expect("one scene_object minted");
        assert_eq!(scene_object.handle.as_deref(), Some("Floor"), "inherits the group's handle");
        assert!(def.wires.iter().any(|w| w.from_node == 1
            && w.from_port == "vertices"
            && w.to_node == scene_object.id
            && w.to_port == "vertices"));
        assert!(def.wires.iter().any(|w| w.from_node == scene_object.id
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_0"));
    }

    /// Regression (AntiqueCamera.glb, found running the P2 GPU gate): a
    /// mesh producer group can legitimately share its handle string with
    /// an UNRELATED sibling node (a glTF model of a camera, named "camera",
    /// alongside the scene's actual `node.orbit_camera` also named
    /// "camera") — minting a scene_object that borrows the group's handle
    /// verbatim collides with that sibling at `Graph::add_node_named` load
    /// time. Migration must dedupe against the WHOLE scope's existing
    /// handles (except the donor group itself, per the test above).
    #[test]
    fn minted_handle_deduped_against_an_unrelated_sibling_with_the_same_name() {
        use crate::effect_graph_def::{GroupInterface, InterfacePortDef};

        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        use crate::effect_graph_def::GROUP_OUTPUT_TYPE_ID;

        let view_camera = node(1, "node.orbit_camera", Some("camera"));
        let mut group_node = node(2, "group", Some("camera")); // the antique-camera PROP, same name
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![InterfacePortDef {
                    name: "vertices".to_string(),
                    port_type: "Array".to_string(),
                }],
                params: vec![],
            },
            nodes: vec![
                node(20, "node.grid_mesh", None),
                node(21, GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![wire(20, "out", 21, "vertices")],
            tint: None,
        }));
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, view_camera, group_node],
            wires: vec![
                wire(1, "out", 0, "camera"),
                wire(2, "vertices", 0, "mesh_0"),
            ],
        };

        assert!(migrate_scene_object_wires(&mut def));
        let scene_object = def
            .nodes
            .iter()
            .find(|n| n.type_id == SCENE_OBJECT_TYPE_ID)
            .expect("one scene_object minted");
        assert_ne!(
            scene_object.handle.as_deref(),
            Some("camera"),
            "must not collide with the unrelated view-camera node's handle"
        );
        assert_eq!(scene_object.handle.as_deref(), Some("camera_2"));
        // The donor group itself is never added to a live Graph (flatten
        // consumes it, replacing it with its — here empty — prefixed
        // children), so the ACTUAL invariant `Graph::add_node_named`
        // enforces is checked post-flatten, not on the raw migrated def
        // (which may still show the donor's un-consumed handle sitting
        // alongside the sibling it will vacate the name for).
        let flat = crate::flatten::flatten_groups(&def).expect("must flatten");
        let mut handles: Vec<&str> = flat.nodes.iter().filter_map(|n| n.handle.as_deref()).collect();
        let before = handles.len();
        handles.sort_unstable();
        handles.dedup();
        assert_eq!(handles.len(), before, "no two flattened nodes may share a handle");
    }

    /// D5 "at any group depth": a render_scene living INSIDE a group,
    /// alongside its object producers, migrates the same way — the walk
    /// must recurse into group bodies, not just the def root.
    #[test]
    fn render_scene_nested_inside_a_group_migrates_too() {
        let render = node(10, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mesh = node(11, "node.grid_mesh", Some("Inner Mesh"));
        let mat = node(12, "node.pbr_material", None);
        let mut outer = node(1, "group", Some("Scene Wrapper"));
        outer.group = Some(Box::new(GroupDef {
            interface: crate::effect_graph_def::GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![render, mesh, mat],
            wires: vec![wire(11, "out", 10, "mesh_0"), wire(12, "out", 10, "material_0")],
            tint: None,
        }));
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![outer],
            wires: vec![],
        };

        assert!(migrate_scene_object_wires(&mut def));
        let group = &def.nodes[0];
        let body = group.group.as_ref().unwrap();
        assert!(body.nodes.iter().any(|n| n.type_id == SCENE_OBJECT_TYPE_ID));
        assert!(body.wires.iter().any(|w| w.to_port == "object_0" && w.to_node == 10));
    }

    /// Flatten-equivalence (DESIGN_DOC_STANDARD §5's migration test trio,
    /// third leg): a group-wrapped def migrates and flattens cleanly —
    /// flattening after migration must not lose or mis-wire the minted
    /// scene_object or its `object_0` wire into render_scene, the same
    /// verification recipe GROUPING_GRAPHS.md prescribes for any grouping
    /// change.
    #[test]
    fn migrated_grouped_def_flattens_with_the_scene_object_wire_intact() {
        use crate::effect_graph_def::{GROUP_OUTPUT_TYPE_ID, GroupInterface, InterfacePortDef};
        use crate::flatten::flatten_groups;

        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mut group_node = node(1, "group", Some("Floor"));
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![
                    InterfacePortDef { name: "vertices".to_string(), port_type: "Array".to_string() },
                    InterfacePortDef {
                        name: "material".to_string(),
                        port_type: "Material".to_string(),
                    },
                ],
                params: vec![],
            },
            nodes: vec![
                node(2, "node.grid_mesh", None),
                node(3, "node.pbr_material", None),
                node(4, GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![wire(2, "out", 4, "vertices"), wire(3, "out", 4, "material")],
            tint: None,
        }));
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, group_node],
            wires: vec![wire(1, "vertices", 0, "mesh_0"), wire(1, "material", 0, "material_0")],
        };

        assert!(migrate_scene_object_wires(&mut def));
        let flat = flatten_groups(&def).expect("migrated grouped def must flatten");
        assert!(
            !flat.nodes.iter().any(|n| n.group.is_some()),
            "flattening must remove every group node"
        );
        assert!(
            flat.nodes.iter().any(|n| n.type_id == SCENE_OBJECT_TYPE_ID),
            "the minted scene_object must survive flattening"
        );
        assert!(
            flat.wires.iter().any(|w| w.to_node == 0 && w.to_port == "object_0"),
            "render_scene's object_0 wire must survive flattening"
        );
    }

    #[test]
    fn fresh_ids_never_collide_with_a_group_bodys_ids() {
        // The def's root max is small (2), but a group body's inner id (99)
        // is larger — the fresh scene_object id must clear BOTH scopes'
        // maxima, not just the root's.
        let render = node(0, RENDER_SCENE_TYPE_ID, Some("scene"));
        let mesh = node(2, "node.grid_mesh", None);
        let mut sibling_group = node(1, "group", Some("Other"));
        sibling_group.group = Some(Box::new(GroupDef {
            interface: crate::effect_graph_def::GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![node(99, "node.value", None)],
            wires: vec![],
            tint: None,
        }));
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, mesh, sibling_group],
            wires: vec![wire(2, "out", 0, "mesh_0")],
        };
        migrate_scene_object_wires(&mut def);
        let minted = def.nodes.iter().find(|n| n.type_id == SCENE_OBJECT_TYPE_ID).unwrap();
        assert!(minted.id > 99, "fresh id must clear every scope's max id, not just the root's");
    }
}
