//! The liquid conformance table (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.8 (Committed signatures), I2 and I10): one row per liquid domain
//! type with its scenes, the setup changes it refuses by name, and the checks
//! it is exempt from, each with its reason. The GPU checks read this table
//! in `tests/gpu_proofs/liquid_conformance.rs`; the CPU checks live here.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};

use crate::node_graph::bundled_presets::bundled_preset_def;
use crate::node_graph::matter::{MatterPoint, MatterTickStats, STATS_WORDS, WATER_DENSITY};

/// A scene the checks run on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fixture {
    /// A pool at rest.
    StillPool,
    /// A column of liquid released into a shallow pool.
    DamBreak,
    /// A box falling onto a weightless pool with open faces, `density_ratio`
    /// times as dense as the liquid.
    Collision { density_ratio: f32 },
    /// A half-density box dropped into a pool.
    FloatingBox,
    /// A density-1 box held under the surface.
    SubmergedBox,
}

/// One conformance check (section 4 (Invariants & enforcement)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// I4: a coupled Box3D world steps once per tick, only through its owner.
    CoupledWorldStepsOnce,
    /// I5: momentum and energy hold when a box hits the liquid.
    Collision,
    /// I5: a floating box settles at its waterline.
    FloatingDraft,
    /// I5: a held box feels the liquid's weight it displaces.
    HydrostaticLift,
    /// I5: a box in the air moves as Box3D alone would move it.
    FreeFlight,
    /// I6: pause holds the frames.
    PauseHoldsFrames,
    /// I6: pause discards impulses.
    PauseDiscardsImpulses,
    /// I7: a 30 fps export equals a 60 fps one.
    ExportFrameRateIndependent,
    /// I8: a non-finite tick is never published.
    NonfiniteTickNotPublished,
    /// I11: overflow is counted and reported.
    OverflowReported,
    /// I13: live frames never wait on the GPU.
    LiveFramesNeverWait,
    /// Speed 0.5 runs half the water time.
    HalfSpeed,
    /// Reset starts a new epoch.
    Reset,
}

const BOX_FALLS: &[Fixture] = &[
    Fixture::Collision { density_ratio: 0.1 },
    Fixture::Collision { density_ratio: 1.0 },
    Fixture::Collision { density_ratio: 10.0 },
];

impl Check {
    pub const ALL: [Check; 13] = [
        Check::CoupledWorldStepsOnce,
        Check::Collision,
        Check::FloatingDraft,
        Check::HydrostaticLift,
        Check::FreeFlight,
        Check::PauseHoldsFrames,
        Check::PauseDiscardsImpulses,
        Check::ExportFrameRateIndependent,
        Check::NonfiniteTickNotPublished,
        Check::OverflowReported,
        Check::LiveFramesNeverWait,
        Check::HalfSpeed,
        Check::Reset,
    ];

    /// Whether the check needs a Box3D body in the liquid.
    pub fn coupled(self) -> bool {
        matches!(
            self,
            Check::CoupledWorldStepsOnce | Check::Collision | Check::FloatingDraft | Check::HydrostaticLift | Check::FreeFlight
        )
    }

    /// The scenes the check runs on. On a row that couples, export and the
    /// live wait run with a box, where the host and the liquid exchange
    /// between ticks.
    pub fn fixtures(self, coupled: bool) -> &'static [Fixture] {
        match self {
            Check::CoupledWorldStepsOnce | Check::FloatingDraft => &[Fixture::FloatingBox],
            Check::Collision => BOX_FALLS,
            Check::HydrostaticLift => &[Fixture::SubmergedBox],
            Check::FreeFlight => &[Fixture::Collision { density_ratio: 1.0 }],
            Check::PauseDiscardsImpulses => &[Fixture::StillPool],
            Check::ExportFrameRateIndependent | Check::LiveFramesNeverWait if coupled => &[Fixture::FloatingBox],
            _ => &[Fixture::DamBreak],
        }
    }

    /// Whether the check reads the row's [`LiquidSolverRow::totals`].
    pub fn needs_totals(self) -> bool {
        matches!(self, Check::Collision | Check::ExportFrameRateIndependent | Check::NonfiniteTickNotPublished)
    }
}

