//! Uses the PIC/FLIP ratio (0.95 FLIP) and the Dam Break scene values from FLIP Fluids fluidsimulation.h `_ratioPICFLIP` and the Dam Break preset (MIT); see THIRD_PARTY_NOTICES.md.
//! Checked against FLIP Fluids the engine's Dam Break scene (WaterDamBreak.json) (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP, the GPU water solver (docs/GPU_FLIP_PRESSURE_SOLVE.md), as
//! graphs built for any lattice. `water_def` is a running liquid on the
//! liquid seam (docs/LIQUID_SOLVER_SEAM_DESIGN.md P7a): node.gpu_flip_domain's clock runs
//! node.liquid_state's tick region, whose body is one Sim Rate tick of
//! one node.gpu_flip_step of [`STEPS_PER_TICK`] substeps, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakGpuFlip.json`, which is its own Dam Break at 64, and
//! `particle_view_def` draws that render's particles for the shipped
//! `WaterDamBreakParticles.json`. Every node reads the domain's lattice off
//! its wires, so a Resolution change reaches the running graph; the params
//! only seed the planned sizes.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use serde_json::{Value, json};

use super::gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry};
use super::gpu_flip_step::{AUTO_PRESSURE_ITERATIONS, FACE_VALID_LAYERS};
use crate::node_graph::bundled_presets::bundled_preset_json;
#[cfg(all(test, feature = "gpu-proofs"))]
use crate::node_graph::fluid::{FluidDomainLayout, domain_layout};
use crate::node_graph::liquid::clock::INTERVAL_DURATION_INPUTS;
use crate::node_graph::liquid::grid::FACE_INPUT_PORTS;
use crate::node_graph::transform::Transform;

/// The FLIP Fluids engine's Dam Break tank side, the scenes' default Domain
/// Size; a scene's own `size` is what every measure reads.
const DAM_BREAK_METRES: f64 = 4.0;

/// Water substeps per liquid tick, the step node's Steps. A collider
/// moves per substep: each places it where its tick's row has it at the
/// substep's end.
pub(crate) const STEPS_PER_TICK: usize = 1;

/// The main solve's iterations: the step's Auto.
pub(crate) const PRESSURE_ITERATIONS: usize = AUTO_PRESSURE_ITERATIONS as usize;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PressureShape {
    /// Cells per side of the cubic lattice.
    pub n: usize,
    /// Conjugate gradient iterations, one V-cycle each.
    pub iterations: usize,
}

impl PressureShape {
    pub fn at(n: usize) -> Self {
        Self { n, iterations: PRESSURE_ITERATIONS }
    }
}

/// The FLIP Fluids engine's Dam Break (`WaterDamBreak.json`): a 4 m tank
/// over the floor, a 0.16 m pool, and the `initial_column` block, seeded by
/// the engine's half-cell site rule.
pub(crate) const DAM_FILL_HEIGHT: f64 = 0.16;
pub(crate) const DAM_COLUMN: [[f64; 2]; 3] = [[-1.84, -0.66], [0.16, 2.08], [-1.75, 1.75]];

/// The Dam Break's box obstacle, the transform of `WaterDamBreak.json`'s
/// `obstacle_transform`: a unit cube scaled to 0.6 × 1.16 × 0.85 m standing
/// on the floor in the column's path. Position, then scale.
pub(crate) const DAM_OBSTACLE: [[f64; 3]; 2] = [[0.35, 0.58, -0.1], [0.6, 1.16, 0.85]];

/// Fluid role Collider and the role source's built-in cube, whose circumradius
/// 0.866 makes it a unit cube before the transform.
const COLLIDER_ROLE: usize = 3;
const CUBE_SHAPE: usize = 1;
const UNIT_CUBE_RADIUS: f64 = 0.866_025_4;

/// A liquid in a cubic tank: a pool `fill_height` deep plus one box, both
/// in metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaterScene {
    pub pressure: PressureShape,
    /// The tank's side in metres: the domain's Domain Size.
    pub size: f64,
    /// Water substeps per tick: the step node's Steps.
    pub steps: usize,
    /// The FLIP share kept per step, the FLIP Fluids engine's 0.95.
    pub flip: f64,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// Surface lattice nodes per cell (`resolution_scale` of the surface's
    /// volume and mesh): Surface Detail 1 is subdivision 2, matching the
    /// effective native WaterDamBreak preset.
    pub surface_scale: usize,
    /// Publish the face grid: three node.face_sample_component named
    /// [`FACE_NODES`] on the state's faces after the region, into the frame.
    /// The tick always hands its last step's faces to the state.
    pub faces: bool,
    /// Place the free surface where the particles' distance crosses zero
    /// (ghost fluid). Off wires zero distances: air at zero pressure on its
    /// cell centres, the race's comparison.
    pub ghost_fluid: bool,
    /// Optional non-native density projection (Volume Projection). Off by
    /// default: matched-input motion is closer to native without its extra
    /// solve. Explicit saved opt-ins remain supported.
    pub volume_projection: bool,
    /// The Dam Break's box as a Collider role (`obstacle_transform` into
    /// `obstacle_collider` into the domain's `role_0`).
    pub obstacle: bool,
    /// The tank's closed faces, bit 2d the low face of axis d and bit 2d + 1
    /// the high one; an open face's Closed param is off on the domain.
    pub closed_faces: u32,
}

/// The domain's Closed params, in mask bit order.
const CLOSED_PARAMS: [&str; 6] = ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"];

/// The face grid's nodes in a scene built with `faces`, x, y and z.
pub(crate) const FACE_NODES: [&str; 3] = ["face_u", "face_v", "face_w"];

/// The water step node in every scene.
pub(crate) const STEP_NODE: &str = "step";

/// Particles per cell the fill seeds: one per half-cell site.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) const REST_PER_CELL: f64 = 8.0;

impl WaterScene {
    /// The engine's Dam Break, its box obstacle standing in the column's path.
    pub fn dam_break(n: usize) -> Self {
        Self {
            pressure: PressureShape::at(n),
            size: DAM_BREAK_METRES,
            steps: STEPS_PER_TICK,
            flip: 0.95,
            fill_height: DAM_FILL_HEIGHT,
            column: DAM_COLUMN,
            surface: false,
            surface_scale: 2,
            faces: false,
            ghost_fluid: true,
            volume_projection: false,
            obstacle: true,
            closed_faces: 63,
        }
    }

    /// The scene with only the faces in `mask` closed.
    #[cfg(test)]
    pub fn with_closed_faces(self, mask: u32) -> Self {
        Self { closed_faces: mask, ..self }
    }

    /// The Dam Break the FLIP engine races: no obstacle, as `race_probe` and
    /// the race clips run the engine.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn race_dam_break(n: usize) -> Self {
        Self { obstacle: false, ..Self::dam_break(n) }
    }

    /// The scene with the Dam Break's box obstacle.
    #[cfg(test)]
    pub fn with_obstacle(self) -> Self {
        Self { obstacle: true, ..self }
    }

    /// A pool 1 m deep and nothing else (I5). Every scene built from it
    /// leaves the box out unless it asks with [`Self::with_obstacle`].
    #[cfg(any(test, feature = "gpu-proofs"))]
    pub fn still_pool(n: usize) -> Self {
        Self { fill_height: 1.0, column: [[0.0; 2]; 3], obstacle: false, ..Self::dam_break(n) }
    }

    /// A pool `fill` deep in a tank `size` on a side at `n` cells: the
    /// conformance box scenes' water.
    #[cfg(any(test, feature = "gpu-proofs"))]
    pub fn pool(n: usize, size: f64, fill: f64) -> Self {
        Self { size, fill_height: fill, ..Self::still_pool(n) }
    }

    /// A pool 3 m deep in the 4 m tank: the coarsest multigrid level is
    /// water but for its top row, the hardest case for the coarse solve.
    #[cfg(test)]
    pub fn deep_pool(n: usize) -> Self {
        Self { fill_height: 3.0, ..Self::still_pool(n) }
    }

    /// The deep pool with a 1 m × 0.5 m × 1 m block dropped in from 0.2 m
    /// above: a still pool's density source is zero, this one crowds.
    #[cfg(test)]
    pub fn deep_drop(n: usize) -> Self {
        Self { column: [[-0.5, 0.5], [3.2, 3.7], [-0.5, 0.5]], ..Self::deep_pool(n) }
    }

    /// A 0.9 m pool with a 0.2 m slab over its left half: 1 m mean depth,
    /// a step in the surface whose sloshing is mostly the tank's first
    /// standing wave.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn slosh(n: usize) -> Self {
        Self { fill_height: 0.9, column: [[-2.0, 0.0], [0.9, 1.1], [-2.0, 2.0]], obstacle: false, ..Self::dam_break(n) }
    }

    /// A 1 m block of water high in the tank, clear of every wall.
    #[cfg(test)]
    pub fn free_fall(n: usize) -> Self {
        Self { fill_height: 0.0, column: [[-0.5, 0.5], [2.5, 3.5], [-0.5, 0.5]], ..Self::still_pool(n) }
    }

    pub fn with_surface(self) -> Self {
        Self { surface: true, ..self }
    }

    /// Publish the face grid (section 3.2 (Grid outputs) of the seam).
    #[cfg(any(test, feature = "gpu-proofs"))]
    pub fn with_faces(self) -> Self {
        Self { faces: true, ..self }
    }

    /// Meshed at `scale` surface nodes per cell.
    #[cfg(test)]
    pub fn with_surface_scale(self, scale: usize) -> Self {
        Self { surface: true, surface_scale: scale, ..self }
    }

    /// The same scene with `iterations` per pressure solve.
    #[cfg(test)]
    pub fn with_iterations(self, iterations: usize) -> Self {
        Self { pressure: PressureShape { iterations, ..self.pressure }, ..self }
    }

    /// The cell side in metres: the tank's side over the lattice.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn cell_size(&self) -> f64 {
        self.size / self.pressure.n as f64
    }

    /// `steps` water steps a frame.
    #[cfg(test)]
    pub fn with_steps(self, steps: usize) -> Self {
        Self { steps, ..self }
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    /// The tank: the domain's layout at this resolution, no domain box.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn layout(&self) -> FluidDomainLayout {
        domain_layout(None, self.size as f32, self.pressure.n as u32).expect("the tank's layout")
    }

    /// The tank's lowest corner.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn min(&self) -> [f64; 3] {
        self.layout().min.map(f64::from)
    }

    /// The column as the domain's initial volume; none when it is empty.
    pub fn initial_volume(&self) -> Option<Transform> {
        let size = self.column.map(|[lo, hi]| hi - lo);
        (size.iter().all(|s| *s > 0.0)).then(|| Transform {
            pos: self.column.map(|[lo, hi]| (0.5 * (lo + hi)) as f32),
            scale: size.map(|s| s as f32),
            ..Transform::default()
        })
    }

    /// The domain's own reading of the scene: the fill's sites, the padded
    /// lattice and the particle count, from node.gpu_flip_domain's function.
    pub fn geometry(&self) -> GpuFlipGeometry {
        let n = self.pressure.n as f32;
        let read = |name: &str, default: f32| match name {
            "resolution" => n,
            "domain_size" => self.size as f32,
            "fill_height" => self.fill_height as f32,
            _ => default,
        };
        gpu_flip_geometry(read, None, self.initial_volume()).expect("the scene fits its domain")
    }

    #[cfg(test)]
    pub fn pool_sites(&self) -> u32 {
        self.geometry().setup.pool_sites
    }

    #[cfg(test)]
    pub fn box_sites(&self) -> [[u32; 2]; 3] {
        self.geometry().setup.box_sites
    }

    /// Particles the fill places; the particle arrays hold exactly this many.
    #[cfg(test)]
    pub fn particles(&self) -> u64 {
        self.geometry().particles
    }
}

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn int(v: usize) -> Value {
    json!({"type": "Int", "value": v})
}

