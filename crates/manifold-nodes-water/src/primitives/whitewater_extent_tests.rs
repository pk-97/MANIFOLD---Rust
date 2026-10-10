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
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_core::fluid_domain::domain_layout;
use crate::liquid::lattice::LiquidLattice;
use crate::matter::lattice_nodes;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::primitive::Primitive;
use crate::whitewater::{KnownValue, MAX_REFINEMENT, SurfaceCrossing, cell_total, face_offset, grid_box, grid_cells, refinement};

/// The solid lattice both hosts publish at 64: the matter layout's.
fn lattice_at_64() -> LiquidLattice {
    LiquidLattice::from_layout(&domain_layout(None, 4.0, 64).expect("layout"))
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
    let nodes = lattice_at_64().nodes();
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

    // The tick solver at res 64 uses 67 cells / 68 nodes (the legacy
    // matter lattice above has 70 / 71). Only its pad-zero mode aliases.
    use super::whitewater_step::{DEFAULT_CAPACITY, StepShape};
    let tick = StepShape::new([68; 3], [68; 3], [67; 3], 1.0,
        Some(manifold_node_engine::scene::transform::Transform { scale: [4.1875; 3], ..Default::default() }),
        DEFAULT_CAPACITY).expect("tick grid");
    let particles = u64::from(PARTICLE_SLOTS);
    assert_eq!(tick.held_bytes(particles, true), 275_854_872,
        "67-cubed grid, particle/pool/scan/output storage and reinitialisation scratch");
    // Packed input adds three adapter-sized 68³ float arrays: 3 * 68³ * 4
    // = 3,773,184 bytes to the axis baseline, 275,854,872 + 3,773,184.
    assert_eq!(tick.unpacked_face_bytes(), 3_773_184);
    assert_eq!(tick.held_bytes(particles, true) + tick.unpacked_face_bytes(), 279_628_056);
    assert_eq!(tick.held_bytes(particles, false) - tick.held_bytes(particles, true), 2_406_104);
    assert_eq!(2 * 67u64.pow(3) * 4, 2_406_104);
    let padded = StepShape::new([68; 3], [68; 3], [63; 3], 1.0,
        Some(manifold_node_engine::scene::transform::Transform { scale: [4.1875; 3], ..Default::default() }),
        DEFAULT_CAPACITY).expect("padded tick grid");
    assert_eq!(padded.held_bytes(particles, false), padded.held_bytes(particles, true));
}

