//! Water, Sand and Water Source (SCENE_DOCUMENT_DESIGN.md section 7.10
//! (Water, Water Source, Ocean)): each Water or Sand splices one copy of the
//! family template for its kind and Draw As (`scene_schema::water_family`),
//! its node ids `<item id>:<template node id>`, and draws the template's
//! objects. Its Transform node is the domain box. Each Water Source child
//! compiles to a `node.fluid_role_source` placed by its own Transform (a
//! Model's by its Object's), wired into the domain's role inputs in tree
//! order; the domain has a role input per source.
//!
//! Inside a Group, the domain box and the sources sit in the group's frame
//! and the water follows the group's visibility. The domain stays
//! axis-aligned: a turned group moves the box but does not turn it.
//!
//! Every Sand in a scene shares one Matter solver (MATTER_SAND_DESIGN.md
//! section 7), up to `MAX_MATTER_SLOTS` to a solver: the first splices the
//! whole template, each later one only its material and draw, reading the
//! first's solver, and its Fill takes the solver's role ports for its slot.
//!
//! The water draws with its Material, as any object does; with none it
//! keeps the template's water look. Solids reach the water through the
//! World (section 7.8): the coupling install pairs every Water with the
//! World's Colliders and Bodies.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_core::liquid_domain::{
    MAX_MATTER_SLOTS, MAX_WATERS, SHARED_TABLE, WATER_PUSH_NODE, WATER_PUSH_STEP,
};
use manifold_core::scene_doc::{Item, ItemFields, ItemKind, Quality, SceneDoc, SourceShape};
use manifold_core::scene_schema::{
    DOMAIN_ROLE, GRAIN_SHAPE_ROLE, GRAIN_SQUASH_ROLE, PHYSICS_VIEW_SWITCHES, SOLID,
    SURFACE_DETAIL_NODES, SURFACE_DETAIL_OFFSET, WATER_MATERIAL_ROLE, WORLD_FOLLOWS, WaterFamily,
    role_index, rows, template_roles, water_family,
};
use manifold_core::scene_world_controls::WORLD_INPUTS;
use manifold_foundation::ItemId;
use manifold_node_engine::scene::render_mode::RENDER_MODE_PHYSICS_VIEW;

use super::builder::Builder;
use super::material::surface_for;

/// Every Water and Sand in tree order, with the Group it sits in, if any.
pub(crate) fn waters(doc: &SceneDoc) -> Vec<(Item, Option<ItemId>)> {
    items_of(doc, ItemKind::holds_sources)
}

/// Every item of the kinds `wanted` accepts, in tree order, with the Group it
/// sits in, if any.
pub(crate) fn items_of(
    doc: &SceneDoc,
    wanted: fn(ItemKind) -> bool,
) -> Vec<(Item, Option<ItemId>)> {
    let mut found = Vec::new();
    doc.walk(|item, ancestors| {
        if wanted(item.kind()) {
            let group = ancestors
                .iter()
                .rev()
                .find(|ancestor| ancestor.kind() == ItemKind::Group)
                .map(|group| group.id.clone());
            found.push((item.clone(), group));
        }
    });
    found
}

