//! Authored preset fixtures for the liquid conformance suite.
use manifold_nodes_water::liquid::conformance::testkit::{G, liquid_totals, matter_totals, matter_faces, set_source_param};
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::id::NodeId;
use manifold_core::liquid_domain::{MATTER_DOMAIN_TYPE_ID, GPU_FLIP_DOMAIN_TYPE_ID};

use crate::bundled_presets::bundled_preset_def;
use manifold_node_engine::particles::FluidParticle;
use manifold_nodes_water::matter::{MatterPoint, STATS_WORDS};
use manifold_nodes_water::primitives::face_grid_scenes::matter_dam_break_faces;
use manifold_nodes_water::primitives::liquid_stats::LIQUID_STATS_WORDS;
use manifold_nodes_water::primitives::matter_face_component::MATTER_FACE_VALID_LAYERS;
use manifold_nodes_water::primitives::gpu_flip_preset::{SHIPPED_PRESET, WaterScene, render_def};
use manifold_nodes_water::primitives::whitewater_step::WHITEWATER_STEP_SHADER;

use manifold_nodes_water::liquid::conformance::*;

const MPM_MOVES_ITS_OWN_BODIES: &str = "MPM moves its bodies by its own per-substep law (D7), which tracks when \
     its reaction lands; it shares the force handoff (D17) but the coupled motion law is not its prediction, so \
     there is no law to agree with until section 7 (Deferred) moves MPM onto it";

const GPU_FLIP_WALLS_IN_SOLVE: &str = "GPU FLIP's tank walls are in its pressure solve on every face by \
     design, as in the engine: an open face is a sink that removes particles, not a hole in the wall. The \
     floor's push back on a box pressing the pool is real ground reaction, so the walls absorb momentum and \
     body plus liquid momentum cannot balance; the check is valid only for a solver with no walls in the solve";

// Known bugs, each logged with its numbers; a known red comes off with its fix.
const GPU_FLIP_CORNER_LIFT: &str = "BUG-o3kj8 (GPU FLIP pushes a light box tilted into a corner down instead of \
     up): the liquid pins the box flat in the corner, so it never lifts, and Box3D and the law part on its tilted ticks";
const GPU_FLIP_FLOATS_HIGH: &str = "BUG-u8nqr (GPU FLIP floating boxes keep bobbing at rest and float about a cell \
     high): the bob and the height are the liquid's, and a box dropped 5 cm above where it should float lands near \
     its own high rest, short of the centimetre of travel the handover check needs; the handover on these boxes is exact";
const GPU_FLIP_STACK_WOBBLE: &str = "BUG-tsdw3 (GPU FLIP stack: the top box wobbles just over the rest bound): \
     each box stands on the one below and no water is lost, but the top box rests at about 1.07 cm/s RMS";
const MPM_FLOATS_LOW: &str = "BUG-28j99 (MPM floats boxes about 3.5 cm low and drifts at rest): hidden on main by \
     the old impulse handoff, which pushed floating bodies up";

const fn known(check: Check, miss: &'static str, reason: &'static str) -> KnownRed {
    KnownRed { check, miss, reason }
}