/// A level set that doesn't refine the grid by one whole number, the same on
/// every axis and 1 to 4, is a named refusal, never a guessed stride.
#[test]
fn whitewater_refuses_fractional_refinement() {
    let nodes = lattice_at_64().nodes();
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
    let nodes = lattice.nodes();
    assert_eq!(face_offset(nodes, [64; 3]), Ok([3; 3]));
    for face_cells in [[63, 64, 64], [64, 64, 62], [72; 3], [0; 3]] {
        let refusal = face_offset(nodes, face_cells).expect_err("refused");
        assert!(refusal.contains("does not sit centred"), "{face_cells:?}: {refusal}");
    }
    let (origin, cell_size) = grid_box(lattice.bounds(), nodes).expect("cube cells");
    assert!((cell_size - 4.0 / 64.0).abs() < 1e-6, "{cell_size}");
    for axis in 0..3 {
        assert!((origin[axis] - lattice.min()[axis]).abs() < 1e-5, "axis {axis}: {origin:?} against {:?}", lattice.min());
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
    use crate::whitewater_handoff::SnapshotShape;
    let lattice = lattice_at_64();
    let (origin, cell_size) = grid_box(lattice.bounds(), lattice.nodes()).expect("grid box");
    let shape = SnapshotShape {
        grid: manifold_fluids::WhitewaterGrid { cells: grid_cells(lattice.nodes()).expect("cells"), cell_size, origin },
        face_cells: [64; 3],
        face_offset: face_offset(lattice.nodes(), [64; 3]).expect("offset"),
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

/// Particle slots of the extent proofs: more than any res-64 solver holds
/// (8 per cell over the whole 64³ tank).
const PARTICLE_SLOTS: u32 = 8 * 64 * 64 * 64;

/// The particle half at 64: every emitter atom's output holds exactly the
/// particle slots, one dispatch covers them, and every face a stencil can
/// reach from a position inside the whitewater grid lands inside its face
/// array or reads FLIP's padding 0, never past the array.
#[test]
fn whitewater_particle_extents_at_64() {
    use super::emission_count::EmissionCount;
    use super::energy_potential::EnergyPotential;
    use super::jitter_particles::JitterParticles;
    use super::sample_faces_at_particles::SampleFacesAtParticles;
    use super::wavecrest_potential::WavecrestPotential;
    use super::whitewater_particle_cpu::face_index;
    use crate::liquid::grid::face_len;

    let nodes = lattice_at_64().nodes();
    let cells = grid_cells(nodes).expect("cells");
    let face_cells = [64; 3];
    let pad = face_offset(nodes, face_cells).expect("offset")[0] as i32;
    assert_eq!(pad, 3);
    let params = grid_params(nodes);
    let particles = [("particles", PARTICLE_SLOTS)];
    let one = |capacity: Option<u32>, name: &str| {
        assert_eq!(capacity, Some(PARTICLE_SLOTS), "{name} holds exactly the particle slots");
    };
    one(JitterParticles::new().array_output_capacity("out", &params, &particles), "jitter");
    let gathers = [("particles", PARTICLE_SLOTS), ("face_u", 266_240), ("face_v", 266_240), ("face_w", 266_240)];
    one(SampleFacesAtParticles::new().array_output_capacity("out", &params, &gathers), "sampled");
    one(EnergyPotential::new().array_output_capacity("out", &params, &particles), "energy");
    let fields = [("particles", PARTICLE_SLOTS), ("distance", 343_000), ("curvature", 343_000), ("cells", 343_000)];
    one(WavecrestPotential::new().array_output_capacity("out", &params, &fields), "wavecrest");
    let coincident = [("particles", PARTICLE_SLOTS), ("energy", PARTICLE_SLOTS), ("wavecrest", PARTICLE_SLOTS)];
    one(EmissionCount::new().array_output_capacity("out", &params, &coincident), "counts");
    assert_eq!(PARTICLE_SLOTS.div_ceil(256), 8192, "workgroups per particle dispatch");

    // Spawn slots are the lifecycle's capacity, whatever the particles hold.
    use super::spawn_whitewater::SpawnWhitewater;
    use super::whitewater_lifecycle::{DEFAULT_CAPACITY, MAX_CAPACITY};
    use super::whitewater_type::WhitewaterType;
    let spawn_inputs = [("offsets", PARTICLE_SLOTS), ("particles", PARTICLE_SLOTS), ("energy", PARTICLE_SLOTS), ("solid", 357_911)];
    let slots = SpawnWhitewater::new().array_output_capacity("out", &params, &spawn_inputs).expect("spawn slots");
    assert_eq!(slots, DEFAULT_CAPACITY);
    let mut largest = params.clone();
    largest.insert("capacity".into(), ParamValue::Float(MAX_CAPACITY as f32));
    assert_eq!(SpawnWhitewater::new().array_output_capacity("out", &largest, &spawn_inputs), Some(MAX_CAPACITY));
    let typed = WhitewaterType::new().array_output_capacity("out", &params, &[("spawns", slots), ("distance", 343_000), ("cells", 343_000)]);
    assert_eq!(typed, Some(slots), "types hold exactly the spawn slots");
    assert_eq!(u64::from(MAX_CAPACITY) * std::mem::size_of::<crate::fluid_particles::WhitewaterSpawn>() as u64, 8_000_000);
    const { assert!(PARTICLE_SLOTS < 16_777_216, "the emitter count is exact in an f32 scalar") };

    // A stencil's lower corner runs from −1 (half a cell below the grid's
    // first centre) to cells − 1 on the axes across the component, 0 to
    // cells − 1 along it; its upper corner one more.
    for axis in 0..3 {
        let len = face_len(face_cells, axis) as usize;
        let mut reached = 0;
        let mut last = 0;
        for z in -1..=cells[2] as i32 {
            for y in -1..=cells[1] as i32 {
                for x in -1..=cells[0] as i32 {
                    if let Some(i) = face_index([x, y, z], axis, pad, face_cells) {
                        assert!(i < len, "axis {axis}: face {i} past {len}");
                        reached += 1;
                        last = last.max(i);
                    }
                }
            }
        }
        assert_eq!(reached, len, "axis {axis}: every seam face is reachable exactly once");
        assert_eq!(last, len - 1);
    }
}

/// The emitter chain after the grid chain, as a graph: particles and faces
/// from test sources, the grid chain for distance, cells and curvature, and
/// the counts to a sink.
fn emitter_chain_def() -> (manifold_core::effect_graph_def::EffectGraphDef, Vec<&'static str>) {
    whitewater_chain_def(false)
}

/// With `spawn`, the whole chain the Whitewater group runs: the counts'
/// running total, spawn and type into the lifecycle, its foam to a sink.
/// Without, the counts go straight to a sink.
fn whitewater_chain_def(spawn: bool) -> (manifold_core::effect_graph_def::EffectGraphDef, Vec<&'static str>) {
    let nodes = lattice_at_64().nodes();
    let level = refined_nodes(nodes.map(|n| n as f32), 3);
    let grid = |extra: &[(&str, Value)]| {
        let mut params = json!({"nodes_x": float(71.0), "nodes_y": float(71.0), "nodes_z": float(71.0)});
        for (name, value) in extra {
            params[*name] = value.clone();
        }
        params
    };
    let h = float(4.0 / 64.0);
    let mut names_and_types: Vec<(&'static str, &str, Value)> = vec![
        ("level", "test.value_source", json!({"max_capacity": int(cell_total(level))})),
        ("solid", "test.value_source", json!({"max_capacity": int(lattice_nodes(nodes))})),
        ("face_u", "test.value_source", json!({"max_capacity": int(266_240)})),
        ("face_v", "test.value_source", json!({"max_capacity": int(266_240)})),
        ("face_w", "test.value_source", json!({"max_capacity": int(266_240)})),
        ("particles", "test.liquid_source", json!({"max_capacity": int(u64::from(PARTICLE_SLOTS))})),
        (
            "crossings",
            "node.surface_crossings",
            grid(&[("level_nodes_x", float(211.0)), ("level_nodes_y", float(211.0)), ("level_nodes_z", float(211.0))]),
        ),
        ("nearest1", "node.nearest_crossing", grid(&[])),
        ("nearest2", "node.nearest_crossing", grid(&[])),
        ("nearest3", "node.nearest_crossing", grid(&[])),
        ("distance", "node.crossing_distance", grid(&[("cell_size", h.clone())])),
        ("kinds", "node.liquid_cells", grid(&[])),
        ("curvature", "node.lattice_curvature", grid(&[("cell_size", h.clone())])),
        ("extend1", "node.extend_lattice", grid(&[])),
        ("extend2", "node.extend_lattice", grid(&[])),
        ("extend3", "node.extend_lattice", grid(&[])),
        ("jitter", "node.jitter_particles", json!({"cell_size": h})),
        ("sample", "node.sample_faces_at_particles", grid(&[])),
        ("energy", "node.energy_potential", json!({})),
        ("wavecrest", "node.wavecrest_potential", grid(&[])),
        ("counts", "node.emission_count", json!({})),
        ("output", "system.final_output", json!({})),
    ];
    if spawn {
        names_and_types.extend([
            ("offsets", "node.running_total", json!({})),
            ("spawn", "node.spawn_whitewater", grid(&[])),
            ("type", "node.whitewater_type", grid(&[])),
            ("bounds", "node.transform_3d", json!({})),
            (
                "lifecycle",
                "node.whitewater_lifecycle",
                json!({"grid_nodes_x": float(71.0), "grid_nodes_y": float(71.0), "grid_nodes_z": float(71.0)}),
            ),
            ("sink", "test.liquid_sink", json!({})),
        ]);
    } else {
        names_and_types.push(("sink", "test.count_sink", json!({})));
    }
    let id = |name: &str| names_and_types.iter().position(|(n, _, _)| *n == name).expect("node");
    let nodes_json: Vec<Value> = names_and_types
        .iter()
        .enumerate()
        .map(|(i, (name, type_id, params))| json!({"id": i, "nodeId": name, "typeId": type_id, "params": params}))
        .collect();
    let tail: Vec<(&str, &str, &str, &str)> = if spawn {
        vec![
            ("counts", "out", "offsets", "in"),
            ("offsets", "out", "spawn", "offsets"),
            ("sample", "out", "spawn", "particles"),
            ("energy", "out", "spawn", "energy"),
            ("face_u", "out", "spawn", "face_u"),
            ("face_v", "out", "spawn", "face_v"),
            ("face_w", "out", "spawn", "face_w"),
            ("solid", "out", "spawn", "solid"),
            ("spawn", "out", "type", "spawns"),
            ("distance", "out", "type", "distance"),
            ("kinds", "out", "type", "cells"),
            ("type", "out", "lifecycle", "spawns"),
            ("offsets", "out", "lifecycle", "offsets"),
            ("face_u", "out", "lifecycle", "face_u"),
            ("face_v", "out", "lifecycle", "face_v"),
            ("face_w", "out", "lifecycle", "face_w"),
            ("distance", "out", "lifecycle", "level"),
            ("solid", "out", "lifecycle", "solid"),
            ("bounds", "transform", "lifecycle", "grid_bounds"),
            ("lifecycle", "foam_particles", "sink", "particles"),
            ("sink", "out", "output", "in"),
        ]
    } else {
        vec![("counts", "out", "sink", "values"), ("sink", "out", "output", "in")]
    };
    let wires: Vec<Value> = [
        ("level", "out", "crossings", "level_set"),
        ("solid", "out", "crossings", "solid"),
        ("crossings", "out", "nearest1", "crossings"),
        ("nearest1", "out", "nearest2", "crossings"),
        ("nearest2", "out", "nearest3", "crossings"),
        ("nearest3", "out", "distance", "crossings"),
        ("solid", "out", "distance", "solid"),
        ("distance", "out", "kinds", "distance"),
        ("solid", "out", "kinds", "solid"),
        ("distance", "out", "curvature", "distance"),
        ("curvature", "out", "extend1", "values"),
        ("extend1", "out", "extend2", "values"),
        ("extend2", "out", "extend3", "values"),
        ("particles", "out", "jitter", "particles"),
        ("jitter", "out", "sample", "particles"),
        ("face_u", "out", "sample", "face_u"),
        ("face_v", "out", "sample", "face_v"),
        ("face_w", "out", "sample", "face_w"),
        ("sample", "out", "energy", "particles"),
        ("sample", "out", "wavecrest", "particles"),
        ("distance", "out", "wavecrest", "distance"),
        ("extend3", "out", "wavecrest", "curvature"),
        ("kinds", "out", "wavecrest", "cells"),
        ("sample", "out", "counts", "particles"),
        ("energy", "out", "counts", "energy"),
        ("wavecrest", "out", "counts", "wavecrest"),
    ]
    .into_iter()
    .chain(tail)
    .map(|(from, from_port, to, to_port)| json!({"fromNode": id(from), "fromPort": from_port, "toNode": id(to), "toPort": to_port}))
    .collect();
    let def = serde_json::from_value(json!({"version": 3, "nodes": nodes_json, "wires": wires})).expect("whitewater chain def");
    (def, names_and_types.iter().map(|(name, _, _)| *name).collect())
}

/// Where the emitter chain fuses (section 3.3: jitter to count in at most
/// two dispatches). With the counts its only way out, all five atoms fold
/// into one kernel, faces and grid fields gathered; the GPU proof of that
/// kernel is `whitewater_emitter_chain_fused_matches_unfused`.
#[test]
fn whitewater_emitter_chain_fuses() {
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    let (def, names) = emitter_chain_def();
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    assert!(report.preparation_error.is_none(), "{:?}", report.preparation_error);
    let name = |id: u32| names[id as usize];
    let emitter = ["jitter", "sample", "energy", "wavecrest", "counts"];
    let rows: Vec<_> = report
        .nodes
        .iter()
        .filter(|n| emitter.contains(&name(n.node_id)))
        .map(|n| (name(n.node_id), n.kind.as_str(), n.fused, n.region_index, n.cut_reason.as_deref()))
        .collect();
    let regions: Vec<Vec<&str>> = report
        .regions
        .iter()
        .map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect())
        .collect();
    let dispatches = rows.iter().filter(|r| !r.2).count()
        + regions.iter().filter(|members| members.iter().any(|m| emitter.contains(m))).count();
    assert!(dispatches <= 2, "the emitter chain takes {dispatches} dispatches: {rows:#?}\n{regions:?}");
    let mut chain: Vec<&str> =
        regions.iter().find(|members| members.contains(&"counts")).expect("the counts fuse").clone();
    chain.sort_unstable();
    assert_eq!(chain, ["counts", "energy", "jitter", "sample", "wavecrest"], "{rows:#?}");
}

/// Where the whole chain fuses once spawn reads the sampled particles and
/// the energy (section 3.3). Spawn and type fold into one kernel. The five
/// emitter atoms now feed two consumers outside their region (the counts'
/// running total and spawn's gathers), and a fused buffer region writes one
/// output, so the partitioner refuses the region whole and they run one by
/// one: BUG-imy3.5 (fan-out buffer regions split at the escaping output).
/// When that lands this test fails and takes the new count.
#[test]
fn whitewater_spawn_chain_fuses() {
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    let (def, names) = whitewater_chain_def(true);
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    assert!(report.preparation_error.is_none(), "{:?}", report.preparation_error);
    let name = |id: u32| names[id as usize];
    let particle_side = ["jitter", "sample", "energy", "wavecrest", "counts", "spawn", "type"];
    let rows: Vec<_> = report
        .nodes
        .iter()
        .filter(|n| particle_side.contains(&name(n.node_id)))
        .map(|n| (name(n.node_id), n.kind.as_str(), n.fused, n.region_index, n.cut_reason.as_deref()))
        .collect();
    let regions: Vec<Vec<&str>> = report
        .regions
        .iter()
        .map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect())
        .collect();
    let mut spawn_region: Vec<&str> =
        regions.iter().find(|members| members.contains(&"spawn")).expect("spawn fuses").clone();
    spawn_region.sort_unstable();
    assert_eq!(spawn_region, ["spawn", "type"], "{rows:#?}\n{regions:?}");
    let emitter = &particle_side[..5];
    let dispatches = rows.iter().filter(|r| emitter.contains(&r.0) && !r.2).count()
        + regions.iter().filter(|members| members.iter().any(|m| emitter.contains(m))).count();
    assert_eq!(dispatches, 5, "the emitter atoms run one by one until fan-out regions split: {rows:#?}\n{regions:?}");
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
    let nodes = lattice_at_64().nodes();
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
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    let (def, names) = grid_chain_def();
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
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

/// node.whitewater_step at 64, from the node's own sizing: every placement
/// rule refuses by name, a missing lattice included, never skipping the
/// per-cell cap's grid; and what it holds at FLIP's default capacity over
/// the res-64 particle slots.
#[test]
fn whitewater_step_extents_at_64() {
    use super::whitewater_step::{DEFAULT_CAPACITY, MAX_CAPACITY, StepShape};
    use crate::fluid_particles::{MAX_BINS, bin_total};
    let lattice = lattice_at_64();
    let nodes = lattice.nodes();
    let level = refined_nodes(nodes.map(|n| n as f32), 3);
    let bounds = Some(lattice.bounds());
    let step = StepShape::new(nodes, level, [64; 3], 1.0, bounds, DEFAULT_CAPACITY).expect("placed");
    assert_eq!(step.cells, [70; 3]);
    assert_eq!(step.cell_count(), 343_000);
    assert_eq!(step.bins, [70; 3], "one sort bin a cell");
    assert!(bin_total(step.bins) <= MAX_BINS);
    assert_eq!(step.population_bytes(), 3_200_000);
    // Exactly 64 face cells: three 65³ float arrays, 3 * 65³ * 4.
    assert_eq!(step.unpacked_face_bytes(), 3_295_500);
    assert_eq!(step.pool_bytes(), 4_800_000);
    assert_eq!(step.slot_scan_values(), 400_000);
    assert_eq!([0, 1, 2].map(|a| step.face_bytes(a)), [266_240 * 4; 3]);
    assert_eq!(step.level_bytes(), cell_total(level) * 4);
    assert_eq!(step.solid_bytes(), 357_911 * 4);
    let held = step.held_bytes(u64::from(PARTICLE_SLOTS), false);
    println!("whitewater_step holds {held} bytes at 64 over {PARTICLE_SLOTS} particle slots");
    // Account for the turbulence/influence fields, inside potential and
    // fourth population. The former 256 MiB assertion was a budget for the
    // incomplete three-emitter port, not a limit on FLIP's emitter features.
    // Added storage, derived independently: padded surface phi, three
    // 64³ reinitialisation arrays, ceil(64/6)³ block flags, four state words, the sweep's indirect grid
    // (16 bytes) and a zero clock plan (48 bytes).
    let engine_distance = 4 * 70u64.pow(3) + 12 * 64u64.pow(3) + 4 * 11u64.pow(3) + 16 + 16 + 48;
    assert_eq!(held, 278_878_580 + engine_distance, "all emitter and engine-distance storage is accounted for");

    let refusals = [
        (StepShape::new([0; 3], level, [64; 3], 1.0, bounds, DEFAULT_CAPACITY), "solid lattice is missing"),
        (StepShape::new(nodes, level, [64; 3], 1.0, None, DEFAULT_CAPACITY), "grid_bounds is not wired"),
        (StepShape::new(nodes, [212; 3], [64; 3], 1.0, bounds, DEFAULT_CAPACITY), "is not a whole refinement"),
        (StepShape::new(nodes, level, [63, 64, 64], 1.0, bounds, DEFAULT_CAPACITY), "does not sit centred"),
        (StepShape::new(nodes, level, [64; 3], 0.0, bounds, DEFAULT_CAPACITY), "whitewater needs at least 1"),
        (StepShape::new(nodes, level, [64; 3], 1.0, bounds, 0), "capacity 0 is outside"),
        (StepShape::new(nodes, level, [64; 3], 1.0, bounds, MAX_CAPACITY + 1), "is outside 1 to"),
    ];
    for (i, (result, phrase)) in refusals.into_iter().enumerate() {
        let refusal = result.expect_err("refused");
        assert!(refusal.contains(phrase), "refusal {i}: {refusal}");
    }
}
