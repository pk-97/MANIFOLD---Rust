//! Checked against FLIP Fluids the engine's coupled tank (gravity tests) (MIT); see THIRD_PARTY_NOTICES.md.
//! The liquid conformance table (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.8 (Committed signatures), I2 and I10): one row per liquid domain
//! type with its scenes, the setup changes it refuses by name, the checks it
//! is exempt from, each with its reason, and the named misses a logged bug
//! keeps red in checks it still runs. The GPU checks read this table
//! in `tests/gpu_proofs/liquid_conformance.rs`; the CPU checks live here.

#[cfg(any(test, feature = "testkit"))]
use serde_json::Value;
use manifold_water_liquid::testkit::CUBE_EDGE_PER_SCALE;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::id::NodeId;

use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::{face_coords, face_len};
use manifold_water_liquid::WATER_DENSITY;
use manifold_water_liquid::lattice::PADDING_NODES;
use crate::matter::{MatterGridNode, MatterTickStats};
use manifold_water_liquid::primitives::liquid_stats::LiquidTickStats;

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
    /// A flat box `density_ratio` times as dense as the liquid, let go 5 cm
    /// above where it floats in a still pool.
    FloatingAt { density_ratio: f32 },
    /// A box `density_ratio` times as dense as the liquid, resting on the
    /// floor against a wall under a still pool, turned `tilt` radians about
    /// the wall's horizontal so it stands on one bottom edge.
    Resting { density_ratio: f32, tilt: f32 },
    /// [`STACK_HEIGHT`] boxes, each half again as dense as the liquid,
    /// stacked on the floor under a still pool.
    Stack,
    /// The Dam Break at resolution [`FACE_GRID_RESOLUTION`] with its face
    /// grid wired into the frame.
    FaceGrid,
}

/// Boxes in the [`Fixture::Stack`] scene.
pub const STACK_HEIGHT: u32 = 3;

/// The face grid scene's resolution: the publish path is the same at any
/// size, and 32 keeps the check cheap.
pub const FACE_GRID_RESOLUTION: u32 = 32;

/// GPU FLIP's `face_valid_layers` in the face grid scene: the step's
/// `FACE_VALID_LAYERS`, held to it by `gpu_flip_band_follows_the_cfl_guard`.
pub const FACE_GRID_GPU_FLIP_LAYERS: u32 = 2;

/// One conformance check (section 4 (Invariants & enforcement)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// I4: a coupled Box3D world steps once per tick, only through its owner.
    CoupledWorldStepsOnce,
    /// I5: momentum holds when a box hits the liquid.
    CollisionMomentum,
    /// I5: energy never grows when a box hits the liquid.
    CollisionEnergy,
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
    /// I13: uncoupled live frames never wait on the GPU.
    LiveFramesNeverWait,
    /// Speed 0.5 runs half the water time.
    HalfSpeed,
    /// Reset, and the runtime's state reset, start a new epoch.
    Reset,
    /// P10 (D5): the frame publishes the faces the solver's grid gives at
    /// its last tick, bit for bit, and holds them while paused.
    FaceGridPublished,
    /// I20: a light floating box comes to rest at any Sim Rate, and no water
    /// is removed.
    FloatingRest,
    /// I20: a box resting on the floor against a wall stays put, and no
    /// water is removed.
    RestingContact,
    /// I20: a light box resting on the floor under water lifts off and
    /// floats.
    LiftOff,
    /// I20: boxes stacked under water settle without blowing up, and no
    /// water is removed.
    SubmergedStack,
    /// I19: Box3D ends each tick where the coupled motion law put the body
    /// (D18).
    HandoverAgreement,
}

/// The floating rest boxes: foam-light, and half the liquid's density.
const FLOATING_REST: &[Fixture] = &[Fixture::FloatingAt { density_ratio: 0.05 }, Fixture::FloatingAt { density_ratio: 0.5 }];

const BOX_FALLS: &[Fixture] = &[
    Fixture::Collision { density_ratio: 0.1 },
    Fixture::Collision { density_ratio: 1.0 },
    Fixture::Collision { density_ratio: 10.0 },
];