/// The liquid at the end of a tick, in SI units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidTotals {
    /// kg.
    pub mass: f64,
    /// kg·m/s.
    pub momentum: [f64; 3],
    /// Kinetic plus stored elastic energy, joules.
    pub energy: f64,
    /// Records holding a non-finite position or velocity.
    pub nonfinite: u32,
}

/// Where a row's solver publishes its end-of-tick totals: a node type's
/// array output, its length in words and how to read it.
pub struct TotalsReadout {
    pub type_id: &'static str,
    pub port: &'static str,
    pub words: usize,
    pub read: fn(&[u32]) -> LiquidTotals,
}

/// The solver state a check corrupts between frames: a node type's array
/// output of records that each start with a position (three f32).
pub struct StateArray {
    pub type_id: &'static str,
    pub port: &'static str,
    pub record_bytes: usize,
}

/// A setup change that overflows a run-time capacity. The error must carry
/// the count and each of `names`.
pub struct OverflowCase {
    pub what: &'static str,
    pub fixture: Fixture,
    pub edit: fn(&mut EffectGraphDef),
    pub names: &'static [&'static str],
}

/// A setup change a row's domain refuses, and the controls the refusal
/// must name.
pub struct RefusalCase {
    pub what: &'static str,
    pub fixture: Fixture,
    pub edit: fn(&mut EffectGraphDef),
    /// Params or inputs of the row's domain. The refusal carries each
    /// param's label (less any unit in brackets) or each input's name in
    /// words.
    pub names: &'static [&'static str],
}

pub struct LiquidSolverRow {
    pub type_id: &'static str,
    /// The row's scene for a fixture; `None` when the row has no such scene,
    /// which every check needing it must be exempt from.
    pub fixture: fn(Fixture) -> Option<EffectGraphDef>,
    pub gpu: bool,
    pub coupled: bool,
    /// Atoms whose WGSL may not use atomics.
    pub atomic_free: &'static [&'static str],
    pub refusals: &'static [RefusalCase],
    /// Needed by every check with [`Check::needs_totals`] the row runs.
    pub totals: Option<TotalsReadout>,
    /// Needed by [`Check::NonfiniteTickNotPublished`].
    pub state: Option<StateArray>,
    /// Needed by [`Check::OverflowReported`].
    pub overflow: Option<OverflowCase>,
    /// A closed list, each with its reason.
    pub exempt: &'static [(Check, &'static str)],
}

impl LiquidSolverRow {
    pub fn exemption(&self, check: Check) -> Option<&'static str> {
        self.exempt.iter().find(|(exempt, _)| *exempt == check).map(|(_, reason)| *reason)
    }
}

const FLIP_COUPLES_NATIVELY: &str = "synchronous coupling (D3): FLIP steps its bodies inside its native solve and \
     takes them from the scene layer's roles, so it has no rigid owner to count, no host sync between coupled \
     ticks, and no box scene a preset can carry";