pub const LIQUID_SOLVERS: &[LiquidSolverRow] = &[
    LiquidSolverRow {
        type_id: MATTER_DOMAIN_TYPE_ID,
        fixture: matter_fixture,
        gpu: true,
        coupled: true,
        atomic_free: &[],
        atomic_free_shaders: &[],
        refusals: &[
            RefusalCase {
                what: "Resolution 256 on Dam Break Matter at the default Grid Budget",
                fixture: Fixture::DamBreak,
                edit: |def| set_type_param(def, MATTER_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 256 }),
                names: &["grid_budget_mcells", "resolution"],
            },
            RefusalCase {
                what: "Points per Cell 27 at Resolution 176 on Still Pool Matter",
                fixture: Fixture::StillPool,
                edit: |def| {
                    set_type_param(def, MATTER_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 176 });
                    set_type_param(def, MATTER_DOMAIN_TYPE_ID, "points_per_cell", SerializedParamValue::Enum { value: 1 });
                },
                names: &["resolution", "points_per_cell"],
            },
            RefusalCase {
                what: "Initial Fill Height at the top of Dam Break Matter's domain",
                fixture: Fixture::DamBreak,
                edit: |def| set_type_param(def, MATTER_DOMAIN_TYPE_ID, "fill_height", SerializedParamValue::Float { value: 4.0 }),
                names: &["fill_height"],
            },
            RefusalCase {
                what: "Dam Break Matter's initial volume turned 0.3 rad",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, MATTER_DOMAIN_TYPE_ID, "initial_volume", "rot_y", 0.3),
                names: &["initial_volume"],
            },
            RefusalCase {
                what: "Dam Break Matter's initial volume moved out of the domain",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, MATTER_DOMAIN_TYPE_ID, "initial_volume", "pos_x", 10.0),
                names: &["initial_volume"],
            },
        ],
        totals: Some(TotalsReadout {
            type_id: "node.matter_state",
            port: "stats",
            words: STATS_WORDS as usize,
            read: matter_totals,
        }),
        state: Some(StateArray {
            type_id: "node.matter_state",
            port: "out",
            record_bytes: std::mem::size_of::<MatterPoint>(),
        }),
        overflow: Some(OverflowCase {
            what: "Mesh Capacity 3 on Dam Break Matter's liquid surface",
            fixture: Fixture::DamBreak,
            edit: |def| set_type_param(def, "node.volume_surface_mesh", "max_capacity", SerializedParamValue::Int { value: 3 }),
            names: &["Mesh Capacity"],
        }),
        faces: Some(FaceSource {
            type_id: "node.matter_state",
            port: "grid",
            resample: matter_faces,
            valid_layers: MATTER_FACE_VALID_LAYERS,
            ulps: (
                1,
                "the component's f32 mean of up to four nodes is compiled with Metal's fast math, which may round a \
                 sum or the division differently from the CPU; a few faces land one unit off",
            ),
        }),
        exempt: &[(Check::HandoverAgreement, MPM_MOVES_ITS_OWN_BODIES)],
        known_red: &[
            known(Check::FloatingRest, "rms", MPM_FLOATS_LOW),
            known(Check::FloatingRest, "drift", MPM_FLOATS_LOW),
            known(Check::FloatingDraft, "draft", MPM_FLOATS_LOW),
        ],
    },
    LiquidSolverRow {
        type_id: GPU_FLIP_DOMAIN_TYPE_ID,
        fixture: gpu_flip_fixture,
        gpu: true,
        coupled: true,
        atomic_free: &[],
        atomic_free_shaders: &[("node.whitewater_step", WHITEWATER_STEP_SHADER)],
        refusals: &[
            RefusalCase {
                what: "Resolution 256 on Dam Break GPU FLIP: more particles than a count carries exactly",
                fixture: Fixture::DamBreak,
                edit: |def| set_type_param(def, GPU_FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 256 }),
                names: &["resolution", "fill_height"],
            },
            RefusalCase {
                what: "Initial Fill Height at the top of Dam Break GPU FLIP's domain",
                fixture: Fixture::DamBreak,
                edit: |def| set_type_param(def, GPU_FLIP_DOMAIN_TYPE_ID, "fill_height", SerializedParamValue::Float { value: 4.0 }),
                names: &["fill_height"],
            },
            RefusalCase {
                what: "Dam Break GPU FLIP's initial volume turned 0.3 rad",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, GPU_FLIP_DOMAIN_TYPE_ID, "initial_volume", "rot_y", 0.3),
                names: &["initial_volume"],
            },
            RefusalCase {
                what: "Dam Break GPU FLIP's initial volume moved out of the domain",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, GPU_FLIP_DOMAIN_TYPE_ID, "initial_volume", "pos_x", 10.0),
                names: &["initial_volume"],
            },
        ],
        totals: Some(TotalsReadout {
            type_id: "node.liquid_state",
            port: "stats",
            words: LIQUID_STATS_WORDS as usize,
            read: liquid_totals,
        }),
        state: Some(StateArray {
            type_id: "node.liquid_state",
            port: "out",
            record_bytes: std::mem::size_of::<FluidParticle>(),
        }),
        overflow: Some(OverflowCase {
            what: "Mesh Capacity 3 on Dam Break GPU FLIP's liquid surface",
            fixture: Fixture::DamBreak,
            edit: |def| set_type_param(def, "node.volume_surface_mesh", "max_capacity", SerializedParamValue::Int { value: 3 }),
            names: &["Mesh Capacity"],
        }),
        faces: Some(FaceSource {
            type_id: "node.liquid_state",
            port: "faces",
            resample: gpu_flip_faces,
            valid_layers: FACE_GRID_GPU_FLIP_LAYERS,
            // A gather: the published faces are the solver's projected faces.
            ulps: (0, ""),
        }),
        exempt: &[(Check::CollisionMomentum, GPU_FLIP_WALLS_IN_SOLVE)],
        known_red: &[
            known(Check::FloatingRest, "centre", GPU_FLIP_FLOATS_HIGH),
            known(Check::FloatingRest, "rms", GPU_FLIP_FLOATS_HIGH),
            known(Check::FloatingRest, "drift", GPU_FLIP_FLOATS_HIGH),
            known(Check::LiftOff, "left", GPU_FLIP_CORNER_LIFT),
            known(Check::LiftOff, "surface", GPU_FLIP_CORNER_LIFT),
            known(Check::LiftOff, "lost", GPU_FLIP_CORNER_LIFT),
            known(Check::LiftOff, "refused", GPU_FLIP_CORNER_LIFT),
            known(Check::SubmergedStack, "rms", GPU_FLIP_STACK_WOBBLE),
            known(Check::HandoverAgreement, "travel", GPU_FLIP_FLOATS_HIGH),
            known(Check::HandoverAgreement, "bound", GPU_FLIP_CORNER_LIFT),
        ],
    },
];