impl Check {
    pub const ALL: [Check; 20] = [
        Check::CoupledWorldStepsOnce,
        Check::CollisionMomentum,
        Check::CollisionEnergy,
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
        Check::FaceGridPublished,
        Check::FloatingRest,
        Check::RestingContact,
        Check::LiftOff,
        Check::SubmergedStack,
        Check::HandoverAgreement,
    ];

    /// Whether the check needs a Box3D body in the liquid.
    pub fn coupled(self) -> bool {
        matches!(
            self,
            Check::CoupledWorldStepsOnce
                | Check::CollisionMomentum
                | Check::CollisionEnergy
                | Check::FloatingDraft
                | Check::HydrostaticLift
                | Check::FreeFlight
                | Check::FloatingRest
                | Check::RestingContact
                | Check::LiftOff
                | Check::SubmergedStack
                | Check::HandoverAgreement
        )
    }

    /// The scenes the check runs on. On a row that couples,
    /// export runs with a box, where the host and the liquid exchange
    /// between ticks. The no wait check uses the uncoupled dam break below.
    pub fn fixtures(self, coupled: bool) -> &'static [Fixture] {
        match self {
            Check::CoupledWorldStepsOnce | Check::FloatingDraft => &[Fixture::FloatingBox],
            Check::CollisionMomentum | Check::CollisionEnergy => BOX_FALLS,
            Check::FloatingRest => FLOATING_REST,
            Check::RestingContact => &[Fixture::Resting { density_ratio: 2.0, tilt: 0.0 }],
            // On an edge, so water reaches under it: a box flat on the
            // floor has no water cell beneath it and feels no lift.
            Check::LiftOff => &[Fixture::Resting { density_ratio: 0.3, tilt: 0.3 }],
            Check::SubmergedStack => &[Fixture::Stack],
            Check::HandoverAgreement => &[
                Fixture::FloatingAt { density_ratio: 0.05 },
                Fixture::FloatingAt { density_ratio: 0.5 },
                Fixture::Resting { density_ratio: 0.3, tilt: 0.3 },
            ],
            Check::HydrostaticLift => &[Fixture::SubmergedBox],
            Check::FreeFlight => &[Fixture::Collision { density_ratio: 1.0 }],
            Check::PauseDiscardsImpulses => &[Fixture::StillPool],
            Check::FaceGridPublished => &[Fixture::FaceGrid],
            Check::ExportFrameRateIndependent if coupled => &[Fixture::FloatingBox],
            Check::LiveFramesNeverWait => &[Fixture::DamBreak],
            _ => &[Fixture::DamBreak],
        }
    }

    /// Whether the check reads the row's [`LiquidSolverRow::totals`].
    pub fn needs_totals(self) -> bool {
        matches!(
            self,
            Check::CollisionMomentum
                | Check::CollisionEnergy
                | Check::ExportFrameRateIndependent
                | Check::NonfiniteTickNotPublished
                | Check::FloatingRest
                | Check::RestingContact
                | Check::LiftOff
                | Check::SubmergedStack
        )
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

/// Where a row's solver keeps the grid its published faces come from, and
/// how that grid reads as the seam's faces (section 3.2 (Grid outputs)).
pub struct FaceSource {
    pub type_id: &'static str,
    pub port: &'static str,
    /// The face arrays, x, y and z over `cells`, from the grid's bytes, as
    /// the solver's resample computes them.
    pub resample: fn(&[u8], [u32; 3]) -> [Vec<f32>; 3],
    /// The frame's `face_valid_layers`.
    pub valid_layers: u32,
    /// Units in the last place a published face may sit from the CPU
    /// resample, with the reason when it is not 0.
    pub ulps: (u32, &'static str),
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
    /// Hand shaders that may not use atomics, by the node that runs them.
    pub atomic_free_shaders: &'static [(&'static str, &'static str)],
    pub refusals: &'static [RefusalCase],
    /// Needed by every check with [`Check::needs_totals`] the row runs.
    pub totals: Option<TotalsReadout>,
    /// Needed by [`Check::NonfiniteTickNotPublished`].
    pub state: Option<StateArray>,
    /// Needed by [`Check::OverflowReported`].
    pub overflow: Option<OverflowCase>,
    /// Needed by [`Check::FaceGridPublished`].
    pub faces: Option<FaceSource>,
    /// A closed list, each with its reason.
    pub exempt: &'static [(Check, &'static str)],
    /// Named misses a logged bug keeps failing in checks the row still runs.
    pub known_red: &'static [KnownRed],
}