/// The Sand template's particle frame, state, domain and Points to Grid, and
/// the Water template's step and wet map: sand reaches the water from the
/// one to the others, and the water pushes the sand back in the sand's tick
/// ([`water_push`]). The step reads the sand of the same tick from
/// `MATTER_TICK_STATS`.
const SAND_FRAME: &str = "frame";
const SAND_STATE: &str = "state";
const SAND_DOMAIN: &str = "domain";
const MATTER_TO_GRID: &str = "matter_points_to_grid";
/// The Sand template's `node.matter_material`: what the Sand is made of.
const MATTER_MATERIAL: &str = "material";
/// The Sand template's solver nodes. A later Sand splices none of them and
/// reads the first's.
const MATTER_SOLVER: [&str; 10] = [
    SAND_DOMAIN,
    "matter_fill",
    SAND_STATE,
    MATTER_TO_GRID,
    "matter_grid_update",
    "matter_grid_to_points",
    MATTER_TICK_STATS,
    "matter_move_bodies",
    "matter_body_reaction",
    "matter_static_solid",
];
/// The Water template's solver nodes. A later Water sharing the solver
/// splices none of them and reads the first's: one solver steps every
/// Water's particles, each Water keeping its own frame, surface and look.
const WATER_SOLVER: [&str; 7] = [
    DOMAIN_ROLE,
    "fill",
    "state",
    "step",
    "stats",
    "whitewater",
    "whitewater_obstacle_source",
];
/// The whitewater a shared solver makes draws once, with its first Water.
const WATER_WHITEWATER_OBJECTS: [&str; 2] = ["spray_object", "bubble_object"];
/// A material node's folds, each fed to the next slot's (`node.matter_material`).
const MATERIAL_CHAIN: [(&str, &str); 6] = [
    ("materials", "materials_in"),
    ("wave", "wave_in"),
    ("snow_wave", "snow_wave_in"),
    ("diffusivity", "diffusivity_in"),
    ("density", "density_in"),
    ("wall_friction", "wall_friction_in"),
];
/// The frame inputs a draw frame reads from the solver.
const FRAME_INPUTS: [&str; 6] = [
    "points",
    "stats",
    "count",
    "simulation_time",
    "display_time",
    "epoch",
];

/// Where a Water sits in the liquid solver it shares: its slot (the Water
/// index its particles carry), the role port its first source takes and the
/// ports of every Water on the solver, and the first Water's nodes, which a
/// later one reads.
struct WaterSlot<'a> {
    slot: usize,
    first_port: usize,
    ports: usize,
    solver: Option<&'a BTreeMap<String, u32>>,
}

/// The solver a Water shares, by its item id: the first Water's id and the
/// Water's slot. Every Boundless Water without Narrow Band shares one,
/// `MAX_WATERS` a solver in scene order, when more than one does; a tank
/// (Bounds on) or a Narrow Band Water keeps its own solver and is not here.
fn shared_waters(all: &[(Item, Option<ItemId>)]) -> BTreeMap<String, (String, usize)> {
    let joins: Vec<&Item> = all
        .iter()
        .map(|(item, _)| item)
        .filter(|item| {
            matches!(&item.fields, ItemFields::Water(water) if !water.bounds && !water.narrow_band)
                && water_family(&item.fields).is_some()
        })
        .collect();
    let mut shared = BTreeMap::new();
    for solver in joins.chunks(MAX_WATERS as usize) {
        if solver.len() < 2 {
            continue;
        }
        for (slot, item) in solver.iter().enumerate() {
            shared.insert(
                item.id.as_str().to_owned(),
                (solver[0].id.as_str().to_owned(), slot),
            );
        }
    }
    shared
}

/// Where a Sand sits in its Matter solver: its slot, how many Sands the
/// solver runs, the role ports each slot's sources take, and the first
/// Sand's nodes, which a later one reads.
struct MatterSlot<'a> {
    slot: usize,
    items: usize,
    per_slot: usize,
    solver: Option<&'a BTreeMap<String, u32>>,
}
const WATER_WETNESS: &str = "water_wetness";
/// The Sand template's tick-end node (`node.matter_stats`), whose
/// `grain_view` holds every grain at the end of each tick.
const MATTER_TICK_STATS: &str = "matter_tick_stats";

/// What Smoke and Fire flow round inside each tick of the World's loop
/// (`gas::emit_all`): the first Water's step, whose tiles hold its water as
/// the tick left it, and the first Sand's tick-end stats, whose `grain_view`
/// holds its grains.
#[derive(Clone, Copy, Default)]
pub(crate) struct MatterFrames {
    pub(crate) water_step: Option<u32>,
    pub(crate) grains: Option<u32>,
}