pub const LIQUID_SOLVERS: &[LiquidSolverRow] = &[
    LiquidSolverRow {
        type_id: MATTER_DOMAIN_TYPE_ID,
        fixture: matter_fixture,
        gpu: true,
        coupled: true,
        atomic_free: &[],
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
        exempt: &[(
            Check::PauseDiscardsImpulses,
            "owed to P8: GPU liquids refuse impulses until P8 routes them (LIQUID_SCENE_OWED)",
        )],
    },
    LiquidSolverRow {
        type_id: FLIP_DOMAIN_TYPE_ID,
        fixture: flip_fixture,
        gpu: false,
        coupled: true,
        atomic_free: &[],
        refusals: &[
            RefusalCase {
                what: "Resolution 256 on Dam Break at the default Grid Budget",
                fixture: Fixture::DamBreak,
                edit: |def| set_type_param(def, FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 256 }),
                names: &["grid_budget_mcells", "resolution"],
            },
            RefusalCase {
                what: "Dam Break's initial volume turned 0.3 rad",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, FLIP_DOMAIN_TYPE_ID, "initial_volume", "rot_y", 0.3),
                names: &["initial_volume"],
            },
            RefusalCase {
                what: "Dam Break's initial volume moved out of the domain",
                fixture: Fixture::DamBreak,
                edit: |def| set_source_param(def, FLIP_DOMAIN_TYPE_ID, "initial_volume", "pos_x", 10.0),
                names: &["initial_volume"],
            },
        ],
        totals: None,
        state: None,
        overflow: None,
        exempt: &[
            (Check::CoupledWorldStepsOnce, FLIP_COUPLES_NATIVELY),
            (Check::Collision, FLIP_COUPLES_NATIVELY),
            (Check::FloatingDraft, FLIP_COUPLES_NATIVELY),
            (Check::HydrostaticLift, FLIP_COUPLES_NATIVELY),
            (Check::FreeFlight, FLIP_COUPLES_NATIVELY),
            (Check::ExportFrameRateIndependent, FLIP_COUPLES_NATIVELY),
            (
                Check::LiveFramesNeverWait,
                "live debt policy (D3): FLIP's HeldClock keeps live debt on its worker, not on the liquid clock \
                 the wait counter sits on",
            ),
            (
                Check::PauseDiscardsImpulses,
                "BUG-xt71 (MIDI impulse during pause lands on resume): FLIP is frozen (D3)",
            ),
            (
                Check::NonfiniteTickNotPublished,
                "FLIP conforms as built (D3): its state lives in the native engine on its worker, which no check \
                 can reach to corrupt a tick",
            ),
            (
                Check::OverflowReported,
                "FLIP conforms as built (D3): it grows its mesh and particle storage to fit, so its only run-time \
                 limit is device memory, which it refuses by name",
            ),
        ],
    },
];

fn matter_totals(words: &[u32]) -> LiquidTotals {
    let stats = MatterTickStats::from_words(words);
    LiquidTotals {
        mass: f64::from(stats.mass),
        momentum: stats.momentum.map(f64::from),
        energy: f64::from(stats.kinetic) + f64::from(stats.elastic),
        nonfinite: stats.nonfinite,
    }
}

/// A box in a pool on the Floating Box preset: the MPM coupling proofs'
/// scenes, with the numbers their checks measure against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxScene {
    pub domain_size: f32,
    pub resolution: i32,
    pub fill: f32,
    pub liquid_gravity: f32,
    pub open_faces: bool,
    pub centre: [f32; 3],
    pub rotation: [f32; 3],
    pub edge: f32,
    pub mass: f32,
}

const G: f32 = 9.81;

/// Every fixture's liquid is water, kg/m³.
pub const FIXTURE_DENSITY: f32 = WATER_DENSITY;

/// The rigid body's cube edge per unit of transform scale.
pub const CUBE_EDGE_PER_SCALE: f32 = 1.154_700_5;

impl BoxScene {
    pub fn of(fixture: Fixture) -> Option<Self> {
        match fixture {
            Fixture::FloatingBox => Some(Self {
                domain_size: 2.0,
                resolution: 32,
                fill: 0.5,
                liquid_gravity: -G,
                open_faces: false,
                centre: [0.2, 0.78, 0.1],
                rotation: [0.21, 0.35, 0.13],
                edge: 0.5,
                mass: 62.5,
            }),
            Fixture::SubmergedBox => Some(Self {
                domain_size: 2.0,
                resolution: 32,
                fill: 0.8,
                liquid_gravity: -G,
                open_faces: false,
                centre: [0.0, 0.4, 0.0],
                rotation: [0.0; 3],
                edge: 0.4,
                mass: 64.0,
            }),
            Fixture::Collision { density_ratio } => Some(Self {
                domain_size: 1.0,
                resolution: 32,
                fill: 0.5,
                liquid_gravity: 0.0,
                open_faces: true,
                centre: [0.0, 0.75, 0.0],
                rotation: [0.0; 3],
                edge: 0.2,
                mass: density_ratio * FIXTURE_DENSITY * 0.2f32.powi(3),
            }),
            Fixture::StillPool | Fixture::DamBreak => None,
        }
    }

