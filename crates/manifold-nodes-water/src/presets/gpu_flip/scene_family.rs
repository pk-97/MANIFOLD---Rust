//! The families a scene's Water splices (SCENE_DOCUMENT_DESIGN.md section
//! 7.10 (Water, Water Source, Ocean)), one per solver and draw: no fill of
//! their own and no host interface. The scene compiler wires the Water's
//! sources into the domain's role inputs, the objects into its render node
//! and, for GPU FLIP, a whitewater capacity that follows Resolution.
//!
//! GPU FLIP is the Dam Break family. MPM is main's Matter solver (the "Live
//! Matter" group of its Matter scenes, kept as `assets/matter_solver.json`)
//! with the same display: the surface from the push-out particles, the water
//! object and Physics View's dots. MPM has no whitewater yet: that waits on a
//! whitewater stage that reads any solver (LIQUID_SOLVER_SEAM_DESIGN.md P10).
//! Particles draws the push-out particles as small spheres in place of the
//! surface, as main's Particle View did.

use std::sync::{Arc, OnceLock};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{WaterDraw, WaterSolver};
use manifold_core::scene_schema::{WaterFamily, WaterTemplate};
use serde_json::json;

use super::{
    Builder, DOTS_OUTPUT, FAMILY_OUTPUTS, LIQUID_BODY_OUTPUT, PHYSICS_VIEW_SWITCHES, WaterScene,
    display_dots, display_head, feed_intervals, float, surface,
};

/// The Platonic shape a drawn particle is, scaled by the particle's radius.
const ICOSAHEDRON: usize = 3;

/// Draw the push-out particles as instanced spheres on the water object, in
/// place of the surface and Physics View's dots.
pub(super) fn particles_draw(b: &mut Builder) {
    b.remove(&[
        "surface",
        "dots_mesh",
        "dots_material",
        "dots_blend",
        "dots_copies",
        "dots_object",
    ]);
    b.remove(&PHYSICS_VIEW_SWITCHES);
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
fn flip(draw: WaterDraw) -> WaterFamily {
    let scene = WaterScene {
        column: [[0.0; 2]; 3],
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
fn mpm(draw: WaterDraw) -> WaterFamily {
    let solver: EffectGraphDef =
        serde_json::from_str(include_str!("../../../assets/matter_solver.json"))
            .expect("the Matter solver parses");
    let mut b = Builder::from_def(solver);
    let [domain, frame] = ["domain", "frame"].map(|name| b.id(name));
    let scene = WaterScene::dam_break(64);
    let (surface, _) = surface(&mut b, scene, frame);
    // The Matter frame publishes no interior; the surface runs without it.
    b.wires
        .retain(|wire| !(wire.to_node == surface as u32 && wire.to_port == "interior"));
    let water = display_head(&mut b, frame, surface);
    display_dots(&mut b, frame, surface, water);
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
    ($name:ident, $solver:expr, $draw:expr, $build:expr) => {
        fn $name() -> Arc<WaterFamily> {
            static FAMILY: OnceLock<Arc<WaterFamily>> = OnceLock::new();
            FAMILY.get_or_init(|| Arc::new($build($draw))).clone()
        }
        inventory::submit! { WaterTemplate { solver: $solver, draw: $draw, family: $name } }
    };
}

template!(flip_surface, WaterSolver::Flip, WaterDraw::Surface, flip);
template!(
    flip_particles,
    WaterSolver::Flip,
    WaterDraw::Particles,
    flip
);
template!(mpm_surface, WaterSolver::Mpm, WaterDraw::Surface, mpm);
template!(mpm_particles, WaterSolver::Mpm, WaterDraw::Particles, mpm);
