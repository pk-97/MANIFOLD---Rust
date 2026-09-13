use std::collections::{BTreeMap, BTreeSet, HashSet};

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
};
use manifold_core::flatten::flatten_groups;
use manifold_core::{NodeId, SceneNodeRef};

use super::SceneModifierExpandError;

const MAX_AUTHORED_NODES: usize = 65_536;
const MAX_AUTHORED_WIRES: usize = 262_144;
const MAX_GROUP_DEPTH: usize = 64;

pub(super) struct FlatSceneIndex {
    pub(super) flat: EffectGraphDef,
    pub(super) by_ref: BTreeMap<SceneNodeRef, u32>,
    pub(super) by_id: BTreeMap<u32, SceneNodeRef>,
}

impl FlatSceneIndex {
    pub(super) fn build(owner: &EffectGraphDef) -> Result<Self, SceneModifierExpandError> {
        let mut counts = Counts::default();
        let mut leaves = Vec::new();
        inspect_scope(&owner.nodes, &owner.wires, &[], 0, &mut counts, &mut leaves)?;

        let scratch = EffectGraphDef {
            version: owner.version,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: owner.nodes.clone(),
            wires: owner.wires.clone(),
        };
        let flat = flatten_groups(&scratch).map_err(|error| invalid("graph", error.to_string()))?;

        let mut by_ref = BTreeMap::new();
        let mut by_id = BTreeMap::new();
        let mut flat_ids = BTreeMap::new();
        for node in &flat.nodes {
            if !node.node_id.is_empty() && flat_ids.insert(node.node_id.as_str(), node.id).is_some()
            {
                return Err(SceneModifierExpandError::DuplicateIdentity {
                    path: node.node_id.to_string(),
                    detail: "stable leaf IDs must be unique across the owning graph for runtime bindings".into(),
                });
            }
        }
        for leaf in leaves {
            let Some(&flat_id) = flat_ids.get(leaf.node_id.as_str()) else {
                return Err(missing_target(
                    &leaf.reference,
                    "leaf is missing from the flattened graph",
                ));
            };
            if by_ref.insert(leaf.reference.clone(), flat_id).is_some() {
                return Err(duplicate(
                    &leaf.reference,
                    "stable path resolves more than once",
                ));
            }
            by_id.insert(flat_id, leaf.reference);
        }

        Ok(Self {
            flat,
            by_ref,
            by_id,
        })
    }

    pub(super) fn node(
        &self,
        reference: &SceneNodeRef,
    ) -> Result<&EffectGraphNode, SceneModifierExpandError> {
        let id = self.flat_id(reference)?;
        self.flat
            .nodes
            .iter()
            .find(|node| node.id == id)
            .ok_or_else(|| missing_target(reference, "flattened node is missing"))
    }

    pub(super) fn input(
        &self,
        reference: &SceneNodeRef,
        port: &str,
    ) -> Result<Option<&EffectGraphWire>, SceneModifierExpandError> {
        let target_id = self.flat_id(reference)?;
        let mut incoming = self
            .flat
            .wires
            .iter()
            .filter(|wire| wire.to_node == target_id && wire.to_port == port);
        let Some(wire) = incoming.next() else {
            return Ok(None);
        };
        if incoming.next().is_some() {
            return Err(SceneModifierExpandError::ConflictingSource {
                path: reference_path(reference),
                detail: format!("more than one wire feeds port '{port}'"),
            });
        }
        if !self.has_node(wire.from_node) || !self.has_node(wire.to_node) {
            return Err(SceneModifierExpandError::MissingInput {
                path: reference_path(reference),
                detail: format!("wire for port '{port}' has a missing endpoint"),
            });
        }
        Ok(Some(wire))
    }