    /// Whether a box centred at `centre` lies wholly inside the domain, by
    /// the sphere around it.
    pub fn holds_box(&self, centre: [f32; 3]) -> bool {
        let reach = 0.5 * self.edge * 3f32.sqrt();
        let half = 0.5 * self.domain_size;
        let (min, max) = ([-half, 0.0, -half], [half, self.domain_size, half]);
        (0..3).all(|i| centre[i] - reach >= min[i] && centre[i] + reach <= max[i])
    }

    /// The Floating Box preset set to this scene; its domain is `type_id`,
    /// its box the `box_start` transform and `box_body` body.
    pub fn apply(self, preset: &'static str, type_id: &str) -> EffectGraphDef {
        let mut def = bundled(preset);
        let float = |value: f32| SerializedParamValue::Float { value };
        set_type_param(&mut def, type_id, "domain_size", float(self.domain_size));
        set_type_param(&mut def, type_id, "resolution", SerializedParamValue::Int { value: self.resolution });
        set_type_param(&mut def, type_id, "fill_height", float(self.fill));
        set_type_param(&mut def, type_id, "gravity", float(self.liquid_gravity));
        for face in ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"] {
            set_type_param(&mut def, type_id, face, SerializedParamValue::Bool { value: !self.open_faces });
        }
        let scale = self.edge / CUBE_EDGE_PER_SCALE;
        for (axis, i) in [("x", 0), ("y", 1), ("z", 2)] {
            set_node_param(&mut def, "box_start", &format!("pos_{axis}"), float(self.centre[i]));
            set_node_param(&mut def, "box_start", &format!("rot_{axis}"), float(self.rotation[i]));
            set_node_param(&mut def, "box_start", &format!("scale_{axis}"), float(scale));
        }
        set_node_param(&mut def, "box_body", "mass", float(self.mass));
        def
    }
}

fn matter_fixture(fixture: Fixture) -> Option<EffectGraphDef> {
    Some(match BoxScene::of(fixture) {
        Some(scene) => scene.apply("WaterFloatingBoxMatter", MATTER_DOMAIN_TYPE_ID),
        None if fixture == Fixture::StillPool => bundled("WaterStillPoolMatter"),
        None => bundled("WaterDamBreakMatter"),
    })
}

fn flip_fixture(fixture: Fixture) -> Option<EffectGraphDef> {
    match fixture {
        Fixture::StillPool => {
            let mut def = bundled("WaterBasin");
            set_type_param(&mut def, FLIP_DOMAIN_TYPE_ID, "emission", SerializedParamValue::Float { value: 0.0 });
            Some(def)
        }
        Fixture::DamBreak => Some(bundled("WaterDamBreak")),
        Fixture::Collision { .. } | Fixture::FloatingBox | Fixture::SubmergedBox => None,
    }
}

fn bundled(id: &'static str) -> EffectGraphDef {
    bundled_preset_def(&PresetTypeId::new(id)).unwrap_or_else(|| panic!("no bundled preset {id}")).clone()
}

fn for_each_node<F: FnMut(&mut EffectGraphNode)>(nodes: &mut [EffectGraphNode], visit: &mut F) {
    for node in nodes {
        visit(node);
        if let Some(group) = node.group.as_mut() {
            for_each_node(&mut group.nodes, visit);
        }
    }
}

