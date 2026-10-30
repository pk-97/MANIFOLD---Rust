//! Uses the PIC/FLIP ratio (0.95 FLIP) and the Dam Break scene values from FLIP Fluids fluidsimulation.h `_ratioPICFLIP` and the Dam Break preset (MIT); see THIRD_PARTY_NOTICES.md.
//! Checked against FLIP Fluids the engine's Dam Break scene (WaterDamBreak.json) (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP, the GPU water solver, as
//! graphs built for any lattice. `water_def` is a running liquid on the
//! liquid seam: node.gpu_flip_domain's clock runs
//! node.liquid_state's tick region, whose body is one Sim Rate tick of
//! one node.gpu_flip_step of [`STEPS_PER_TICK`] substeps, then
//! node.liquid_stats; node.liquid_frame publishes each tick to the liquid
//! surface. `render_def` puts it in the render of the shipped
//! `WaterDamBreakGpuFlip.json`, which is its own Dam Break at 64, and
//! `particle_view_def` draws that render's particles for the shipped
//! `WaterDamBreakParticles.json`. Every node reads the domain's cell size,
//! pool and fill box off its wires, so a Cell Size change reaches the
//! running graph; the params only seed the planned sizes.
use manifold_core::effect_graph_def::find_node_mut;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::initial_fill::{FillBox, add_initial_fill};
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use manifold_core::scene_schema::PHYSICS_VIEW_SWITCHES;
use serde_json::{Value, json};

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
use manifold_core::fluid_domain::{FluidDomainLayout, domain_layout};
use manifold_node_engine::scene::transform::Transform;
use manifold_water_gpu_flip::primitives::gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry};
use manifold_water_liquid::clock::INTERVAL_DURATION_INPUTS;

/// The FLIP Fluids engine's Dam Break tank side, the scenes' default Domain
/// Size; a scene's own `size` is what every measure reads.
const DAM_BREAK_METRES: f64 = 4.0;

/// Water substeps per liquid tick, the step node's Steps. A collider
/// moves per substep: each places it where its tick's row has it at the
/// substep's end.
pub(crate) const STEPS_PER_TICK: usize = 1;

manifold_core::testkit_visible! {
/// The main solve's iterations: the step's Auto.
pub(crate) const PRESSURE_ITERATIONS: usize = manifold_water_gpu_flip::primitives::gpu_flip_pressure::MAX_ITERATIONS as usize;
}

manifold_core::testkit_visible! {
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressureShape {
    /// Cells per side of the cubic lattice.
    pub n: usize,
    /// Conjugate gradient iterations, one V-cycle each.
    pub iterations: usize,
}
}

impl PressureShape {
    pub fn at(n: usize) -> Self {
        Self {
            n,
            iterations: PRESSURE_ITERATIONS,
        }
    }
}

manifold_core::testkit_visible! {
/// The FLIP Fluids engine's Dam Break (`WaterDamBreak.json`): a 4 m tank
/// over the floor, a 0.16 m pool, and the `initial_column` block, seeded by
/// the engine's half-cell site rule.
pub(crate) const DAM_FILL_HEIGHT: f64 = 0.16;
}
manifold_core::testkit_visible! {
pub(crate) const DAM_COLUMN: [[f64; 2]; 3] = [[-1.84, -0.66], [0.16, 2.08], [-1.75, 1.75]];
}
manifold_core::testkit_visible! {
/// The Dam Break's box obstacle, `WaterDamBreak.json`'s `obstacle_transform`:
/// a unit cube scaled to 0.6 × 1.16 × 0.85 m standing on the floor in the
/// column's path. Position, then scale.
pub(crate) const DAM_OBSTACLE: [[f64; 3]; 2] = [[0.35, 0.58, -0.1], [0.6, 1.16, 0.85]];
}

/// The material a new Water draws with: clear water at stage scale, a look
/// the user stylises from. Pure water absorbs about 0.26, 0.054 and 0.0098
/// per metre at 615, 545 and 465 nm (Pope and Fry 1997), so a metre passes
/// 77, 95 and 99 % of red, green and blue; it scatters too little to see
/// over metres, has no colour of its own, and a calm surface is close to a
/// mirror.
pub(crate) const CLEAR_WATER: [(&str, f32); 12] = [
    ("ambient", 0.0),
    ("color_r", 1.0),
    ("color_g", 1.0),
    ("color_b", 1.0),
    ("metallic", 0.0),
    ("roughness", 0.03),
    ("ior", 1.333),
    ("transmission", 1.0),
    ("volume_attenuation_color_r", 0.77),
    ("volume_attenuation_color_g", 0.95),
    ("volume_attenuation_color_b", 0.99),
    ("volume_attenuation_distance", 1.0),
];

manifold_core::testkit_visible! {
/// A liquid in a cubic tank: a pool `fill_height` deep plus one box, both
/// in metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaterScene {
    pub pressure: PressureShape,
    /// The tank's side in metres: the domain's Domain Size.
    pub size: f64,
    /// Water substeps per tick: the step node's Steps.
    pub steps: usize,
    pub fill_height: f64,
    pub column: [[f64; 2]; 3],
    /// Mesh the liquid with the shipped GPU liquid surface.
    pub surface: bool,
    /// Surface lattice nodes per cell (`resolution_scale` of the surface's
    /// volume and mesh): Surface Detail 0 is subdivision 1, the FLIP Fluids
    /// engine default.
    pub surface_scale: usize,
    /// Place the free surface where the particles' distance crosses zero
    /// (ghost fluid). Off wires zero distances: air at zero pressure on its
    /// cell centres, the race's comparison.
    pub ghost_fluid: bool,
    /// Optional non-native density projection (Volume Projection). Off by
    /// default: matched-input motion is closer to native without its extra
    /// solve. Explicit saved opt-ins remain supported.
    pub volume_projection: bool,
    /// The tank's closed faces, bit 2d the low face of axis d and bit 2d + 1
    /// the high one; an open face's Closed param is off on the domain.
    pub closed_faces: u32,
    /// The Dam Break's box as a scene Collider ([`DAM_OBSTACLE`]): a Fixed
    /// body in a Physics World the render scene couples to the water. Only
    /// [`render_def`] draws it, since the coupling is the render scene's.
    #[allow(dead_code)]
    pub obstacle: bool,
    /// GPU FLIP Narrow Band: particles only near the surface, the deep water
    /// on the step's grid, drawn through `node.interior_fill`.
    pub narrow_band: bool,
}
}

/// The domain's Closed params, in mask bit order.
const CLOSED_PARAMS: [&str; 6] = [
    "closed_neg_x",
    "closed_pos_x",
    "closed_neg_y",
    "closed_pos_y",
    "closed_neg_z",
    "closed_pos_z",
];

manifold_core::testkit_visible! {
/// The water step node in every scene.
pub(crate) const STEP_NODE: &str = "step";
}

/// Particles per cell the fill seeds: one per half-cell site.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub const REST_PER_CELL: f64 = 8.0;

impl WaterScene {
    /// The engine's Dam Break; its box obstacle is a scene Collider
    /// (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision shapes)).
    pub fn dam_break(n: usize) -> Self {
        Self {
            pressure: PressureShape::at(n),
            size: DAM_BREAK_METRES,
            steps: STEPS_PER_TICK,
            fill_height: DAM_FILL_HEIGHT,
            column: DAM_COLUMN,
            surface: false,
            surface_scale: 1,
            ghost_fluid: true,
            volume_projection: false,
            closed_faces: 63,
            obstacle: false,
            narrow_band: false,
        }
    }

