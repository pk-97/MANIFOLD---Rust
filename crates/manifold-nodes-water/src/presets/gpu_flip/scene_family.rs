//! The families a scene's Water or Sand splices (SCENE_DOCUMENT_DESIGN.md
//! section 7.10 (Water, Water Source, Ocean)), one per kind and draw: no
//! fill of their own and no host interface. The scene compiler wires the
//! sources into the domain's role inputs and the objects into its render
//! node.
//!
//! A Water is the GPU FLIP Dam Break family. A Sand is the Matter solver
//! (`assets/matter_solver.json`) with the shared surface build, drawn as an
//! opaque mesh through the ordinary material path (MATTER_SAND_DESIGN.md
//! section 4), not the water renderer, which only draws glass.
//! Particles draws the push-out particles as small solids in place of the
//! surface.

use std::sync::{Arc, OnceLock};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{ItemKind, WaterDraw};
use manifold_core::scene_schema::{
    MATTER_SKIN_ROLE, PHYSICS_VIEW_SWITCHES, WaterFamily, WaterTemplate,
};
use serde_json::json;

use super::{
    Builder, DISPLAY_SOLID, DISPLAY_TILES, DOTS_OUTPUT, FAMILY_OUTPUTS, LIQUID_BODY_OUTPUT,
    VELOCITY_SWITCH, VERTICES_SWITCH, WaterScene, display_dots, display_head, feed_intervals,
    float, surface,
};

/// The Platonic shape a drawn particle is, scaled by the particle's radius.
const ICOSAHEDRON: usize = 3;
/// A sand grain's shape: eight faces, turned per grain, read as a faceted
/// grain at a few pixels across for under half an icosahedron's triangles.
const OCTAHEDRON: usize = 2;

/// Draw the push-out particles as instanced spheres on the water object, in
/// place of the surface and Physics View's dots.
pub(super) fn particles_draw(b: &mut Builder) {
    let copies = b.node(
        "liquid_particle_copies",
        "node.particles_to_copies",
        json!({}),
    );
    let frame = b.id("frame");
    instanced_draw(b, copies, (frame, "count_b"), ICOSAHEDRON);
}

/// The object Sand's loose grains draw on, beside the surface.
const SAND_GRAINS_OBJECT: &str = "sand_grains_object";

/// Sand's Particles: the surface, with its material's grain, draws the bulk,
/// and the loose sand (flying grains, thin streams) draws as small turned
/// grains (node.matter_grains) on their own object with the same material.
fn grains_draw(b: &mut Builder) {
    let grains = b.node("sand_grains", "node.matter_grains", json!({}));
    let domain = b.id("domain");
    b.wires(domain, grains, &["cell_size", "points_per_cell"]);
    let mesh = b.node(
        "sand_grain_mesh",
        "node.platonic_solid_mesh",
        json!({"radius": float(1.0), "shape": {"type": "Enum", "value": OCTAHEDRON}}),
    );
    let object = b.node(SAND_GRAINS_OBJECT, "node.scene_object", json!({}));
    let [frame, particles, material, visible, surface] = [
        "frame",
        "particle_push_out",
        "water_material",
        PHYSICS_VIEW_SWITCHES[4],
        "surface",
    ]
    .map(|name| b.id(name));
    // The surface builds from the packed points alone: loose ones are grains.
    b.wires
        .retain(|w| !(w.to_node == surface as u32 && w.to_port == "particles"));
    for (from, to) in [
        ((particles, "out"), (grains, "particles")),
        ((grains, "bulk"), (surface, "particles")),
        ((frame, "count_b"), (grains, "live_count")),
        ((grains, "copies"), (object, "instances")),
        ((grains, "count"), (object, "instance_count")),
        ((mesh, "vertices"), (object, "vertices")),
        ((material, "out"), (object, "material")),
        ((visible, "out"), (object, "visible")),
    ] {
        b.wire(from, to.0, to.1);
    }
}