#[derive(Default)]
struct Builder {
    nodes: Vec<Value>,
    wires: Vec<Value>,
}

type Port = (usize, &'static str);

impl Builder {
    fn node(&mut self, name: &str, type_id: &str, params: Value) -> usize {
        let id = self.nodes.len();
        self.nodes.push(json!({"id": id, "nodeId": name, "typeId": type_id, "params": params}));
        id
    }

    fn wire(&mut self, from: Port, to: usize, port: &str) {
        self.wires.push(json!({"fromNode": from.0, "fromPort": from.1, "toNode": to, "toPort": port}));
    }

    /// The same-named scalar outputs of `from` into `to`.
    fn wires(&mut self, from: usize, to: usize, ports: &[&'static str]) {
        for &port in ports {
            self.wire((from, port), to, port);
        }
    }
}

/// Feeds every interval input ([`INTERVAL_DURATION_INPUTS`]) among `nodes`
/// that no wire feeds yet from `domain`'s accepted interval.
fn feed_intervals(nodes: &[Value], wires: &mut Vec<Value>, domain: u64) {
    for node in nodes {
        for &(type_id, port) in &INTERVAL_DURATION_INPUTS {
            let id = &node["id"];
            if node["typeId"] != type_id || wires.iter().any(|w| &w["toNode"] == id && w["toPort"] == port) {
                continue;
            }
            wires.push(json!({"fromNode": domain, "fromPort": "interval_duration", "toNode": id, "toPort": port}));
        }
    }
}

/// The padded lattice's scalars, as the domain publishes them.
const LATTICE_WIRES: [&str; 7] = ["lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z"];

/// The fill's sites, as the domain publishes them.
const FILL_WIRES: [&str; 7] = ["pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1"];

/// The scene's forces and impulses, as the domain publishes them.
const FIELD_WIRES: [&str; 9] = [
    "forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices",
    "first_tick", "impulse_tick",
];

/// A scene as a running liquid on the seam. The domain seeds the fill and
/// runs the clock; the state's region runs one tick per due tick: `steps`
/// water steps from `state.out`, then the tick's stats, closing into
/// `state.in` and `state.stats_in`. The frame publishes each tick with the
/// solid lattice. The harness sink holds the frame, or the surface mesh when
/// `surface`.
pub(crate) fn water_def(scene: WaterScene) -> EffectGraphDef {
    let mut b = Builder::default();
    let geometry = scene.geometry();
    let mut params = json!({
        "resolution": int(scene.pressure.n),
        "domain_size": float(scene.size),
        "fill_height": float(scene.fill_height),
    });
    for (bit, name) in CLOSED_PARAMS.iter().enumerate() {
        if scene.closed_faces & (1 << bit) == 0 {
            params[*name] = json!({"type": "Bool", "value": false});
        }
    }
    let domain = b.node("domain", GPU_FLIP_DOMAIN_TYPE_ID, params);
    if let Some(volume) = scene.initial_volume() {
        let column = b.node(
            "initial_column",
            "node.transform_3d",
            json!({
                "pos_x": float(f64::from(volume.pos[0])),
                "pos_y": float(f64::from(volume.pos[1])),
                "pos_z": float(f64::from(volume.pos[2])),
                "scale_x": float(f64::from(volume.scale[0])),
                "scale_y": float(f64::from(volume.scale[1])),
                "scale_z": float(f64::from(volume.scale[2])),
            }),
        );
        b.wire((column, "transform"), domain, "initial_volume");
    }
    if scene.obstacle {
        let [pos, scale] = DAM_OBSTACLE;
        let transform = b.node(
            "obstacle_transform",
            "node.transform_3d",
            json!({
                "pos_x": float(pos[0]), "pos_y": float(pos[1]), "pos_z": float(pos[2]),
                "scale_x": float(scale[0]), "scale_y": float(scale[1]), "scale_z": float(scale[2]),
            }),
        );
        let collider = b.node(
            "obstacle_collider",
            "node.fluid_role_source",
            json!({
                "role": {"type": "Enum", "value": COLLIDER_ROLE},
                "shape": {"type": "Enum", "value": CUBE_SHAPE},
                "radius": float(UNIT_CUBE_RADIUS),
            }),
        );
        b.wire((transform, "transform"), collider, "transform");
        b.wire((collider, "role"), domain, "role_0");
    }
    // The params hold the domain's own sites, so the planned storage is the
    // fill's; the wires carry any change.
    let sites = geometry.setup.box_sites;
    let fill = b.node(
        "fill",
        "node.liquid_fill",
        padded_lattice(
            &scene,
            &[
                ("pool_sites", int(geometry.setup.pool_sites as usize)),
                ("box_x0", int(sites[0][0] as usize)),
                ("box_x1", int(sites[0][1] as usize)),
                ("box_y0", int(sites[1][0] as usize)),
                ("box_y1", int(sites[1][1] as usize)),
                ("box_z0", int(sites[2][0] as usize)),
                ("box_z1", int(sites[2][1] as usize)),
                ("jitter", float(0.0)),
                ("seed", int(0)),
            ],
        ),
    );
    b.wires(domain, fill, &FILL_WIRES);
    b.wires(domain, fill, &LATTICE_WIRES);
    b.wires(domain, fill, &["bodies", "shapes", "atlas", "body_count", "epoch", "particle_capacity"]);
    let count = (fill, "count");
    let state = b.node("state", "node.liquid_state", json!({}));
    b.wire((fill, "particles"), state, "seed");
    b.wire(count, state, "count");
    b.wires(domain, state, &["ticks", "epoch", "simulation_time", "target_time", "dropped_seconds"]);
    let particles: Port = (state, "out");
    let step = water_step(&mut b, scene, (domain, state));
    b.wire(particles, step, "particles");
    b.wire((state, "identity"), step, "identity");
    b.wire((step, "identity_out"), state, "identity_in");
    b.wire(count, step, "count");
    b.wire((domain, "reaction"), step, "reaction");
    b.wires(domain, step, &["regions", "region_count", "epoch"]);
    let (particles, faces) = ((step, "out"), (step, "faces"));
    let stats = b.node("stats", "node.liquid_stats", json!({}));
    b.wire(particles, stats, "particles");
    b.wire((state, "stats"), stats, "stats");
    b.wire(count, stats, "count");
    b.wire((domain, "particle_mass"), stats, "particle_mass");
    b.wire((step, "capped"), stats, "capped");
    b.wire(particles, state, "in");
    b.wire((stats, "stats_out"), state, "stats_in");
    // The tick's last faces leave the region beside its particles, into the
    // lattice's face grid.
    b.wire(faces, state, "faces_in");
    b.wire((step, "interior"), state, "interior_in");
    b.wires(domain, state, &["nodes_x", "nodes_y", "nodes_z"]);

    // Solver faces, published surface and whitewater share the native grid.
    const NATIVE_GRID: [(&str, &str); 6] = [
        ("mesh_min_x", "lattice_min_x"), ("mesh_min_y", "lattice_min_y"), ("mesh_min_z", "lattice_min_z"),
        ("mesh_nodes_x", "nodes_x"), ("mesh_nodes_y", "nodes_y"), ("mesh_nodes_z", "nodes_z"),
    ];
    let solid = b.node("mesh_solid", "node.liquid_solid_distance", json!({}));
    let source = b.node("whitewater_obstacle_source", "node.whitewater_obstacle_source", json!({}));
    for node in [solid, source] {
        b.wires(domain, node, &["bodies", "shapes", "atlas", "closed_faces", "body_count"]);
        b.wire((domain, "cell_size"), node, "cell_size");
        for (from, to) in NATIVE_GRID {
            b.wire((domain, from), node, to);
        }
        b.wire((domain, "body_rows"), node, "rows");
        b.wire((domain, "mesh_wall_inset"), node, "wall_inset");
    }
    let frame = b.node("frame", "node.liquid_frame", json!({"face_valid_layers": int(FACE_VALID_LAYERS as usize)}));
    b.wire((state, "out"), frame, "particles");
    b.wire((state, "stats"), frame, "stats");
    b.wire((state, "identity"), frame, "identity");
    b.wire((state, "interior"), frame, "interior");
    b.wire((solid, "solid"), frame, "solid");
    b.wire(count, frame, "count");
    b.wires(domain, frame, &LATTICE_WIRES);
    b.wires(domain, frame, &["closed_faces", "simulation_time", "display_time", "epoch"]);
    if scene.faces {
        let nodes = scene.geometry().setup.lattice.nodes();
        for (axis, name) in FACE_NODES.into_iter().enumerate() {
            let params = json!({
                "axis": {"type": "Enum", "value": axis},
                "nodes_x": float(f64::from(nodes[0])),
                "nodes_y": float(f64::from(nodes[1])),
                "nodes_z": float(f64::from(nodes[2])),
            });
            let id = b.node(name, "node.face_sample_component", params);
            b.wire((state, "faces"), id, "faces");
            b.wires(domain, id, &["nodes_x", "nodes_y", "nodes_z"]);
            b.wire((id, "out"), frame, FACE_INPUT_PORTS[axis]);
        }
    }
    // Solids pose their bodies at the end of the accepted interval, so they
    // agree with the step at every Sim Rate.
    feed_intervals(&b.nodes, &mut b.wires, domain as u64);

    let output = b.node("output", "system.final_output", json!({}));
    let sink = if scene.surface {
        let mesh = surface(&mut b, scene, frame);
        let sink = b.node("mesh_sink", "test.mesh_sink", json!({}));
        b.wire(mesh, sink, "vertices");
        sink
    } else {
        let sink = b.node("sink", "test.liquid_sink", json!({}));
        b.wire((frame, "particles_b"), sink, "particles");
        sink
    };
    b.wire((sink, "out"), output, "in");
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("water def")
}

/// The shipped GPU FLIP Dam Break. Its render (camera, lights, environment,
/// tank, water material, tone map) and its Liquid Surface group are the one
/// source every builder scene takes them from; its water is the builder's
/// Dam Break at 64 (`gpu_flip_shipped_preset_is_the_builders_dam_break`).
pub(crate) const SHIPPED_PRESET: &str = "WaterDamBreakGpuFlip";

fn shipped_preset() -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(SHIPPED_PRESET)).expect("the GPU FLIP preset is bundled");
    serde_json::from_str(&json).expect("the GPU FLIP preset parses")
}

/// The shipped preset's Liquid Surface group.
fn surface_group() -> Value {
    let preset = shipped_preset();
    let nodes = preset["nodes"].as_array().expect("preset nodes");
    nodes.iter().find(|node| node["nodeId"] == "surface").expect("liquid surface group").clone()
}