    pub(super) fn scene_objects(
        &self,
        reference: &SceneNodeRef,
    ) -> Result<Vec<SceneNodeRef>, SceneModifierExpandError> {
        let scene_id = self.flat_id(reference)?;
        let scene = self
            .flat
            .nodes
            .iter()
            .find(|node| node.id == scene_id)
            .ok_or_else(|| missing_target(reference, "render scene is missing"))?;
        if scene.type_id != "node.render_scene" {
            return Err(SceneModifierExpandError::MissingScene {
                path: reference_path(reference),
                detail: format!("target is '{}', not node.render_scene", scene.type_id),
            });
        }

        let mut objects = BTreeSet::new();
        let mut ports = BTreeSet::new();
        for wire in self
            .flat
            .wires
            .iter()
            .filter(|wire| wire.to_node == scene_id && wire.to_port.starts_with("object_"))
        {
            if !ports.insert(&wire.to_port) {
                return Err(SceneModifierExpandError::ConflictingSource {
                    path: reference_path(reference),
                    detail: format!("multiple objects feed scene port '{}'", wire.to_port),
                });
            }
            if wire
                .to_port
                .strip_prefix("object_")
                .and_then(|suffix| suffix.parse::<u32>().ok())
                .is_none()
            {
                return Err(SceneModifierExpandError::MissingInput {
                    path: reference_path(reference),
                    detail: format!("invalid scene object port '{}'", wire.to_port),
                });
            }
            let producer = self
                .flat
                .nodes
                .iter()
                .find(|node| node.id == wire.from_node)
                .ok_or_else(|| SceneModifierExpandError::MissingTarget {
                    path: reference_path(reference),
                    detail: format!("object wire producer {} is missing", wire.from_node),
                })?;
            if producer.type_id != "node.scene_object" {
                return Err(SceneModifierExpandError::MissingTarget {
                    path: reference_path(reference),
                    detail: format!(
                        "object wire '{}' is produced by '{}'",
                        wire.to_port, producer.type_id
                    ),
                });
            }
            let object_ref = self.by_id.get(&producer.id).ok_or_else(|| {
                SceneModifierExpandError::MissingTarget {
                    path: reference_path(reference),
                    detail: format!("scene object {} has no stable reference", producer.id),
                }
            })?;
            objects.insert(object_ref.clone());
        }
        Ok(objects.into_iter().collect())
    }

    fn flat_id(&self, reference: &SceneNodeRef) -> Result<u32, SceneModifierExpandError> {
        self.by_ref
            .get(reference)
            .copied()
            .ok_or_else(|| missing_target(reference, "stable node reference is unknown"))
    }

    fn has_node(&self, id: u32) -> bool {
        self.flat.nodes.iter().any(|node| node.id == id)
    }
}

#[derive(Default)]
struct Counts {
    nodes: usize,
    wires: usize,
}

struct Leaf {
    reference: SceneNodeRef,
    node_id: NodeId,
}

fn inspect_scope(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    scope: &[NodeId],
    depth: usize,
    counts: &mut Counts,
    leaves: &mut Vec<Leaf>,
) -> Result<(), SceneModifierExpandError> {
    counts.nodes = counts.nodes.saturating_add(nodes.len());
    counts.wires = counts.wires.saturating_add(wires.len());
    if counts.nodes > MAX_AUTHORED_NODES {
        return Err(capacity("graph", "authored node count exceeds 65536"));
    }
    if counts.wires > MAX_AUTHORED_WIRES {
        return Err(capacity("graph", "authored wire count exceeds 262144"));
    }
    if depth > MAX_GROUP_DEPTH {
        return Err(capacity("graph", "group nesting exceeds depth 64"));
    }

    let mut doc_ids = HashSet::new();
    let mut stable_ids = HashSet::new();
    for node in nodes {
        if !doc_ids.insert(node.id) {
            return Err(duplicate(
                &SceneNodeRef {
                    scope: scope.to_vec(),
                    node: node.node_id.clone(),
                },
                &format!("document id {} is duplicated in one scope", node.id),
            ));
        }
        if !node.node_id.is_empty() && !stable_ids.insert(node.node_id.clone()) {
            return Err(duplicate(
                &SceneNodeRef {
                    scope: scope.to_vec(),
                    node: node.node_id.clone(),
                },
                "stable node id is duplicated in one scope",
            ));
        }

        // Group boundary nodes are implementation details of the authored
        // group and are folded away by flatten_groups. They are never valid
        // SceneNodeRef targets, even when an old document gave them a nodeId.
        if node.type_id == GROUP_INPUT_TYPE_ID || node.type_id == GROUP_OUTPUT_TYPE_ID {
            continue;
        }

        if let Some(group) = node.group.as_deref() {
            if node.node_id.is_empty() {
                return Err(duplicate(
                    &SceneNodeRef {
                        scope: scope.to_vec(),
                        node: node.node_id.clone(),
                    },
                    "group scope has no stable node id",
                ));
            }
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            node.handle.as_deref().ok_or_else(|| {
                invalid(
                    &reference_path(&SceneNodeRef {
                        scope: scope.to_vec(),
                        node: node.node_id.clone(),
                    }),
                    "group has no handle",
                )
            })?;
            inspect_scope(
                &group.nodes,
                &group.wires,
                &child_scope,
                depth + 1,
                counts,
                leaves,
            )?;
        } else if !node.node_id.is_empty() {
            let reference = SceneNodeRef {
                scope: scope.to_vec(),
                node: node.node_id.clone(),
            };
            leaves.push(Leaf {
                reference,
                node_id: node.node_id.clone(),
            });
        }
    }
    Ok(())
}