/// GPU FLIP's scenes in the render graph the app shows: the pool scenes at
/// the 64³ lattice its solver is built for, the box scenes at their own. A
/// scene that asks for open faces keeps them closed: its walls stay in the
/// solve either way, and only the checks that end before the box reaches the
/// liquid run on one.
fn gpu_flip_fixture(fixture: Fixture) -> Option<EffectGraphDef> {
    match fixture {
        Fixture::DamBreak => Some(bundled(SHIPPED_PRESET)),
        Fixture::StillPool => Some(render_def(WaterScene::still_pool(64))),
        Fixture::FaceGrid => Some(render_def(WaterScene::dam_break(FACE_GRID_RESOLUTION as usize).with_faces())),
        Fixture::Collision { .. }
        | Fixture::FloatingBox
        | Fixture::SubmergedBox
        | Fixture::FloatingAt { .. }
        | Fixture::Resting { .. }
        | Fixture::Stack => {
            let scene = BoxScene::of(fixture)?;
            let water = WaterScene::pool(scene.resolution as usize, f64::from(scene.domain_size), f64::from(scene.fill));
            Some(scene.test_stacked(fixture, scene.test_set(with_box(render_def(water)), GPU_FLIP_DOMAIN_TYPE_ID)))
        }
    }
}

/// The FLIP Fluids engine's own coupled tank (its gravity tests: 2.4 m at
/// 48 cells, water to 1.5 m) with a density-neutral cube at its body's
/// height, on GPU FLIP. The side by side against the engine runs here
/// because that is where the engine is proven a valid reference.
pub fn gpu_flip_engine_tank() -> (EffectGraphDef, BoxScene) {
    gpu_flip_engine_tank_moved(0.0)
}
/// `gpu_flip_engine_tank` with the cube moved `shift` metres along every
/// axis. Half a cell moves its faces from mid-cell onto the grid's nodes;
/// the engine's reaction does not depend on where they fall.
pub fn gpu_flip_engine_tank_moved(shift: f32) -> (EffectGraphDef, BoxScene) {
    let edge = 0.4;
    let scene = BoxScene {
        domain_size: 2.4,
        resolution: 48,
        fill: 1.5,
        liquid_gravity: -G,
        open_faces: false,
        centre: [shift, 0.9 + shift, shift],
        rotation: [0.0; 3],
        edge,
        mass: FIXTURE_DENSITY * edge.powi(3),
    };
    let water = WaterScene::pool(scene.resolution as usize, f64::from(scene.domain_size), f64::from(scene.fill));
    (scene.test_set(with_box(render_def(water)), GPU_FLIP_DOMAIN_TYPE_ID), scene)
}
/// The GPU FLIP Dam Break with a free box `density_ratio` times as dense as
/// the water standing where the static obstacle stands, and that obstacle
/// removed. A light box is where an explicit body reaction runs away.
pub fn gpu_flip_dam_break_with_box(density_ratio: f32) -> (EffectGraphDef, BoxScene) {
    let water = WaterScene { obstacle: false, ..WaterScene::dam_break(64) };
    let edge = 0.6;
    let scene = BoxScene {
        domain_size: water.size as f32,
        resolution: 64,
        fill: water.fill_height as f32,
        liquid_gravity: -G,
        open_faces: false,
        centre: [0.35, 0.3 + 0.5 * edge, -0.1],
        rotation: [0.0; 3],
        edge,
        mass: density_ratio * FIXTURE_DENSITY * edge.powi(3),
    };
    (scene.test_set(with_box(render_def(water)), GPU_FLIP_DOMAIN_TYPE_ID), scene)
}
/// `def` with [`BOX_PRESET`]'s box in its render scene, which pairs the
/// box's world with the scene's liquid, and the Speed card on the world too.
/// The box takes the scene's next object slot past its `objects` count, as Add
/// Object does (the editing finder is not linkable from the renderer lib).
fn with_box(mut def: EffectGraphDef) -> EffectGraphDef {
    let source = bundled(BOX_PRESET);
    let next = def.nodes.iter().map(|node| node.id).max().expect("a scene has nodes") + 1;
    let moved: Vec<(u32, u32)> = source
        .nodes
        .iter()
        .filter(|node| BOX_NODES.contains(&node.node_id.as_str()))
        .zip(next..)
        .map(|(node, id)| (node.id, id))
        .collect();
    assert_eq!(moved.len(), BOX_NODES.len(), "{BOX_PRESET} holds the box");
    let new_id = |id: u32| moved.iter().find(|(old, _)| *old == id).map(|(_, new)| *new);
    for node in &source.nodes {
        if let Some(id) = new_id(node.id) {
            def.nodes.push(EffectGraphNode { id, ..node.clone() });
        }
    }
    for wire in &source.wires {
        if let (Some(from_node), Some(to_node)) = (new_id(wire.from_node), new_id(wire.to_node)) {
            def.wires.push(EffectGraphWire { from_node, to_node, ..wire.clone() });
        }
    }
    let id_of = |def: &EffectGraphDef, name: &str| {
        def.nodes.iter().find(|node| node.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}")).id
    };
    let (object, scene) = (id_of(&def, "box_object"), id_of(&def, "scene"));
    let slot = match def.nodes.iter().find(|node| node.id == scene).and_then(|node| node.params.get("objects")) {
        Some(SerializedParamValue::Float { value }) => *value as u32,
        Some(SerializedParamValue::Int { value }) => *value as u32,
        other => panic!("{SHIPPED_PRESET}'s render scene has no object count for the box: {other:?}"),
    };
    let port = format!("object_{slot}");
    assert!(
        !def.wires.iter().any(|wire| wire.to_node == scene && wire.to_port == port),
        "{SHIPPED_PRESET}'s render scene has no free object slot for the box: {port} is taken"
    );
    def.wires.push(EffectGraphWire { from_node: object, from_port: "object".into(), to_node: scene, to_port: port });
    let render = def.nodes.iter_mut().find(|node| node.id == scene).expect("the render scene");
    render.params.insert("objects".into(), SerializedParamValue::Float { value: (slot + 1) as f32 });
    let metadata = def.preset_metadata.as_mut().expect("the render's cards");
    let speed = metadata
        .bindings
        .iter()
        .find(|binding| binding.id == "speed")
        .expect("the render's Speed card")
        .clone();
    metadata.bindings.push(BindingDef {
        target: BindingTarget::Node { node_id: NodeId::new("box_world"), param: "speed".into() },
        ..speed
    });
    def
}
fn matter_fixture(fixture: Fixture) -> Option<EffectGraphDef> {
    Some(match BoxScene::of(fixture) {
        Some(scene) => scene.test_stacked(fixture, scene.apply(bundled("WaterFloatingBoxMatter"), MATTER_DOMAIN_TYPE_ID)),
        None if fixture == Fixture::StillPool => bundled("WaterStillPoolMatter"),
        None if fixture == Fixture::FaceGrid => {
            let mut def = matter_dam_break_faces(None, false);
            let resolution = SerializedParamValue::Int { value: FACE_GRID_RESOLUTION as i32 };
            set_type_param(&mut def, MATTER_DOMAIN_TYPE_ID, "resolution", resolution);
            def
        }
        None => bundled("WaterDamBreakMatter"),
    })
}
fn bundled(id: &'static str) -> EffectGraphDef {
    bundled_preset_def(&PresetTypeId::new(id))
        .unwrap_or_else(|| panic!("no bundled preset {id}"))
        .as_ref()
        .clone()
}
/// The Floating Box preset whose box the GPU FLIP box scenes carry.
const BOX_PRESET: &str = "WaterFloatingBoxMatter";