/// The nodes the builder makes: `water_def`'s, the obstacle's render and the
/// display presentation. Every other node of the shipped preset is the
/// authored render `render_def` copies.
#[cfg(any(test, feature = "gpu-proofs"))]
fn builder_made(node_id: &str) -> bool {
    let step = node_id.split_once('.').is_some_and(|(step, _)| {
        step.len() > 1 && step.starts_with('s') && step[1..].bytes().all(|b| b.is_ascii_digit())
    });
    step || FACE_NODES.contains(&node_id)
        || OBSTACLE_RENDER.contains(&node_id)
        || DISPLAY_NODES.iter().any(|(name, _)| *name == node_id)
        || matches!(
            node_id,
            "domain" | "initial_column" | "obstacle_transform" | "obstacle_collider" | "fill" | "state" | STEP_NODE | "stats" | "solid" | "mesh_solid" | "whitewater_obstacle_source" | "frame" | "surface"
        )
}

/// The render's display-time presentation (docs/GPU_FLUID_SURFACE_DESIGN.md
/// D11): frame B's liquid particles interpolated to the display time and
/// pushed out of the display-time solid, and each whitewater population
/// moved from its newest tick to the display time. The builder makes these
/// on every rendered scene. Name and node type.
#[cfg(any(test, feature = "gpu-proofs"))]
const DISPLAY_NODES: [(&str, &str); 8] = [
    ("particle_blend", "node.interpolate_particle_frames"),
    ("solid_blend", "node.mix_arrays"),
    ("particle_push_out", "node.push_out_of_solid"),
    ("display_lattice_box", "node.transform_components"),
    ("foam_blend", "node.interpolate_particle_frames"),
    ("bubble_blend", "node.interpolate_particle_frames"),
    ("spray_blend", "node.interpolate_particle_frames"),
    ("dust_blend", "node.interpolate_particle_frames"),
];

/// The whitewater populations: the state's `{kind}_particles` reach
/// `{kind}_copies` through `{kind}_blend`.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(super) const WHITEWATER_KINDS: [&str; 4] = ["foam", "bubble", "spray", "dust"];

/// `name`'s id in `def`.
#[cfg(any(test, feature = "gpu-proofs"))]
fn id_named(def: &Value, name: &str) -> u64 {
    let nodes = def["nodes"].as_array().expect("nodes");
    let node = nodes.iter().find(|n| n["nodeId"] == name).unwrap_or_else(|| panic!("no node {name}"));
    node["id"].as_u64().expect("numeric id")
}

/// The id after every node of `def`.
#[cfg(any(test, feature = "gpu-proofs"))]
fn next_id(def: &Value) -> u64 {
    def["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1
}

/// Wires `from` into `to`, replacing whatever fed that input.
#[cfg(any(test, feature = "gpu-proofs"))]
fn rewire(def: &mut Value, from: (u64, &str), to: (u64, &str)) {
    let wires = def["wires"].as_array_mut().expect("wires");
    wires.retain(|w| w["toNode"] != to.0 || w["toPort"] != to.1);
    wires.push(json!({"fromNode": from.0, "fromPort": from.1, "toNode": to.0, "toPort": to.1}));
}

/// Adds [`DISPLAY_NODES`] at the next ids, all on default params, and wires
/// them from the frame, the state and the domain into the whitewater copies.
/// The liquid's consumer reads `particle_push_out.out` and `solid_blend.out`.
#[cfg(any(test, feature = "gpu-proofs"))]
fn add_display_presentation(def: &mut Value) {
    let next = next_id(def);
    for (offset, (name, type_id)) in (0..).zip(DISPLAY_NODES) {
        def["nodes"].as_array_mut().expect("nodes").push(json!({"id": next + offset, "nodeId": name, "typeId": type_id}));
    }
    let [frame, state, domain, particles, solid, push_out, lattice_box] =
        ["frame", "state", "domain", "particle_blend", "solid_blend", "particle_push_out", "display_lattice_box"]
            .map(|name| id_named(def, name));
    let mut feeds: Vec<(u64, String, u64, String)> = Vec::new();
    let mut feed = |from: u64, output: &str, to: u64, input: &str| feeds.push((from, output.into(), to, input.into()));
    for port in ["particles_a", "particles_b", "count_a", "count_b", "identity_a", "identity_b", "blend", "span"] {
        feed(frame, port, particles, port);
    }
    feed(frame, "solid_a", solid, "a");
    feed(frame, "solid_b", solid, "b");
    feed(frame, "blend", solid, "amount");
    feed(particles, "out", push_out, "particles");
    feed(solid, "out", push_out, "solid");
    // The push-out samples the solid on the frame's lattice, the native mesh
    // lattice that frame.solid_a/b are sampled on.
    feed(frame, "grid_bounds", lattice_box, "transform");
    for axis in ["x", "y", "z"] {
        feed(lattice_box, &format!("pos_{axis}"), push_out, &format!("center_{axis}"));
        feed(lattice_box, &format!("scale_{axis}"), push_out, &format!("size_{axis}"));
        feed(frame, &format!("grid_nodes_{axis}"), push_out, &format!("nodes_{axis}"));
    }
    for kind in WHITEWATER_KINDS {
        let [blend, copies] = [format!("{kind}_blend"), format!("{kind}_copies")].map(|name| id_named(def, &name));
        feed(state, &format!("{kind}_particles"), blend, "particles_b");
        feed(frame, "blend", blend, "blend");
        feed(frame, "span", blend, "span");
        feed(blend, "out", copies, "particles");
    }
    // Spray flies free, so its move back to the display time follows gravity.
    let spray = id_named(def, "spray_blend");
    for (output, input) in [("gravity_x", "acceleration_x"), ("gravity", "acceleration_y"), ("gravity_z", "acceleration_z")] {
        feed(domain, output, spray, input);
    }
    for (from, output, to, input) in feeds {
        rewire(def, (from, &output), (to, &input));
    }
}

/// The obstacle's render nodes, which `render_def` adds when the scene has
/// the box: its mesh, its material and the object the render scene draws.
#[cfg(any(test, feature = "gpu-proofs"))]
const OBSTACLE_RENDER: [&str; 3] = ["obstacle_mesh", "obstacle_material", "obstacle_object"];

/// The render scene's object slot the box takes.
#[cfg(any(test, feature = "gpu-proofs"))]
const OBSTACLE_SLOT: &str = "object_1";

/// The box as the audience sees it: a unit cube on the collider's own
/// transform, in `WaterDamBreak.json`'s copper.
#[cfg(any(test, feature = "gpu-proofs"))]
fn add_obstacle_render(def: &mut Value, transform: u64, scene: u64) {
    let next = next_id(def);
    let [mesh, material, object] = [next, next + 1, next + 2];
    let material_params = json!({
        "color_r": float(0.52), "color_g": float(0.23), "color_b": float(0.073),
        "metallic": float(0.94), "roughness": float(0.19), "ambient": float(0.0),
    });
    let nodes = def["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({"id": mesh, "nodeId": "obstacle_mesh", "typeId": "node.cube_mesh", "handle": "Obstacle Mesh", "params": {}}));
    nodes.push(json!({"id": material, "nodeId": "obstacle_material", "typeId": "node.pbr_material", "handle": "Obstacle Material", "params": material_params}));
    nodes.push(json!({"id": object, "nodeId": "obstacle_object", "typeId": "node.scene_object", "handle": "Obstacle", "params": {}}));
    let wire = |from: u64, from_port: &str, to: u64, to_port: &str| json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port});
    let wires = def["wires"].as_array_mut().expect("wires");
    wires.push(wire(mesh, "vertices", object, "vertices"));
    wires.push(wire(material, "out", object, "material"));
    wires.push(wire(transform, "transform", object, "transform"));
    wires.push(wire(object, "object", scene, OBSTACLE_SLOT));
}

/// Add Fluid handle suffixes by builder node name: `""` is the bare fluid
/// handle; unnamed nodes carry none.
const BODY_HANDLES: [(&str, &str); 5] = [
    ("domain", "Simulation"),
    ("initial_column", "Initial Volume"),
    ("surface", "Surface"),
    ("water_material", "Material"),
    ("water_object", ""),
];

/// The body's group output node name.
pub const LIQUID_BODY_OUTPUT: &str = "fluid_output";

/// The liquid body Add Fluid inserts, as one graph: the shipped Dam Break's
/// water and Liquid Surface from [`water_def`], and the shipped preset's
/// water material and object, closed into a group output. Built by the same
/// builder and preset `WaterDamBreakGpuFlip.json` is checked against, so the
/// two cannot drift. Nodes keep the builder's names (`domain`,
/// `initial_column`, `water_material`, `water_object`,
/// [`LIQUID_BODY_OUTPUT`]); handles are Add Fluid suffixes.
pub fn gpu_flip_liquid_body() -> EffectGraphDef {
    use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID};

    // The box is the preset's scene dressing, not the liquid: Add Fluid inserts water only.
    let mut def = water_def(WaterScene { obstacle: false, ..WaterScene::dam_break(64).with_surface() });
    let harness: Vec<u32> = def.nodes.iter()
        .filter(|node| matches!(node.node_id.as_str(), "mesh_sink" | "output"))
        .map(|node| node.id)
        .collect();
    def.nodes.retain(|node| !harness.contains(&node.id));
    def.wires.retain(|wire| !harness.contains(&wire.from_node) && !harness.contains(&wire.to_node));

    let preset = shipped_preset();
    let mut next = def.nodes.iter().map(|node| node.id).max().expect("water nodes") + 1;
    let mut take = |name: &str, nodes: &mut Vec<EffectGraphNode>| -> u32 {
        let found = preset["nodes"].as_array().expect("preset nodes").iter()
            .find(|node| node["nodeId"] == name)
            .unwrap_or_else(|| panic!("the GPU FLIP preset has no {name}"));
        let mut node: EffectGraphNode = serde_json::from_value(found.clone()).expect("preset node");
        node.id = next;
        next += 1;
        nodes.push(node);
        nodes.last().expect("pushed").id
    };
    let material = take("water_material", &mut def.nodes);
    let object = take("water_object", &mut def.nodes);
    let output = next;
    def.nodes.push(serde_json::from_value(json!({
        "id": output, "nodeId": LIQUID_BODY_OUTPUT, "typeId": GROUP_OUTPUT_TYPE_ID,
    })).expect("group output"));

    let surface = def.nodes.iter().find(|node| node.node_id.as_str() == "surface").expect("liquid surface").id;
    let wire = |from_node: u32, from_port: &str, to_node: u32, to_port: &str| EffectGraphWire {
        from_node, from_port: from_port.into(), to_node, to_port: to_port.into(),
    };
    def.wires.extend([
        wire(surface, "vertices", object, "vertices"),
        wire(surface, "indices", object, "indices"),
        wire(material, "out", object, "material"),
        wire(object, "object", output, "object"),
    ]);
    for node in &mut def.nodes {
        node.handle = BODY_HANDLES.iter()
            .find(|(name, _)| *name == node.node_id.as_str())
            .map(|(_, suffix)| (*suffix).to_owned());
    }
    def
}