/// Emit every Water and Sand and its sources. With no water solver linked,
/// they draw nothing. The Boundless Waters run on one liquid solver a group
/// of `MAX_WATERS` ([`shared_waters`]), so they mix and push each other; the
/// Sands run on one Matter solver a group of
/// `MAX_MATTER_SLOTS`; the first solver's every grain is porous to every
/// Water: its particles this frame reach each water's step, which slows the
/// water inside the sand, so water soaks into a pile instead of passing
/// through it, and each water's wet map, which marks the sand where the
/// water touches it. The first Water lifts and drags those grains back
/// once a tick ([`water_push`]).
pub(crate) fn emit_all(b: &mut Builder<'_>) -> MatterFrames {
    let all = waters(b.doc);
    let sources = |item: &Item| {
        item.children
            .iter()
            .filter(|child| matches!(child.fields, ItemFields::WaterSource(_)))
            .count()
    };
    let sands: Vec<usize> = all
        .iter()
        .filter(|(item, _)| item.kind() == ItemKind::Sand && water_family(&item.fields).is_some())
        .map(|(item, _)| sources(item))
        .collect();
    let per_slot = sands.iter().copied().max().unwrap_or(0).max(1);
    // The role ports of each shared liquid solver: every Water's sources in
    // turn.
    let shared = shared_waters(&all);
    let mut ports: BTreeMap<&str, usize> = BTreeMap::new();
    let mut first_ports: BTreeMap<&str, usize> = BTreeMap::new();
    for (item, _) in &all {
        if let Some((owner, _)) = shared.get(item.id.as_str()) {
            let taken = ports.entry(owner.as_str()).or_default();
            first_ports.insert(item.id.as_str(), *taken);
            *taken += sources(item);
        }
    }
    let mut liquids: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut solvers: Vec<Solver> = Vec::new();
    let mut water_ids = Vec::new();
    for (item, group) in &all {
        let Some(family) = water_family(&item.fields) else {
            continue;
        };
        if item.kind() != ItemKind::Sand {
            let place = shared.get(item.id.as_str()).map(|(owner, slot)| WaterSlot {
                slot: *slot,
                first_port: first_ports[item.id.as_str()],
                ports: ports[owner.as_str()],
                solver: liquids.get(owner).filter(|_| *slot > 0),
            });
            let ids = emit(b, item, group.as_ref(), &family, None, place);
            if shared
                .get(item.id.as_str())
                .is_some_and(|(_, slot)| *slot == 0)
            {
                liquids.insert(item.id.as_str().to_owned(), ids.clone());
            }
            if item.kind() == ItemKind::Water {
                water_ids.push((item.id.as_str().to_owned(), ids));
            }
            continue;
        }
        let index = solvers
            .iter()
            .map(|solver| solver.slots.len())
            .sum::<usize>();
        let slot = index % MAX_MATTER_SLOTS;
        let items = (sands.len() - (index - slot)).min(MAX_MATTER_SLOTS);
        let solver = solvers
            .last()
            .filter(|_| slot > 0)
            .map(|solver| &solver.ids);
        let place = MatterSlot {
            slot,
            items,
            per_slot,
            solver,
        };
        let ids = emit(b, item, group.as_ref(), &family, Some(place), None);
        let owned = ids.clone();
        if slot == 0 {
            solvers.push(Solver {
                owner: item.id.as_str().to_owned(),
                ids,
                slots: Vec::new(),
            });
        }
        let solver = solvers.last_mut().expect("a solver for every Sand");
        solver.slots.push((
            owned.get(MATTER_MATERIAL).copied(),
            owned.get(SAND_FRAME).copied(),
        ));
    }
    let couples = !water_ids.is_empty();
    let mut sand = None;
    for (index, solver) in solvers.iter().enumerate() {
        let coupling = join_slots(b, solver, couples && index == 0);
        if index == 0 {
            sand = coupling;
        }
    }
    let frames = MatterFrames {
        water_step: water_ids
            .first()
            .and_then(|(_, ids)| ids.get("step").copied()),
        grains: solvers
            .first()
            .and_then(|solver| solver.ids.get(MATTER_TICK_STATS).copied()),
    };
    let (Some(solver), Some(frame)) = (solvers.first(), sand) else {
        return frames;
    };
    let [points, count] = manifold_core::liquid_domain::SAND_INPUTS;
    let tick_end = solver.ids.get(MATTER_TICK_STATS).copied();
    for (_, ids) in &water_ids {
        // The water steps each tick against the grains at the end of the
        // same tick: the World runs Matter's tick, then the water's.
        if let (Some(&step), Some(tick_end)) = (ids.get("step"), tick_end) {
            b.wire(tick_end, "grain_view", step, points);
            b.wire(tick_end, "view_count", step, count);
        }
        if let Some(&wetness) = ids.get(WATER_WETNESS) {
            b.wire(frame, "particles_b", wetness, points);
            b.wire(frame, "count_b", wetness, count);
        }
    }
    if let Some(&step) = water_ids.first().and_then(|(_, ids)| ids.get("step")) {
        water_push(b, step, solver);
        share_tables(b, step, solver);
    }
    frames
}