fn reference_path(reference: &SceneNodeRef) -> String {
    reference
        .scope
        .iter()
        .map(NodeId::as_str)
        .chain(std::iter::once(reference.node.as_str()))
        .collect::<Vec<_>>()
        .join("/")
}

fn duplicate(reference: &SceneNodeRef, detail: &str) -> SceneModifierExpandError {
    SceneModifierExpandError::DuplicateIdentity {
        path: reference_path(reference),
        detail: detail.to_string(),
    }
}

fn missing_target(reference: &SceneNodeRef, detail: &str) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: reference_path(reference),
        detail: detail.to_string(),
    }
}

fn invalid(path: &str, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: path.to_string(),
        detail: detail.into(),
    }
}

fn capacity(path: &str, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::CapacityExceeded {
        path: path.to_string(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
    ));

    fn fixture() -> EffectGraphDef {
        serde_json::from_str(FIXTURE).expect("nested multimaterial fixture must parse")
    }

    fn reference(scope: &[&str], node: &str) -> SceneNodeRef {
        SceneNodeRef {
            scope: scope.iter().map(|id| NodeId::new(*id)).collect(),
            node: NodeId::new(node),
        }
    }

    #[test]
    fn scene_modifier_expand_index_nested_multimaterial_v2() {
        let owner = fixture();
        let canonical = owner.clone();
        let index = FlatSceneIndex::build(&owner).expect("fixture should index");

        let scene = reference(&[], "scan_render");
        let objects = index
            .scene_objects(&scene)
            .expect("group outputs should route");
        assert_eq!(
            objects,
            vec![
                reference(&["scan_left_group"], "left_object"),
                reference(&["scan_right_group"], "right_object"),
            ]
        );
        assert_eq!(
            index.node(&scene).expect("scene target").type_id,
            "node.render_scene"
        );
        assert_eq!(
            owner, canonical,
            "indexing must not mutate the canonical owner"
        );
    }

    #[test]
    fn scene_modifier_expand_index_renamed_groups_and_numeric_ids_keep_identity() {
        let mut owner = fixture();
        rename_groups(&mut owner.nodes);
        let mut next_id = 1000;
        renumber_scope(&mut owner.nodes, &mut owner.wires, &mut next_id);
        let index = FlatSceneIndex::build(&owner).expect("renamed fixture should index");

        assert_eq!(
            index
                .scene_objects(&reference(&[], "scan_render"))
                .expect("renamed group outputs should route"),
            vec![
                reference(&["scan_left_group"], "left_object"),
                reference(&["scan_right_group"], "right_object"),
            ]
        );
    }

    #[test]
    fn scene_modifier_expand_index_rejects_bad_targets_duplicates_and_conflicts() {
        let index = FlatSceneIndex::build(&fixture()).expect("fixture should index");
        let missing = reference(&["missing_group"], "object");
        assert!(matches!(
            index.node(&missing),
            Err(SceneModifierExpandError::MissingTarget { .. })
        ));

        let mut duplicate = fixture();
        duplicate.nodes[1].id = duplicate.nodes[0].id;
        assert!(matches!(
            FlatSceneIndex::build(&duplicate),
            Err(SceneModifierExpandError::DuplicateIdentity { .. })
        ));

        let mut conflict = fixture();
        let camera_wire = conflict
            .wires
            .iter()
            .find(|wire| wire.to_port == "camera")
            .cloned()
            .expect("fixture camera wire");
        conflict.wires.push(camera_wire);
        let index = FlatSceneIndex::build(&conflict).expect("wire conflict is query-time");
        assert!(matches!(
            index.input(&reference(&[], "scan_render"), "camera"),
            Err(SceneModifierExpandError::ConflictingSource { .. })
        ));
    }

    fn rename_groups(nodes: &mut [EffectGraphNode]) {
        for node in nodes {
            if let Some(handle) = node.handle.as_mut()
                && node.group.is_some()
            {
                handle.push_str(" renamed");
            }
            if let Some(group) = node.group.as_mut() {
                rename_groups(&mut group.nodes);
            }
        }
    }

    fn renumber_scope(
        nodes: &mut [EffectGraphNode],
        wires: &mut [EffectGraphWire],
        next: &mut u32,
    ) {
        let mut ids = BTreeMap::new();
        for node in nodes.iter_mut() {
            let old = node.id;
            let new = *next;
            *next += 1;
            ids.insert(old, new);
            node.id = new;
            if let Some(group) = node.group.as_mut() {
                renumber_scope(&mut group.nodes, &mut group.wires, next);
            }
        }
        for wire in wires {
            wire.from_node = *ids.get(&wire.from_node).expect("wire producer id");
            wire.to_node = *ids.get(&wire.to_node).expect("wire consumer id");
        }
    }
}