/// One of a check's named misses that a logged bug keeps red on a row. The
/// check runs whole and prints it; the rest of its misses still fail it, and
/// a run where this one never fails fails too, so it comes off with the fix.
pub struct KnownRed {
    pub check: Check,
    pub miss: &'static str,
    pub reason: &'static str,
}

impl LiquidSolverRow {
    pub fn exemption(&self, check: Check) -> Option<&'static str> {
        self.exempt.iter().find(|(exempt, _)| *exempt == check).map(|(_, reason)| *reason)
    }

    pub fn known_red(&self, check: Check, miss: &str) -> Option<&'static str> {
        self.known_red.iter().find(|known| known.check == check && known.miss == miss).map(|known| known.reason)
    }
}

/// A particle liquid's tick statistics: it stores no elastic energy.
fn liquid_totals(words: &[u32]) -> LiquidTotals {
    let stats = LiquidTickStats::from_words(words);
    LiquidTotals {
        mass: f64::from(stats.mass),
        momentum: stats.momentum.map(f64::from),
        energy: f64::from(stats.kinetic),
        nonfinite: stats.nonfinite,
    }
}













/// GPU FLIP's faces: component `axis` of the FaceSample lattice's padded cell,
/// (cells + 1)³ records x fastest; 0 where no weight reached the face.
pub fn gpu_flip_faces(bytes: &[u8], cells: [u32; 3]) -> [Vec<f32>; 3] {
    let lattice: Vec<FaceSample> = bytemuck::pod_collect_to_vec(bytes);
    let m = cells.map(|n| n as usize + 1);
    std::array::from_fn(|axis| {
        (0..face_len(cells, axis) as usize)
            .map(|index| {
                let f = face_coords(cells, axis, index).map(|n| n as usize);
                let sample = lattice[f[0] + m[0] * (f[1] + m[1] * f[2])];
                if sample.weight[axis] > 0.0 { sample.velocity[axis] } else { 0.0 }
            })
            .collect()
    })
}

/// MPM's faces: the mean velocity of the four lattice nodes around each
/// face's centre that carry mass, past the lattice's padding, summed in the
/// component's order.
fn matter_faces(bytes: &[u8], cells: [u32; 3]) -> [Vec<f32>; 3] {
    let grid: Vec<MatterGridNode> = bytemuck::pod_collect_to_vec(bytes);
    let pad = PADDING_NODES as usize;
    let nodes = cells.map(|n| n as usize + 1 + 2 * pad);
    std::array::from_fn(|axis| {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        (0..face_len(cells, axis) as usize)
            .map(|index| {
                let f = face_coords(cells, axis, index).map(|n| n as usize + pad);
                let (mut sum, mut hits) = (0.0_f32, 0.0_f32);
                for db in 0..2 {
                    for dc in 0..2 {
                        let mut q = f;
                        q[b] += db;
                        q[c] += dc;
                        let node = grid[q[0] + nodes[0] * (q[1] + nodes[1] * q[2])].velocity_mass;
                        if node[3] > 0.0 {
                            sum += node[axis];
                            hits += 1.0;
                        }
                    }
                }
                if hits > 0.0 { sum / hits } else { 0.0 }
            })
            .collect()
    })
}

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

/// The rest proofs' tank edge, pool depth and box edge, metres.
const REST_DOMAIN: f32 = 2.4;
const REST_FILL: f32 = 1.0;
const REST_EDGE: f32 = 0.4;
/// The stack's box edge: three stand 0.75 m tall under the 1 m pool.
const STACK_EDGE: f32 = 0.25;