/// The push-out particles through `copies` (particles and live_count in,
/// copies out) as instances of a unit Platonic `shape` on the water object,
/// `count` of them, in place of the surface and Physics View's dots.
fn instanced_draw(b: &mut Builder, copies: usize, count: (usize, &str), shape: usize) {
    b.remove(&[
        "surface",
        "water_mesh",
        "dots_mesh",
        "dots_material",
        "dots_blend",
        "dots_copies",
        "dots_object",
        "water_ripple_detail",
        "foam_blend",
        "water_mist",
        "water_wetness",
    ]);
    b.remove(&PHYSICS_VIEW_SWITCHES);
    // The display table keeps only the liquid's tiles, which push-out reads:
    // no spray mist reads the spray's.
    let [water, tiles] = ["water_object", DISPLAY_TILES].map(|name| b.id(name) as u32);
    b.wires.retain(|wire| {
        (wire.to_node != water || !wire.to_port.starts_with("field_"))
            && (wire.to_node != tiles || wire.to_port != "spray")
    });
    let sphere = b.node(
        "liquid_particle_mesh",
        "node.platonic_solid_mesh",
        json!({"radius": float(1.0), "shape": {"type": "Enum", "value": shape}}),
    );
    let [frame, particles, water] =
        ["frame", "particle_push_out", "water_object"].map(|name| b.id(name));
    for (from, to) in [
        ((particles, "out"), (copies, "particles")),
        ((frame, "count_b"), (copies, "live_count")),
        ((copies, "copies"), (water, "instances")),
        ((sphere, "vertices"), (water, "vertices")),
        (count, (water, "instance_count")),
    ] {
        b.wire(from, to.0, to.1);
    }
}

/// The Dam Break family less its output and whitewater budget; its objects
/// in output order.
fn flip(draw: WaterDraw, narrow_band: bool) -> WaterFamily {
    let scene = WaterScene {
        column: [[0.0; 2]; 3],
        narrow_band,
        ..WaterScene::dam_break(64)
    };
    let def = scene.family_def();
    let output = def
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == LIQUID_BODY_OUTPUT)
        .expect("family output")
        .id;
    let mut objects: Vec<String> = FAMILY_OUTPUTS
        .iter()
        .chain([&DOTS_OUTPUT])
        .map(|port| {
            let wire = def
                .wires
                .iter()
                .find(|wire| wire.to_node == output && wire.to_port == *port)
                .expect("family object output");
            let object = def
                .nodes
                .iter()
                .find(|node| node.id == wire.from_node)
                .expect("family object");
            object.node_id.as_str().to_owned()
        })
        .collect();
    let mut b = Builder::from_def(def);
    b.remove(&[LIQUID_BODY_OUTPUT, "whitewater_budget"]);
    if draw == WaterDraw::Particles {
        particles_draw(&mut b);
        objects.retain(|object| object != "dots_object");
    }
    family(b, objects)
}

/// Dry sand's starting look: opaque, rough, the colour of a beach. Its
/// Material starts here; the stills tune it.
const DRY_SAND: [(&str, f32); 9] = [
    ("ambient", 0.0),
    ("color_r", 0.55),
    ("color_g", 0.45),
    ("color_b", 0.30),
    ("metallic", 0.0),
    ("roughness", 0.9),
    ("ior", 1.5),
    ("transmission", 0.0),
    ("grain", 2.0),
];

/// The Sand's surface: the tile mesh the surface already builds, drawn as an
/// ordinary opaque object, so it casts shadows and takes any Material. The
/// water's field inputs and its ripples go; the surface's velocity moves to
/// the mesh, whose previous positions give the object its motion blur.
fn sand_surface_draw(b: &mut Builder) {
    let switch = b.id(VELOCITY_SWITCH) as u32;
    let velocity = b
        .wires
        .iter()
        .find(|wire| wire.to_node == switch && wire.to_port == "in_0")
        .map(|wire| (wire.from_node as usize, wire.from_port.clone()))
        .expect("the surface's velocity feeds the velocity switch");
    b.remove(&PHYSICS_VIEW_SWITCHES[..4]);
    b.remove(&["water_ripple_detail"]);
    let [water, mesh] = ["water_object", "water_mesh"].map(|name| b.id(name));
    b.wires
        .retain(|wire| wire.to_node != water as u32 || !wire.to_port.starts_with("field"));
    b.wire((velocity.0, &velocity.1), mesh, "velocity");
    b.wire((mesh, "prev_positions"), water, "prev_positions");
}