/// One Matter solver as emitted: the first Sand's id and node ids, and each
/// Sand's material and draw frame, by slot.
struct Solver {
    owner: String,
    ids: BTreeMap<String, u32>,
    slots: Vec<(Option<u32>, Option<u32>)>,
}

/// Join a solver's Sands: chain their materials in slot order, the last one
/// feeding the solver, and give each draw its own run of points. With
/// `couples`, add the frame of every point the water's wet map reads (the
/// first Sand's frame when it runs alone); returns it.
fn join_slots(b: &mut Builder<'_>, solver: &Solver, couples: bool) -> Option<u32> {
    let (Some(&domain), Some(&(_, Some(first_frame)))) =
        (solver.ids.get(SAND_DOMAIN), solver.slots.first())
    else {
        return None;
    };
    if solver.slots.len() < 2 {
        return Some(first_frame);
    }
    let coupling = couples.then(|| {
        let frame = b.add(&solver.owner, "coupling_frame", "node.matter_frame");
        for port in FRAME_INPUTS {
            if let Some((from, from_port)) = b.input(first_frame, port) {
                b.wire(from, &from_port, frame, port);
            }
        }
        frame
    });
    let materials: Vec<u32> = solver
        .slots
        .iter()
        .filter_map(|(material, _)| *material)
        .collect();
    if let (Some(&first), Some(&last)) = (materials.first(), materials.last()) {
        for (port, _) in MATERIAL_CHAIN {
            b.splice(first, port, last, port);
        }
        for pair in materials.windows(2) {
            for (port, into) in MATERIAL_CHAIN {
                b.wire(pair[0], port, pair[1], into);
            }
        }
    }
    for (slot, (_, frame)) in solver.slots.iter().enumerate() {
        if let Some(frame) = *frame {
            b.rewire(domain, &format!("points_{slot}"), frame, "count");
            b.wire(domain, &format!("first_{slot}"), frame, "first");
        }
    }
    coupling
}

/// The first Water lifts and drags the Sand once a tick
/// (`node.matter_water_push`, between the Sand's state and its Points to Grid
/// in the Sand's tick), from the water's step as its last tick left it: in
/// the World's tick loop the Sand steps before the water, which reads the
/// grains of the same tick.
fn water_push(b: &mut Builder<'_>, step: u32, solver: &Solver) {
    let ids = &solver.ids;
    let (Some(&state), Some(&domain), Some(&to_grid)) = (
        ids.get(SAND_STATE),
        ids.get(SAND_DOMAIN),
        ids.get(MATTER_TO_GRID),
    ) else {
        return;
    };
    let (Some(cell_size), Some(materials), Some(count)) = (
        b.input(step, "cell_size"),
        b.input(to_grid, "materials"),
        b.input(to_grid, "active_count"),
    ) else {
        return;
    };
    let node = b.add(&solver.owner, "water_push", WATER_PUSH_NODE);
    b.wire(state, "out", node, "points");
    b.rewire(node, "points_out", to_grid, "points");
    b.wire(state, "tick_start", node, "tick_start");
    b.wire(domain, "interval_duration", node, "interval_duration");
    for port in ["gravity_x", "gravity", "gravity_z"] {
        b.wire(domain, port, node, port);
    }
    for ((from, port), to) in [
        (materials, "materials"),
        (count, "active_count"),
        (cell_size, "cell_size"),
    ] {
        b.wire(from, &port, node, to);
    }
    for port in WATER_PUSH_STEP {
        b.wire(step, port, node, port);
    }
}

