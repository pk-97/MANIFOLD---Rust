//! Which graph nodes are liquid domains. The scene layer asks this one
//! predicate instead of comparing type ids, so every liquid solver is found by
//! the same walk (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.5 (Scene
//! recognition)). No other file writes a domain type-id literal.

use std::collections::BTreeSet;

use crate::SceneNodeRef;
use crate::effect_graph_def::{EffectGraphNode, GROUP_TYPE_ID};
use crate::scene_index::{FlatSceneIndex, SceneIndexError};

/// The FLIP liquid domain.
pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface";
/// The GPU MLS-MPM liquid domain.
pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain";
/// The GPU FLIP liquid domain: PIC/FLIP particles on a face grid with a
/// multigrid-preconditioned pressure solve (`docs/GPU_FLIP_PRESSURE_SOLVE.md`).
pub const GPU_FLIP_DOMAIN_TYPE_ID: &str = "node.gpu_flip_domain";

/// Every liquid domain type.
pub const LIQUID_DOMAIN_TYPE_IDS: &[&str] = &[FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID, GPU_FLIP_DOMAIN_TYPE_ID];

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
                out.push(NestedLiquidDomain { groups: groups.clone(), node: node.id });
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

/// Water-panel params per domain type, under FLIP's names where the meaning is
/// shared (`docs/GPU_MPM_SOLVER_DESIGN.md` D17).
pub const LIQUID_DIAL_PARAMS: &[(&str, &[&str])] = &[
    (
        FLIP_DOMAIN_TYPE_ID,
        &[
            "seed", "domain_size", "fill_height", "liquid_density", "viscosity", "surface_tension",
            "gravity_x", "gravity", "gravity_z",
            "emission", "inflow_speed", "speed", "reset", "surface_subdivisions",
            "surface_particle_scale", "surface_smoothing", "surface_smoothing_iterations",
            "resolution", "grid_budget_mcells", "transfer", "whitewater", "whitewater_capacity",
            "whitewater_wavecrest_rate", "whitewater_turbulence_rate",
            "whitewater_min_energy", "whitewater_max_energy",
            "closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y",
            "closed_neg_z", "closed_pos_z",
        ],
    ),
    (
        MATTER_DOMAIN_TYPE_ID,
        &[
            "seed", "domain_size", "fill_height",
            "gravity_x", "gravity", "gravity_z",
            "speed", "reset",
            "resolution", "grid_budget_mcells",
            "closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y",
            "closed_neg_z", "closed_pos_z",
            "points_per_cell", "stiffness", "cohesion", "liveliness",
        ],
    ),
    (
        GPU_FLIP_DOMAIN_TYPE_ID,
        &[
            "domain_size", "fill_height",
            "gravity_x", "gravity", "gravity_z",
            "speed", "reset",
            "resolution",
        ],
    ),
];

/// The liquid domain an object's surface is built from. Water is recognised
/// by its particle producer, not by the port that feeds the object: the walk
/// starts at the object's `vertices` producer, follows every wired input
/// upstream and stops at each liquid domain it meets. A CPU surface
/// (`fluid_surface.vertices`) and a GPU surface (particles → sort → blobs →
/// volume → marching cubes) both reach their domain.
pub fn liquid_domain_of(
    index: &FlatSceneIndex,
    object: &SceneNodeRef,
) -> Result<Option<SceneNodeRef>, SceneIndexError> {
    let unsupported = |path: String, detail: &str| SceneIndexError::Unsupported {
        path,
        detail: detail.into(),
    };
    let Some(wire) = index.input(object, "vertices")? else {
        return Ok(None);
    };
    let mut pending = vec![wire.from_node];
    let mut seen = BTreeSet::from([wire.from_node]);
    let mut domains = BTreeSet::new();
    while let Some(id) = pending.pop() {
        let Some(node) = index.flat.nodes.iter().find(|node| node.id == id) else {
            return Err(unsupported(
                format!("{object:?}.vertices"),
                "surface chain names a missing producer",
            ));
        };
        if is_liquid_domain(&node.type_id) {
            let reference = index.by_id.get(&id).ok_or_else(|| {
                unsupported(
                    format!("{object:?}.vertices"),
                    "liquid domain has no stable scene reference",
                )
            })?;
            domains.insert(reference.clone());
            continue;
        }
        for wire in index.flat.wires.iter().filter(|wire| wire.to_node == id) {
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

/// The water-panel params of `type_id`, or `None` when it is not a liquid
/// domain.
pub fn liquid_dial_params(type_id: &str) -> Option<&'static [&'static str]> {
    LIQUID_DIAL_PARAMS
        .iter()
        .find(|(domain, _)| *domain == type_id)
        .map(|(_, params)| *params)
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
            serde_json::from_value(serde_json::json!({"id": id, "nodeId": format!("n{id}"), "typeId": type_id}))
                .expect("node parses")
        };
        let group = |id: u32, nodes: Vec<EffectGraphNode>| {
            let mut group = node(id, GROUP_TYPE_ID);
            group.group = Some(Box::new(GroupDef {
                interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
                nodes,
                wires: vec![],
                tint: None,
            }));
            group
        };
        let fluid_group = group(1, vec![group(2, vec![node(3, MATTER_DOMAIN_TYPE_ID)]), node(4, "node.value")]);
        assert_eq!(
            liquid_domains_in(&[fluid_group, node(5, FLIP_DOMAIN_TYPE_ID)]),
            vec![
                NestedLiquidDomain { groups: vec![1, 2], node: 3 },
                NestedLiquidDomain { groups: vec![], node: 5 },
            ]
        );
    }

    #[test]
    fn liquid_dial_params_cover_every_domain() {
        for type_id in LIQUID_DOMAIN_TYPE_IDS {
            assert!(
                liquid_dial_params(type_id).is_some_and(|params| !params.is_empty()),
                "{type_id} has no water-panel row in LIQUID_DIAL_PARAMS"
            );
        }
        for (type_id, _) in LIQUID_DIAL_PARAMS {
            assert!(is_liquid_domain(type_id), "{type_id} has dials but is not a liquid domain");
        }
    }

    /// I1: the literals of `LIQUID_DOMAIN_TYPE_IDS` live only here, in the
    /// load-time rename table, and in each domain's own primitive file.
    #[test]
    fn liquid_type_ids_live_in_one_place() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory");
        let allowed = [
            "manifold-core/src/liquid_domain.rs",
            "manifold-core/src/type_id_migration.rs",
            "manifold-renderer/src/node_graph/primitives/fluid_surface.rs",
            "manifold-renderer/src/node_graph/primitives/matter_domain.rs",
            "manifold-renderer/src/node_graph/primitives/gpu_flip_domain.rs",
        ];
        let patterns: Vec<String> = LIQUID_DOMAIN_TYPE_IDS
            .iter()
            .flat_map(|id| [format!("\"{id}\""), format!("\\\"{id}\\\"")])
            .collect();
        let mut offenders = Vec::new();
        let mut stack = vec![crates.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("readable crates tree") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name != "target") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(crates)
                    .expect("under crates")
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowed.contains(&relative.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable source");
                for (number, line) in text.lines().enumerate() {
                    if patterns.iter().any(|pattern| line.contains(pattern.as_str())) {
                        offenders.push(format!("{relative}:{}", number + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "liquid domain type-id literals outside manifold_core::liquid_domain: {offenders:#?}"
        );
    }
}
