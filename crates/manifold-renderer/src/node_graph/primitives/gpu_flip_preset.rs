//! GPU FLIP, the GPU water solver (docs/GPU_FLIP_PRESSURE_SOLVE.md), as
//! graphs built for any lattice. `water_def` is a running liquid on the
//! liquid seam (docs/LIQUID_SOLVER_SEAM_DESIGN.md P7a): node.gpu_flip_domain's clock runs
//! node.liquid_state's tick region, whose body is one 60 Hz tick of
//! one node.gpu_flip_step of [`STEPS_PER_TICK`] substeps, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakGpuFlip.json`, which is its own Dam Break at 64. Every node
//! reads the domain's lattice off its wires, so a Resolution change reaches
//! the running graph; the params only seed the planned sizes.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use serde_json::{Value, json};

use super::gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry};
use super::gpu_flip_step::{AUTO_PRESSURE_ITERATIONS, DEFAULT_TOP_SPEED, FACE_VALID_LAYERS};
use crate::node_graph::bundled_presets::bundled_preset_json;
#[cfg(all(test, feature = "gpu-proofs"))]
use crate::node_graph::fluid::{FluidDomainLayout, domain_layout};
use crate::node_graph::liquid::grid::FACE_INPUT_PORTS;
use crate::node_graph::transform::Transform;

/// The FLIP Fluids engine's Dam Break tank side, the scenes' default Domain
/// Size; a scene's own `size` is what every measure reads.
const DAM_BREAK_METRES: f64 = 4.0;

/// Water substeps per 60 Hz liquid tick, the step node's Steps. A collider
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
    /// volume and mesh): the shipped Surface Detail 0 is 2, which fits the
    /// frame budget at 64 (BUG-mjhx, surface scale at GPU FLIP 64).
    pub surface_scale: usize,
    /// Publish the face grid: three node.face_sample_component named
    /// [`FACE_NODES`] on the state's faces after the region, into the frame.
    /// The tick always hands its last step's faces to the state.
    pub faces: bool,
    /// Place the free surface where the particles' distance crosses zero
    /// (ghost fluid). Off wires zero distances: air at zero pressure on its
    /// cell centres, the race's comparison.
    pub ghost_fluid: bool,
    /// The step's density projection (Volume Projection); off is the
    /// comparison without it.
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

/// The fastest water a step is built for (m/s): the Dam Break's splash tops
/// out near 13 m/s at 64³ and 19–25 m/s at 128³, the FLIP Fluids engine's
/// at 12 and 17. The step turns it into the CFL guard.
pub(crate) const TOP_SPEED: f64 = DEFAULT_TOP_SPEED as f64;

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
            volume_projection: true,
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
    #[cfg(test)]
    pub fn cell_size(&self) -> f64 {
        self.size / self.pressure.n as f64
    }

    /// `steps` water steps a frame.
    #[cfg(test)]
    pub fn with_steps(self, steps: usize) -> Self {
        Self { steps, ..self }
    }

    #[cfg(test)]
    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    /// The step's CFL guard at this scene's step and cell size.
    #[cfg(all(test, feature = "water-race-probes"))]
    pub fn travel_cells(&self) -> usize {
        let travel = super::gpu_flip_step::travel_cells(DEFAULT_TOP_SPEED, self.step_dt() as f32, self.cell_size() as f32);
        travel as usize
    }

    /// The layers the step extends its projected faces by.
    #[cfg(all(test, feature = "water-race-probes"))]
    pub fn band_layers(&self) -> usize {
        super::gpu_flip_step::band_layers(self.travel_cells() as u32) as usize
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
    b.wires(domain, state, &["ticks", "epoch"]);
    let particles: Port = (state, "out");
    let step = water_step(&mut b, scene, (domain, state));
    b.wire(particles, step, "particles");
    b.wire(count, step, "count");
    b.wire((domain, "reaction"), step, "reaction");
    b.wires(domain, step, &["regions", "region_count"]);
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
    b.wires(domain, state, &["nodes_x", "nodes_y", "nodes_z"]);

    let solid = b.node("solid", "node.liquid_solid_distance", json!({}));
    b.wires(domain, solid, &["bodies", "shapes", "atlas", "closed_faces", "body_count"]);
    b.wires(domain, solid, &LATTICE_WIRES);
    b.wire((domain, "body_rows"), solid, "rows");
    let frame = b.node("frame", "node.liquid_frame", json!({"face_valid_layers": int(FACE_VALID_LAYERS as usize)}));
    b.wire((state, "out"), frame, "particles");
    b.wire((state, "stats"), frame, "stats");
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

/// The nodes `water_def` makes; every other node of the shipped preset is
/// its render.
#[cfg(any(test, feature = "gpu-proofs"))]
fn built_by_water_def(node_id: &str) -> bool {
    let step = node_id.split_once('.').is_some_and(|(step, _)| {
        step.len() > 1 && step.starts_with('s') && step[1..].bytes().all(|b| b.is_ascii_digit())
    });
    step || FACE_NODES.contains(&node_id)
        || OBSTACLE_RENDER.contains(&node_id)
        || matches!(
            node_id,
            "domain" | "initial_column" | "obstacle_transform" | "obstacle_collider" | "fill" | "state" | STEP_NODE | "stats" | "solid" | "frame" | "surface"
        )
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
    let next = def["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1;
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
const SURFACE_DETAIL_OFFSET: usize = 2;

/// A meshed scene inside the shipped preset's render: `water_def`'s water
/// and surface, the preset's other nodes, and the preset's wires between the
/// two, matched by node name.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn render_def(scene: WaterScene) -> EffectGraphDef {
    // The render's Whitewater group reads the face grid.
    let mut def = serde_json::to_value(water_def(scene.with_surface().with_faces())).expect("water def serialises");
    let preset = shipped_preset();
    let id_of = |graph: &Value, name: &str| -> u64 {
        let nodes = graph["nodes"].as_array().expect("nodes");
        let node = nodes.iter().find(|n| n["nodeId"] == name).unwrap_or_else(|| panic!("no node {name}"));
        node["id"].as_u64().expect("numeric id")
    };
    let harness = [id_of(&def, "mesh_sink"), id_of(&def, "output")];
    let ends = |wire: &Value| [wire["fromNode"].as_u64().expect("from"), wire["toNode"].as_u64().expect("to")];
    def["nodes"].as_array_mut().expect("nodes").retain(|n| !harness.contains(&n["id"].as_u64().expect("id")));
    def["wires"].as_array_mut().expect("wires").retain(|w| ends(w).iter().all(|id| !harness.contains(id)));
    let next = def["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1;
    let name_of = |node: &Value| node["nodeId"].as_str().expect("node name").to_string();
    let preset_nodes = preset["nodes"].as_array().expect("preset nodes");
    let render: Vec<&Value> = preset_nodes.iter().filter(|n| !built_by_water_def(&name_of(n))).collect();
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
        .filter(|n| built_by_water_def(&name_of(n)))
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
                // The obstacle's render wires are the builder's own, below.
                if OBSTACLE_RENDER.contains(&name.as_str()) {
                    continue 'wires;
                }
                id_of(&def, name)
            });
        }
        def["wires"].as_array_mut().expect("wires").push(wire);
    }
    if scene.obstacle {
        let transform = id_of(&def, "obstacle_transform");
        let scene_node = id_of(&def, "scene");
        add_obstacle_render(&mut def, transform, scene_node);
    }
    for key in ["name", "description"] {
        def[key] = preset[key].clone();
    }
    def["presetMetadata"] = scene_cards(&preset["presetMetadata"], scene);
    serde_json::from_value(def).expect("render def")
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

