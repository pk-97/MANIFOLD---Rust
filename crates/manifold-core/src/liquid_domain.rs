//! Which graph nodes are liquid domains. The scene layer asks this one
//! predicate instead of comparing type ids, so every liquid solver is found by
//! the same walk. No other file writes a domain type-id literal.

use std::collections::BTreeSet;

use crate::NodeId;
use crate::effect_graph_def::{EffectGraphNode, GROUP_TYPE_ID};
use crate::scene_index::{FlatSceneIndex, SceneIndexError};

/// The FLIP liquid domain.
pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface";
/// The GPU MLS-MPM liquid domain.
pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain";
/// The GPU FLIP liquid domain: PIC/FLIP particles on a face grid with a
/// multigrid-preconditioned pressure solve.
pub const GPU_FLIP_DOMAIN_TYPE_ID: &str = "node.gpu_flip_domain";

/// Every liquid domain type.
pub const LIQUID_DOMAIN_TYPE_IDS: &[&str] = &[
    FLIP_DOMAIN_TYPE_ID,
    MATTER_DOMAIN_TYPE_ID,
    GPU_FLIP_DOMAIN_TYPE_ID,
];

/// The inputs a Water's step takes a Sand on: its particles this frame and
/// their count. The sand slows the water there; it is not part of the
/// water's surface, so the surface walk never follows them.
pub const SAND_INPUTS: [&str; 2] = ["sand", "sand_count"];

/// The ports a material stepping a World tick after the first Sand reads
/// its grid through (a Water's step, a Smoke or Fire's): the grid's nodes,
/// the bulk its Points to Grid lays, its table, pool and cell. The sand is
/// not part of the reader's domain.
pub const SAND_GRID: [&str; 5] = [
    "sand_grid",
    "sand_bulk",
    "sand_tiles",
    "sand_pool_tiles",
    "sand_cell",
];

/// Most Matter items (Sands) one solver holds: its material table's rows.
/// A scene runs one Matter solver for each group of this many.
pub const MAX_MATTER_SLOTS: usize = 8;

/// Most Waters one liquid solver holds: each particle carries its Water's
/// index in the low bits of its radius. A scene's Boundless Waters share one
/// solver for each group of this many.
pub const MAX_WATERS: u32 = 8;

/// The node a Water lifts and drags a Sand through, once a tick inside the
/// Sand's tick. These ports read the water's step as its last tick left it
/// (state-capture inputs), and `cell_size` the water's cell. The water is
/// not part of the sand's surface either.
pub const WATER_PUSH_NODE: &str = "node.matter_water_push";
pub const WATER_PUSH_STEP: [&str; 4] = ["faces", "distance", "tiles", "pool_tiles"];

/// The ports a World's water step and Matter's Points to Grid read each
/// other's world-tile table through, so both tables share one key set
/// (`world_tiles::SharedTable`): the table and its pool. The other material
/// is not part of this one's surface.
pub const SHARED_TABLE: [&str; 2] = ["shared_tiles", "shared_pool_tiles"];

/// How fast sand slows the water inside it, per second at a full cell of
/// sand: water seeps through packed sand at about g / 40 (0.25 m/s), so it
/// soaks into a pile over seconds instead of passing through it. The sand
/// takes the same push back, scaled by water's density over the sand's.
pub const SAND_WATER_DRAG: f32 = 40.0;

pub fn is_liquid_domain(type_id: &str) -> bool {
    LIQUID_DOMAIN_TYPE_IDS.contains(&type_id)
}

/// A liquid domain found under some graph level: the doc ids of the groups
/// entered to reach it, outermost first, and its own doc id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedLiquidDomain {
    pub groups: Vec<u32>,
    pub node: u32,
}

