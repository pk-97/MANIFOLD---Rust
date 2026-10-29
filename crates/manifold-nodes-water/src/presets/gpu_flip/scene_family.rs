//! The families a scene's Water splices (SCENE_DOCUMENT_DESIGN.md section
//! 7.10 (Water, Water Source, Ocean)), one per solver and draw: no fill of
//! their own and no host interface. The scene compiler wires the Water's
//! sources into the domain's role inputs and the objects into its render
//! node.
//!
//! GPU FLIP is the Dam Break family. MPM is main's Matter solver (the "Live
//! Matter" group of its Matter scenes, kept as `assets/matter_solver.json`)
//! with the same display: the surface's field from the push-out particles, the water
//! object and Physics View's dots. MPM has no whitewater yet: that waits on a
//! whitewater stage that reads any solver.
//! Particles draws the push-out particles as small spheres in place of the
//! surface, as main's Particle View did.

use std::sync::{Arc, OnceLock};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{ItemKind, WaterDraw};
use manifold_core::scene_schema::{PHYSICS_VIEW_SWITCHES, WaterFamily, WaterTemplate};
use serde_json::json;

use super::{
    Builder, DISPLAY_TILES, DOTS_OUTPUT, FAMILY_OUTPUTS, LIQUID_BODY_OUTPUT, WaterScene,
    display_dots, display_head, feed_intervals, float, surface,
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

/// Main's Matter solver with the shared display.
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
    let mut objects = vec!["water_object".to_owned(), "dots_object".to_owned()];
    if draw == WaterDraw::Particles {
        particles_draw(&mut b);
        objects.retain(|object| object != "dots_object");
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
