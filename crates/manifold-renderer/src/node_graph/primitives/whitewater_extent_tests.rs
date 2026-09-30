//! CPU proof that the whitewater grid atoms' arrays hold everything their
//! dispatches read and write (GPU_WHITEWATER_DESIGN.md I9, an extent proof
//! before every new GPU size), at 64: the grid half, surface crossings to
//! the extended curvature. No GPU: every size comes from the functions the
//! atoms size and dispatch with.

use serde_json::{Value, json};

use super::crossing_distance::CrossingDistance;
use super::extend_lattice::ExtendLattice;
use super::lattice_curvature::LatticeCurvature;
use super::liquid_cells::LiquidCells;
use super::nearest_crossing::NearestCrossing;
use super::particle_volume::{ParticleVolume, refined_nodes};
use super::surface_crossings::SurfaceCrossings;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::fluid::domain_layout;
use crate::node_graph::matter::{MatterLattice, lattice_nodes};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{
    KnownValue, MAX_REFINEMENT, SurfaceCrossing, cell_total, face_offset, grid_box, grid_cells, refinement,
};

/// The solid lattice both hosts publish at 64: the matter layout's.
fn lattice_at_64() -> MatterLattice {
    MatterLattice::from_layout(&domain_layout(None, 4.0, 64).expect("layout"))
}

fn grid_params(nodes: [u32; 3]) -> ParamValues {
    let mut params = ParamValues::default();
    for (name, n) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().zip(nodes) {
        params.insert(name.into(), ParamValue::Float(n as f32));
    }
    params
}

/// The last solid node any grid atom reads: the far corner of the last cell.
fn solid_last_read(nodes: [u32; 3]) -> u64 {
    let cells = grid_cells(nodes).expect("cells");
    let q = cells.map(u64::from);
    let n = nodes.map(u64::from);
    q[0] + n[0] * (q[1] + n[1] * q[2])
}

/// The last refined node surface_crossings reads: the last cell's s + 1
/// nodes a side from cell·s.
fn level_last_read(cells: [u32; 3], s: u32, level: [u32; 3]) -> u64 {
    let q = cells.map(|c| u64::from((c - 1) * s + s));
    let n = level.map(u64::from);
    q[0] + n[0] * (q[1] + n[1] * q[2])
}

/// At 64: the grid is 70³ = 343,000 cells over the 71³ solid lattice, the
/// level set 211³ at Surface Detail 3. Every chain output holds exactly the
/// cells, and every read lands inside its array at every refinement the
/// surface group allows.
#[test]
fn whitewater_extents_at_64() {
    let nodes = lattice_at_64().nodes;
    assert_eq!(nodes, [71; 3]);
    let cells = grid_cells(nodes).expect("cells");
    assert_eq!(cells, [70; 3]);
    let total = cell_total(cells);
    assert_eq!(total, 343_000);
    let solid = lattice_nodes(nodes);
    assert_eq!(solid, 357_911);
    assert!(solid_last_read(nodes) < solid, "a corner read past the solid lattice");

    for s in 1..=MAX_REFINEMENT {
        let level = refined_nodes(nodes.map(|n| n as f32), s);
        assert_eq!(refinement(nodes, level), Ok(s), "Surface Detail {s}");
        let mut scale = ParamValues::default();
        scale.insert("resolution_scale".into(), ParamValue::Float(s as f32));
        let capacity = ParticleVolume::new()
            .array_output_capacity("levelset", &scale, &[("solid", solid as u32)])
            .expect("levelset capacity");
        assert!(cell_total(level) <= u64::from(capacity), "Surface Detail {s}: the level set outgrows its array");
        assert!(level_last_read(cells, s, level) < u64::from(capacity), "Surface Detail {s}: a read past the level set");
    }
    let level = refined_nodes(nodes.map(|n| n as f32), 3);
    assert_eq!(level, [211; 3]);
    assert_eq!(cell_total(level), 9_393_931);

    let params = grid_params(nodes);
    let crossings = SurfaceCrossings::new().array_output_capacity("out", &params, &[]).expect("crossings");
    assert_eq!(u64::from(crossings), total);
    let spread = NearestCrossing::new().array_output_capacity("out", &params, &[("crossings", crossings)]).expect("nearest");
    let distance = CrossingDistance::new().array_output_capacity("out", &params, &[("crossings", spread)]).expect("distance");
    let kinds = LiquidCells::new().array_output_capacity("out", &params, &[("distance", distance)]).expect("kinds");
    let curvature = LatticeCurvature::new().array_output_capacity("out", &params, &[("distance", distance)]).expect("curvature");
    let extended = ExtendLattice::new().array_output_capacity("out", &params, &[("values", curvature)]).expect("extended");
    for (name, capacity) in [("nearest", spread), ("distance", distance), ("kinds", kinds), ("curvature", curvature), ("extended", extended)] {
        assert_eq!(u64::from(capacity), total, "{name} holds exactly the cells");
    }

    assert_eq!(total * std::mem::size_of::<SurfaceCrossing>() as u64, 10_976_000);
    assert_eq!(total * std::mem::size_of::<KnownValue>() as u64, 2_744_000);
    assert_eq!(total * 4, 1_372_000);
    assert_eq!(total.div_ceil(256), 1340, "workgroups per grid dispatch");
}