/// Every liquid domain in `nodes` and in the groups nested under them. A GPU
/// liquid keeps its domain inside a group (Live Matter), so a direct-child
/// check misses it.
pub fn liquid_domains_in(nodes: &[EffectGraphNode]) -> Vec<NestedLiquidDomain> {
    fn walk(nodes: &[EffectGraphNode], groups: &mut Vec<u32>, out: &mut Vec<NestedLiquidDomain>) {
        for node in nodes {
            if is_liquid_domain(&node.type_id) {
                out.push(NestedLiquidDomain {
                    groups: groups.clone(),
                    node: node.id,
                });
            }
            if node.type_id == GROUP_TYPE_ID
                && let Some(body) = node.group.as_deref()
            {
                groups.push(node.id);
                walk(&body.nodes, groups, out);
                groups.pop();
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut Vec::new(), &mut out);
    out
}

/// The liquid domain an object's surface is built from. Water is recognised
/// by its particle producer, not by the port that feeds the object: the walk
/// starts at the object's `field` producer (a level set the renderer marches),
/// or its `vertices` producer when no field is wired, and at its `instances`
/// producer, follows every wired input upstream and stops at each liquid
/// domain it meets. A field surface (particles → sort → blobs → volume →
/// smoothing), a mesh drawn from the particles, and particles or grains drawn
/// as copies of a fixed mesh all reach their domain. A Sand wired into a
/// Water's step ([`SAND_INPUTS`], [`SAND_GRID`]) is not the water's, nor a Water pushing a
/// Sand ([`WATER_PUSH_NODE`]) the sand's, nor the other material's table a
/// solver shares its keys with ([`SHARED_TABLE`]).
pub fn liquid_domain_of(
    index: &FlatSceneIndex,
    object: &NodeId,
) -> Result<Option<NodeId>, SceneIndexError> {
    let unsupported = |path: String, detail: &str| SceneIndexError::Unsupported {
        path,
        detail: detail.into(),
    };
    let port = if index.input(object, "field")?.is_some() {
        "field"
    } else {
        "vertices"
    };
    let mut pending = Vec::new();
    for port in [port, "instances"] {
        if let Some(wire) = index.input(object, port)? {
            pending.push(wire.from_node);
        }
    }
    if pending.is_empty() {
        return Ok(None);
    }
    let mut seen: BTreeSet<_> = pending.iter().copied().collect();
    let mut domains = BTreeSet::new();
    while let Some(id) = pending.pop() {
        let Some(node) = index.flat.nodes.iter().find(|node| node.id == id) else {
            return Err(unsupported(
                format!("{object:?}.{port}"),
                "surface chain names a missing producer",
            ));
        };
        if is_liquid_domain(&node.type_id) {
            let reference = index.by_id.get(&id).ok_or_else(|| {
                unsupported(
                    format!("{object:?}.{port}"),
                    "liquid domain has no stable scene reference",
                )
            })?;
            domains.insert(reference.clone());
            continue;
        }
        for wire in index.flat.wires.iter().filter(|wire| {
            wire.to_node == id
                && !SAND_INPUTS.contains(&wire.to_port.as_str())
                && !SAND_GRID.contains(&wire.to_port.as_str())
                && !SHARED_TABLE.contains(&wire.to_port.as_str())
                && !(node.type_id == WATER_PUSH_NODE
                    && (WATER_PUSH_STEP.contains(&wire.to_port.as_str())
                        || wire.to_port == "cell_size"))
        }) {
            if seen.insert(wire.from_node) {
                pending.push(wire.from_node);
            }
        }
    }
    let domain = domains.pop_first();
    if !domains.is_empty() {
        return Err(unsupported(
            format!("{object:?}"),
            "scene object's surface is built from more than one liquid domain",
        ));
    }
    Ok(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_domain_predicate_covers_every_solver() {
        assert!(is_liquid_domain(FLIP_DOMAIN_TYPE_ID));
        assert!(is_liquid_domain(MATTER_DOMAIN_TYPE_ID));
        assert!(is_liquid_domain(GPU_FLIP_DOMAIN_TYPE_ID));
        assert!(!is_liquid_domain("node.physics_world"));
        assert!(!is_liquid_domain("node.matter_state"));
    }

    #[test]
    fn liquid_domains_in_finds_nested_gpu_domain() {
        use crate::effect_graph_def::{GroupDef, GroupInterface};
        let node = |id: u32, type_id: &str| -> EffectGraphNode {
            serde_json::from_value(
                serde_json::json!({"id": id, "nodeId": format!("n{id}"), "typeId": type_id}),
            )
            .expect("node parses")
        };
        let group = |id: u32, nodes: Vec<EffectGraphNode>| {
            let mut group = node(id, GROUP_TYPE_ID);
            group.group = Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: vec![],
                    outputs: vec![],
                    params: vec![],
                },
                nodes,
                wires: vec![],
                tint: None,
            }));
            group
        };
        let fluid_group = group(
            1,
            vec![
                group(2, vec![node(3, MATTER_DOMAIN_TYPE_ID)]),
                node(4, "node.value"),
            ],
        );
        assert_eq!(
            liquid_domains_in(&[fluid_group, node(5, FLIP_DOMAIN_TYPE_ID)]),
            vec![
                NestedLiquidDomain {
                    groups: vec![1, 2],
                    node: 3
                },
                NestedLiquidDomain {
                    groups: vec![],
                    node: 5
                },
            ]
        );
    }
}