/// Surface Detail adds this to its value to give the surface nodes' scale.
#[cfg(any(test, feature = "gpu-proofs"))]
const SURFACE_DETAIL_OFFSET: usize = 1;

/// A meshed scene inside the shipped preset's render: `water_def`'s water
/// and surface, the preset's other nodes, and the preset's wires between the
/// two, matched by node name.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn render_def(scene: WaterScene) -> EffectGraphDef {
    // The render's Whitewater group reads the face grid.
    let mut def = serde_json::to_value(water_def(scene.with_surface().with_faces())).expect("water def serialises");
    let preset = shipped_preset();
    let harness = [id_named(&def, "mesh_sink"), id_named(&def, "output")];
    let ends = |wire: &Value| [wire["fromNode"].as_u64().expect("from"), wire["toNode"].as_u64().expect("to")];
    def["nodes"].as_array_mut().expect("nodes").retain(|n| !harness.contains(&n["id"].as_u64().expect("id")));
    def["wires"].as_array_mut().expect("wires").retain(|w| ends(w).iter().all(|id| !harness.contains(id)));
    let next = next_id(&def);
    let name_of = |node: &Value| node["nodeId"].as_str().expect("node name").to_string();
    let preset_nodes = preset["nodes"].as_array().expect("preset nodes");
    let render: Vec<&Value> = preset_nodes.iter().filter(|n| !builder_made(&name_of(n))).collect();
    let first = render.iter().filter_map(|n| n["id"].as_u64()).min().expect("the preset has a render");
    // Render ids move up only when this scene's water outgrows the preset's.
    let shift = next.saturating_sub(first);
    let render_ids: Vec<u64> = render.iter().map(|n| n["id"].as_u64().expect("preset id")).collect();
    for node in &render {
        let mut node = (*node).clone();
        node["id"] = json!(node["id"].as_u64().expect("preset id") + shift);
        def["nodes"].as_array_mut().expect("nodes").push(node);
    }
    let water_name: Vec<(u64, String)> = preset_nodes
        .iter()
        .filter(|n| builder_made(&name_of(n)))
        .map(|n| (n["id"].as_u64().expect("preset id"), name_of(n)))
        .collect();
    'wires: for wire in preset["wires"].as_array().expect("preset wires") {
        if ends(wire).iter().all(|id| !render_ids.contains(id)) {
            continue;
        }
        let mut wire = wire.clone();
        for end in ["fromNode", "toNode"] {
            let id = wire[end].as_u64().expect("end");
            wire[end] = json!(if render_ids.contains(&id) {
                id + shift
            } else {
                let name = &water_name.iter().find(|(water, _)| *water == id).expect("a preset node").1;
                // The obstacle's render and the display presentation are
                // wired by the builder, below.
                if OBSTACLE_RENDER.contains(&name.as_str()) || DISPLAY_NODES.iter().any(|(display, _)| *display == name.as_str()) {
                    continue 'wires;
                }
                id_named(&def, name)
            });
        }
        // A render-stage presentation wire replaces the raw frame wire that
        // water_def supplies to its surface test harness.
        let wires = def["wires"].as_array_mut().expect("wires");
        wires.retain(|old| old["toNode"] != wire["toNode"] || old["toPort"] != wire["toPort"]);
        wires.push(wire);
    }
    // Persistent render nodes, then the builder's display nodes, then the
    // obstacle render: every regeneration assigns the same ids.
    add_dust_render(&mut def);
    add_display_presentation(&mut def);
    let [surface, particles, solid] = ["surface", "particle_push_out", "solid_blend"].map(|name| id_named(&def, name));
    rewire(&mut def, (particles, "out"), (surface, "particles"));
    rewire(&mut def, (solid, "out"), (surface, "solid"));
    if scene.obstacle {
        let transform = id_named(&def, "obstacle_transform");
        let scene_node = id_named(&def, "scene");
        add_obstacle_render(&mut def, transform, scene_node);
    }
    for key in ["name", "description"] {
        def[key] = preset[key].clone();
    }
    let source = id_named(&def, "whitewater_obstacle_source");
    let whitewater = id_named(&def, "whitewater");
    let state = id_named(&def, "state");
    let step = id_named(&def, STEP_NODE);
    let domain = id_named(&def, "domain");
    let nodes = def["nodes"].as_array().expect("nodes").clone();
    let wires = def["wires"].as_array_mut().expect("wires");
    // Whitewater emits, ages and moves over the accepted interval.
    feed_intervals(&nodes, wires, domain);
    // Regeneration is idempotent: the source and new capture may already be in the seed preset.
    wires.retain(|w| !(w["toNode"] == whitewater && w["toPort"] == "obstacle_source"
        || w["toNode"] == state && w["toPort"] == "dust_particles_in"));
    wires.push(json!({"fromNode":source, "fromPort":"solid", "toNode":whitewater, "toPort":"obstacle_source"}));
    wires.push(json!({"fromNode":whitewater, "fromPort":"dust_particles", "toNode":state, "toPort":"dust_particles_in"}));
    for (from, ports) in [
        (step, &["substep_schedule", "substep_u", "substep_v", "substep_w", "substep_count"][..]),
        (domain, &["forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices", "impulse_tick", "first_tick", "regions", "region_count", "shapes", "atlas"][..]),
    ] {
        for port in ports {
            wires.retain(|w| !(w["toNode"] == whitewater && w["toPort"] == *port));
            wires.push(json!({"fromNode":from, "fromPort":port, "toNode":whitewater, "toPort":port}));
        }
    }
    def["presetMetadata"] = scene_cards(&preset["presetMetadata"], scene);
    serde_json::from_value(def).expect("render def")
}

/// The shipped Particle View, which [`particle_view_def`] builds from the
/// shipped Dam Break (`gpu_flip_particle_view_is_built_from_the_dam_break`).
#[cfg(test)]
pub(crate) const PARTICLE_VIEW_PRESET: &str = "WaterDamBreakParticles";

#[cfg(test)]
const PARTICLE_VIEW_NAME: &str = "Water — Dam Break (Particle View)";

#[cfg(test)]
const PARTICLE_VIEW_DESCRIPTION: &str = "GPU FLIP dam break drawn as small sphere copies of the blended, solid-clamped liquid particles, with the same display-time whitewater as the surface preset.";

/// The Platonic shape the Particle View draws each particle with, scaled by
/// the particle's radius.
#[cfg(test)]
const ICOSAHEDRON: usize = 3;