/// Every fixture's liquid is water, kg/m³.
pub const FIXTURE_DENSITY: f32 = WATER_DENSITY;

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
            Fixture::FloatingAt { density_ratio } => {
                let (fill, edge) = (REST_FILL, REST_EDGE);
                // Flat, its bottom density_ratio · edge under the surface.
                let floating = fill + edge * (0.5 - density_ratio);
                Some(Self {
                    centre: [0.3, floating + 0.05, -0.2],
                    rotation: [0.0; 3],
                    mass: density_ratio * FIXTURE_DENSITY * edge.powi(3),
                    ..Self::rest_tank()
                })
            }
            Fixture::Resting { density_ratio, tilt } => {
                let edge = REST_EDGE;
                let half = 0.5 * REST_DOMAIN;
                // Half its extent along x and y once turned about z.
                let reach = 0.5 * edge * (tilt.cos() + tilt.sin());
                Some(Self {
                    // On the floor and against the −x wall, a hair off each.
                    centre: [-half + reach + 5e-4, reach + 5e-4, 0.3],
                    rotation: [0.0, 0.0, tilt],
                    mass: density_ratio * FIXTURE_DENSITY * edge.powi(3),
                    ..Self::rest_tank()
                })
            }
            Fixture::Stack => {
                let edge = STACK_EDGE;
                Some(Self {
                    centre: [0.0, 0.5 * edge + 5e-4, 0.0],
                    rotation: [0.0; 3],
                    edge,
                    mass: 1.5 * FIXTURE_DENSITY * edge.powi(3),
                    ..Self::rest_tank()
                })
            }
            Fixture::StillPool | Fixture::DamBreak | Fixture::FaceGrid => None,
        }
    }

    /// `def` with the [`Fixture::Stack`] scene's upper boxes on its box, a
    /// millimetre apart; any other fixture's `def` as it is. Each copies the
    /// box's start, body, mesh and object, takes the next body and pose ports
    /// of the box's world and the scene's next object slot, and shares its
    /// material.
    fn stacked(&self, fixture: Fixture, mut def: EffectGraphDef) -> EffectGraphDef {
        if fixture != Fixture::Stack {
            return def;
        }
        let id_of = |def: &EffectGraphDef, name: &str| {
            def.nodes.iter().find(|node| node.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}")).id
        };
        let (world, material, render) = (id_of(&def, "box_world"), id_of(&def, "box_material"), id_of(&def, "scene"));
        for level in 1..STACK_HEIGHT {
            let next = def.nodes.iter().map(|node| node.id).max().expect("a scene has nodes") + 1;
            let mut ids = [0u32; 4];
            for (i, name) in ["box_start", "box_body", "box_mesh", "box_object"].into_iter().enumerate() {
                let source = def.nodes.iter().find(|node| node.node_id.as_str() == name).expect("the box").clone();
                ids[i] = next + i as u32;
                let handle = source.handle.as_ref().map(|handle| format!("{handle} {}", level + 1));
                def.nodes.push(EffectGraphNode { id: ids[i], node_id: NodeId::new(format!("{name}_{level}")), handle, ..source });
            }
            let [start, body, mesh, object] = ids;
            let slot = match def.nodes.iter().find(|node| node.id == render).and_then(|node| node.params.get("objects")) {
                Some(SerializedParamValue::Float { value }) => *value as u32,
                Some(SerializedParamValue::Int { value }) => *value as u32,
                other => panic!("the render scene has no object count: {other:?}"),
            };
            let wire = |from_node: u32, from_port: String, to_node: u32, to_port: String| EffectGraphWire {
                from_node,
                from_port,
                to_node,
                to_port,
            };
            def.wires.extend([
                wire(start, "transform".into(), body, "transform".into()),
                wire(body, "body".into(), world, format!("body_{level}")),
                wire(body, "shape".into(), mesh, "shape".into()),
                wire(mesh, "vertices".into(), object, "vertices".into()),
                wire(material, "out".into(), object, "material".into()),
                wire(world, format!("pose_{level}"), object, "transform".into()),
                wire(object, "object".into(), render, format!("object_{slot}")),
            ]);
            let scene = def.nodes.iter_mut().find(|node| node.id == render).expect("the render scene");
            scene.params.insert("objects".into(), SerializedParamValue::Float { value: (slot + 1) as f32 });
            let y = self.centre[1] + level as f32 * (self.edge + 1e-3);
            set_node_param(&mut def, &format!("box_start_{level}"), "pos_y", SerializedParamValue::Float { value: y });
        }
        def
    }

    /// The rest proofs' tank: the FLIP Fluids engine's coupled tank size and
    /// cell (`gpu_flip_engine_tank`), where GPU FLIP is proven against the
    /// engine, with a shallower pool.
    fn rest_tank() -> Self {
        Self {
            domain_size: REST_DOMAIN,
            resolution: 48,
            fill: REST_FILL,
            liquid_gravity: -G,
            open_faces: false,
            centre: [0.0; 3],
            rotation: [0.0; 3],
            edge: REST_EDGE,
            mass: 0.0,
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

    /// The prepared Floating Box definition set to this scene; its domain is `type_id`,
    /// its box the `box_start` transform and `box_body` body.
    pub fn apply(self, mut def: EffectGraphDef, type_id: &str) -> EffectGraphDef {
        for face in ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"] {
            set_type_param(&mut def, type_id, face, SerializedParamValue::Bool { value: !self.open_faces });
        }
        self.set(def, type_id)
    }

    /// `def` set to this scene, but for which faces are open.
    fn set(self, mut def: EffectGraphDef, type_id: &str) -> EffectGraphDef {
        let float = |value: f32| SerializedParamValue::Float { value };
        set_type_param(&mut def, type_id, "domain_size", float(self.domain_size));
        set_type_param(&mut def, type_id, "resolution", SerializedParamValue::Int { value: self.resolution });
        set_type_param(&mut def, type_id, "fill_height", float(self.fill));
        set_type_param(&mut def, type_id, "gravity", float(self.liquid_gravity));
        let scale = self.edge / CUBE_EDGE_PER_SCALE;
        for (axis, i) in [("x", 0), ("y", 1), ("z", 2)] {
            set_node_param(&mut def, "box_start", &format!("pos_{axis}"), float(self.centre[i]));
            set_node_param(&mut def, "box_start", &format!("rot_{axis}"), float(self.rotation[i]));
            set_node_param(&mut def, "box_start", &format!("scale_{axis}"), float(scale));
        }
        // The body takes density; the cube's volume is edge³.
        set_node_param(&mut def, "box_body", "density", float(self.mass / self.edge.powi(3)));
        def
    }
}