/// The first Water's and the Sand's world-tile tables share one key set
/// (`world_tiles::SharedTable`): each reads the other's table and pool, the
/// water the Sand's of the same tick, the Sand the water's last tick's.
fn share_tables(b: &mut Builder<'_>, step: u32, solver: &Solver) {
    let Some(&to_grid) = solver.ids.get(MATTER_TO_GRID) else {
        return;
    };
    let Some((pool, pool_port)) = b.input(to_grid, "pool_tiles") else {
        return;
    };
    let [tiles, pool_tiles] = SHARED_TABLE;
    b.wire(to_grid, "tiles", step, tiles);
    b.wire(pool, &pool_port, step, pool_tiles);
    b.wire(step, "tiles", to_grid, tiles);
    b.wire(step, "pool_tiles", to_grid, pool_tiles);
}

fn emit(
    b: &mut Builder<'_>,
    item: &Item,
    group: Option<&ItemId>,
    family: &WaterFamily,
    matter: Option<MatterSlot<'_>>,
    water: Option<WaterSlot<'_>>,
) -> BTreeMap<String, u32> {
    let owner = item.id.as_str();
    let solver = matter
        .as_ref()
        .and_then(|place| place.solver.map(|solver| (solver, &MATTER_SOLVER[..])))
        .or_else(|| {
            water
                .as_ref()
                .and_then(|place| place.solver.map(|solver| (solver, &WATER_SOLVER[..])))
        });
    let ids = match solver {
        Some((solver, shared)) => splice_member(b, owner, family, solver, shared),
        None => splice(b, owner, family),
    };
    let solver = solver.map(|(solver, _)| solver);
    let node = |name: &str| ids.get(name).copied();
    for role in template_roles(rows(item.kind()))
        .chain(PHYSICS_VIEW_SWITCHES)
        .chain(SURFACE_DETAIL_NODES)
    {
        if let Some(id) = node(role) {
            b.adopt(owner, role, id);
        }
    }
    let Some(domain) = solver.map_or_else(
        || node(DOMAIN_ROLE),
        |solver| solver.get(DOMAIN_ROLE).copied(),
    ) else {
        return ids;
    };
    let frame = group.and_then(|group| b.group_frames.get(group).copied());
    let visible = group.and_then(|group| b.group_visible.get(group).copied());
    // A Sand's grains take its matter's shape.
    if let ItemFields::Sand(sand) = &item.fields {
        let shape = manifold_core::scene_schema::grain_shape(sand.matter);
        if let Some(switch) = node(GRAIN_SHAPE_ROLE) {
            b.set_number(switch, "selector", if shape.pebble { 1.0 } else { 0.0 });
        }
        if let Some(squash) = node(GRAIN_SQUASH_ROLE) {
            for (axis, scale) in ["scale_x", "scale_y", "scale_z"]
                .into_iter()
                .zip(shape.squash)
            {
                b.set_number(squash, axis, scale);
            }
        }
    }
    if let (ItemFields::Sand(sand), Some(material)) = (&item.fields, node(MATTER_MATERIAL)) {
        b.set(
            material,
            "material",
            SerializedParamValue::Enum {
                value: sand.matter.index(),
            },
        );
        if let Some(place) = &matter {
            b.set(
                material,
                "slot",
                SerializedParamValue::Int {
                    value: place.slot as i32,
                },
            );
        }
    }
    if solver.is_none() {
        solver_setup(b, item, domain, frame);
    }

    let sources: Vec<&Item> = item
        .children
        .iter()
        .filter(|child| matches!(child.fields, ItemFields::WaterSource(_)))
        .collect();
    // One Sand or Water alone takes a role port a source; Sands sharing a
    // solver take `per_slot` ports each, from their slot's; Waters sharing a
    // solver take theirs after the Waters before them, and each Water's frame
    // draws its own particles.
    let first_port = match (&matter, &water) {
        (Some(place), _) if place.items > 1 => {
            if place.slot == 0 {
                b.set(
                    domain,
                    "roles",
                    SerializedParamValue::Int {
                        value: (place.items * place.per_slot) as i32,
                    },
                );
                b.set(
                    domain,
                    "roles_per_slot",
                    SerializedParamValue::Int {
                        value: place.per_slot as i32,
                    },
                );
            }
            place.slot * place.per_slot
        }
        (_, Some(place)) => {
            if place.slot == 0 {
                b.set(
                    domain,
                    "roles",
                    SerializedParamValue::Int {
                        value: place.ports as i32,
                    },
                );
            }
            if let Some(frame) = node(SAND_FRAME) {
                b.set(
                    frame,
                    "water",
                    SerializedParamValue::Int {
                        value: place.slot as i32,
                    },
                );
            }
            place.first_port
        }
        _ => {
            b.set(
                domain,
                "roles",
                SerializedParamValue::Int {
                    value: sources.len() as i32,
                },
            );
            0
        }
    };
    for (port, source) in sources.into_iter().enumerate() {
        let water = water.as_ref().map(|place| place.slot);
        emit_source(b, source, domain, first_port + port, frame, water);
    }

    // The water's look is its Material's, as for any object, maps included
    // on every object drawing with it (a Sand's grains as well as its
    // surface).
    if b.doc.effective_material(&item.id).is_some()
        && let Some(look) = node(WATER_MATERIAL_ROLE)
    {
        let (surface, maps) = surface_for(b, &item.id);
        let drawn = b.fed(look, "out", "material");
        b.splice(look, "out", surface, "out");
        for (source, port) in maps {
            for &object in &drawn {
                b.wire(source, "out", object, port);
            }
        }
    }

    let later_water = water.as_ref().is_some_and(|place| place.slot > 0);
    for object in &family.objects {
        if later_water && WATER_WHITEWATER_OBJECTS.contains(&object.as_str()) {
            continue;
        }
        if let Some(id) = node(object) {
            if let Some(visible) = visible {
                b.wire(visible, "out", id, "parent_visible");
            }
            b.objects.push(id);
            b.object_items.push(item.id.clone());
        }
    }
    // Surface Detail drives every surface node from one value, shown from 0.
    if family.holds(SURFACE_DETAIL_NODES[0]) {
        b.add(owner, "surface_detail", "node.value");
    }
    b.rows(item);
    if let Some(detail) = b.id(owner, "surface_detail") {
        for target in SURFACE_DETAIL_NODES {
            if let Some(target) = b.id(owner, target) {
                b.follow_offset(
                    detail,
                    "value",
                    target,
                    "resolution_scale",
                    SURFACE_DETAIL_OFFSET,
                );
            }
        }
    }
    ids
}