/// The preset's Box3D world, the box's start, body and drawn object.
const BOX_NODES: [&str; 6] = ["box_world", "box_start", "box_body", "box_mesh", "box_material", "box_object"];
#[cfg(test)]
mod tests {

    use super::*;
    use manifold_node_engine::exec::extent::{ExtentError};
use manifold_nodes_water::liquid::extent::LiquidPreset;
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_core::liquid_domain::{LIQUID_DOMAIN_TYPE_IDS, is_liquid_domain};

    /// The GPU runs no matter scene above resolution 64 until BUG-gwe4
    /// (staged GPU check above res 64) closes.
    const GPU_FIXTURE_RESOLUTION: u32 = 64;



    fn build(row: &LiquidSolverRow, fixture: Fixture, def: &EffectGraphDef) -> LiquidPreset {
        LiquidPreset::build(def).unwrap_or_else(|error| panic!("{} {fixture:?}: {error}", row.type_id))
    }

    fn conformance_registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    /// I2: every registered liquid domain type has one row; every check a row does not
    /// name as exempt has its scenes, each holding the row's domain and
    /// passing the extent check as authored.
    #[test]
    fn liquid_conformance_covers_every_domain() {
        let registry = conformance_registry();
        for &type_id in LIQUID_DOMAIN_TYPE_IDS.iter().filter(|id| registry.contains(id)) {
            let rows = LIQUID_SOLVERS.iter().filter(|row| row.type_id == type_id).count();
            assert_eq!(rows, 1, "{type_id} has {rows} conformance rows");
        }
        for row in LIQUID_SOLVERS {
            assert!(is_liquid_domain(row.type_id), "{} has a row but is not a liquid domain", row.type_id);
            for (i, (check, reason)) in row.exempt.iter().enumerate() {
                assert!(!reason.trim().is_empty(), "{}: {check:?} is exempt without a reason", row.type_id);
                assert!(!row.exempt[..i].iter().any(|(c, _)| c == check), "{}: {check:?} is exempt twice", row.type_id);
            }
            for (i, known) in row.known_red.iter().enumerate() {
                let (check, miss) = (known.check, known.miss);
                assert!(!known.reason.trim().is_empty(), "{}: {check:?} {miss} is known red without a reason", row.type_id);
                assert!(row.exemption(check).is_none(), "{}: {check:?} is both exempt and known red", row.type_id);
                assert!(
                    !row.known_red[..i].iter().any(|k| k.check == check && k.miss == miss),
                    "{}: {check:?} {miss} is known red twice",
                    row.type_id
                );
            }
            for atom in row.atomic_free {
                let node = registry.construct(atom).unwrap_or_else(|| panic!("{}: atomic-free atom {atom} is not registered", row.type_id));
                // Codegen wraps the body and its includes; any atomic lives there.
                let body = node.wgsl_body().unwrap_or_else(|| panic!("{}: {atom} has no codegen body to check", row.type_id));
                let atomic = std::iter::once(body).chain(node.wgsl_includes().iter().copied()).any(|wgsl| wgsl.contains("atomic"));
                assert!(!atomic, "{}: atomic-free atom {atom} uses an atomic", row.type_id);
            }
            for (node, wgsl) in row.atomic_free_shaders {
                assert!(registry.construct(node).is_some(), "{}: {node}, which runs an atomic-free shader, is not registered", row.type_id);
                let code = wgsl.lines().map(|line| line.split("//").next().unwrap_or_default());
                assert!(!code.clone().any(|line| line.contains("atomic")), "{}: {node}'s hand shader uses an atomic", row.type_id);
                assert!(code.clone().any(|line| line.contains("@compute")), "{}: {node}'s hand shader has no entry point", row.type_id);
            }
            let mut scenes: Vec<Fixture> = Vec::new();
            for check in Check::ALL {
                if row.exemption(check).is_some() {
                    continue;
                }
                assert!(row.coupled || !check.coupled(), "{}: {check:?} needs a coupled row or an exemption", row.type_id);
                assert!(!check.needs_totals() || row.totals.is_some(), "{}: {check:?} needs the row's totals", row.type_id);
                assert!(
                    check != Check::NonfiniteTickNotPublished || row.state.is_some(),
                    "{}: {check:?} needs the row's state array",
                    row.type_id
                );
                assert!(
                    check != Check::OverflowReported || row.overflow.is_some(),
                    "{}: {check:?} needs an overflow case",
                    row.type_id
                );
                assert!(
                    check != Check::FaceGridPublished || row.faces.is_some(),
                    "{}: {check:?} needs the row's face source",
                    row.type_id
                );
                for &fixture in check.fixtures(row.coupled) {
                    if !scenes.contains(&fixture) {
                        scenes.push(fixture);
                    }
                }
            }
            // An overflow is a run-time count, never a setup refusal.
            if let Some(case) = &row.overflow {
                let mut def = (row.fixture)(case.fixture).unwrap_or_else(|| panic!("{}: no {:?} scene", row.type_id, case.fixture));
                (case.edit)(&mut def);
                build(row, case.fixture, &def)
                    .check_authored()
                    .unwrap_or_else(|error| panic!("{}: {} is refused at setup: {error}", row.type_id, case.what));
            }
            for fixture in scenes {
                let def = (row.fixture)(fixture)
                    .unwrap_or_else(|| panic!("{}: no {fixture:?} scene for a check that is not exempt", row.type_id));
                let mut preset = build(row, fixture, &def);
                let domains = preset.domains();
                assert!(
                    domains.len() == 1 && domains[0].0 == row.type_id,
                    "{} {fixture:?} holds {domains:?}",
                    row.type_id
                );
                let resolution = domains[0].1;
                if row.gpu {
                    assert!(resolution <= GPU_FIXTURE_RESOLUTION, "{} {fixture:?} runs at {resolution}", row.type_id);
                }
                if let Some(scene) = BoxScene::of(fixture) {
                    assert_eq!(resolution, scene.resolution as u32, "{} {fixture:?}: the card kept its resolution", row.type_id);
                }
                let report =
                    preset.check_authored().unwrap_or_else(|error| panic!("{} {fixture:?} at {resolution}: {error}", row.type_id));
                println!(
                    "{} {fixture:?}: resolution {resolution}, {} nodes checked, {:.1} MB",
                    row.type_id,
                    report.checked,
                    report.scene_bytes as f64 / 1e6
                );
            }
        }
    }