/// The Particle View: the shipped Dam Break's render, simulation, scene,
/// whitewater and dust, with the Liquid Surface replaced by
/// `particle_push_out`'s particles drawn as instanced spheres by the water
/// object. Its cards are the Dam Break's, less those that only reached the
/// surface.
#[cfg(test)]
pub(crate) fn particle_view_def() -> EffectGraphDef {
    let mut def = serde_json::to_value(render_def(WaterScene::dam_break(64).with_faces())).expect("render def serialises");
    let surface = id_named(&def, "surface");
    def["nodes"].as_array_mut().expect("nodes").retain(|n| n["id"] != surface);
    def["wires"].as_array_mut().expect("wires").retain(|w| w["fromNode"] != surface && w["toNode"] != surface);
    let next = next_id(&def);
    let (copies, sphere) = (next, next + 1);
    let nodes = def["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({"id": copies, "nodeId": "liquid_particle_copies", "typeId": "node.particles_to_copies"}));
    nodes.push(json!({
        "id": sphere, "nodeId": "liquid_particle_mesh", "typeId": "node.platonic_solid_mesh",
        "params": {"radius": float(1.0), "shape": {"type": "Enum", "value": ICOSAHEDRON}},
    }));
    let [frame, particles, water] = ["frame", "particle_push_out", "water_object"].map(|name| id_named(&def, name));
    for (from, to) in [
        ((particles, "out"), (copies, "particles")),
        ((frame, "count_b"), (copies, "live_count")),
        ((copies, "copies"), (water, "instances")),
        ((sphere, "vertices"), (water, "vertices")),
        ((frame, "count_b"), (water, "instance_count")),
    ] {
        rewire(&mut def, from, to);
    }
    def["name"] = json!(PARTICLE_VIEW_NAME);
    def["description"] = json!(PARTICLE_VIEW_DESCRIPTION);
    def["presetMetadata"] = particle_view_cards(&def);
    serde_json::from_value(def).expect("particle view def")
}

/// `def`'s cards under the Particle View's id, less every binding whose node
/// is gone and every card left with none of the bindings it had.
#[cfg(test)]
fn particle_view_cards(def: &Value) -> Value {
    fn names(nodes: &Value, into: &mut Vec<String>) {
        for node in nodes.as_array().into_iter().flatten() {
            into.extend(node["nodeId"].as_str().map(str::to_owned));
            names(&node["group"]["nodes"], into);
        }
    }
    let mut present = Vec::new();
    names(&def["nodes"], &mut present);
    let mut metadata = def["presetMetadata"].clone();
    for (bindings, cards) in [("bindings", "params"), ("stringBindings", "stringParams")] {
        let Some(list) = metadata[bindings].as_array_mut() else { continue };
        let bound = |list: &[Value]| list.iter().filter_map(|b| b["id"].as_str().map(str::to_owned)).collect::<Vec<_>>();
        let before = bound(list.as_slice());
        list.retain(|b| b["target"]["kind"] != "node" || b["target"]["nodeId"].as_str().is_some_and(|node| present.iter().any(|name| name == node)));
        let after = bound(list.as_slice());
        if let Some(cards) = metadata[cards].as_array_mut() {
            cards.retain(|card| card["id"].as_str().is_none_or(|id| !before.iter().any(|b| b == id) || after.iter().any(|a| a == id)));
        }
    }
    metadata["id"] = json!(PARTICLE_VIEW_PRESET);
    metadata["displayName"] = json!(PARTICLE_VIEW_NAME);
    metadata["oscPrefix"] = json!(PARTICLE_VIEW_PRESET.to_lowercase());
    metadata
}

/// Dust follows the same particle-frame render path as the other populations.
#[cfg(any(test, feature = "gpu-proofs"))]
fn add_dust_render(def: &mut Value) {
    for node in def["nodes"].as_array_mut().expect("nodes") {
        if node["nodeId"].as_str().is_some_and(|name| name.starts_with("dust_"))
            && let Some(handle) = node["handle"].as_str() {
            node["handle"] = json!(handle.replace("Foam", "Dust"));
        }
    }
    let nodes = def["nodes"].as_array().expect("nodes");
    if nodes.iter().any(|n| n["nodeId"] == "dust_copies") { return; }
    let first = nodes.iter().map(|n| n["id"].as_u64().expect("id")).max().expect("nodes") + 1;
    let mut map = Vec::new();
    let mut added = Vec::new();
    for (i, name) in ["foam_mesh", "foam_material", "foam_object", "foam_copies"].iter().enumerate() {
        let mut n = nodes.iter().find(|n| n["nodeId"] == *name).expect("foam render").clone();
        map.push((n["id"].as_u64().expect("id"), first + i as u64));
        n["id"] = json!(first + i as u64);
        n["nodeId"] = json!(name.replace("foam_", "dust_"));
        if let Some(handle) = n["handle"].as_str() { n["handle"] = json!(handle.replace("Foam", "Dust")); }
        added.push(n);
    }
    let scene = nodes.iter().find(|n| n["nodeId"] == "scene").expect("scene")["id"].clone();
    let mut wires = Vec::new();
    for w in def["wires"].as_array().expect("wires") {
        let from = w["fromNode"].as_u64().expect("from");
        let to = w["toNode"].as_u64().expect("to");
        if !map.iter().any(|(old, _)| *old == to) { continue; }
        let mut w = w.clone();
        if let Some((_, new)) = map.iter().find(|(old, _)| *old == from) { w["fromNode"] = json!(new); }
        w["toNode"] = json!(map.iter().find(|(old, _)| *old == to).expect("target").1);
        if w["fromPort"] == "foam_particles" { w["fromPort"] = json!("dust_particles"); }
        wires.push(w);
    }
    wires.push(json!({"fromNode":first+2,"fromPort":"object","toNode":scene,"toPort":"object_10"}));
    def["nodes"].as_array_mut().expect("nodes").extend(added);
    def["wires"].as_array_mut().expect("wires").extend(wires);
    let scene = def["nodes"].as_array_mut().expect("nodes").iter_mut().find(|n| n["nodeId"] == "scene").expect("scene");
    scene["params"]["objects"] = int(11);
}

/// The shipped cards with every default at the value this scene's def bakes,
/// so no card overwrites what the extent proof checked: Resolution at the
/// lattice, Surface Detail at the surface scale, gone past its range.
#[cfg(any(test, feature = "gpu-proofs"))]
fn scene_cards(metadata: &Value, scene: WaterScene) -> Value {
    let mut metadata = metadata.clone();
    let detail = scene.surface_scale.checked_sub(SURFACE_DETAIL_OFFSET).filter(|detail| *detail <= 2);
    for list in ["params", "bindings"] {
        let entries = metadata[list].as_array_mut().expect("card lists");
        entries.retain(|entry| entry["id"] != "surface_detail" || detail.is_some());
        for entry in entries.iter_mut() {
            if entry["id"] == "resolution" {
                entry["defaultValue"] = json!(scene.pressure.n as f64);
            }
            if let Some(detail) = detail
                && entry["id"] == "surface_detail"
            {
                entry["defaultValue"] = json!(detail as f64);
            }
        }
    }
    metadata
}

/// Sets `resolution_scale` on the surface schedule, volume and mesh in `value`,
/// however deep the group nests them.
fn set_surface_scale(value: &mut Value, scale: usize) {
    match value {
        Value::Object(map) => {
            let surface = map.get("nodeId").is_some_and(|id| id == "liquid_volume" || id == "liquid_mesh" || id == "liquid_bricks");
            if surface && let Some(params) = map.get_mut("params") {
                params["resolution_scale"] = int(scale);
            }
            map.values_mut().for_each(|v| set_surface_scale(v, scale));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| set_surface_scale(v, scale)),
        _ => {}
    }
}

/// The liquid surface on the frame's seam outputs: frame B's particles and
/// count, its solid lattice, and the lattice's bounds and nodes.
fn surface(b: &mut Builder, scene: WaterScene, frame: usize) -> Port {
    let mut group = surface_group();
    set_surface_scale(&mut group, scene.surface_scale);
    let id = b.nodes.len();
    group["id"] = json!(id);
    group["nodeId"] = json!("surface");
    b.nodes.push(group);
    for (from, to) in [
        ("particles_b", "particles"),
        ("interior_b", "interior"),
        ("count_b", "count"),
        ("solid_b", "solid"),
        ("grid_bounds", "bounds"),
        ("grid_nodes_x", "nodes_x"),
        ("grid_nodes_y", "nodes_y"),
        ("grid_nodes_z", "nodes_z"),
    ] {
        b.wire((frame, from), id, to);
    }
    (id, "vertices")
}

/// The scene's padded lattice (the domain's `LATTICE_WIRES`) as params,
/// plus `extra`. The wires carry any change at run time.
fn padded_lattice(scene: &WaterScene, extra: &[(&str, Value)]) -> Value {
    let lattice = scene.geometry().setup.lattice;
    let mut params = json!({"cell_size": float(f64::from(lattice.cell_size()))});
    for (axis, name) in ["lattice_min_x", "lattice_min_y", "lattice_min_z"].into_iter().enumerate() {
        params[name] = float(f64::from(lattice.min()[axis]));
    }
    for (axis, name) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().enumerate() {
        params[name] = int(lattice.nodes()[axis] as usize);
    }
    for (name, value) in extra {
        params[*name] = value.clone();
    }
    params
}

/// The tick's water (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)):
/// one node.gpu_flip_step of `scene.steps` substeps on the domain's lattice, gravity,
/// fields and bodies, and the state's tick index. The caller wires its
/// particles and count.
fn water_step(b: &mut Builder, scene: WaterScene, tick: (usize, usize)) -> usize {
    let (domain, state) = tick;
    let iterations = |n: usize, auto: u32| int(if n == auto as usize { 0 } else { n });
    let step = b.node(
        STEP_NODE,
        "node.gpu_flip_step",
        padded_lattice(
            &scene,
            &[
                ("steps", int(scene.steps)),
                ("flip", float(scene.flip)),
                ("iterations", iterations(scene.pressure.iterations, AUTO_PRESSURE_ITERATIONS)),
                ("ghost_fluid", int(usize::from(scene.ghost_fluid))),
                ("volume_projection", int(usize::from(scene.volume_projection))),
            ],
        ),
    );
    b.wires(domain, step, &LATTICE_WIRES);
    b.wire((domain, "interval_duration"), step, "interval_duration");
    b.wire((domain, "gravity_x"), step, "gravity_x");
    b.wire((domain, "gravity"), step, "gravity_y");
    b.wire((domain, "gravity_z"), step, "gravity_z");
    b.wires(domain, step, &FIELD_WIRES);
    b.wire((state, "tick_index"), step, "tick_index");
    b.wire((state, "retired_max_speed"), step, "retired_max_speed");
    b.wire((domain, "initial_obstacle_speed"), step, "initial_obstacle_speed");
    b.wires(domain, step, &["bodies", "shapes", "atlas", "body_count", "dynamic_bodies", "closed_faces", "solve_level"]);
    b.wire((domain, "body_rows"), step, "rows");
    step
}

/// Device bytes a scene holds inside the render graph at 1920×1080, as the
/// liquid extent check counts them: every array at the size the walk reached
/// plus what each node holds for itself. Textures are not counted.
#[cfg(test)]
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    tests::walk(&render_def(scene), false).expect("the rendered scene covers every dispatch").scene_bytes
}

/// `def` as the app renders a generator: fused when it has regions, `None`
/// when it has none and runs as authored. Regions that refuse to fuse fail.
#[cfg(test)]
pub(super) fn fused_as_rendered(
    def: &EffectGraphDef,
    registry: &crate::node_graph::PrimitiveRegistry,
) -> Option<crate::node_graph::freeze::install::FusedGeneratorView> {
    let view = crate::node_graph::freeze::install::fuse_generator_view(def, registry);
    if view.is_none() {
        let regions = crate::node_graph::fusion_report(def, registry).regions;
        assert!(regions.is_empty(), "the def has {} fusable regions but does not fuse", regions.len());
    }
    view
}