/// Sets `resolution_scale` on every surface volume and mesh node in `value`,
/// however deep the group nests them.
fn set_surface_scale(value: &mut Value, scale: usize) {
    match value {
        Value::Object(map) => {
            let surface = map.get("nodeId").is_some_and(|id| id == "liquid_volume" || id == "liquid_mesh");
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
                ("top_speed", float(TOP_SPEED)),
                ("ghost_fluid", int(usize::from(scene.ghost_fluid))),
                ("volume_projection", int(usize::from(scene.volume_projection))),
            ],
        ),
    );
    b.wires(domain, step, &LATTICE_WIRES);
    b.wire((domain, "gravity_x"), step, "gravity_x");
    b.wire((domain, "gravity"), step, "gravity_y");
    b.wire((domain, "gravity_z"), step, "gravity_z");
    b.wires(domain, step, &FIELD_WIRES);
    b.wire((state, "tick_index"), step, "tick_index");
    b.wires(domain, step, &["bodies", "shapes", "atlas", "body_count", "dynamic_bodies", "closed_faces"]);
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
    use crate::node_graph::{EffectGraphDefExt, ExecutionPlan, Graph, PrimitiveRegistry, compile};

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
        let outside = ["domain", "fill", "solid", "frame", "initial_column"];
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

    /// The step's CFL guard and band at the lattices the extent walk covers,
    /// never under the valid layers the frame publishes, which the
    /// conformance row's face grid scene holds.
    #[test]
    fn gpu_flip_band_follows_the_cfl_guard() {
        use super::super::gpu_flip_step::{band_layers, travel_cells};
        use crate::node_graph::liquid::conformance::FACE_GRID_GPU_FLIP_LAYERS;
        assert_eq!(FACE_GRID_GPU_FLIP_LAYERS, FACE_VALID_LAYERS);
        let at = |n: usize, steps: usize| {
            let s = WaterScene::dam_break(n).with_steps(steps);
            let travel = travel_cells(TOP_SPEED as f32, s.step_dt() as f32, s.cell_size() as f32);
            (travel, band_layers(travel))
        };
        let bands = [at(64, 2), at(64, 1), at(128, 2), at(96, 2), at(16, 2)];
        assert_eq!(bands, [(3, 9), (6, 14), (6, 14), (4, 10), (1, 5)]);
        assert!(bands.iter().all(|&(_, band)| band >= FACE_VALID_LAYERS));
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