/// A level set that doesn't refine the grid by one whole number, the same on
/// every axis and 1 to 4, is a named refusal, never a guessed stride.
#[test]
fn whitewater_refuses_fractional_refinement() {
    let nodes = lattice_at_64().nodes;
    for level in [[212; 3], [211, 141, 211], [351; 3], [70; 3]] {
        let refusal = refinement(nodes, level).expect_err("refused");
        assert!(refusal.contains("is not a whole refinement of 1 to 4"), "{level:?}: {refusal}");
    }
    assert!(refinement([2, 71, 71], [4, 211, 211]).is_err(), "a grid needs three nodes a side");
    assert_eq!(grid_cells([4097, 71, 71]), None, "past the largest lattice");
}

/// I2: the seam's face grid sits centred on the whitewater grid by one whole
/// number of cells, the same on every axis (3 at 64), or its placement is a
/// named refusal, never a guessed offset.
#[test]
fn whitewater_refuses_misplaced_face_grid() {
    let lattice = lattice_at_64();
    let nodes = lattice.nodes;
    assert_eq!(face_offset(nodes, [64; 3]), Ok([3; 3]));
    for face_cells in [[63, 64, 64], [64, 64, 62], [72; 3], [0; 3]] {
        let refusal = face_offset(nodes, face_cells).expect_err("refused");
        assert!(refusal.contains("does not sit centred"), "{face_cells:?}: {refusal}");
    }
    let (origin, cell_size) = grid_box(lattice.bounds(), nodes).expect("cube cells");
    assert!((cell_size - 4.0 / 64.0).abs() < 1e-6, "{cell_size}");
    for axis in 0..3 {
        assert!((origin[axis] - lattice.min[axis]).abs() < 1e-5, "axis {axis}: {origin:?} against {:?}", lattice.min);
    }
    let mut stretched = lattice.bounds();
    stretched.scale[1] *= 1.5;
    assert!(grid_box(stretched, nodes).expect_err("refused").contains("cube cells"));
}

/// One snapshot slot at 64 holds the spawns at FLIP's default capacity, the
/// emitted count, the seam's three face arrays, the distance and the solid:
/// 9.2 MB (D6), and the lifecycle's outputs plan at that capacity.
#[test]
fn whitewater_snapshot_holds_one_frame_at_64() {
    use crate::node_graph::whitewater_handoff::SnapshotShape;
    let lattice = lattice_at_64();
    let (origin, cell_size) = grid_box(lattice.bounds(), lattice.nodes).expect("grid box");
    let shape = SnapshotShape {
        grid: manifold_fluids::WhitewaterGrid { cells: grid_cells(lattice.nodes).expect("cells"), cell_size, origin },
        face_cells: [64; 3],
        face_offset: face_offset(lattice.nodes, [64; 3]).expect("offset"),
        capacity: super::whitewater_lifecycle::DEFAULT_CAPACITY,
    };
    assert_eq!([0, 1, 2].map(|a| shape.face_bytes(a)), [266_240 * 4; 3]);
    assert_eq!(shape.level_bytes(), 343_000 * 4);
    assert_eq!(shape.solid_bytes(), 357_911 * 4);
    assert_eq!(shape.spawn_bytes(), 3_200_000);
    assert_eq!(shape.slot_bytes(), 9_198_528);
    let capacity = super::whitewater_lifecycle::WhitewaterLifecycle::new()
        .array_output_capacity("foam_particles", &ParamValues::default(), &[])
        .expect("planned");
    assert_eq!(capacity, 100_000, "copies downstream hold FLIP's whole budget");
}

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn int(v: u64) -> Value {
    json!({"type": "Int", "value": v})
}