/// CPU size proofs for every GPU FLIP graph, run before any GPU run of it: the
/// shared liquid extent rules (`liquid::extent`) at every lattice, bare,
/// meshed, rendered and frozen.
#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::node_graph::liquid::extent::{AtomExtent, ExtentError, ExtentReport, ExtentRule, LIQUID_EXTENT_RULES, Verdict, check_graph};
    use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
    use crate::node_graph::{EffectGraphDefExt, ExecutionPlan, Graph, ParamValue, PrimitiveRegistry, compile};

    #[test]
    fn gpu_flip_defaults_disable_optional_corrections_and_preserve_opt_ins() {
        use super::super::gpu_flip_step::GpuFlipStep;
        use crate::node_graph::parameters::ParamValue;
        use crate::node_graph::primitive::PrimitiveSpec;

        assert!(!WaterScene::dam_break(64).volume_projection);
        for name in ["volume_projection", "narrow_band", "solve_level"] {
            let param = GpuFlipStep::PARAMS.iter().find(|p| p.name == name).unwrap();
            assert_eq!(param.default, ParamValue::Float(0.0), "{name}");
        }
        for enabled in [false, true] {
            let def = water_def(WaterScene { volume_projection: enabled, ..WaterScene::dam_break(64) });
            let step = def.nodes.iter().find(|n| n.node_id.as_str() == STEP_NODE).unwrap();
            let params = serde_json::to_value(&step.params).unwrap();
            assert_eq!(params["volume_projection"]["value"], i32::from(enabled));
        }
    }

    fn registry() -> PrimitiveRegistry {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        registry
    }

    /// Test sources hold what the planner gives them; sinks read nothing.
    fn harness_node(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
        Ok(())
    }

    /// Fused regions are the freeze compiler's contract (BUG-2efy (fused
    /// output capacity probe)); the walk sizes what they read and write.
    fn rules(frozen: bool) -> Vec<ExtentRule> {
        let mut rules = LIQUID_EXTENT_RULES.to_vec();
        for type_id in ["test.value_source", "test.face_source", "test.value_sink", "test.liquid_sink", "test.mesh_sink"] {
            rules.push(ExtentRule { type_id, check: harness_node });
        }
        if frozen {
            rules.push(ExtentRule { type_id: "node.wgsl_compute", check: harness_node });
        }
        rules
    }

    fn built(def: &EffectGraphDef) -> (Graph, ExecutionPlan) {
        let graph = def.clone().into_graph(&registry(), &Default::default()).expect("the def builds");
        let plan = compile(&graph).expect("the def compiles");
        (graph, plan)
    }

    /// `def` walked by the liquid extent rules, frozen as the app renders it
    /// when `frozen`.
    pub(in crate::node_graph::primitives) fn walk(def: &EffectGraphDef, frozen: bool) -> Result<ExtentReport, ExtentError> {
        let view = frozen.then(|| fused_as_rendered(def, &registry())).flatten();
        let (mut graph, plan) = if let Some(view) = view {
            let graph = (*view.def).clone().into_graph(&registry(), &view.mesh_rules).expect("the fused def builds");
            let plan = compile(&graph).expect("the fused def compiles");
            (graph, plan)
        } else {
            built(def)
        };
        check_graph(&mut graph, &plan, &rules(frozen))
    }

    fn walked(def: &EffectGraphDef, frozen: bool, what: &str) -> ExtentReport {
        walk(def, frozen).unwrap_or_else(|error| panic!("{what}: {error}"))
    }

    /// Every lattice a scene may use, multiples of 16 from 16 to 256, the
    /// sides between the powers of two included. Each is proven here before
    /// any GPU run at it. 256 holds the pool and column only with a lower
    /// fill: the Dam Break there places more particles than a count carries,
    /// and the domain refuses it by name
    /// (`gpu_flip_dam_break_past_the_count_rail_is_refused`).
    const LATTICES: [usize; 6] = [16, 32, 48, 64, 96, 128];

    #[test]
    fn gpu_flip_mesh_grid_uses_native_solid_coordinates() {
        for resolution in [8, 32, 64] {
            let scene = WaterScene::dam_break(resolution).with_surface();
            let geometry = scene.geometry();
            let outputs = geometry.outputs();
            let read = |name: &str| outputs.iter().find(|(port, _)| *port == name).unwrap().1;
            let surface = geometry.setup.lattice.surface();
            let def = water_def(scene);
            let id = |name: &str| def.nodes.iter().find(|n| n.node_id.as_str() == name).unwrap().id;
            let domain = id("domain");
            let solid = id("mesh_solid");
            let frame = id("frame");
            for (d, axis) in ["x", "y", "z"].into_iter().enumerate() {
                assert_eq!(read(&format!("mesh_nodes_{axis}")), (resolution + 4) as f32);
                assert_eq!(read(&format!("mesh_min_{axis}")), surface.min()[d]);
                for (source, target) in [(format!("mesh_min_{axis}"), format!("lattice_min_{axis}")),
                                         (format!("mesh_nodes_{axis}"), format!("nodes_{axis}"))] {
                    assert!(def.wires.iter().any(|w| w.from_node == domain && w.to_node == solid
                        && w.from_port == source && w.to_port == target));
                }
            }
            assert!(def.wires.iter().any(|w| w.from_node == solid && w.to_node == frame
                && w.from_port == "solid" && w.to_port == "solid"));
            // Frame's surface() uses the same source rule; the simulation
            // lattice continues to carry its original cells and padding.
            assert_eq!(geometry.setup.lattice.cells(), [resolution as u32; 3]);
            assert_eq!(geometry.setup.lattice.nodes(), [resolution as u32 + 7; 3]);
        }
    }

    /// Every running scene at every lattice, and the probes' variants, before
    /// any GPU run of it: the tick region's steps, the stats, the frame and
    /// the surface. The solves run inside each step, so the tick has no
    /// inner region.
    #[test]
    fn gpu_flip_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::deep_pool, WaterScene::deep_drop, WaterScene::free_fall];
        let all = LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).flat_map(|scene| [scene, scene.with_surface()]);
        // The splash probes' scenes.
        let refined = WaterScene::dam_break(128).with_surface();
        let step = WaterScene::dam_break(128);
        let probes = [
            refined.with_iterations(12),
            WaterScene { steps: 4, ..refined },
            step.with_iterations(4),
            WaterScene::dam_break(64).with_steps(1).with_surface(),
        ];
        // The obstacle, bare and meshed, and in the still pool the kinematic
        // proof moves it through.
        let obstacle = LATTICES.into_iter().flat_map(|n| {
            let dam = WaterScene::dam_break(n).with_obstacle();
            [dam, dam.with_surface(), WaterScene::still_pool(n).with_obstacle()]
        });
        // The open-face drain.
        let open = [WaterScene::still_pool(64).with_closed_faces(63 & !1)];
        for scene in all.chain(probes).chain(obstacle).chain(open) {
            let n = scene.pressure.n;
            let def = water_def(scene);
            let (graph, plan) = built(&def);
            let regions = plan.substep_regions();
            assert_eq!(regions.len(), 1, "one tick region");
            let report = walked(&def, false, &format!("scene {n}³, {} steps", scene.steps));
            assert!(report.checked > 7, "checked only {} nodes at {n}³, {} steps", report.checked, scene.steps);
            let meshed = plan.steps().iter().any(|step| {
                graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
            });
            assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
        }
    }

    /// The tick region's body is the tick: the step and the stats, and
    /// nothing the frame or the domain runs once a frame.
    #[test]
    fn gpu_flip_tick_region_is_the_tick() {
        let scene = WaterScene::dam_break(64).with_surface();
        let (graph, plan) = built(&water_def(scene));
        let region = &plan.substep_regions()[0];
        let name = |step: usize| graph.get_node(plan.steps()[step].node).expect("plan node").node_id.as_str().to_string();
        assert_eq!(graph.get_node(region.boundary).expect("boundary").node_id.as_str(), "state");
        let body: Vec<String> = region.steps.iter().map(|&step| name(step)).collect();
        assert_eq!(body.iter().filter(|node| *node == STEP_NODE).count(), 1, "the tick runs one step node");
        assert!(body.iter().any(|node| node == "stats"), "the stats run every tick");
        let outside = ["domain", "fill", "mesh_solid", "frame", "initial_column"];
        assert!(!body.iter().any(|node| outside.contains(&node.as_str()) || node.starts_with("surface")), "{body:?}");
    }

    /// The rendered Dam Break's device bytes at every lattice, for the size
    /// ladder.
    #[test]
    fn gpu_flip_memory_at_every_lattice() {
        for n in LATTICES {
            for scale in [1, 2, 3] {
                let scene = WaterScene::dam_break(n).with_surface_scale(scale);
                let bytes = rendered_scene_bytes(scene);
                println!(
                    "GPU FLIP rendered Dam Break {n}³, surface scale {scale}: {} particles, {:.2} GB",
                    scene.particles(),
                    bytes as f64 / 1e9
                );
                assert!(bytes > 0);
            }
        }
    }

    /// Full array and held-buffer accounting at the shipped resolution. This
    /// excludes textures and later mesh growth; it is not a frame-time proof.
    #[test]
    fn gpu_flip_native_surface_detail_memory_at_64() {
        let coarse = rendered_scene_bytes(WaterScene::dam_break(64).with_surface_scale(1));
        let matched = rendered_scene_bytes(WaterScene::dam_break(64).with_surface_scale(2));
        assert!(matched > coarse);
        println!("GPU FLIP 64³ rendered arrays/held buffers: detail 0 {coarse} bytes; native-matched detail 1 {matched} bytes; increase {} bytes", matched - coarse);
        assert_eq!(WaterScene::dam_break(64).surface_scale, 2);
    }

    /// Resolution is a card: the graph built at 64 runs at any Resolution,
    /// odd and uneven sides included, because every lattice node reads the
    /// domain's wires and the step's faces follow them (BUG-o65k (GPU FLIP
    /// lattice wiring), BUG-9an1 (resolution change)).
    #[test]
    fn gpu_flip_any_resolution_walks_on_the_built_graph() {
        for n in [16, 24, 32, 63, 72, 100, 128] {
            let mut def = render_def(WaterScene::dam_break(64));
            let domain = def.nodes.iter_mut().find(|node| node.node_id.as_str() == "domain").expect("domain");
            domain.params.insert("resolution".into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: n });
            let report = walked(&def, false, &format!("the 64³ graph at Resolution {n}"));
            assert!(report.scene_bytes > 0);
        }
    }

    /// Past the count a wire carries exactly, the domain refuses the Dam
    /// Break by name before any GPU work.
    #[test]
    fn gpu_flip_dam_break_past_the_count_rail_is_refused() {
        let scene = WaterScene::dam_break(128);
        let mut def = water_def(scene);
        let domain = def.nodes.iter_mut().find(|node| node.node_id.as_str() == "domain").expect("domain");
        domain.params.insert("resolution".into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: 256 });
        match walk(&def, false) {
            Err(ExtentError::Refused { node, reason }) => {
                assert!(node.starts_with("domain") && reason.contains("Resolution") && reason.contains("Initial Fill Height"), "{node}: {reason}");
            }
            other => panic!("expected the domain to refuse 256³, got {other:?}"),
        }
    }

    /// The scenes as the render smoke runs them, inside the shipped render
    /// graph (`render_def`), at every lattice, and the shipped lattice at
    /// every Surface Detail. No card overwrites a def param at build, so the
    /// runtime runs the graph this walk checks.
    #[test]
    fn gpu_flip_rendered_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool];
        let coarser = LATTICES.into_iter().flat_map(|n| [1, 2].map(|scale| WaterScene::dam_break(n).with_surface_scale(scale)));
        let detail = (SURFACE_DETAIL_OFFSET..=SURFACE_DETAIL_OFFSET + 2).map(|scale| WaterScene::dam_break(64).with_surface_scale(scale));
        // The cadence probe: one step a tick.
        let cadence = [WaterScene::dam_break(64).with_steps(1)];
        // The published face grid, at every lattice.
        let faces = LATTICES.into_iter().map(|n| WaterScene::dam_break(n).with_faces());
        let registry = PrimitiveRegistry::with_builtin();
        for scene in LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).chain(coarser).chain(detail).chain(cadence).chain(faces) {
            let n = scene.pressure.n;
            let def = render_def(scene);
            let (_, plan) = built(&def);
            assert_eq!(plan.substep_regions().len(), 1, "one tick region");
            let report = walked(&def, false, &format!("rendered {n}³"));
            assert!(report.checked > 7, "checked only {} nodes at {n}³", report.checked);
            let runtime = crate::preset_runtime::PresetRuntime::from_def(def, &registry, None).expect("the rendered scene builds");
            let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
            assert!(shadowed.is_empty(), "{n}³ at surface scale {}: cards overwrite def params: {shadowed:?}", scene.surface_scale);
        }
    }

    /// The fill is the engine's: its site rule on the engine's boxes, read
    /// by the domain.
    #[test]
    fn gpu_flip_dam_break_fill_matches_the_engine_boxes() {
        let at64 = WaterScene::dam_break(64);
        assert_eq!((at64.pool_sites(), at64.box_sites()), (5, [[5, 43], [5, 67], [8, 120]]));
        assert_eq!(at64.particles(), 128 * 5 * 128 + 38 * 62 * 112);
        let at128 = WaterScene::dam_break(128);
        assert_eq!((at128.pool_sites(), at128.box_sites()), (10, [[10, 86], [10, 133], [16, 240]]));
        // The still pool is 1 m of floor and no box; the falling block is 1 m on a side.
        let pool = WaterScene::still_pool(64);
        assert_eq!((pool.pool_sites(), pool.particles()), (32, 128 * 32 * 128));
        let block = WaterScene::free_fall(64);
        assert_eq!((block.pool_sites(), block.particles()), (0, 32 * 32 * 32));
    }

    /// Each fused region of `def` as its members' node ids, `a + b`.
    fn fused_regions(def: &EffectGraphDef) -> Vec<String> {
        let report = crate::node_graph::fusion_report(def, &registry());
        let name = |id: u32| def.nodes.iter().find(|n| n.id == id).map_or("?".to_string(), |n| n.node_id.as_str().to_string());
        report.regions.iter().map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect::<Vec<_>>().join(" + ")).collect()
    }

    /// The frozen graphs at every lattice, before any GPU run of them: the
    /// running scene bare and meshed, and the render graph.
    #[test]
    fn gpu_flip_frozen_graphs_cover_every_dispatch() {
        for n in LATTICES {
            let scene = WaterScene::dam_break(n);
            for scene in [scene.with_surface(), scene.with_faces()] {
                let report = walked(&render_def(scene), true, &format!("frozen render {n}³"));
                assert!(report.checked > 7, "checked only {} nodes at {n}³", report.checked);
            }
        }
    }

    /// The step is one boundary node: nothing in the water fuses.
    #[test]
    fn gpu_flip_step_does_not_fuse() {
        assert_eq!(fused_regions(&water_def(WaterScene::dam_break(64))), Vec::<String>::new());
    }

    /// Nodes by id and wires sorted, so a hand edit's order does not count.
    fn canonical(def: &Value) -> Value {
        let mut def = def.clone();
        def["nodes"].as_array_mut().expect("nodes").sort_by_key(|node| node["id"].as_u64());
        def["wires"].as_array_mut().expect("wires").sort_by_key(|wire| {
            let end = |key: &str| wire[key].as_u64().expect("wire end");
            let port = |key: &str| wire[key].as_str().expect("wire port").to_string();
            (end("fromNode"), port("fromPort"), end("toNode"), port("toPort"))
        });
        def
    }

    /// The tick hands the state the step's faces, whatever the step count.
    #[test]
    fn gpu_flip_state_takes_the_last_steps_extended_faces() {
        for scene in [WaterScene::dam_break(64), WaterScene::dam_break(64).with_steps(2)] {
            let def = serde_json::to_value(water_def(scene)).expect("def");
            let name = |id: &Value| -> String {
                let nodes = def["nodes"].as_array().expect("nodes");
                nodes.iter().find(|n| n["id"] == *id).expect("wired node")["nodeId"].as_str().expect("name").to_string()
            };
            let wires = def["wires"].as_array().expect("wires");
            let into: Vec<_> = wires.iter().filter(|w| w["toPort"] == "faces_in").collect();
            assert_eq!(into.len(), 1, "one faces_in wire");
            assert_eq!(name(&into[0]["toNode"]), "state");
            assert_eq!(name(&into[0]["fromNode"]), STEP_NODE);
            assert_eq!(into[0]["fromPort"], "faces");
        }
    }

    /// Velocity extension follows the native configured CFL and covers the
    /// face grid's published valid layers.
    #[test]
    fn gpu_flip_band_uses_engine_cfl() {
        use super::super::gpu_flip_step::{ENGINE_CFL, band_layers};
        use crate::node_graph::liquid::conformance::FACE_GRID_GPU_FLIP_LAYERS;
        assert_eq!(FACE_GRID_GPU_FLIP_LAYERS, FACE_VALID_LAYERS);
        assert_eq!(band_layers(ENGINE_CFL), 12);
        assert!(band_layers(ENGINE_CFL) >= FACE_VALID_LAYERS);
    }

    /// Solver presets share the authored surface structure. Particle support
    /// and surface detail remain scene-specific tuning on that same graph.
    #[test]
    fn gpu_flip_surface_group_is_shared_with_all_water_presets() {
        let source = surface_group();
        let group = &source["group"];
        assert_eq!(group["nodes"].as_array().unwrap().len(), 33);
        let structure = |value: &Value| {
            let mut value = value.clone();
            for node in value["nodes"].as_array_mut().unwrap() {
                match node["nodeId"].as_str() {
                    Some("liquid_blobs") => node["params"]["particle_scale"]["value"] = json!(0.0),
                    Some("liquid_volume" | "liquid_mesh" | "liquid_bricks") => {
                        node["params"]["resolution_scale"]["value"] = json!(0);
                    }
                    _ => {}
                }
            }
            value
        };
        let registry = PrimitiveRegistry::with_builtin();
        for name in [SHIPPED_PRESET, "WaterDamBreakGpu", "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"] {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap();
            let preset: Value = serde_json::from_str(&json).unwrap();
            let surface = preset["nodes"].as_array().unwrap().iter()
                .find(|n| n["handle"] == "Liquid Surface").unwrap();
            assert_eq!(structure(&surface["group"]), structure(group), "{name}: authored surface drift");
            let particle_scale = if name == SHIPPED_PRESET { 2.2_f32 } else { 3.0 };
            let mut defaults = surface["params"].clone();
            assert_eq!(defaults["particle_scale"]["value"].as_f64().unwrap() as f32, particle_scale, "{name}: particle support");
            defaults["particle_scale"] = source["params"]["particle_scale"].clone();
            assert_eq!(defaults, source["params"], "{name}: other defaults drift");
            let blobs = surface["group"]["nodes"].as_array().unwrap().iter()
                .find(|n| n["nodeId"] == "liquid_blobs").unwrap();
            assert_eq!(blobs["params"]["particle_scale"]["value"].as_f64().unwrap() as f32, particle_scale);
            let surface_detail = if name == SHIPPED_PRESET { 1.0 } else { 0.0 };
            for node in surface["group"]["nodes"].as_array().unwrap().iter()
                .filter(|n| matches!(n["nodeId"].as_str(), Some("liquid_volume" | "liquid_mesh" | "liquid_bricks"))) {
                assert_eq!(node["params"]["resolution_scale"]["value"].as_f64().unwrap(), surface_detail + 1.0, "{name}: {}", node["nodeId"]);
            }
            for list in ["params", "bindings"] {
                let entries = preset["presetMetadata"][list].as_array().unwrap();
                let support: Vec<_> = entries.iter().filter(|entry| entry["id"] == "surface_particle_scale").collect();
                assert_eq!(support.len(), 1, "{name}: one particle support {list} entry");
                assert_eq!(support[0]["defaultValue"].as_f64().unwrap() as f32, particle_scale, "{name}: {list}");
                let detail: Vec<_> = entries.iter().filter(|entry| entry["id"] == "surface_detail").collect();
                assert_eq!(detail.len(), if list == "params" { 1 } else { 3 }, "{name}: surface detail {list} entries");
                for entry in detail {
                    assert_eq!(entry["defaultValue"].as_f64().unwrap(), surface_detail, "{name}: {list}");
                }
            }
            for param in ["stretch", "smoothing", "fill_pits", "smoothing_iterations"] {
                assert!(surface["params"][param]["value"].is_number(), "{name}: {param}");
            }
            let wires = group["wires"].as_array().unwrap();
            let nodes = group["nodes"].as_array().unwrap();
            let mesh = &nodes.iter().find(|n| n["nodeId"] == "liquid_mesh").unwrap()["id"];
            let clamp = &nodes.iter().find(|n| n["typeId"] == "node.clamp_liquid_to_solids").unwrap()["id"];
            for port in ["solid", "solid_nodes_x", "solid_nodes_y", "solid_nodes_z"] {
                let source = |id: &Value| {
                    let wire = wires.iter().find(|w| &w["toNode"] == id && w["toPort"] == port)
                        .unwrap_or_else(|| panic!("{name}: missing {port}"));
                    (wire["fromNode"].clone(), wire["fromPort"].clone())
                };
                assert_eq!(source(mesh), source(clamp), "{name}: mesh and clamp must share {port}");
            }
            let mut destinations = std::collections::HashSet::new();
            for wire in wires {
                assert!(destinations.insert((wire["toNode"].as_u64().unwrap(), wire["toPort"].as_str().unwrap())),
                    "{name}: duplicate input wire {wire}");
            }
            crate::preset_runtime::PresetRuntime::from_json_str(&json, &registry)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let detail = preset["presetMetadata"]["bindings"].as_array().unwrap().iter()
                .filter(|b| b["id"] == "surface_detail").collect::<Vec<_>>();
            assert_eq!(detail.len(), 3, "{name}: detail reaches volume, mesh and bricks");
            assert!(detail.iter().all(|b| b["offset"] == 1.0));
        }
    }

    #[test]
    fn gpu_flip_surface_defaults_match_effective_native_dam_break_cards() {
        let registry = PrimitiveRegistry::with_builtin();
        let native = bundled_preset_json(&PresetTypeId::new("WaterDamBreak")).unwrap();
        let native = crate::preset_runtime::PresetRuntime::from_json_str(&native, &registry).unwrap();
        let gpu = crate::preset_runtime::PresetRuntime::from_def(
            render_def(WaterScene::dam_break(64)), &registry, None,
        ).unwrap();
        let param = |runtime: &crate::preset_runtime::PresetRuntime, node: &str, name: &str| {
            let id = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new(node)).unwrap();
            runtime.graph.get_node(id).unwrap().params.get(name).cloned().unwrap()
        };
        assert_eq!(param(&native, "fluid_surface", "surface_particle_scale"), ParamValue::Float(2.2));
        assert_eq!(param(&native, "fluid_surface", "surface_subdivisions"), ParamValue::Float(1.0));
        assert_eq!(param(&gpu, "liquid_blobs", "particle_scale"), param(&native, "fluid_surface", "surface_particle_scale"));
        for node in ["liquid_volume", "liquid_mesh", "liquid_bricks"] {
            assert_eq!(param(&gpu, node, "resolution_scale"), ParamValue::Float(2.0), "{node}");
        }
        assert_eq!(param(&gpu, "liquid_mesh_relaxation", "value"), param(&native, "fluid_surface", "surface_smoothing"));
        assert_eq!(param(&gpu, "liquid_smooth_mesh", "iterations"), param(&native, "fluid_surface", "surface_smoothing_iterations"));
        assert_eq!(param(&gpu, "liquid_blobs", "stretch"), ParamValue::Float(1.0));
        assert_eq!(param(&gpu, "liquid_blobs", "smoothing"), ParamValue::Float(0.0));
        assert_eq!(param(&gpu, "liquid_smoothing_passes", "value"), ParamValue::Float(0.0));
        assert!(gpu.shadowed_def_params().next().is_none(), "fresh defaults agree with the authored graph");
    }

    /// Native layer 0's water_material in flipEngineVSGPUFLIP.manifold:
    /// saved RGB (.37254903, .7294118, 1), roughness .5540391, absorption
    /// distance .1 and scattering .08826533 override its raw graph defaults.
    /// The other authored values and all omitted PBR defaults match that save.
    #[test]
    fn gpu_flip_water_material_matches_saved_native_reference() {
        let registry = PrimitiveRegistry::with_builtin();
        for def in [render_def(WaterScene::dam_break(64)), particle_view_def()] {
            let material = def.nodes.iter().find(|n| n.node_id.as_str() == "water_material").unwrap();
            assert_eq!(material.params.len(), 18, "the material retains its authored parameter surface");
            let runtime = crate::preset_runtime::PresetRuntime::from_def(def, &registry, None).unwrap();
            let id = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new("water_material")).unwrap();
            let material = runtime.graph.get_node(id).unwrap();
            for (name, expected) in [
                ("ambient", 0.0), ("color_r", 0.37254903), ("color_g", 0.7294118), ("color_b", 1.0),
                ("metallic", 0.0), ("roughness", 0.5540391), ("ior", 1.333), ("transmission", 1.0),
                ("volume_attenuation_color_r", 0.35), ("volume_attenuation_color_g", 0.72), ("volume_attenuation_color_b", 0.8),
                ("volume_attenuation_distance", 0.1), ("volume_geometry", 1.0), ("volume_thickness", 0.09),
                ("volume_scattering_color_r", 0.68), ("volume_scattering_color_g", 0.86), ("volume_scattering_color_b", 0.92),
                ("volume_scattering_density", 0.08826533),
            ] {
                assert_eq!(material.params.get(name), Some(&ParamValue::Float(expected)), "{name}");
            }
            assert!(runtime.shadowed_def_params().next().is_none(), "material defaults agree with the authored graph");
        }
        let preset = shipped_preset();
        for (card, target, expected) in [
            ("water_attenuation", "volume_attenuation_distance", 0.1_f32),
            ("water_scattering", "volume_scattering_density", 0.08826533_f32),
        ] {
            for list in ["params", "bindings"] {
                let entries: Vec<_> = preset["presetMetadata"][list].as_array().unwrap().iter()
                    .filter(|entry| entry["id"] == card).collect();
                assert_eq!(entries.len(), 1, "{card}: {list}");
                assert_eq!(entries[0]["defaultValue"].as_f64().unwrap() as f32, expected);
                if list == "bindings" {
                    assert_eq!(entries[0]["target"]["nodeId"], "water_material");
                    assert_eq!(entries[0]["target"]["param"], target);
                }
            }
        }
    }

    /// The shipped `WaterDamBreakGpuFlip.json` is the builder's Dam Break at 64,
    /// so the tests that build it run what ships. `UPDATE_GPU_FLIP_PRESET=1`
    /// rewrites it from the builder.
    #[test]
    fn gpu_flip_shipped_preset_is_the_builders_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{SHIPPED_PRESET}.json"));
        let built = serde_json::to_value(render_def(WaterScene::dam_break(64).with_faces())).expect("serialise");
        if std::env::var("UPDATE_GPU_FLIP_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the shipped preset");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the shipped preset reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{SHIPPED_PRESET}.json differs from the builder's Dam Break; rerun with UPDATE_GPU_FLIP_PRESET=1");
    }

    /// The shipped `WaterDamBreakParticles.json` is the builder's Particle
    /// View of the shipped Dam Break, and its cards bind the graph it ships.
    /// `UPDATE_GPU_FLIP_PRESET=1` rewrites it; regenerate the Dam Break
    /// first, since this view is built from it.
    #[test]
    fn gpu_flip_particle_view_is_built_from_the_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{PARTICLE_VIEW_PRESET}.json"));
        let def = particle_view_def();
        let built = serde_json::to_value(&def).expect("serialise");
        if std::env::var("UPDATE_GPU_FLIP_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the Particle View");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the Particle View reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{PARTICLE_VIEW_PRESET}.json differs from the builder's Particle View; rerun with UPDATE_GPU_FLIP_PRESET=1");
        let runtime = crate::preset_runtime::PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).expect("the Particle View builds");
        let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
        assert!(shadowed.is_empty(), "the Particle View's cards overwrite def params: {shadowed:?}");
    }

    /// Every per-step duration input in every graph the builder ships is fed
    /// by its domain's accepted interval. Left on its param, one holds 1/60 s
    /// at every Sim Rate: at 30 Hz whitewater emits, ages and moves at half
    /// rate, and export poses moving solids at the wrong time.
    #[test]
    fn gpu_flip_builder_graphs_feed_every_interval_input() {
        let registry = PrimitiveRegistry::with_builtin();
        for (type_id, port) in INTERVAL_DURATION_INPUTS {
            let node = registry.construct(type_id).unwrap_or_else(|| panic!("{type_id} is not registered"));
            assert!(node.inputs().iter().any(|input| input.name == port), "{type_id} has no {port} input");
        }
        let bundled = |name: &'static str| -> EffectGraphDef {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
            serde_json::from_str(&json).expect("the preset parses")
        };
        for (name, def) in [
            (SHIPPED_PRESET, bundled(SHIPPED_PRESET)),
            (PARTICLE_VIEW_PRESET, bundled(PARTICLE_VIEW_PRESET)),
            ("Add Fluid's liquid body", gpu_flip_liquid_body()),
        ] {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let fed_by_domain = |node: u32, port: &str| {
                let feeds: Vec<_> = flat.wires.iter().filter(|w| w.to_node == node && w.to_port == port).collect();
                feeds.len() == 1
                    && feeds[0].from_port == "interval_duration"
                    && flat.nodes.iter().any(|n| n.id == feeds[0].from_node && manifold_core::liquid_domain::is_liquid_domain(&n.type_id))
            };
            let mut checked = 0;
            for node in &flat.nodes {
                for (type_id, port) in INTERVAL_DURATION_INPUTS {
                    if node.type_id == type_id {
                        assert!(fed_by_domain(node.id, port), "{name}: {}.{port} is not fed by its domain's interval_duration", node.node_id.as_str());
                        checked += 1;
                    }
                }
            }
            assert!(checked > 0, "{name} has no interval inputs");
        }
    }

    #[test]
    fn liquid_presets_feed_state_dropped_time_from_their_clock_domain() {
        let mut graphs = vec![
            ("GPU FLIP builder", render_def(WaterScene::dam_break(16).with_faces())),
            ("Add Fluid's liquid body", gpu_flip_liquid_body()),
        ];
        for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET, "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"] {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
            graphs.push((name, serde_json::from_str(&json).expect("the preset parses")));
        }
        for (name, def) in graphs {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let mut checked = 0;
            for state in flat.nodes.iter().filter(|node| matches!(node.type_id.as_str(), "node.liquid_state" | "node.matter_state")) {
                let feeds: Vec<_> = flat.wires.iter().filter(|w| w.to_node == state.id && w.to_port == "dropped_seconds").collect();
                assert_eq!(feeds.len(), 1, "{name}: {} needs one dropped_seconds wire", state.node_id.as_str());
                assert_eq!(feeds[0].from_port, "dropped_seconds");
                let tick_source = flat.wires.iter().find(|w| w.to_node == state.id && w.to_port == "ticks").expect("state has a clock");
                assert_eq!(feeds[0].from_node, tick_source.from_node, "{name}: dropped time belongs to the state's clock");
                assert!(flat.nodes.iter().any(|node| node.id == feeds[0].from_node && manifold_core::liquid_domain::is_liquid_domain(&node.type_id)));
                checked += 1;
            }
            assert!(checked > 0, "{name} has no liquid state");
        }
    }

    #[test]
    fn gpu_flip_presets_feed_retired_speed_from_the_particles_state() {
        for def in [water_def(WaterScene::dam_break(16)), gpu_flip_liquid_body()] {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let mut checked = 0;
            for step in flat.nodes.iter().filter(|node| node.type_id == "node.gpu_flip_step") {
                let particles = flat.wires.iter().find(|wire| wire.to_node == step.id && wire.to_port == "particles").expect("step has particles");
                let speed: Vec<_> = flat.wires.iter().filter(|wire| wire.to_node == step.id && wire.to_port == "retired_max_speed").collect();
                assert_eq!(speed.len(), 1);
                assert_eq!(speed[0].from_node, particles.from_node);
                assert_eq!(speed[0].from_port, "retired_max_speed");
                assert!(flat.nodes.iter().any(|node| node.id == speed[0].from_node && node.type_id == "node.liquid_state"));
                checked += 1;
            }
            assert!(checked > 0);
        }
    }

    /// BUG-215v: whitewater belongs to the liquid region, consumes the
    /// solver phi, and every persistent or rendered result closes through
    /// the boundary. This checks real preset compilation without a GPU.
    #[test]
    fn whitewater_per_tick_preset_closes_the_liquid_region() {
        let def = render_def(WaterScene::dam_break(16).with_faces());
        let (graph, plan) = built(&def);
        let whitewater_node = graph.nodes().find(|n| n.node_id.as_str() == "whitewater").unwrap();
        let whitewater = whitewater_node.id;
        let region = plan.substep_regions().iter().find(|r|
            r.steps.iter().any(|&i| plan.steps()[i].node == whitewater)).expect("whitewater is per tick");
        let wire = |from, from_port: &str, to, to_port: &str| def.wires.iter().any(|w|
            w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port);
        let boundary_name = &graph.get_node(region.boundary).unwrap().node_id;
        let boundary = def.nodes.iter().find(|n| &n.node_id == boundary_name).unwrap().id;
        let whitewater = crate::node_graph::NodeInstanceId(def.nodes.iter()
            .find(|n| n.node_id.as_str() == "whitewater").unwrap().id);
        let distance = def.wires.iter().find(|w| w.to_node == whitewater.0 && w.to_port == "distance").unwrap();
        assert_eq!(distance.from_port, "distance");
        assert!(def.nodes.iter().any(|n| n.id == distance.from_node && n.type_id == "node.gpu_flip_step"));
        for port in ["substep_schedule", "substep_u", "substep_v", "substep_w", "substep_count"] {
            assert!(wire(distance.from_node, port, whitewater.0, port), "missing accepted {port}");
        }
        for port in ["forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices", "first_tick", "regions", "region_count", "shapes", "atlas"] {
            let water=def.wires.iter().find(|w|w.to_node==distance.from_node && w.to_port==port).expect(port);
            assert!(wire(water.from_node, &water.from_port, whitewater.0, port), "{port} must share the liquid source");
        }
        for (output, capture, held) in [
            ("pool_out", "whitewater_pool_in", "whitewater_pool"),
            ("state_out", "whitewater_state_in", "whitewater_state"),
            ("counts_out", "whitewater_counts_in", "whitewater_counts"),
            ("foam_particles", "foam_particles_in", "foam_particles"),
            ("bubble_particles", "bubble_particles_in", "bubble_particles"),
            ("spray_particles", "spray_particles_in", "spray_particles"),
            ("dust_particles", "dust_particles_in", "dust_particles"),
        ] {
            assert!(wire(whitewater.0, output, boundary, capture), "missing {capture}");
            assert!(!def.wires.iter().any(|w| w.from_node == whitewater.0 && w.from_port == output && w.to_node != boundary), "{held} escapes the boundary");
        }
        for frozen in [false, true] {
            walked(&def, frozen, "16³ per-tick whitewater");
        }
    }

    /// The shipped preset loads, saves and reloads unchanged, the Whitewater
    /// group's params and its cards with it.
    #[test]
    fn gpu_flip_preset_round_trips_with_its_whitewater() {
        let shipped = shipped_preset();
        let def: EffectGraphDef = serde_json::from_value(shipped.clone()).expect("the preset loads");
        let loaded = serde_json::to_value(&def).expect("serialise");
        assert!(canonical(&loaded) == canonical(&shipped), "loading {SHIPPED_PRESET}.json dropped or changed a field");
        // A save prints each f32 at its shortest; the reload is the same f32s.
        let saved = serde_json::to_string_pretty(&def).expect("the preset saves");
        let again: EffectGraphDef = serde_json::from_str(&saved).expect("the saved preset reloads");
        let reloaded = serde_json::to_value(&again).expect("serialise");
        assert!(canonical(&reloaded) == canonical(&loaded), "a save and reload changed {SHIPPED_PRESET}.json");
        let nodes = reloaded["nodes"].as_array().expect("nodes");
        let group = nodes.iter().find(|n| n["nodeId"] == "whitewater").expect("the Whitewater group");
        for param in ["capacity", "wavecrest_emission", "min_energy", "max_energy"] {
            assert!(group["params"][param]["value"].is_number(), "the group lost {param}");
        }
        let cards = reloaded["presetMetadata"]["bindings"].as_array().expect("bindings");
        for card in ["whitewater_capacity", "foam_radius", "spray_radius", "bubble_density"] {
            assert!(cards.iter().any(|c| c["id"] == card), "the preset lost the {card} card");
        }
    }
}