    /// The scene with Narrow Band on.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_narrow_band(self) -> Self {
        Self {
            narrow_band: true,
            ..self
        }
    }

    /// The scene with only the faces in `mask` closed.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_closed_faces(self, mask: u32) -> Self {
        Self {
            closed_faces: mask,
            ..self
        }
    }

    /// The scene with the Dam Break's box obstacle as a scene Collider.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_obstacle(self) -> Self {
        Self {
            obstacle: true,
            ..self
        }
    }

    /// The Dam Break the FLIP engine races, as `race_probe` and the race
    /// clips run the engine.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn race_dam_break(n: usize) -> Self {
        Self::dam_break(n)
    }

    /// A pool 1 m deep and nothing else (I5).
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub fn still_pool(n: usize) -> Self {
        Self {
            fill_height: 1.0,
            column: [[0.0; 2]; 3],
            ..Self::dam_break(n)
        }
    }

    /// A pool `fill` deep in a tank `size` on a side at `n` cells: the
    /// conformance box scenes' water.
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub fn pool(n: usize, size: f64, fill: f64) -> Self {
        Self {
            size,
            fill_height: fill,
            ..Self::still_pool(n)
        }
    }

    /// A pool 3 m deep in the 4 m tank: the coarsest multigrid level is
    /// water but for its top row, the hardest case for the coarse solve.
    #[cfg(any(test, feature = "testkit"))]
    pub fn deep_pool(n: usize) -> Self {
        Self {
            fill_height: 3.0,
            ..Self::still_pool(n)
        }
    }

    /// The deep pool with a 1 m × 0.5 m × 1 m block dropped in from 0.2 m
    /// above: a still pool's density source is zero, this one crowds.
    #[cfg(any(test, feature = "testkit"))]
    pub fn deep_drop(n: usize) -> Self {
        Self {
            column: [[-0.5, 0.5], [3.2, 3.7], [-0.5, 0.5]],
            ..Self::deep_pool(n)
        }
    }

    /// A 0.9 m pool with a 0.2 m slab over its left half: 1 m mean depth,
    /// a step in the surface whose sloshing is mostly the tank's first
    /// standing wave.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn slosh(n: usize) -> Self {
        Self {
            fill_height: 0.9,
            column: [[-2.0, 0.0], [0.9, 1.1], [-2.0, 2.0]],
            ..Self::dam_break(n)
        }
    }

    /// A 1 m block of water high in the tank, clear of every wall.
    #[cfg(any(test, feature = "testkit"))]
    pub fn free_fall(n: usize) -> Self {
        Self {
            fill_height: 0.0,
            column: [[-0.5, 0.5], [2.5, 3.5], [-0.5, 0.5]],
            ..Self::still_pool(n)
        }
    }

    pub fn with_surface(self) -> Self {
        Self {
            surface: true,
            ..self
        }
    }

    /// Meshed at `scale` surface nodes per cell.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_surface_scale(self, scale: usize) -> Self {
        Self {
            surface: true,
            surface_scale: scale,
            ..self
        }
    }

    /// The same scene with `iterations` per pressure solve.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_iterations(self, iterations: usize) -> Self {
        Self {
            pressure: PressureShape {
                iterations,
                ..self.pressure
            },
            ..self
        }
    }

    /// The cell side in metres: the tank's side over the lattice.
    pub fn cell_size(&self) -> f64 {
        self.size / self.pressure.n as f64
    }

    /// `steps` water steps a frame.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_steps(self, steps: usize) -> Self {
        Self { steps, ..self }
    }

    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn step_dt(&self) -> f64 {
        1.0 / (60.0 * self.steps as f64)
    }

    /// The tank: the domain's layout at this cell size, no domain box.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn layout(&self) -> FluidDomainLayout {
        domain_layout(None, self.size as f32, self.cell_size()).expect("the tank's layout")
    }

    /// The tank's lowest corner.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
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

    /// The domain's own reading of the scene: the fill's sites, its box, the
    /// pool and the particle count, from node.gpu_flip_domain's function.
    pub fn geometry(&self) -> GpuFlipGeometry {
        let read = |name: &str, default: f32| match name {
            "cell_metres" => self.cell_size() as f32,
            "domain_size" => self.size as f32,
            "fill_height" => self.fill_height as f32,
            _ => default,
        };
        gpu_flip_geometry(read, None, self.initial_volume()).expect("the scene fits its domain")
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn pool_sites(&self) -> u32 {
        self.geometry().pool_sites
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn box_sites(&self) -> [[u32; 2]; 3] {
        self.geometry().box_sites
    }

    /// Particles the fill places; the particle arrays hold exactly this many.
    #[cfg(any(test, feature = "testkit"))]
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
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
}

type Port = (usize, &'static str);

impl Builder {
    fn node(&mut self, name: &str, type_id: &str, params: Value) -> usize {
        let id = self
            .nodes
            .iter()
            .map(|n| n.id as usize + 1)
            .max()
            .unwrap_or(0);
        self.nodes.push(
            serde_json::from_value(
                json!({"id": id, "nodeId": name, "typeId": type_id, "params": params}),
            )
            .expect("recipe node"),
        );
        id
    }

    fn wire(&mut self, from: (usize, &str), to: usize, port: &str) {
        self.wires.push(EffectGraphWire {
            from_node: from.0 as u32,
            from_port: from.1.into(),
            to_node: to as u32,
            to_port: port.into(),
        });
    }

    /// The same-named scalar outputs of `from` into `to`.
    fn wires(&mut self, from: usize, to: usize, ports: &[&'static str]) {
        for &port in ports {
            self.wire((from, port), to, port);
        }
    }

    /// The column as an Initial Fill beside `domain`: the `initial_column`
    /// transform inside the group, wired into the first free role input.
    fn initial_fill(&mut self, domain: usize, volume: Transform) {
        self.node(
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
        let column = self.nodes.pop().expect("just added");
        let mut taken = self
            .nodes
            .iter()
            .map(|node| node.node_id.to_string())
            .collect();
        add_initial_fill(
            &mut self.nodes,
            &mut self.wires,
            domain as u32,
            FillBox::Node(Box::new(column)),
            &mut taken,
        );
    }

    fn from_def(def: EffectGraphDef) -> Self {
        Self {
            nodes: def.nodes,
            wires: def.wires,
        }
    }

    /// Look up a node in this recipe scope; callers enter a group explicitly.
    fn id(&self, name: &str) -> usize {
        self.nodes
            .iter()
            .find(|n| n.node_id.as_str() == name)
            .unwrap_or_else(|| panic!("recipe scope has no {name}"))
            .id as usize
    }

    fn remove(&mut self, names: &[&str]) {
        let ids: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| names.contains(&n.node_id.as_str()))
            .map(|n| n.id)
            .collect();
        self.nodes.retain(|n| !ids.contains(&n.id));
        self.wires
            .retain(|w| !ids.contains(&w.from_node) && !ids.contains(&w.to_node));
    }

    fn finish(self) -> EffectGraphDef {
        serde_json::from_value(json!({"version": 3, "nodes": self.nodes, "wires": self.wires}))
            .expect("recipe graph")
    }
}

/// Feeds every interval input ([`INTERVAL_DURATION_INPUTS`]) among `nodes`
/// that no wire feeds yet from `domain`'s accepted interval.
fn feed_intervals(nodes: &[EffectGraphNode], wires: &mut Vec<EffectGraphWire>, domain: u32) {
    for node in nodes {
        for &(type_id, port) in &INTERVAL_DURATION_INPUTS {
            if node.type_id != type_id
                || wires
                    .iter()
                    .any(|w| w.to_node == node.id && w.to_port == port)
            {
                continue;
            }
            wires.push(EffectGraphWire {
                from_node: domain,
                from_port: "interval_duration".into(),
                to_node: node.id,
                to_port: port.into(),
            });
        }
    }
}

/// The fill's box, as the domain publishes it: its first corner and cell
/// size in metres, then its cells per axis.
const FILL_BOX_WIRES: [&str; 7] = [
    "fill_min_x",
    "fill_min_y",
    "fill_min_z",
    "cell_size",
    "fill_cells_x",
    "fill_cells_y",
    "fill_cells_z",
];

/// The step's cell size and tile pool, as the domain publishes them.
const POOL_WIRES: [&str; 2] = ["cell_size", "pool_tiles"];

/// The Bounds box in metres as placed, as the domain publishes it.
const BOUNDS_WIRES: [&str; 6] = [
    "bounds_lo_x",
    "bounds_lo_y",
    "bounds_lo_z",
    "bounds_hi_x",
    "bounds_hi_y",
    "bounds_hi_z",
];

/// Each Bounds face plane's velocity along its axis, as the domain
/// publishes it.
const WALL_VELOCITY_WIRES: [&str; 6] = [
    "wall_velocity_lo_x",
    "wall_velocity_lo_y",
    "wall_velocity_lo_z",
    "wall_velocity_hi_x",
    "wall_velocity_hi_y",
    "wall_velocity_hi_z",
];

/// The colliders and the shaped fill's region the fill seeds around, as the
/// domain publishes them.
pub(crate) const FILL_SHAPE_WIRES: [&str; 9] = [
    "bodies",
    "shapes",
    "atlas",
    "collider_nodes",
    "collider_triangles",
    "regions",
    "fill_region",
    "body_count",
    "epoch",
];

/// The fill's sites, as the domain publishes them.
const FILL_WIRES: [&str; 7] = [
    "pool_sites",
    "box_x0",
    "box_x1",
    "box_y0",
    "box_y1",
    "box_z0",
    "box_z1",
];

/// The scene's forces and impulses, as the domain publishes them.
const FIELD_WIRES: [&str; 9] = [
    "forces",
    "impulses",
    "field_nodes_x",
    "field_nodes_y",
    "field_nodes_z",
    "field_spacing",
    "force_lattices",
    "first_tick",
    "impulse_tick",
];

/// The domain's bodies and walls into a node that rasterises the solids on
/// world tiles: whitewater's obstacle source or the display's solid.
fn solid_wires(b: &mut Builder, domain: usize, node: usize) {
    b.wires(
        domain,
        node,
        &[
            "bodies",
            "shapes",
            "atlas",
            "collider_nodes",
            "collider_triangles",
            "closed_faces",
            "body_count",
        ],
    );
    b.wire((domain, "body_rows"), node, "rows");
    b.wires(domain, node, &BOUNDS_WIRES);
}

/// The display table's tiles, pool and cell size into `node`.
fn on_display_tiles(b: &mut Builder, tiles: usize, node: usize) {
    b.wires(tiles, node, &["tiles", "pool_tiles", "cell_size"]);
}

manifold_core::testkit_visible! {
/// A scene as a running liquid on the seam. The domain seeds the fill and
/// runs the clock; the state's region runs one tick per due tick: `steps`
/// water steps from `state.out`, then the tick's stats, closing into
/// `state.in` and `state.stats_in`. The frame publishes each tick with the
/// solid lattice. The harness sink holds the frame, or the surface's level set
/// when `surface`.
pub(crate) fn water_def(scene: WaterScene) -> EffectGraphDef {
    let mut b = Builder::default();
    let geometry = scene.geometry();
    let mut params = json!({
        "cell_metres": float(scene.cell_size()),
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
        b.initial_fill(domain, volume);
    }
    // The params hold the domain's own sites, so the planned storage is the
    // fill's; the wires carry any change.
    let sites = geometry.box_sites;
    let fill = b.node(
        "fill",
        "node.liquid_fill",
        seeded(
            &scene,
            &FILL_BOX_WIRES,
            &[
                ("pool_sites", int(geometry.pool_sites as usize)),
                ("box_x0", int(sites[0][0] as usize)),
                ("box_x1", int(sites[0][1] as usize)),
                ("box_y0", int(sites[1][0] as usize)),
                ("box_y1", int(sites[1][1] as usize)),
                ("box_z0", int(sites[2][0] as usize)),
                ("box_z1", int(sites[2][1] as usize)),
                ("jitter", float(SEED_JITTER)),
                ("seed", int(0)),
            ],
        ),
    );
    b.wires(domain, fill, &FILL_WIRES);
    b.wires(domain, fill, &FILL_BOX_WIRES);
    b.wires(domain, fill, &FILL_SHAPE_WIRES);
    b.wires(domain, fill, &BOUNDS_WIRES);
    b.wire((domain, "closed_faces"), fill, "closed_faces");
    let state = b.node("state", "node.liquid_state", json!({}));
    b.wire((fill, "particles"), state, "seed");
    b.wire((fill, "count"), state, "count");
    b.wires(domain, state, &["ticks", "epoch", "simulation_time", "target_time", "dropped_seconds"]);
    // The inflows' most births a tick: the state's storage grows ahead of them.
    b.wire((domain, "region_site_total"), state, "emit_sites");
    let particles: Port = (state, "out");
    let step = water_step(&mut b, scene, (domain, state));
    if scene.narrow_band {
        // The grid's water and the table it lives on leave the tick for the
        // display.
        b.wire((step, "interior"), state, "interior_in");
        b.wire((step, "tiles"), state, "interior_tiles_in");
    }
    b.wire(particles, step, "particles");
    b.wire((state, "identity"), step, "identity");
    b.wire((step, "identity_out"), state, "identity_in");
    b.wire((domain, "reaction"), step, "reaction");
    b.wires(
        domain,
        step,
        &["regions", "region_count", "region_sites", "region_site_total"],
    );
    let particles = (step, "out");
    let stats = b.node("stats", "node.liquid_stats", json!({}));
    b.wire(particles, stats, "particles");
    b.wire((state, "stats"), stats, "stats");
    b.wire((step, "identity_out"), stats, "identity");
    b.wire((domain, "particle_mass"), stats, "particle_mass");
    b.wire((step, "capped"), stats, "capped");
    b.wire(particles, state, "in");
    b.wire((stats, "stats_out"), state, "stats_in");

    let frame = b.node("frame", "node.liquid_frame", json!({}));
    b.wire((state, "out"), frame, "particles");
    b.wire((state, "stats"), frame, "stats");
    b.wire((state, "identity"), frame, "identity");
    b.wires(domain, frame, &["simulation_time", "display_time", "epoch", "display_cursor", "dropped_seconds"]);

    let output = b.node("output", "system.final_output", json!({}));
    let sink = if scene.surface {
        let level_set = surface(&mut b, scene, frame);
        let sink = b.node("surface_sink", "test.value_sink", json!({}));
        b.wire(level_set, sink, "values");
        sink
    } else {
        let sink = b.node("sink", "test.liquid_sink", json!({}));
        b.wire((frame, "particles_b"), sink, "particles");
        sink
    };
    b.wire((sink, "out"), output, "in");
    // Solids pose their bodies at the end of the accepted interval, so they
    // agree with the step at every Sim Rate.
    feed_intervals(&b.nodes, &mut b.wires, domain as u32);
    serde_json::from_value(json!({"version": 3, "nodes": b.nodes, "wires": b.wires})).expect("water def")
}
}

manifold_core::testkit_visible! {
/// The GPU FLIP Dam Break engine scene (`manifold-nodes/tests/fixtures/presets`) supplies the authored camera, lights,
/// environment, tank and tone map. The family itself is constructed by
/// the recipe below.
pub(crate) const SHIPPED_PRESET: &str = "WaterDamBreakGpuFlip";
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn shipped_preset() -> Value {
    let json =
        include_str!("../../../manifold-nodes/tests/fixtures/presets/WaterDamBreakGpuFlip.json");
    serde_json::from_str(json).expect("the GPU FLIP preset parses")
}

/// The liquid surface group, kept with the solver that feeds it.
fn surface_group() -> Value {
    serde_json::from_str(include_str!("../../assets/liquid_surface_group.json"))
        .expect("the liquid surface group parses")
}

/// A bubble's material: a silvery mirror, drawn before the water.
pub(crate) const SILVER_BUBBLE: [(&str, f32); 3] = [
    ("metallic", 1.0),
    ("roughness", 0.15),
    ("transmission", 0.0),
];

/// Stage and capture populations, including the unrendered dust oracle.
pub(super) const WHITEWATER_KINDS: [&str; 4] = ["foam", "bubble", "spray", "dust"];
/// Child looks in family-output order. Dust has no display chain.
const RENDERED_KINDS: [&str; 2] = ["spray", "bubble"];
const FAMILY_OUTPUTS: [&str; 3] = ["object", "object_1", "object_2"];
/// Physics View's dots: a fourth output beside the three recognised roles.
const DOTS_OUTPUT: &str = "object_4";
/// The render slot the dots take: the one the tank layout leaves empty.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const DOTS_RENDER_SLOT: &str = "object_6";
/// Physics View's switches, the field's on the first. The card drives them
/// all and the first owns it.
const FIELD_SWITCH: &str = PHYSICS_VIEW_SWITCHES[0];
const VELOCITY_SWITCH: &str = PHYSICS_VIEW_SWITCHES[1];
const TILES_SWITCH: &str = PHYSICS_VIEW_SWITCHES[2];
const EXTENT_SWITCH: &str = PHYSICS_VIEW_SWITCHES[3];
const WATER_VISIBLE: &str = PHYSICS_VIEW_SWITCHES[4];
const DOTS_VISIBLE: &str = PHYSICS_VIEW_SWITCHES[5];
const DOTS_INSTANCES_SWITCH: &str = PHYSICS_VIEW_SWITCHES[6];
const DOTS_MESH_SWITCH: &str = PHYSICS_VIEW_SWITCHES[7];
const MIST_SWITCH: &str = PHYSICS_VIEW_SWITCHES[8];
const WETNESS_SWITCH: &str = PHYSICS_VIEW_SWITCHES[9];
const VERTICES_SWITCH: &str = PHYSICS_VIEW_SWITCHES[10];
/// The display's world tiles and the solid on them, shared by every drawn
/// field (`SURFACE_DETAIL_NODES` names the table).
const DISPLAY_TILES: &str = manifold_core::scene_schema::SURFACE_DETAIL_NODES[0];
const DISPLAY_SOLID: &str = "display_solid";
/// The Platonic shape a physics dot is drawn with, scaled by the particle's radius.
const TETRAHEDRON: usize = 0;

/// The share of sheet seeds the shipped water keeps, FLIP Fluids' sheet fill
/// rate. On by default so thin sheets keep their water.
pub const SHEET_FILL_RATE: f64 = 0.5;

/// The body output retained by the insertion template.
pub const LIQUID_BODY_OUTPUT: &str = "fluid_output";

impl WaterScene {
    /// The shared insertion body: simulation, captured display and its looks.
    /// Preset colliders enter through the ordinary domain role interface.
    fn family_def(self) -> EffectGraphDef {
        let mut b = Builder::from_def(water_def(self.with_surface()));
        b.remove(&["surface_sink", "output"]);
        let [domain, state, step, frame, surface] =
            ["domain", "state", STEP_NODE, "frame", "surface"].map(|n| b.id(n));
        find_node_mut(&mut b.nodes, "domain")
            .expect("family domain")
            .params
            .insert(
                "sheet_fill_rate".into(),
                serde_json::from_value(float(SHEET_FILL_RATE)).expect("sheet fill rate"),
            );
        let whitewater = b.node(
            "whitewater",
            "node.whitewater_step",
            json!({
                "capacity": int(100_000), "wavecrest_emission": float(175.0),
                "min_energy": float(0.1), "max_energy": float(60.0), "amount": float(1.0),
            }),
        );
        let budget = b.node(
            "whitewater_budget",
            "node.value",
            json!({"value": float(100_000.0)}),
        );
        b.wire((budget, "out"), whitewater, "capacity");
        b.wire((budget, "out"), state, "whitewater_capacity");
        // The bodies' distance and emission source on the step's world tiles.
        let source = b.node(
            "whitewater_obstacle_source",
            "node.whitewater_obstacle_source",
            json!({}),
        );
        solid_wires(&mut b, domain, source);
        b.wire((domain, "cell_size"), source, "cell_size");
        b.wires(step, source, &["tiles", "pool_tiles"]);
        b.wires(
            step,
            whitewater,
            &[
                "distance",
                "faces",
                "tiles",
                "pool_tiles",
                "substep_schedule",
                "substep_faces",
                "substep_count",
            ],
        );
        b.wire((step, "out"), whitewater, "particles");
        b.wire((step, "identity_out"), whitewater, "identity");
        b.wire((source, "distance"), whitewater, "solid");
        b.wire((source, "source"), whitewater, "obstacle_source");
        b.wires(
            domain,
            whitewater,
            &[
                "cell_size",
                "bounds_lo_x",
                "bounds_lo_y",
                "bounds_lo_z",
                "bounds_hi_x",
                "bounds_hi_y",
                "bounds_hi_z",
                "open_faces",
                "kill_height",
                "epoch",
                "gravity_x",
                "gravity",
                "gravity_z",
                "forces",
                "impulses",
                "field_nodes_x",
                "field_nodes_y",
                "field_nodes_z",
                "field_spacing",
                "force_lattices",
                "impulse_tick",
                "first_tick",
                "regions",
                "region_count",
                "shapes",
                "atlas",
                "collider_nodes",
                "collider_triangles",
            ],
        );
        b.wire((state, "tick_index"), whitewater, "tick_index");
        for (held, input) in [
            ("whitewater_pool", "pool"),
            ("whitewater_state", "pool_state"),
        ] {
            b.wire((state, held), whitewater, input);
        }
        for (output, capture) in [
            ("pool_out", "whitewater_pool_in"),
            ("state_out", "whitewater_state_in"),
            ("counts_out", "whitewater_counts_in"),
        ] {
            b.wire((whitewater, output), state, capture);
        }
        for kind in WHITEWATER_KINDS {
            b.wire(
                (whitewater, &format!("{kind}_particles")),
                state,
                &format!("{kind}_particles_in"),
            );
            // All four captures remain observable; foam draws as the water's
            // cover, spray and bubbles as objects.
            b.wire(
                (state, &format!("{kind}_particles")),
                frame,
                &format!("{kind}_in"),
            );
        }

        let water = display_head(&mut b, frame, surface);
        let output = b.node(LIQUID_BODY_OUTPUT, "system.group_output", json!({}));
        b.wire((water, "object"), output, FAMILY_OUTPUTS[0]);
        for (index, kind) in RENDERED_KINDS.into_iter().enumerate() {
            // Spray is clear drops. A bubble seen from the water is a bright
            // silvery ball, light reflected whole off its inside, so it is a
            // mirror drawn before the water and seen through it.
            let (radius, roughness, transmission, metallic) = match kind {
                "spray" => (0.003, 0.12, 1.0, 0.0),
                "bubble" => (
                    0.0035,
                    f64::from(SILVER_BUBBLE[1].1),
                    f64::from(SILVER_BUBBLE[2].1),
                    f64::from(SILVER_BUBBLE[0].1),
                ),
                _ => unreachable!("rendered kind"),
            };
            let mesh = b.node(
                &format!("{kind}_mesh"),
                "node.platonic_solid_mesh",
                json!({"radius": float(radius), "shape": {"type": "Enum", "value": 3}}),
            );
            let params = json!({
                "color_r": float(0.95), "color_g": float(0.98), "color_b": float(1.0), "ior": float(1.333),
                "metallic": float(metallic), "roughness": float(roughness), "transmission": float(transmission),
                "volume_thickness": float(radius * 2.0),
            });
            let material = b.node(&format!("{kind}_material"), "node.pbr_material", params);
            let object = b.node(
                &format!("{kind}_object"),
                "node.scene_object",
                json!({"cast_shadows": float(0.0)}),
            );
            let copies = b.node(
                &format!("{kind}_copies"),
                "node.particles_to_copies",
                json!({}),
            );
            let display = b.node(
                &format!("{kind}_blend"),
                "node.interpolate_particle_frames",
                json!({}),
            );
            b.wire((frame, &format!("{kind}_b")), display, "particles_b");
            b.wires(frame, display, &["blend", "span"]);
            b.wire((display, "out"), copies, "particles");
            b.wire((copies, "copies"), object, "instances");
            b.wire((mesh, "vertices"), object, "vertices");
            b.wire((material, "out"), object, "material");
            b.wire((object, "object"), output, FAMILY_OUTPUTS[index + 1]);
            if kind == "spray" {
                for (from, to) in [
                    ("gravity_x", "acceleration_x"),
                    ("gravity", "acceleration_y"),
                    ("gravity_z", "acceleration_z"),
                ] {
                    b.wire((domain, from), display, to);
                }
            }
        }
        display_foam(&mut b, frame, water);
        let level_set = drawn_level_set(&b, self, surface);
        display_mist_and_wetness(&mut b, water);
        let dots = display_dots(&mut b, frame, (surface, level_set), water);
        b.wire((dots, "object"), output, DOTS_OUTPUT);
        // Resolve durations inside the domain-owning scope before it is wrapped.
        feed_intervals(&b.nodes, &mut b.wires, domain as u32);
        for node in &mut b.nodes {
            if node.group.is_some() {
                continue;
            }
            node.handle = match node.node_id.as_str() {
                "water_material" => Some("Water Material"),
                "water_object" => Some("Water"),
                "water_ripple_detail" => Some("Ripple Detail"),
                "water_mesh" => Some("Water Mesh"),
                "whitewater" => Some("Whitewater"),
                "whitewater_budget" => Some("Whitewater Budget"),
                "spray_object" => Some("Spray"),
                "bubble_object" => Some("Bubbles"),
                "spray_mesh" => Some("Spray Mesh"),
                "spray_material" => Some("Spray Material"),
                "spray_copies" => Some("Spray Copies"),
                "bubble_mesh" => Some("Bubble Mesh"),
                "bubble_material" => Some("Bubble Material"),
                "bubble_copies" => Some("Bubble Copies"),
                "water_mist" => Some("Mist"),
                "water_wetness" => Some("Wetness"),
                "dots_object" => Some("Physics Dots"),
                _ => None,
            }
            .map(str::to_owned);
        }
        let mut def = b.finish();
        // Author the parent gate once. Insertion remaps this metadata and
        // presets copy it intact; neither consumer recreates the target list.
        let mut bindings: Vec<_> = FAMILY_OUTPUTS
            .iter()
            .chain([&DOTS_OUTPUT])
            .map(|port| {
                let wire = def
                    .wires
                    .iter()
                    .find(|wire| wire.to_node == output as u32 && wire.to_port == *port)
                    .expect("family object output");
                let object = def
                    .nodes
                    .iter()
                    .find(|node| node.id == wire.from_node)
                    .expect("family object");
                json!({"id":"parent_visible", "label":"Visible", "defaultValue":1.0,
                "convert":{"type":"Float"},
                "target":{"kind":"node", "nodeId":object.node_id, "param":"parent_visible"}})
            })
            .collect();
        let budget_node = def
            .nodes
            .iter()
            .find(|node| node.id == budget as u32)
            .expect("family budget");
        let domain_node = def
            .nodes
            .iter()
            .find(|node| node.id == domain as u32)
            .expect("family domain");
        bindings.push(
            json!({"id":"sheet_fill_rate", "label":"Sheet Fill Rate", "defaultValue":SHEET_FILL_RATE,
            "defaultMirrorsNodeParam":true, "convert":{"type":"Float"},
            "target":{"kind":"node", "nodeId":domain_node.node_id, "param":"sheet_fill_rate"}}),
        );
        bindings.push(
            json!({"id":"whitewater_capacity", "label":"Whitewater Budget", "defaultValue":100000.0,
            "defaultMirrorsNodeParam":true, "convert":{"type":"IntRound"},
            "target":{"kind":"node", "nodeId":budget_node.node_id, "param":"value"}}),
        );
        // The field switch comes first: a card belongs to its first target.
        for switch in PHYSICS_VIEW_SWITCHES {
            bindings.push(
                json!({"id":"physics_view", "label":"Physics View", "defaultValue":0.0,
                "convert":{"type":"Float"},
                "target":{"kind":"node", "nodeId":switch, "param":"selector"}}),
            );
        }
        def.preset_metadata = Some(serde_json::from_value(json!({
            "id":"WaterFamily", "displayName":"Water", "category":"Geometry", "oscPrefix":"water",
            "params":[{"id":"parent_visible", "name":"Visible", "defaultValue":1.0,
                "min":0.0, "max":1.0, "isToggle":true, "cardVisible":false, "section":"Water"},
                {"id":"sheet_fill_rate", "name":"Sheet Fill Rate", "defaultValue":SHEET_FILL_RATE,
                "min":0.0, "max":1.0, "formatString":"F2", "wholeNumbers":false,
                "isToggle":false, "isTrigger":false, "section":"Fluid"},
                {"id":"whitewater_capacity", "name":"Whitewater Budget", "defaultValue":100000.0,
                "min":1000.0, "max":250000.0, "wholeNumbers":true, "formatString":"F0", "section":"Water Detail"},
                {"id":"physics_view", "name":"Physics View", "defaultValue":0.0,
                "min":0.0, "max":1.0, "isToggle":true, "section":"Water"}],
            "bindings":bindings
        })).expect("family visibility metadata"));
        def
    }
}

mod scene_family;

/// The insertion body is the same family used by both presets.
pub fn gpu_flip_liquid_body() -> EffectGraphDef {
    let mut body = WaterScene::dam_break(64).family_def();
    for (id, handle) in [("water_object", ""), ("domain", "Simulation")] {
        body.nodes
            .iter_mut()
            .find(|node| node.node_id.as_str() == id)
            .expect("insertion body node")
            .handle = Some(handle.into());
    }
    body
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
/// The four scene objects the Water family declares: water, spray,
/// bubbles, then Physics View's dots.
fn family_group_outputs() -> Vec<manifold_core::effect_graph_def::InterfacePortDef> {
    FAMILY_OUTPUTS
        .iter()
        .chain([&DOTS_OUTPUT])
        .map(|name| manifold_core::effect_graph_def::InterfacePortDef {
            name: (*name).into(),
            port_type: "SceneObject".into(),
        })
        .collect()
}

/// The water family group's stable node id in the bundled presets.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const FAMILY_NODE_ID: &str = "water_family";

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
/// The family `body` wrapped as one group node: its interface declares every
/// object output, so a host routes them by port type.
fn wrap_family(body: EffectGraphDef) -> EffectGraphNode {
    let local_id = body
        .nodes
        .iter()
        .map(|node| node.id)
        .max()
        .map_or(1, |max| max + 1);
    let mut group: EffectGraphNode = serde_json::from_value(json!({
        "id": local_id, "nodeId": FAMILY_NODE_ID, "typeId": "group", "handle": "{handle} Graph",
    }))
    .expect("family group node");
    group.group = Some(Box::new(manifold_core::effect_graph_def::GroupDef {
        interface: manifold_core::effect_graph_def::GroupInterface {
            inputs: Vec::new(),
            outputs: family_group_outputs(),
            params: Vec::new(),
        },
        nodes: body.nodes,
        wires: body.wires,
        tint: None,
    }));
    group
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const SURFACE_DETAIL_OFFSET: usize = 1;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const DRESSING: &[&str] = &[
    "input",
    "camera",
    "environment",
    "light",
    "floor_mesh",
    "basin_material",
    "floor_transform",
    "floor_object",
    "rear_mesh",
    "rear_transform",
    "rear_object",
    "left_mesh",
    "left_transform",
    "left_object",
    "right_mesh",
    "right_transform",
    "right_object",
    "scene",
    "final_output",
    "rim_light",
    "filmic_display",
    "sky_environment",
];

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn assert_preset_root(nodes: &[EffectGraphNode]) {
    for node in nodes {
        assert!(
            DRESSING.contains(&node.node_id.as_str())
                || matches!(
                    node.node_id.as_str(),
                    FAMILY_NODE_ID
                        | "obstacle_transform"
                        | "obstacle_collider"
                        | "obstacle_mesh"
                        | "obstacle_material"
                        | "obstacle_object"
                ),
            "unclassified top-level preset node would be discarded: {}",
            node.node_id.as_str()
        );
    }
}

/// The render slot the obstacle draws in: the one the tank layout leaves free.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
const OBSTACLE_RENDER_SLOT: &str = "object_1";

/// The Dam Break's box as the scene compiler makes a Collider
/// (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision shapes)): a
/// Fixed rigid body of a unit cube in a Physics World, drawn at the body's
/// pose in the render scene, which pairs that world with the water.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn dam_obstacle(b: &mut Builder, render: usize) {
    let [pos, scale] = DAM_OBSTACLE;
    let transform = b.node(
        "obstacle_transform",
        "node.transform_3d",
        json!({
            "pos_x": float(pos[0]), "pos_y": float(pos[1]), "pos_z": float(pos[2]),
            "scale_x": float(scale[0]), "scale_y": float(scale[1]), "scale_z": float(scale[2]),
        }),
    );
    let mesh = b.node("obstacle_mesh", "node.cube_mesh", json!({}));
    let body = b.node(
        "obstacle_body",
        "node.rigid_body",
        json!({"motion": {"type": "Enum", "value": 0}}),
    );
    let world = b.node(
        "obstacle_world",
        "node.physics_world",
        json!({"bodies": int(1)}),
    );
    let material = b.node(
        "obstacle_material",
        "node.pbr_material",
        json!({"roughness": float(0.6)}),
    );
    let object = b.node("obstacle_object", "node.scene_object", json!({}));
    b.wire((material, "out"), object, "material");
    b.wire((transform, "transform"), body, "transform");
    b.wire((mesh, "source"), body, "source");
    b.wire((body, "body"), world, "body_0");
    b.wire((world, "pose_0"), object, "transform");
    b.wire((mesh, "vertices"), object, "vertices");
    b.wire((object, "object"), render, OBSTACLE_RENDER_SLOT);
}

/// Only authored environment/camera/tank dressing is read from the preset.
/// The family is always constructed by WaterScene::family_def.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub fn render_def(scene: WaterScene) -> EffectGraphDef {
    let preset = shipped_preset();
    let mut def: EffectGraphDef = serde_json::from_value(preset.clone()).expect("preset");
    assert_preset_root(&def.nodes);
    def.nodes.retain(|n| DRESSING.contains(&n.node_id.as_str()));
    let ids: Vec<_> = def.nodes.iter().map(|n| n.id).collect();
    def.wires
        .retain(|w| ids.contains(&w.from_node) && ids.contains(&w.to_node));
    let mut b = Builder::from_def(def.clone());
    let render = b.id("scene");
    let body = scene.family_def();
    let family_metadata = body.preset_metadata.clone().expect("family metadata");
    let family = b.node(FAMILY_NODE_ID, "group", json!({}));
    let node = b.nodes.last_mut().expect("family");
    node.handle = Some("Water".into());
    node.group = wrap_family(body).group;
    // Preserve the existing physical render order: water, spray, bubbles.
    for (output, slot) in FAMILY_OUTPUTS
        .into_iter()
        .zip(["object_0", "object_9", "object_8"])
    {
        b.wire((family, output), render, slot);
    }
    b.wire((family, DOTS_OUTPUT), render, DOTS_RENDER_SLOT);
    if scene.obstacle {
        dam_obstacle(&mut b, render);
    }
    let render_node = b
        .nodes
        .iter_mut()
        .find(|n| n.id == render as u32)
        .expect("render");
    // Physical slots span 0 through 9; slot 6 holds Physics View's dots.
    render_node.params.insert(
        "objects".into(),
        serde_json::from_value(int(10)).expect("object count"),
    );
    def.nodes = b.nodes;
    def.wires = b.wires;
    let mut metadata: manifold_core::effect_graph_def::PresetMetadata =
        serde_json::from_value(scene_cards(&preset["presetMetadata"], scene)).expect("scene cards");
    for spec in family_metadata.params {
        let bindings: Vec<_> = family_metadata
            .bindings
            .iter()
            .filter(|binding| binding.id == spec.id)
            .cloned()
            .collect();
        let insertion = metadata
            .bindings
            .iter()
            .position(|binding| binding.id == spec.id)
            .unwrap_or(metadata.bindings.len());
        metadata.bindings.retain(|binding| binding.id != spec.id);
        metadata.bindings.splice(insertion..insertion, bindings);
        if let Some(existing) = metadata
            .params
            .iter_mut()
            .find(|existing| existing.id == spec.id)
        {
            *existing = spec;
        } else {
            metadata.params.push(spec);
        }
    }
    author_surface_scene(&mut metadata.params);
    def.preset_metadata = Some(metadata);
    def
}

/// The water surface's controls and the scene group each joins. They shape
/// how the surface is built, set once per scene: the Scene tab shows them,
/// the card does not (card
/// projection), the Scene band's setup-role surface row).
pub const SURFACE_CONTROLS: [(&str, manifold_foundation::scene_surface::ControlGroup); 8] = {
    use manifold_foundation::scene_surface::ControlGroup::{Look, Whitewater};
    [
        ("surface_detail", Look),
        ("surface_particle_scale", Look),
        ("surface_smoothness", Look),
        ("surface_ripple_detail", Look),
        ("surface_foam", Look),
        ("surface_mist", Look),
        ("surface_drying", Look),
        ("whitewater_capacity", Whitewater),
    ]
};

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn author_surface_scene(params: &mut [manifold_core::effect_graph_def::ParamSpecDef]) {
    use manifold_foundation::scene_surface::{CardLabel, SceneParamDesc, SurfaceRole};
    for param in params {
        if let Some((_, group)) = SURFACE_CONTROLS.iter().find(|(id, _)| *id == param.id) {
            param.scene = Some(SceneParamDesc {
                role: SurfaceRole::Setup,
                group: *group,
                band: None,
                card_order: 0,
                card_label: CardLabel::Param,
                gate: None,
                switch: None,
                colour: None,
            });
        }
    }
}

/// The shipped Particle View, which [`particle_view_def`] builds from the
/// shipped Dam Break (`gpu_flip_particle_view_is_built_from_the_dam_break`).
#[cfg(any(test, feature = "testkit"))]
pub const PARTICLE_VIEW_PRESET: &str = "WaterDamBreakParticles";

#[cfg(any(test, feature = "testkit"))]
const PARTICLE_VIEW_NAME: &str = "Water — Dam Break (Particle View)";

#[cfg(any(test, feature = "testkit"))]
const PARTICLE_VIEW_DESCRIPTION: &str = "GPU FLIP dam break drawn as small sphere copies of the blended, solid-clamped liquid particles, with the same display-time whitewater as the surface preset.";

/// The Particle View: the shipped Dam Break's render, simulation, scene,
/// whitewater, with the Liquid Surface replaced by
/// `particle_push_out`'s particles drawn as instanced spheres by the water
/// object. Its cards are the Dam Break's, less those that only reached the
/// surface.
#[cfg(any(test, feature = "testkit"))]
pub fn particle_view_def() -> EffectGraphDef {
    let mut def = render_def(WaterScene::dam_break(64));
    let family = find_node_mut(&mut def.nodes, FAMILY_NODE_ID).expect("Water family");
    let group = family.group.as_mut().expect("family group");
    let mut b = Builder {
        nodes: std::mem::take(&mut group.nodes),
        wires: std::mem::take(&mut group.wires),
    };
    // The particles are already the picture: no surface and no Physics View.
    group
        .interface
        .outputs
        .retain(|output| output.name != DOTS_OUTPUT);
    let family_id = family.id;
    def.wires
        .retain(|wire| wire.from_node != family_id || wire.from_port != DOTS_OUTPUT);
    scene_family::particles_draw(&mut b);
    group.nodes = b.nodes;
    group.wires = b.wires;
    def.name = Some(PARTICLE_VIEW_NAME.into());
    def.description = Some(PARTICLE_VIEW_DESCRIPTION.into());
    let value = serde_json::to_value(&def).expect("particle view");
    def.preset_metadata =
        Some(serde_json::from_value(particle_view_cards(&value)).expect("particle cards"));
    def
}

/// `def`'s cards under the Particle View's id, less every binding whose node
/// is gone and every card left with none of the bindings it had.
#[cfg(any(test, feature = "testkit"))]
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
        let Some(list) = metadata[bindings].as_array_mut() else {
            continue;
        };
        let bound = |list: &[Value]| {
            list.iter()
                .filter_map(|b| b["id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let before = bound(list.as_slice());
        list.retain(|b| {
            b["target"]["kind"] != "node"
                || b["target"]["nodeId"]
                    .as_str()
                    .is_some_and(|node| present.iter().any(|name| name == node))
        });
        let after = bound(list.as_slice());
        if let Some(cards) = metadata[cards].as_array_mut() {
            cards.retain(|card| {
                card["id"].as_str().is_none_or(|id| {
                    !before.iter().any(|b| b == id) || after.iter().any(|a| a == id)
                })
            });
        }
    }
    metadata["id"] = json!(PARTICLE_VIEW_PRESET);
    metadata["displayName"] = json!(PARTICLE_VIEW_NAME);
    metadata["oscPrefix"] = json!(PARTICLE_VIEW_PRESET.to_lowercase());
    metadata
}

/// The shipped cards with every default at the value this scene's def bakes,
/// so no card overwrites what the extent proof checked: Cell Size at the
/// lattice's, Surface Detail at the surface scale, gone past its range.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
fn scene_cards(metadata: &Value, scene: WaterScene) -> Value {
    let mut metadata = metadata.clone();
    let detail = scene
        .surface_scale
        .checked_sub(SURFACE_DETAIL_OFFSET)
        .filter(|detail| *detail <= 2);
    for list in ["params", "bindings"] {
        let entries = metadata[list].as_array_mut().expect("card lists");
        entries.retain(|entry| entry["id"] != "surface_detail" || detail.is_some());
        for entry in entries.iter_mut() {
            if entry["id"] == "cell_size" {
                entry["defaultValue"] = json!(scene.cell_size());
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

/// The liquid's field on the display's own world tiles: the display table
/// from frame B's particles at the scene's surface scale, the solid on it at
/// the bodies' current poses, and the surface group over both. With Narrow
/// Band the table also holds the grid's water and the field is filled from
/// it ([`drawn_level_set`]).
fn surface(b: &mut Builder, scene: WaterScene, frame: usize) -> Port {
    let domain = b.id("domain");
    let tiles = b.node(
        DISPLAY_TILES,
        "node.surface_tiles",
        json!({"resolution_scale": int(scene.surface_scale)}),
    );
    b.wire((frame, "particles_b"), tiles, "particles");
    b.wire((domain, "cell_size"), tiles, "sim_cell_size");
    let solid = b.node(DISPLAY_SOLID, "node.liquid_solid_distance", json!({}));
    solid_wires(b, domain, solid);
    on_display_tiles(b, tiles, solid);
    let mut group = surface_group();
    let id = b
        .nodes
        .iter()
        .map(|node| node.id as usize + 1)
        .max()
        .unwrap_or(0);
    group["id"] = json!(id);
    group["nodeId"] = json!("surface");
    b.nodes
        .push(serde_json::from_value(group).expect("surface group"));
    b.wire((frame, "particles_b"), id, "particles");
    b.wire((frame, "count_b"), id, "count");
    b.wire((solid, "solid"), id, "solid");
    on_display_tiles(b, tiles, id);
    b.wire((domain, "cell_size"), id, "sim_cell_size");
    if !scene.narrow_band {
        return (id, "level_set");
    }
    let state = b.id("state");
    let fill = b.node(INTERIOR_FILL, "node.interior_fill", json!({}));
    for node in [tiles, fill] {
        b.wire((state, "interior"), node, "interior");
        b.wire((state, "interior_tiles"), node, "interior_tiles");
    }
    b.wire((id, "level_set"), fill, "levelset");
    on_display_tiles(b, tiles, fill);
    b.wire((domain, "cell_size"), fill, "sim_cell_size");
    (fill, "out")
}

/// Narrow Band's fill of the surface's level set.
const INTERIOR_FILL: &str = "interior_fill";

/// The level set the water is drawn from: the surface group's, filled from
/// the grid's water when the scene runs Narrow Band.
fn drawn_level_set(b: &Builder, scene: WaterScene, surface: usize) -> Port {
    if scene.narrow_band {
        (b.id(INTERIOR_FILL), "out")
    } else {
        (surface, "level_set")
    }
}

/// The domain's own values of `wires` for the scene as params, plus
/// `extra`: they seed the planned sizes, and the wires carry any change at
/// run time. Cell counts and the pool are Int params, the rest Float.
fn seeded(scene: &WaterScene, wires: &[&str], extra: &[(&str, Value)]) -> Value {
    let outputs = scene.geometry().outputs();
    let mut params = json!({});
    for &name in wires {
        let value = outputs
            .iter()
            .find(|(port, _)| *port == name)
            .map(|&(_, value)| value)
            .expect("the domain publishes every seeded wire");
        params[name] = if name.starts_with("fill_cells_") || name == "pool_tiles" {
            int(value as usize)
        } else {
            float(f64::from(value))
        };
    }
    for (name, value) in extra {
        params[*name] = value.clone();
    }
    params
}

/// How far a Fill or an inflow moves each new particle off its half-cell
/// site, as a fraction of the most (a quarter cell each way): off the grid,
/// so the water starts without rows or stripes.
const SEED_JITTER: f64 = 1.0;

/// The tick's water:
/// one node.gpu_flip_step of `scene.steps` substeps on the domain's lattice, gravity,
/// fields and bodies, and the state's tick index. The caller wires its
/// particles and identity.
fn water_step(b: &mut Builder, scene: WaterScene, tick: (usize, usize)) -> usize {
    let (domain, state) = tick;
    let iterations = |n: usize, auto: u32| int(if n == auto as usize { 0 } else { n });
    let step = b.node(
        STEP_NODE,
        "node.gpu_flip_step",
        seeded(
            &scene,
            &POOL_WIRES,
            &[
                ("steps", int(scene.steps)),
                (
                    "iterations",
                    iterations(scene.pressure.iterations, PRESSURE_ITERATIONS as u32),
                ),
                ("ghost_fluid", int(usize::from(scene.ghost_fluid))),
                ("inflow_jitter", float(SEED_JITTER)),
                (
                    "volume_projection",
                    int(usize::from(scene.volume_projection)),
                ),
            ],
        ),
    );
    b.wires(domain, step, &POOL_WIRES);
    b.wire((domain, "interval_duration"), step, "interval_duration");
    b.wire((domain, "gravity_x"), step, "gravity_x");
    b.wire((domain, "gravity"), step, "gravity_y");
    b.wire((domain, "gravity_z"), step, "gravity_z");
    b.wires(domain, step, &FIELD_WIRES);
    b.wires(domain, step, &BOUNDS_WIRES);
    b.wires(domain, step, &WALL_VELOCITY_WIRES);
    b.wire((state, "tick_index"), step, "tick_index");
    b.wires(
        domain,
        step,
        &[
            "bodies",
            "contacts",
            "shapes",
            "atlas",
            "collider_nodes",
            "collider_triangles",
            "body_count",
            "dynamic_bodies",
            "closed_faces",
            "open_faces",
            "kill_height",
            "solve_level",
            "max_iterations",
            "sheet_fill_rate",
        ],
    );
    b.wire((domain, "body_rows"), step, "rows");
    if scene.narrow_band {
        find_node_mut(&mut b.nodes, STEP_NODE)
            .expect("the step")
            .params
            .insert(
                "narrow_band".into(),
                serde_json::from_value(int(1)).expect("narrow band"),
            );
    }
    step
}

/// Device bytes a scene holds inside the render graph at 1920×1080, as the
/// liquid extent check counts them: every array at the size the walk reached
/// plus what each node holds for itself. Textures are not counted.
#[cfg(any(any(test, feature = "testkit"), feature = "testkit"))]
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    crate::testkit::liquid_extents::walk(&render_def(scene), false)
        .expect("the rendered scene covers every dispatch")
        .scene_bytes
}

/// `def` as the app renders a generator: fused when it has regions, `None`
/// when it has none and runs as authored. Regions that refuse to fuse fail.
#[cfg(any(any(test, feature = "testkit"), feature = "testkit"))]
pub(super) fn fused_as_rendered(
    def: &EffectGraphDef,
    registry: &manifold_node_engine::persistence::PrimitiveRegistry,
) -> Option<manifold_node_engine::freeze::install::FusedGeneratorView> {
    let view = manifold_node_engine::freeze::install::fuse_generator_view(def, registry);
    if view.is_none() {
        let regions = manifold_node_engine::freeze::fusion_report(def, registry).regions;
        assert!(
            regions.is_empty(),
            "the def has {} fusable regions but does not fuse",
            regions.len()
        );
    }
    view
}

// CPU size proofs for every GPU FLIP graph, run before any GPU run of it: the
// shared liquid extent rules (`liquid::extent`) at every lattice, bare,
// meshed, rendered and frozen.

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testkit;

/// The water drawn from frame B through the push-out: the surface's field
/// fed from the blended particles and solids, and the water object with its
/// material. Returns the water object.
fn display_head(b: &mut Builder, frame: usize, surface: usize) -> usize {
    let [tiles, solid] = [DISPLAY_TILES, DISPLAY_SOLID].map(|name| b.id(name));
    let particles = b.node(
        "particle_blend",
        "node.interpolate_particle_frames",
        json!({}),
    );
    let push_out = b.node("particle_push_out", "node.push_out_of_solid", json!({}));
    b.wires(
        frame,
        particles,
        &[
            "particles_a",
            "particles_b",
            "count_a",
            "count_b",
            "identity_a",
            "identity_b",
            "blend",
            "span",
        ],
    );
    b.wire((particles, "out"), push_out, "particles");
    b.wire((solid, "solid"), push_out, "solid");
    on_display_tiles(b, tiles, push_out);
    // The surface draws the blended particles pushed out of the solids. The
    // display table stays on frame B's particles: the blend and the push-out
    // move them less than the table's ring of tiles, and the blend keeps one
    // reader so it fuses into the push-out.
    b.wires
        .retain(|w| !(w.to_node == surface as u32 && w.to_port == "particles"));
    b.wire((push_out, "out"), surface, "particles");

    let material = b.node(
        "water_material",
        "node.pbr_material",
        serde_json::Value::Object(
            CLEAR_WATER
                .iter()
                .map(|&(name, value)| (name.to_owned(), float(f64::from(value))))
                .collect(),
        ),
    );
    let water = b.node("water_object", "node.scene_object", json!({}));
    b.wire((material, "out"), water, "material");
    b.wire((tiles, "pool_tiles"), water, "field_pool_tiles");
    b.wire((tiles, "cell_size"), water, "field_cell_size");
    let ripple_detail = b.node(
        "water_ripple_detail",
        "node.value",
        json!({"value": float(1.0)}),
    );
    b.wire((ripple_detail, "out"), water, "field_ripple_detail");
    water
}

/// The water's foam: the foam particles' display blend fed to the water
/// object, which draws them at pixel resolution on its field (none in Physics
/// View, which takes the field away).
fn display_foam(b: &mut Builder, frame: usize, water: usize) {
    let blend = b.node("foam_blend", "node.interpolate_particle_frames", json!({}));
    b.wire((frame, "foam_b"), blend, "particles_b");
    b.wires(frame, blend, &["blend", "span"]);
    b.wire((blend, "out"), water, "field_foam");
}

/// The water's mist and wet map: the spray's extinction on the liquid's
/// display tiles (spray holds none of its own) and the map of where the drawn water touched
/// the solids, each behind the switch that feeds it to the water object (off
/// in Physics View, which hides the water).
fn display_mist_and_wetness(b: &mut Builder, water: usize) {
    let tiles = b.id(DISPLAY_TILES);
    let [spray, domain, solid, drawn] =
        ["spray_blend", "domain", DISPLAY_SOLID, "particle_push_out"].map(|name| b.id(name));
    let mist = b.node(
        "water_mist",
        "node.spray_mist",
        json!({"strength": float(1.0)}),
    );
    let wet_map = b.node(
        "water_wetness",
        "node.wet_map",
        json!({"drying": float(1.0)}),
    );
    b.wire((spray, "out"), mist, "particles");
    b.wire((drawn, "out"), wet_map, "particles");
    b.wire((solid, "solid"), wet_map, "solid");
    b.wire((domain, "cell_size"), wet_map, "sim_cell_size");
    b.wire((domain, "display_time"), wet_map, "time");
    b.wire((domain, "epoch"), wet_map, "epoch");
    for node in [mist, wet_map] {
        on_display_tiles(b, tiles, node);
    }
    // The map is u32 words, so its switch is the u32 one.
    for (name, kind, from, port, to) in [
        (MIST_SWITCH, "node.switch_array", mist, "mist", "field_mist"),
        (
            WETNESS_SWITCH,
            "node.switch_indices",
            wet_map,
            "map",
            "wet_map",
        ),
    ] {
        let switch = b.node(name, kind, json!({"selector": float(0.0)}));
        b.wire((from, port), switch, "in_0");
        b.wire((switch, "out"), water, to);
    }
}

/// Physics View: the switches between the surface and the particle dots,
/// and the dots object. Returns the dots object.
fn display_dots(
    b: &mut Builder,
    frame: usize,
    (surface, level_set): (usize, Port),
    water: usize,
) -> usize {
    // Physics View: the surface's field sits on in_0 of its switches and the
    // dots on in_1 of theirs, so the card prunes whichever side is off. In
    // Physics View the water is hidden and reads no field. An unwired choice
    // draws nothing; dots_visible hides the object.
    // The table is u32 words, so its switch is the u32 one.
    let tiles = b.id(DISPLAY_TILES);
    for (name, kind, from, to) in [
        (FIELD_SWITCH, "node.switch_array", level_set, "field"),
        (
            VELOCITY_SWITCH,
            "node.switch_array",
            (surface, "velocity"),
            "field_velocity",
        ),
        (
            TILES_SWITCH,
            "node.switch_indices",
            (tiles, "tiles"),
            "field_tiles",
        ),
        (
            EXTENT_SWITCH,
            "node.switch_array",
            (tiles, "extent"),
            "field_extent",
        ),
    ] {
        let switch = b.node(name, kind, json!({"selector": float(0.0)}));
        b.wire(from, switch, "in_0");
        b.wire((switch, "out"), water, to);
    }
    // The surface's own mesh: the renderer draws its depth and marches each
    // ray only round it.
    let mesh = b.node("water_mesh", "node.tile_surface_mesh", json!({}));
    b.wire(level_set, mesh, "levelset");
    on_display_tiles(b, tiles, mesh);
    let switch = b.node(
        VERTICES_SWITCH,
        "node.switch_mesh",
        json!({"selector": float(0.0)}),
    );
    b.wire((mesh, "vertices"), switch, "in_0");
    b.wire((switch, "out"), water, "vertices");
    let water_visible = b.node(
        WATER_VISIBLE,
        "node.switch_value",
        json!({"selector": float(0.0), "in_0": float(1.0), "in_1": float(0.0)}),
    );
    b.wire((water_visible, "out"), water, "visible");
    let dots_mesh = b.node(
        "dots_mesh",
        "node.platonic_solid_mesh",
        json!({"radius": float(1.0), "shape": {"type": "Enum", "value": TETRAHEDRON}}),
    );
    let dots_material = b.node(
        "dots_material",
        "node.unlit_material",
        json!({
            "color_r": float(0.37254903), "color_g": float(0.7294118), "color_b": float(1.0),
        }),
    );
    // Their own display blend, so the surface's push-out keeps its one
    // consumer and its fusion; no push-out, so dots may touch solids.
    let dots_blend = b.node("dots_blend", "node.interpolate_particle_frames", json!({}));
    b.wires(
        frame,
        dots_blend,
        &[
            "particles_a",
            "particles_b",
            "count_a",
            "count_b",
            "identity_a",
            "identity_b",
            "blend",
            "span",
        ],
    );
    let dots_copies = b.node("dots_copies", "node.particles_to_copies", json!({}));
    let dots = b.node(
        "dots_object",
        "node.scene_object",
        json!({"cast_shadows": float(0.0)}),
    );
    let dots_visible = b.node(
        DOTS_VISIBLE,
        "node.switch_value",
        json!({"selector": float(0.0), "in_0": float(0.0), "in_1": float(1.0)}),
    );
    let mesh_switch = b.node(
        DOTS_MESH_SWITCH,
        "node.switch_mesh",
        json!({"selector": float(0.0)}),
    );
    let instances_switch = b.node(
        DOTS_INSTANCES_SWITCH,
        "node.switch_instances",
        json!({"selector": float(0.0)}),
    );
    for (from, to) in [
        ((dots_blend, "out"), (dots_copies, "particles")),
        ((frame, "count_b"), (dots_copies, "live_count")),
        ((dots_copies, "copies"), (instances_switch, "in_1")),
        ((instances_switch, "out"), (dots, "instances")),
        ((frame, "count_b"), (dots, "instance_count")),
        ((dots_mesh, "vertices"), (mesh_switch, "in_1")),
        ((mesh_switch, "out"), (dots, "vertices")),
        ((dots_material, "out"), (dots, "material")),
        ((dots_visible, "out"), (dots, "visible")),
    ] {
        b.wire(from, to.0, to.1);
    }
    dots
}