/// Set `param` on the node whose stable id is `node_id`, and on any card
/// bound to it, so the card does not put the old value back at build.
pub fn set_node_param(def: &mut EffectGraphDef, node_id: &str, param: &str, value: SerializedParamValue) {
    let card = match &value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Enum { value } => *value as f32,
        SerializedParamValue::Bool { value } => f32::from(u8::from(*value)),
        other => panic!("no card carries {other:?}"),
    };
    let mut found = false;
    for_each_node(&mut def.nodes, &mut |node| {
        if node.node_id.as_str() == node_id {
            node.params.insert(param.into(), value.clone());
            found = true;
        }
    });
    assert!(found, "no node {node_id}");
    let Some(metadata) = def.preset_metadata.as_mut() else { return };
    let mut cards = Vec::new();
    for binding in &mut metadata.bindings {
        if matches!(&binding.target, BindingTarget::Node { node_id: id, param: p } if id.as_str() == node_id && p == param) {
            binding.default_value = card;
            cards.push(binding.id.clone());
        }
    }
    for spec in metadata.params.iter_mut().filter(|spec| cards.contains(&spec.id)) {
        spec.default_value = card;
    }
}

/// Set `param` on every node of `type_id` and the cards bound to it.
pub fn set_type_param(def: &mut EffectGraphDef, type_id: &str, param: &str, value: SerializedParamValue) {
    let mut ids = Vec::new();
    for_each_node(&mut def.nodes, &mut |node| {
        if node.type_id == type_id {
            ids.push(node.node_id.clone());
        }
    });
    assert!(!ids.is_empty(), "no {type_id} node");
    for id in ids {
        set_node_param(def, id.as_str(), param, value.clone());
    }
}

/// Set `param` on the transform feeding the domain's `port`. The def is
/// flattened first, so the wire is direct.
fn set_source_param(def: &mut EffectGraphDef, type_id: &str, port: &str, param: &str, value: f32) {
    *def = manifold_core::flatten::flatten_groups(def).expect("a liquid preset flattens");
    let domain = def.nodes.iter().find(|node| node.type_id == type_id).unwrap_or_else(|| panic!("no {type_id} node")).id;
    let wire = def.wires.iter().find(|wire| wire.to_node == domain && wire.to_port == port);
    let from = wire.unwrap_or_else(|| panic!("{type_id}.{port} is not wired")).from_node;
    let source = def.nodes.iter().find(|node| node.id == from).expect("the wire's source");
    assert_eq!(source.type_id, "node.transform_3d", "{type_id}.{port} comes from a transform");
    let node_id = source.node_id.clone();
    set_node_param(def, node_id.as_str(), param, SerializedParamValue::Float { value });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::liquid::extent::{ExtentError, LiquidPreset};
    use crate::node_graph::PrimitiveRegistry;
    use manifold_core::liquid_domain::{LIQUID_DOMAIN_TYPE_IDS, is_liquid_domain};

    /// The GPU runs no matter scene above resolution 64 until BUG-gwe4
    /// (staged GPU check above res 64) closes.
    const GPU_FIXTURE_RESOLUTION: u32 = 64;

    fn build(row: &LiquidSolverRow, fixture: Fixture, def: &EffectGraphDef) -> LiquidPreset {
        LiquidPreset::build(def).unwrap_or_else(|error| panic!("{} {fixture:?}: {error}", row.type_id))
    }

    /// I2: every liquid domain type has one row; every check a row does not
    /// name as exempt has its scenes, each holding the row's domain and
    /// passing the extent check as authored.
    #[test]
    fn liquid_conformance_covers_every_domain() {
        for &type_id in LIQUID_DOMAIN_TYPE_IDS {
            let rows = LIQUID_SOLVERS.iter().filter(|row| row.type_id == type_id).count();
            assert_eq!(rows, 1, "{type_id} has {rows} conformance rows");
        }
        let registry = PrimitiveRegistry::with_builtin();
        for row in LIQUID_SOLVERS {
            assert!(is_liquid_domain(row.type_id), "{} has a row but is not a liquid domain", row.type_id);
            for (i, (check, reason)) in row.exempt.iter().enumerate() {
                assert!(!reason.trim().is_empty(), "{}: {check:?} is exempt without a reason", row.type_id);
                assert!(!row.exempt[..i].iter().any(|(c, _)| c == check), "{}: {check:?} is exempt twice", row.type_id);
            }
            for atom in row.atomic_free {
                assert!(registry.construct(atom).is_some(), "{}: atomic-free atom {atom} is not registered", row.type_id);
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
        let registry = PrimitiveRegistry::with_builtin();
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