/// The domain's own nodes, for a Water or the first Sand of a solver: its
/// box (the item's Transform, in its group's frame), the World's cell, the
/// Bounds switch and the World's clock and gravity.
fn solver_setup(b: &mut Builder<'_>, item: &Item, domain: u32, frame: Option<u32>) {
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(frame) = frame {
        b.wire(frame, "transform", transform, "parent");
    }
    b.wire(transform, "transform", domain, "domain");

    // The World's Cell Size reaches it in `follow_world`; a scene with no
    // World runs at the default cell.
    let cell_size = b.add(owner, "cell_size", "node.value");
    b.set_number(
        cell_size,
        "value",
        manifold_core::fluid_domain::DEFAULT_CELL_SIZE,
    );
    b.wire(cell_size, "out", domain, "cell_metres");
    let bounds = match &item.fields {
        ItemFields::Water(water) => water.bounds,
        ItemFields::Sand(sand) => sand.bounds,
        _ => false,
    };
    b.set(
        domain,
        "bounds",
        SerializedParamValue::Bool { value: bounds },
    );
    // Every water runs at the World's Speed, as the bodies do: one clock for
    // everything that touches. A per-region speed would feed the domain's
    // `speed` input here.
    for (control, role) in WORLD_INPUTS.iter().zip(WORLD_FOLLOWS) {
        let value = b.add(owner, role, "node.value");
        b.wire(value, "out", domain, control.input);
    }
}

