//! The whitewater atoms on world tiles as the planner sees them: where the
//! emitter chain fuses over a pool of tiles. No GPU.

use serde_json::{Value, json};

use manifold_node_engine::ports::ArrayType;
use manifold_node_engine::scene::world_tiles::{pool_len, table_words};
use manifold_water_liquid::fluid_particles::FaceSample;

/// Particle slots of the chain: more than any res-64 solver holds (8 per
/// cell over the whole 64³ tank).
const PARTICLE_SLOTS: u32 = 8 * 64 * 64 * 64;

/// Tiles in the chain's pool: not a cube, not a power of two.
const POOL_TILES: u32 = 37;

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn int(v: u64) -> Value {
    json!({"type": "Int", "value": v})
}

/// The emitter chain over the step's pooled grid, as a graph: distance,
/// solid, faces and the table from test sources, cells and the extended
/// curvature from the grid atoms, and the counts to a sink.
fn emitter_chain_def() -> (
    manifold_core::effect_graph_def::EffectGraphDef,
    Vec<&'static str>,
) {
    let h = float(4.0 / 64.0);
    let grid = json!({"cell_size": h, "pool_tiles": float(f64::from(POOL_TILES))});
    let elements = pool_len(POOL_TILES);
    let names_and_types: Vec<(&'static str, &str, Value)> = vec![
        (
            "distance",
            "test.value_source",
            json!({"max_capacity": int(elements)}),
        ),
        (
            "solid",
            "test.value_source",
            json!({"max_capacity": int(elements)}),
        ),
        (
            "faces",
            "test.face_source",
            json!({"max_capacity": int(elements)}),
        ),
        (
            "tiles",
            "test.word_source",
            json!({"max_capacity": int(table_words(POOL_TILES))}),
        ),
        (
            "particles",
            "test.liquid_source",
            json!({"max_capacity": int(u64::from(PARTICLE_SLOTS))}),
        ),
        ("kinds", "node.liquid_cells", grid.clone()),
        ("curvature", "node.lattice_curvature", grid.clone()),
        ("extend1", "node.extend_lattice", grid.clone()),
        ("extend2", "node.extend_lattice", grid.clone()),
        ("extend3", "node.extend_lattice", grid.clone()),
        ("jitter", "node.jitter_particles", json!({"cell_size": h})),
        ("sample", "node.sample_faces_at_particles", grid.clone()),
        ("energy", "node.energy_potential", json!({})),
        ("wavecrest", "node.wavecrest_potential", grid),
        ("counts", "node.emission_count", json!({})),
        ("sink", "test.count_sink", json!({})),
        ("output", "system.final_output", json!({})),
    ];
    let id = |name: &str| {
        names_and_types
            .iter()
            .position(|(n, _, _)| *n == name)
            .expect("node")
    };
    let nodes_json: Vec<Value> = names_and_types
        .iter()
        .enumerate()
        .map(|(i, (name, type_id, params))| json!({"id": i, "nodeId": name, "typeId": type_id, "params": params}))
        .collect();
    let wires: Vec<Value> = [
        ("distance", "out", "kinds", "distance"),
        ("solid", "out", "kinds", "solid"),
        ("tiles", "out", "kinds", "tiles"),
        ("distance", "out", "curvature", "distance"),
        ("tiles", "out", "curvature", "tiles"),
        ("curvature", "out", "extend1", "values"),
        ("extend1", "out", "extend2", "values"),
        ("extend2", "out", "extend3", "values"),
        ("tiles", "out", "extend1", "tiles"),
        ("tiles", "out", "extend2", "tiles"),
        ("tiles", "out", "extend3", "tiles"),
        ("particles", "out", "jitter", "particles"),
        ("jitter", "out", "sample", "particles"),
        ("faces", "out", "sample", "faces"),
        ("tiles", "out", "sample", "tiles"),
        ("sample", "out", "energy", "particles"),
        ("sample", "out", "wavecrest", "particles"),
        ("distance", "out", "wavecrest", "distance"),
        ("extend3", "out", "wavecrest", "curvature"),
        ("kinds", "out", "wavecrest", "cells"),
        ("tiles", "out", "wavecrest", "tiles"),
        ("sample", "out", "counts", "particles"),
        ("energy", "out", "counts", "energy"),
        ("wavecrest", "out", "counts", "wavecrest"),
        ("counts", "out", "sink", "values"),
        ("sink", "out", "output", "in"),
    ]
    .into_iter()
    .map(|(from, from_port, to, to_port)| json!({"fromNode": id(from), "fromPort": from_port, "toNode": id(to), "toPort": to_port}))
    .collect();
    let def = serde_json::from_value(json!({"version": 3, "nodes": nodes_json, "wires": wires}))
        .expect("whitewater chain def");
    (
        def,
        names_and_types.iter().map(|(name, _, _)| *name).collect(),
    )
}

/// Where the emitter chain fuses over a pool of tiles: with the counts its
/// only way out, jitter to count fold into one kernel, the grid fields and
/// faces gathered through the one table every member reads, so their shared
/// tile helpers collapse to one copy. The GPU proof of that kernel is
/// `whitewater_emitter_chain_fused_matches_unfused`.
#[test]
fn whitewater_emitter_chain_fuses() {
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    manifold_node_engine::testkit::substep_nodes::register_array_source(
        &mut registry,
        "test.face_source",
        ArrayType::of_known::<FaceSample>(),
    );
    let (def, names) = emitter_chain_def();
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    assert!(
        report.preparation_error.is_none(),
        "{:?}",
        report.preparation_error
    );
    let name = |id: u32| names[id as usize];
    let emitter = ["jitter", "sample", "energy", "wavecrest", "counts"];
    let rows: Vec<_> = report
        .nodes
        .iter()
        .filter(|n| emitter.contains(&name(n.node_id)))
        .map(|n| {
            (
                name(n.node_id),
                n.kind.as_str(),
                n.fused,
                n.region_index,
                n.cut_reason.as_deref(),
            )
        })
        .collect();
    let regions: Vec<Vec<&str>> = report
        .regions
        .iter()
        .map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect())
        .collect();
    let mut chain: Vec<&str> = regions
        .iter()
        .find(|members| members.contains(&"counts"))
        .unwrap_or_else(|| panic!("the counts fuse: {rows:#?}\n{regions:?}"))
        .clone();
    chain.sort_unstable();
    assert_eq!(
        chain,
        ["counts", "energy", "jitter", "sample", "wavecrest"],
        "{rows:#?}"
    );
}
