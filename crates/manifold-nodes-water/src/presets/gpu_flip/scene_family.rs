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
use manifold_core::scene_schema::{PHYSICS_VIEW_SWITCHES, WaterFamily, WaterTemplate};
use serde_json::json;

use super::{
    Builder, DISPLAY_TILES, DOTS_OUTPUT, FAMILY_OUTPUTS, LIQUID_BODY_OUTPUT, VELOCITY_SWITCH,
    WaterScene, display_dots, display_head, feed_intervals, float, surface,
};

/// The Platonic shape a drawn particle is, scaled by the particle's radius.
const ICOSAHEDRON: usize = 3;

/// Draw the push-out particles as instanced spheres on the water object, in
/// place of the surface and Physics View's dots.
pub(super) fn particles_draw(b: &mut Builder) {
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
    let copies = b.node(
        "liquid_particle_copies",
        "node.particles_to_copies",
        json!({}),
    );
    let sphere = b.node(
        "liquid_particle_mesh",
        "node.platonic_solid_mesh",
        json!({"radius": float(1.0), "shape": {"type": "Enum", "value": ICOSAHEDRON}}),
    );
    let [frame, particles, water] =
        ["frame", "particle_push_out", "water_object"].map(|name| b.id(name));
    for (from, to) in [
        ((particles, "out"), (copies, "particles")),
        ((frame, "count_b"), (copies, "live_count")),
        ((copies, "copies"), (water, "instances")),
        ((sphere, "vertices"), (water, "vertices")),
        ((frame, "count_b"), (water, "instance_count")),
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
const DRY_SAND: [(&str, f32); 8] = [
    ("ambient", 0.0),
    ("color_r", 0.55),
    ("color_g", 0.45),
    ("color_b", 0.30),
    ("metallic", 0.0),
    ("roughness", 0.9),
    ("ior", 1.5),
    ("transmission", 0.0),
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

/// The Matter solver with the shared surface build, drawn as sand.
fn mpm(draw: WaterDraw, _narrow_band: bool) -> WaterFamily {
    let solver: EffectGraphDef =
        serde_json::from_str(include_str!("../../../assets/matter_solver.json"))
            .expect("the Matter solver parses");
    let mut b = Builder::from_def(solver);
    let [domain, frame] = ["domain", "frame"].map(|name| b.id(name));
    let scene = WaterScene::dam_break(64);
    let (surface, level_set) = surface(&mut b, scene, frame);
    let water = display_head(&mut b, frame, surface);
    display_dots(&mut b, frame, (surface, (surface, level_set)), water);
    feed_intervals(&b.nodes, &mut b.wires, domain as u32);
    sand_material(&mut b);
    let mut objects = vec!["water_object".to_owned(), "dots_object".to_owned()];
    if draw == WaterDraw::Particles {
        particles_draw(&mut b);
        objects.retain(|object| object != "dots_object");
    } else {
        sand_surface_draw(&mut b);
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
        fn $name() -> Arc<WaterFamily> {
            static FAMILY: OnceLock<Arc<WaterFamily>> = OnceLock::new();
            FAMILY
                .get_or_init(|| Arc::new($build($draw, $narrow_band)))
                .clone()
        }
        inventory::submit! {
            WaterTemplate { kind: $kind, draw: $draw, narrow_band: $narrow_band, family: $name }
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