/// The Sand's look on its material node, which a new Sand's own Material
/// starts from.
fn sand_material(b: &mut Builder) {
    let material = b
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "water_material")
        .expect("the display's material");
    for (name, value) in DRY_SAND {
        material.params.insert(
            name.to_owned(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value },
        );
    }
}

/// Display cells the field is carried into solids before Matter's
/// smoothing: past the two nodes a smoothing step's ring reaches.
const SMOOTH_CARRY_CELLS: f32 = 2.0;

/// Matter holds no one-particle sheets, so its smoothing lets thin bumps of
/// stray particles melt back into the surface instead of keeping them as
/// fins (node.field_smooth_blend's Keep Sheets). The field is carried into
/// the solids before it is smoothed too, so the smoothing finds the body's
/// sides running on into the floor instead of air under it and does not
/// curl the bottom edge up.
fn solid_surface(b: &mut Builder, surface: usize) {
    let group = b
        .nodes
        .iter_mut()
        .find(|node| node.id as usize == surface)
        .and_then(|node| node.group.as_mut())
        .expect("the surface group");
    for node in &mut group.nodes {
        if node.type_id.as_str() == "node.field_smooth_blend" {
            node.params.insert(
                "keep_sheets".to_owned(),
                manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false },
            );
        }
    }
    let [input, closed, after] = [
        "system.group_input",
        "liquid_close_gaps",
        "liquid_carry_into_solids",
    ]
    .map(|name| {
        group
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == name || node.type_id.as_str() == name)
            .map(|node| node.id)
            .expect("a surface group node")
    });
    let carry = group.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    group.nodes.push(
        serde_json::from_value(json!({
            "id": carry,
            "nodeId": "liquid_carry_before_smoothing",
            "typeId": "node.carry_liquid_into_solids",
            "handle": "Carry Before Smoothing",
            "params": {"carry_cells": {"type": "Float", "value": SMOOTH_CARRY_CELLS}},
        }))
        .expect("the carry node"),
    );
    let fed: Vec<_> = group
        .wires
        .iter()
        .filter(|wire| wire.to_node == after && wire.from_node == input)
        .map(|wire| (wire.from_port.clone(), wire.to_port.clone()))
        .collect();
    for wire in &mut group.wires {
        if wire.from_node == closed && matches!(wire.to_port.as_str(), "prior" | "reference") {
            wire.from_node = carry;
            wire.from_port = "out".to_owned();
        }
    }
    let wire = |from_node, from_port: &str, to_port: &str| {
        manifold_core::effect_graph_def::EffectGraphWire {
            from_node,
            from_port: from_port.to_owned(),
            to_node: carry,
            to_port: to_port.to_owned(),
        }
    };
    group.wires.push(wire(closed, "out", "levelset"));
    for (from_port, to_port) in fed {
        group.wires.push(wire(input, &from_port, &to_port));
    }
}

/// Jelly's and clay's draw: the surface build and the display's tiles give
/// way to the item's Fill mesh carried by its points (node.matter_skin),
/// drawn on the same object behind Physics View's vertex switch. The scene
/// compiler wires the Fill's transform and mesh and the item's slot; the
/// object keeps last frame's positions itself for motion blur.
fn carried_draw(b: &mut Builder) {
    b.remove(&[
        "surface",
        DISPLAY_TILES,
        DISPLAY_SOLID,
        "particle_blend",
        "particle_push_out",
        "water_mesh",
    ]);
    let skin = b.node(MATTER_SKIN_ROLE, "node.matter_skin", json!({}));
    let [domain, state, fill, water, vertices] = [
        "domain",
        "state",
        "matter_fill",
        "water_object",
        VERTICES_SWITCH,
    ]
    .map(|name| b.id(name));
    b.wires(
        domain,
        skin,
        &[
            "fills",
            "fill_min_x",
            "fill_min_y",
            "fill_min_z",
            "cell_size",
            "points_per_cell",
            "simulation_time",
            "display_time",
            "epoch",
        ],
    );
    for (from, to) in [
        ((state, "out"), (skin, "points")),
        ((state, "stats"), (skin, "stats")),
        ((fill, "points"), (skin, "seeds")),
        ((skin, "vertices"), (vertices, "in_0")),
        ((skin, "indices"), (water, "indices")),
    ] {
        b.wire(from, to.0, to.1);
    }
}