/// The redistance chain as a graph: level set and solid from test sources,
/// crossings, three nearest passes and the distance to a sink. Returns the
/// node names in id order beside the def. The kinds and curvature atoms stay
/// out: no test sink takes their records, and a consumer that reaches no
/// output makes the partitioner refuse its producer's region.
fn grid_chain_def() -> (manifold_core::effect_graph_def::EffectGraphDef, Vec<&'static str>) {
    let nodes = lattice_at_64().nodes;
    let level = refined_nodes(nodes.map(|n| n as f32), 3);
    let grid = |extra: &[(&str, Value)]| {
        let mut params = json!({"nodes_x": float(71.0), "nodes_y": float(71.0), "nodes_z": float(71.0)});
        for (name, value) in extra {
            params[*name] = value.clone();
        }
        params
    };
    let names_and_types: Vec<(&'static str, &str, Value)> = vec![
        ("level", "test.value_source", json!({"max_capacity": int(cell_total(level))})),
        ("solid", "test.value_source", json!({"max_capacity": int(lattice_nodes(nodes))})),
        (
            "crossings",
            "node.surface_crossings",
            grid(&[("level_nodes_x", float(211.0)), ("level_nodes_y", float(211.0)), ("level_nodes_z", float(211.0))]),
        ),
        ("nearest1", "node.nearest_crossing", grid(&[])),
        ("nearest2", "node.nearest_crossing", grid(&[])),
        ("nearest3", "node.nearest_crossing", grid(&[])),
        ("distance", "node.crossing_distance", grid(&[("cell_size", float(4.0 / 64.0))])),
        ("sink", "test.value_sink", json!({})),
        ("output", "system.final_output", json!({})),
    ];
    let id = |name: &str| names_and_types.iter().position(|(n, _, _)| *n == name).expect("node");
    let nodes_json: Vec<Value> = names_and_types
        .iter()
        .enumerate()
        .map(|(i, (name, type_id, params))| json!({"id": i, "nodeId": name, "typeId": type_id, "params": params}))
        .collect();
    let wires: Vec<Value> = [
        ("level", "out", "crossings", "level_set"),
        ("solid", "out", "crossings", "solid"),
        ("crossings", "out", "nearest1", "crossings"),
        ("nearest1", "out", "nearest2", "crossings"),
        ("nearest2", "out", "nearest3", "crossings"),
        ("nearest3", "out", "distance", "crossings"),
        ("solid", "out", "distance", "solid"),
        ("distance", "out", "sink", "values"),
        ("sink", "out", "output", "in"),
    ]
    .into_iter()
    .map(|(from, from_port, to, to_port)| json!({"fromNode": id(from), "fromPort": from_port, "toNode": id(to), "toPort": to_port}))
    .collect();
    let def = serde_json::from_value(json!({"version": 3, "nodes": nodes_json, "wires": wires})).expect("grid chain def");
    (def, names_and_types.iter().map(|(name, _, _)| *name).collect())
}

/// Where the grid chain fuses. Every atom but crossing_distance gathers its
/// input, so only the last nearest pass folds into the distance that takes
/// it in a register; surface_crossings sizes its output from params and
/// never fuses (BUG-u8io, param-sized outputs never fuse). The GPU proof of
/// the folded pair is `nearest_crossing_fused_with_crossing_distance_matches_unfused`.
#[test]
fn whitewater_grid_chain_fuses_only_the_distance_pair() {
    let mut registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    crate::node_graph::substeps::test_nodes::register_substep_test_nodes(&mut registry);
    let (def, names) = grid_chain_def();
    let report = crate::node_graph::fusion_report(&def, &registry);
    assert!(report.preparation_error.is_none(), "{:?}", report.preparation_error);
    let name = |id: u32| names[id as usize];
    let whitewater: Vec<_> = report.nodes.iter().filter(|n| n.type_id.starts_with("node.")).collect();
    assert_eq!(whitewater.len(), 5, "every chain atom is in the report: {whitewater:?}");
    let cuts: Vec<_> = whitewater.iter().map(|n| (name(n.node_id), n.kind.as_str(), n.cut_reason.as_deref())).collect();
    assert_eq!(report.regions.len(), 1, "one fused region: {:?}\n{cuts:#?}", report.regions);
    let mut members: Vec<&str> = report.regions[0].member_node_ids.iter().map(|&id| name(id)).collect();
    members.sort_unstable();
    assert_eq!(members, ["distance", "nearest3"]);
    for node in whitewater.iter().filter(|n| !members.contains(&name(n.node_id))) {
        assert!(!node.fused, "{} stands alone: {node:?}", name(node.node_id));
    }
}