    /// I10: every refusal names the controls to change, in the panel's
    /// words, before any GPU work.
    #[test]
    fn liquid_refusals_name_their_control() {
        let registry = conformance_registry();
        for row in LIQUID_SOLVERS {
            assert!(!row.refusals.is_empty(), "{} lists no refusals", row.type_id);
            let domain = registry.construct(row.type_id).expect("registered domain");
            let words = |name: &str| -> String {
                if let Some(param) = domain.parameters().iter().find(|param| param.name == name) {
                    return param.label.split(" (").next().unwrap_or(param.label).to_string();
                }
                assert!(
                    domain.inputs().iter().any(|port| port.name == name),
                    "{}: {name} is neither a param nor an input",
                    row.type_id
                );
                name.replace('_', " ")
            };
            for case in row.refusals {
                let before = (row.fixture)(case.fixture).unwrap_or_else(|| panic!("{}: no {:?} scene", row.type_id, case.fixture));
                build(row, case.fixture, &before)
                    .check_authored()
                    .unwrap_or_else(|error| panic!("{}: {:?} fails before the edit: {error}", row.type_id, case.fixture));
                let mut def = before;
                (case.edit)(&mut def);
                let (node, reason) = match build(row, case.fixture, &def).check_authored() {
                    Err(ExtentError::Refused { node, reason }) => (node, reason),
                    other => panic!("{}: {} is not refused by name: {other:?}", row.type_id, case.what),
                };
                println!("{}: {}\n    {node}: {reason}", row.type_id, case.what);
                for name in case.names {
                    let label = words(name);
                    assert!(
                        reason.to_lowercase().contains(&label.to_lowercase()),
                        "{}: {} does not name {label}: {reason}",
                        row.type_id,
                        case.what
                    );
                }
            }
        }
    }

}