fn for_each_node<F: FnMut(&mut EffectGraphNode)>(nodes: &mut [EffectGraphNode], visit: &mut F) {
    for node in nodes {
        visit(node);
        if let Some(group) = node.group.as_mut() {
            for_each_node(&mut group.nodes, visit);
        }
    }
}

/// Find a JSON graph node by stable `nodeId`, walking nested group bodies.
#[cfg(any(test, feature = "testkit"))]
pub fn json_node_mut<'a>(value: &'a mut Value, node_id: &str) -> Option<&'a mut Value> {
    let nodes = value["nodes"].as_array_mut()?;
    for node in nodes {
        if node["nodeId"] == node_id {
            return Some(node);
        }
        if node["group"].is_object()
            && let Some(found) = json_node_mut(&mut node["group"], node_id)
        {
            return Some(found);
        }
    }
    None
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
    #[test]
    fn json_node_mut_walks_nested_groups_and_handles_misses() {
        let mut graph = serde_json::json!({
            "nodes": [{
                "nodeId": "outer",
                "group": {"nodes": [{"nodeId": "inner"}]}
            }]
        });
        assert_eq!(json_node_mut(&mut graph, "inner").expect("nested node")["nodeId"], "inner");
        json_node_mut(&mut graph, "inner").expect("nested node")["nodeId"] = serde_json::json!("changed");
        assert_eq!(json_node_mut(&mut graph, "changed").expect("renamed node")["nodeId"], "changed");
        assert!(json_node_mut(&mut graph, "missing").is_none());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