/// One source: its Transform places a `node.fluid_role_source` (a unit box,
/// or a ball one metre across) wired into the domain's `role_<port>`. A
/// Model takes its Object's mesh and stands where the Object does, in the
/// frame of the groups round it; with no Object picked it is off. `water`
/// is the slot of a Water sharing a solver, which the source's particles
/// carry.
fn emit_source(
    b: &mut Builder<'_>,
    item: &Item,
    domain: u32,
    port: usize,
    frame: Option<u32>,
    water: Option<usize>,
) {
    let ItemFields::WaterSource(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(frame) = frame {
        b.wire(frame, "transform", transform, "parent");
    }
    let role = b.add(owner, "role", "node.fluid_role_source");
    b.set(
        role,
        "role",
        SerializedParamValue::Enum {
            value: role_index(fields.role),
        },
    );
    if let Some(water) = water {
        b.set(
            role,
            "water",
            SerializedParamValue::Int {
                value: water as i32,
            },
        );
    }
    let place = |b: &mut Builder<'_>, from: u32| b.wire(from, "transform", role, "transform");
    let shape = |b: &mut Builder<'_>, mesh: u32| b.wire(mesh, "source", role, "mesh_0");
    match fields.shape {
        SourceShape::Box => place(b, transform),
        SourceShape::Sphere => {
            place(b, transform);
            let sphere = b.add(owner, "sphere", "node.sphere_mesh");
            shape(b, sphere);
        }
        SourceShape::Model => {
            let target = fields.model.as_ref().and_then(|model| b.doc.find(model));
            let mesh = target.and_then(|target| {
                let id = target.id.as_str();
                b.id(id, "mesh").or_else(|| b.id(id, SOLID))
            });
            let placed = target.zip(mesh).and_then(|(target, mesh)| {
                Some((super::swarm::emit_frame(b, owner, target)?, mesh))
            });
            match placed {
                Some((frame, mesh)) => {
                    place(b, frame);
                    shape(b, mesh);
                }
                None => {
                    b.wire(transform, "transform", role, "transform");
                    b.set(role, "enabled", SerializedParamValue::Bool { value: false });
                }
            }
        }
    }
    b.wire(role, "role", domain, &format!("role_{port}"));
    b.rows(item);
}

/// Add a copy of the template with fresh doc ids and `<owner>:` node ids;
/// returns each template node id's new doc id, at any depth.
fn splice(b: &mut Builder<'_>, owner: &str, family: &WaterFamily) -> BTreeMap<String, u32> {
    let mut ids = BTreeMap::new();
    let mut nodes = family.nodes.clone();
    let mut wires = family.wires.clone();
    renumber(
        b,
        owner,
        &mut nodes,
        &mut wires,
        &mut ids,
        true,
        &BTreeMap::new(),
    );
    for node in nodes {
        b.push_node(node);
    }
    for wire in wires {
        b.wire(wire.from_node, &wire.from_port, wire.to_node, &wire.to_port);
    }
    ids
}

/// A later Sand's or Water's copy of the template: all but the solver nodes
/// `names`, which are the first one's (`solver`). What the solver fed now
/// reads the first one's nodes; nothing of this copy feeds the solver.
fn splice_member(
    b: &mut Builder<'_>,
    owner: &str,
    family: &WaterFamily,
    solver: &BTreeMap<String, u32>,
    names: &[&str],
) -> BTreeMap<String, u32> {
    let shared: BTreeMap<u32, u32> = family
        .nodes
        .iter()
        .filter_map(|node| {
            let name = node.node_id.as_str();
            names
                .contains(&name)
                .then(|| solver.get(name).map(|&id| (node.id, id)))
                .flatten()
        })
        .collect();
    let mut ids = BTreeMap::new();
    let mut nodes: Vec<EffectGraphNode> = family
        .nodes
        .iter()
        .filter(|node| !shared.contains_key(&node.id))
        .cloned()
        .collect();
    let mut wires: Vec<EffectGraphWire> = family
        .wires
        .iter()
        .filter(|wire| !shared.contains_key(&wire.to_node))
        .cloned()
        .collect();
    renumber(b, owner, &mut nodes, &mut wires, &mut ids, true, &shared);
    for node in nodes {
        b.push_node(node);
    }
    for wire in wires {
        b.wire(wire.from_node, &wire.from_port, wire.to_node, &wire.to_port);
    }
    ids
}

fn renumber(
    b: &mut Builder<'_>,
    owner: &str,
    nodes: &mut [EffectGraphNode],
    wires: &mut [EffectGraphWire],
    ids: &mut BTreeMap<String, u32>,
    top: bool,
    shared: &BTreeMap<u32, u32>,
) {
    let mut level = BTreeMap::new();
    for node in nodes.iter_mut() {
        let fresh = b.fresh();
        level.insert(node.id, fresh);
        node.id = fresh;
        if !node.node_id.as_str().is_empty() {
            ids.insert(node.node_id.as_str().to_owned(), fresh);
            let scoped = format!("{owner}:{}", node.node_id.as_str());
            // Handles are unique within a level and a group's params route
            // by its inner handles, so only the top level's change.
            if top {
                node.handle = Some(scoped.clone());
            }
            node.node_id = manifold_core::NodeId::new(scoped);
        }
        if let Some(body) = node.group.as_deref_mut() {
            renumber(
                b,
                owner,
                &mut body.nodes,
                &mut body.wires,
                ids,
                false,
                &BTreeMap::new(),
            );
        }
    }
    // A wire from a shared node reads the solver's copy of it.
    for wire in wires.iter_mut() {
        wire.from_node = level
            .get(&wire.from_node)
            .or_else(|| shared.get(&wire.from_node))
            .copied()
            .unwrap_or(wire.from_node);
        wire.to_node = level.get(&wire.to_node).copied().unwrap_or(wire.to_node);
    }
}

/// One grid per World: `owner`'s cell is the World's Cell Size times its own
/// Quality's scale. A scene with no World keeps the default cell.
pub(super) fn follow_cell(b: &mut Builder<'_>, owner: &str, quality: Quality) {
    let Some(cell) = b.id(owner, "cell_size") else {
        return;
    };
    let world_cell = b
        .doc
        .world()
        .and_then(|world| b.id(world.id.as_str(), "cell_size"));
    if let Some(world_cell) = world_cell {
        b.follow(world_cell, "value", cell, "value");
    }
    b.scale_param(cell, "value", quality.cell_scale());
}

/// Make every Water follow the World: gravity, still air, speed and reset
/// through its value nodes, and the World's Render Mode node, whose Physics View draws
/// the particle dots instead of the surface. The switches sit at the mode
/// less Physics View's index plus one, so only Physics View reaches 1; the
/// side that is off is never computed. A solver Waters share runs at the
/// finest of their Qualities.
pub(crate) fn follow_world(b: &mut Builder<'_>, world: Option<u32>, mode: u32) {
    let all = waters(b.doc);
    let shared = shared_waters(&all);
    let quality_of = |item: &Item| match &item.fields {
        ItemFields::Water(water) => water.quality,
        ItemFields::Sand(sand) => sand.quality,
        _ => Quality::Medium,
    };
    for (item, _) in &all {
        let owner = item.id.as_str();
        let quality = all
            .iter()
            .map(|(other, _)| other)
            .filter(|other| {
                other.id == item.id
                    || shared
                        .get(other.id.as_str())
                        .is_some_and(|(first, _)| first == owner)
            })
            .map(quality_of)
            .min_by(|a, b| a.cell_scale().total_cmp(&b.cell_scale()))
            .unwrap_or(Quality::Medium);
        follow_cell(b, owner, quality);
        if let Some(world) = world {
            for (control, role) in WORLD_INPUTS.iter().zip(WORLD_FOLLOWS) {
                if let Some(value) = b.id(owner, role) {
                    b.follow(world, control.world, value, "value");
                }
            }
        }
        for switch in PHYSICS_VIEW_SWITCHES {
            if let Some(switch) = b.id(owner, switch) {
                b.follow_offset(
                    mode,
                    "mode",
                    switch,
                    "selector",
                    1.0 - RENDER_MODE_PHYSICS_VIEW as f32,
                );
            }
        }
    }
}