/// The Matter solver with the shared surface build, drawn as sand.
fn mpm(draw: WaterDraw, _narrow_band: bool) -> WaterFamily {
    mpm_family(draw, false)
}

/// The Matter solver drawn as its Fill's carried mesh (`carried_draw`).
fn mpm_carried(draw: WaterDraw, _narrow_band: bool) -> WaterFamily {
    mpm_family(draw, true)
}

fn mpm_family(draw: WaterDraw, carried: bool) -> WaterFamily {
    let solver: EffectGraphDef =
        serde_json::from_str(include_str!("../../../assets/matter_solver.json"))
            .expect("the Matter solver parses");
    let mut b = Builder::from_def(solver);
    let [domain, frame] = ["domain", "frame"].map(|name| b.id(name));
    let scene = WaterScene::dam_break(64);
    let (surface, level_set) = surface(&mut b, scene, frame);
    solid_surface(&mut b, surface);
    let water = display_head(&mut b, frame, surface);
    display_dots(&mut b, frame, (surface, (surface, level_set)), water);
    feed_intervals(&b.nodes, &mut b.wires, domain as u32);
    sand_material(&mut b);
    let mut objects = vec!["water_object".to_owned(), "dots_object".to_owned()];
    sand_surface_draw(&mut b);
    if carried {
        carried_draw(&mut b);
    } else if draw == WaterDraw::Particles {
        grains_draw(&mut b);
        objects.insert(1, SAND_GRAINS_OBJECT.to_owned());
    }
    family(b, objects)
}

fn family(b: Builder, objects: Vec<String>) -> WaterFamily {
    WaterFamily {
        nodes: b.nodes,
        wires: b.wires,
        objects,
    }
}

macro_rules! template {
    ($name:ident, $kind:expr, $draw:expr, $narrow_band:expr, $build:expr) => {
        template!($name, $kind, $draw, $narrow_band, false, $build);
    };
    ($name:ident, $kind:expr, $draw:expr, $narrow_band:expr, $carried:expr, $build:expr) => {
        fn $name() -> Arc<WaterFamily> {
            static FAMILY: OnceLock<Arc<WaterFamily>> = OnceLock::new();
            FAMILY
                .get_or_init(|| Arc::new($build($draw, $narrow_band)))
                .clone()
        }
        inventory::submit! {
            WaterTemplate {
                kind: $kind,
                draw: $draw,
                narrow_band: $narrow_band,
                carried: $carried,
                family: $name,
            }
        }
    };
}

template!(
    flip_surface,
    ItemKind::Water,
    WaterDraw::Surface,
    false,
    flip
);
template!(
    flip_particles,
    ItemKind::Water,
    WaterDraw::Particles,
    false,
    flip
);
template!(
    flip_band_surface,
    ItemKind::Water,
    WaterDraw::Surface,
    true,
    flip
);
template!(
    flip_band_particles,
    ItemKind::Water,
    WaterDraw::Particles,
    true,
    flip
);
template!(mpm_surface, ItemKind::Sand, WaterDraw::Surface, false, mpm);
template!(
    mpm_particles,
    ItemKind::Sand,
    WaterDraw::Particles,
    false,
    mpm
);
template!(
    mpm_carried_surface,
    ItemKind::Sand,
    WaterDraw::Surface,
    false,
    true,
    mpm_carried
);
