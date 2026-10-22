//! The physics box spawner lives on the copies body. A `node.rigid_body`
//! wired into a `node.physics_world`'s copy-group input `copies_k` is the
//! prototype that group copies, and it carries the copy count, spacing,
//! columns and arrangement. Those controls show only on a copies body, so a
//! scene with no boxes has no box sliders, and any scene gets boxes by
//! wiring a body into a copy-group input. This module names that role for
//! the scene stamp and brings saved graphs to today's shape: spawner
//! controls move off the world, and a world's single copy group takes the
//! numbered port names of group 0.

use std::collections::{BTreeMap, BTreeSet};

use crate::NodeId;
use crate::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue};

pub const PHYSICS_WORLD_TYPE_ID: &str = "node.physics_world";
pub const RIGID_BODY_TYPE_ID: &str = "node.rigid_body";
/// Copy group 0's body input.
pub const FIRST_COPIES_PORT: &str = "copies_0";
const BODY_OUTPUT: &str = "body";
/// A world's copy-group ports before it took a group count, and the
/// group-0 names they now have: two inputs, then two outputs.
const LEGACY_GROUP_INPUTS: [(&str, &str); 2] =
    [("copies", "copies_0"), ("copies_acceleration", "copies_acceleration_0")];
const LEGACY_GROUP_OUTPUTS: [(&str, &str); 2] =
    [("instances", "instances_0"), ("active_count", "active_count_0")];

/// The copy group whose body input `port` is: `copies_k` is group `k`.
pub fn copy_group_of_input(port: &str) -> Option<usize> {
    match crate::physics_members::member_input(port) {
        Some((crate::physics_members::Family::Copies, k)) => Some(k as usize),
        _ => None,
    }
}
/// The spawner's params on a copies body. Each is also a scalar input of the
/// same name.
pub const COPY_PARAMS: [&str; 7] = [
    "copy_count",
    "copy_size_spacing",
    "copy_columns",
    "copy_layout",
    "copy_turn_x",
    "copy_turn_y",
    "copy_turn_z",
];
/// The spawner's params as they sat on a Physics World before they moved to
/// the copies body; Spacing was then metres (`copy_spacing`).
const WORLD_SPAWNER_PARAMS: [&str; 4] = ["copy_count", "copy_spacing", "copy_columns", "copy_layout"];
/// Spacing in metres, before Spacing became a multiple of the copy's size.
const METRES_SPACING: &str = "copy_spacing";
const SIZE_SPACING: &str = "copy_size_spacing";
const COPY_LAYOUT: &str = "copy_layout";
const COPY_TURN: [&str; 3] = ["copy_turn_x", "copy_turn_y", "copy_turn_z"];
/// A copies body's params whose old card drivers act differently now.
const LIVE_PARAMS: [&str; 6] = ["copy_count", "copy_layout", "copy_columns", "copy_turn_x", "copy_turn_y", "copy_turn_z"];
const TRANSFORM_TYPE_ID: &str = "node.transform_3d";
const TRANSFORM_INPUT: &str = "transform";
const SOURCE_INPUT: &str = "source";
const SHAPE_PARAM: &str = "shape";
const ROTATION_INPUTS: [&str; 3] = ["rot_x", "rot_y", "rot_z"];
const SCALE_INPUTS: [&str; 3] = ["scale_x", "scale_y", "scale_z"];
/// The metres spacing a copies body had when none was stored.
const OLD_DEFAULT_METRES: f32 = 1.25;
/// A copies body's Spacing default and range, in multiples of the copy's
/// size. The load writes the default where a copy's old size is unknown.
pub const SIZE_SPACING_DEFAULT: f32 = 1.1;
pub const SIZE_SPACING_RANGE: (f32, f32) = (1.0, 20.0);
/// The Layout labels, by stored value. Pile was relabelled Scatter and keeps 1.
/// Spiral and Random are `node.arrange_copies`' arrangements, spaced in copy
/// widths.
pub const COPY_LAYOUT_LABELS: [&str; 6] = ["Grid", "Scatter", "Line", "Ring", "Spiral", "Random"];
pub const COPY_LAYOUT_SCATTER: u32 = 1;

/// The most copies one world makes across all its groups. A memory ceiling, not
/// a frame budget: every copy group's instance buffer is allocated at this size
/// up front, 2 MiB on the GPU and 2 MiB on the CPU. The copy field recipes
/// (`CopyJitter`, `CopyNoise`) seed one noise sample per copy from a 256 by 256
/// grid, and `CopyBlend` lays out this many: they follow it by hand.
pub const MAX_COPIES: usize = 65_536;

/// Scatter jitters each copy by up to this fraction of its spacing per axis.
pub const SCATTER_JITTER: f32 = 0.04;

/// Half the box width of each Platonic solid at radius one, by shape index.
/// The world measures the same widths from the hulls it installs
/// (`manifold_node_engine::platonic::platonic_points`); a rigid test holds
/// the two equal bit for bit.
pub const PLATONIC_HALF_WIDTH: [f32; 5] = [0.577_350_26, 0.577_350_26, 1.0, 0.850_650_8, 0.934_172_33];

/// A Platonic copy's box width per axis at uniform `scale`, metres: what the
/// world measures from the scaled hull it installs.
pub fn platonic_width(shape: u32, scale: f32) -> [f32; 3] {
    let half = PLATONIC_HALF_WIDTH[(shape as usize).min(4)] * scale;
    [half + half; 3]
}

/// The metres one Spacing step is along each layout axis for a copy whose
/// box is `width` metres: its width along that axis, or for Scatter, whose
/// copies turn, the box diagonal widened so jittered neighbours still clear.
/// One formula for the world and the load migration.
pub fn spacing_unit(scatter: bool, width: [f32; 3]) -> [f32; 3] {
    if scatter {
        let diagonal = (width[0] * width[0] + width[1] * width[1] + width[2] * width[2]).sqrt();
        [diagonal / (1.0 - 2.0 * SCATTER_JITTER); 3]
    } else {
        width
    }
}

/// The Spacing multiple whose world spacing `multiple * unit` is `metres`
/// exactly when one exists, else the nearest.
pub fn size_multiple(metres: f32, unit: f32) -> f32 {
    let guess = metres / unit;
    let mut best = guess;
    let mut candidate_up = guess;
    let mut candidate_down = guess;
    for _ in 0..=8 {
        for candidate in [candidate_up, candidate_down] {
            if candidate * unit == metres {
                return candidate;
            }
            if (candidate * unit - metres).abs() < (best * unit - metres).abs() {
                best = candidate;
            }
        }
        candidate_up = candidate_up.next_up();
        candidate_down = candidate_down.next_down();
    }
    best
}

/// Whether `param` of `type_id` is a spawner control, stamped only on a
/// copies body.
pub fn is_copies_body_param(type_id: &str, param: &str) -> bool {
    type_id == RIGID_BODY_TYPE_ID && COPY_PARAMS.contains(&param)
}

/// Whether the node with doc id `node` feeds a Physics World's copy-group
/// input in this graph level.
pub fn is_copies_body(node: u32, nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> bool {
    wires.iter().any(|w| {
        w.from_node == node
            && w.from_port == BODY_OUTPUT
            && copy_group_of_input(&w.to_port).is_some()
            && nodes.iter().any(|n| n.id == w.to_node && n.type_id == PHYSICS_WORLD_TYPE_ID)
    })
}

/// What [`migrate_world_copies`] did to one graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldCopiesMigration {
    /// The graph carried spawner controls or a pre-count copy-group port on
    /// a Physics World and lost them.
    pub changed: bool,
    /// A world's controls moved onto its copies body.
    pub moved: bool,
    /// A world carried controls but copies no body: they did nothing and
    /// are gone.
    pub no_copies_body: bool,
    /// Copies bodies from before copy groups that became live copy groups.
    pub converted: usize,
    /// Cards that carry a converted group's metres Spacing. Each show's
    /// saved card value converts with that show's own card values
    /// ([`SpacingCard::convert`]).
    pub spacing_cards: Vec<SpacingCard>,
    /// Copy groups the load leaves as they were, each with why: their old
    /// look cannot be rebuilt from the saved graph.
    pub kept: Vec<String>,
    /// Cards on an old group's Count, Layout, Columns, Copy Turn or
    /// transform. They now act at the next physics step: Count, Layout and
    /// Columns used to wait for Reset, and a transform change moved the
    /// copies at frame time and kept their speed.
    pub live_cards: Vec<String>,
}

/// Where a converted copy group reads one of the values its size depends on.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Value(f32),
    /// A card; `value` is the graph's own default for it.
    Card { id: String, value: f32 },
}

impl Lookup {
    fn resolve(&self, card: &impl Fn(&str) -> Option<f32>) -> f32 {
        match self {
            Self::Value(value) => *value,
            Self::Card { id, value } => card(id).unwrap_or(*value),
        }
    }
}

/// Where an old copy group's layout, shape and scale come from: what its
/// metres Spacing step was.
#[derive(Debug, Clone, PartialEq)]
pub struct CopySize {
    pub layout: Lookup,
    pub shape: Lookup,
    pub scale: Lookup,
}

impl CopySize {
    fn unit(&self, card: &impl Fn(&str) -> Option<f32>) -> f32 {
        old_unit(self.layout.resolve(card), self.shape.resolve(card), self.scale.resolve(card))
    }

    /// The Spacing multiple `metres` becomes; [`SIZE_SPACING_DEFAULT`] when
    /// the copy's old size is unknown.
    fn multiple(size: Option<&Self>, metres: f32, card: &impl Fn(&str) -> Option<f32>) -> f32 {
        size.map_or(SIZE_SPACING_DEFAULT, |size| size_multiple(metres, size.unit(card)))
    }
}

/// The card that carried an old copy group's Spacing in metres, and its
/// copy size when the saved graph says it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpacingCard {
    pub card: String,
    pub size: Option<CopySize>,
}

impl SpacingCard {
    /// The Spacing multiple a show's saved `metres` becomes, reading that
    /// show's own card values through `card`.
    pub fn convert(&self, metres: f32, card: impl Fn(&str) -> Option<f32>) -> f32 {
        CopySize::multiple(self.size.as_ref(), metres, &card)
    }

    /// The scale card this show's conversion reads, if any.
    pub fn scale_card(&self) -> Option<&str> {
        match self.size.as_ref().map(|size| &size.scale) {
            Some(Lookup::Card { id, .. }) => Some(id),
            _ => None,
        }
    }
}

/// One metres Spacing step's size multiple for a group laid out the old way:
/// any layout value of one or more was Pile, now Scatter.
fn old_unit(layout: f32, shape: f32, scale: f32) -> f32 {
    let scatter = layout.round().clamp(0.0, 1.0) >= 1.0;
    spacing_unit(scatter, platonic_width(shape.round().clamp(0.0, 4.0) as u32, scale))[0]
}

/// A copies body from before copy groups, found at its graph level.
struct LegacyBody {
    body: NodeId,
    /// The Transform node that sized its copies and turned each one; none
    /// when nothing feeds its transform (scale one, no turn).
    transform: Option<NodeId>,
    name: String,
    /// Why its old look cannot be rebuilt; it is then left as it was, bar
    /// Spacing, which always counts copy sizes.
    kept: Option<&'static str>,
    /// Whether the saved graph says the copy's size: not for an imported
    /// mesh, nor a transform from something other than a Transform node.
    sized: bool,
}

/// Whether `def` still carries spawner controls on a Physics World (a
/// stored or exposed copy param, a wire into a copy input, or a card that
/// drives one) or a wire on a world's pre-count copy-group port. Lets the
/// common load borrow the document instead of cloning.
pub fn carries_world_copies(def: &EffectGraphDef) -> bool {
    if any_world_copies(&def.nodes, &def.wires) {
        return true;
    }
    let mut worlds = BTreeSet::new();
    collect_worlds(&def.nodes, &mut worlds);
    !worlds.is_empty()
        && def
            .preset_metadata
            .as_ref()
            .is_some_and(|m| m.bindings.iter().any(|b| world_copy_target(&worlds, &b.target)))
}

/// Bring a graph from before copy groups to today's shape, once. A world's
/// pre-count ports take group 0's names (`copies` to `copies_0`, and so on)
/// and its spawner controls move onto the body wired into `copies_0`
/// (stored values, exposure, wires and the cards that drive them, card ids
/// kept). Each copies body found on a pre-count port then becomes a live
/// copy group that rests exactly where its old copies did:
/// - its transform's rotation, which turned each copy, moves into Copy Turn
///   (stored value, wire or card) and the group itself is unturned; a
///   transform that also feeds other nodes is split so they keep it;
/// - its metres Spacing becomes the multiple of the copy's size that gives
///   the same metres, exactly when one exists; a card's saved values
///   convert per show ([`WorldCopiesMigration::spacing_cards`]);
/// - its Layout card names the four layouts, Pile keeping 1 as Scatter.
///
/// A body whose old look cannot be rebuilt is left as it was and named in
/// [`WorldCopiesMigration::kept`], except that its Spacing counts copy sizes
/// like every body's (the param has no metres form any more): the same
/// metres where the saved graph says the copy's size, else
/// [`SIZE_SPACING_DEFAULT`]. Runs only on a graph that still carries a
/// pre-count port or world controls, so it is idempotent.
pub fn migrate_world_copies(def: &mut EffectGraphDef) -> WorldCopiesMigration {
    if !carries_world_copies(def) {
        return WorldCopiesMigration::default();
    }
    let mut report = WorldCopiesMigration { changed: true, ..WorldCopiesMigration::default() };
    // World stable id → its copies body's stable id, for the cards.
    let mut bodies = BTreeMap::new();
    let mut legacy = Vec::new();
    let mut splits = Vec::new();
    // Every handle and stable id in the graph: the loader registers handles
    // graph-wide, so a split transform's copy needs names no node has.
    let mut names = BTreeSet::new();
    collect_names(&def.nodes, &mut names);
    migrate_level(&mut def.nodes, &mut def.wires, &mut bodies, &mut legacy, &mut splits, &mut names, &mut report);

    if let Some(meta) = def.preset_metadata.as_mut() {
        let mut bound: BTreeSet<(String, String)> = meta
            .bindings
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => Some((node_id.to_string(), param.clone())),
                _ => None,
            })
            .collect();
        let mut dead = vec![false; meta.bindings.len()];
        for (index, binding) in meta.bindings.iter_mut().enumerate() {
            let BindingTarget::Node { node_id, param } = &mut binding.target else { continue };
            let Some(body) = bodies.get(node_id.as_str()) else { continue };
            if !WORLD_SPAWNER_PARAMS.contains(&param.as_str()) {
                continue;
            }
            match body {
                Some(body) if bound.insert((body.to_string(), param.clone())) => {
                    *node_id = body.clone();
                    report.moved = true;
                }
                Some(_) => dead[index] = true,
                None => {
                    dead[index] = true;
                    report.no_copies_body = true;
                }
            }
        }
        let mut dropped = BTreeSet::new();
        let mut index = 0;
        meta.bindings.retain(|b| {
            let keep = !dead[index];
            index += 1;
            if !keep {
                dropped.insert(b.id.clone());
            }
            keep
        });
        // A card that still drives something else stays.
        dropped.retain(|id| !meta.bindings.iter().any(|b| &b.id == id));
        meta.params.retain(|p| !dropped.contains(&p.id));
        meta.param_aliases
            .retain(|a| !dropped.contains(&a.old) && a.new.as_ref().is_none_or(|id| !dropped.contains(id)));
        meta.value_aliases.retain(|a| !dropped.contains(&a.param_id));
    }
    convert_copy_groups(def, &legacy, &splits, &mut report);
    report
}

fn migrate_level(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    bodies: &mut BTreeMap<String, Option<crate::NodeId>>,
    legacy: &mut Vec<LegacyBody>,
    splits: &mut Vec<(NodeId, NodeId)>,
    names: &mut BTreeSet<String>,
    report: &mut WorldCopiesMigration,
) {
    let worlds: Vec<u32> = nodes.iter().filter(|n| n.type_id == PHYSICS_WORLD_TYPE_ID).map(|n| n.id).collect();
    let mut legacy_bodies = Vec::new();
    for wire in wires.iter_mut() {
        let (port, legacy) = if worlds.contains(&wire.to_node) {
            (&mut wire.to_port, &LEGACY_GROUP_INPUTS)
        } else if worlds.contains(&wire.from_node) {
            (&mut wire.from_port, &LEGACY_GROUP_OUTPUTS)
        } else {
            continue;
        };
        if let Some((old, renamed)) = legacy.iter().find(|(old, _)| port == old) {
            if *old == LEGACY_GROUP_INPUTS[0].0 {
                legacy_bodies.push(wire.from_node);
            }
            *port = (*renamed).to_string();
        }
    }
    for world in worlds {
        let body = wires
            .iter()
            .find(|w| w.to_node == world && w.to_port == FIRST_COPIES_PORT)
            .map(|w| w.from_node)
            .filter(|id| nodes.iter().any(|n| n.id == *id && n.type_id == RIGID_BODY_TYPE_ID));
        let Some(world_index) = nodes.iter().position(|n| n.id == world) else { continue };
        let mut values = Vec::new();
        let mut exposed = Vec::new();
        {
            let world_node = &mut nodes[world_index];
            for param in WORLD_SPAWNER_PARAMS {
                if let Some(value) = world_node.params.remove(param) {
                    values.push((param, value));
                }
                if world_node.exposed_params.remove(param) {
                    exposed.push(param);
                }
            }
        }
        let had_wires = wires.iter().any(|w| w.to_node == world && WORLD_SPAWNER_PARAMS.contains(&w.to_port.as_str()));
        let had_state = !values.is_empty() || !exposed.is_empty() || had_wires;
        let world_id = nodes[world_index].node_id.to_string();
        match body {
            Some(body) => {
                let body_node = nodes.iter_mut().find(|n| n.id == body).expect("body found above");
                for (param, value) in values {
                    body_node.params.entry(param.to_string()).or_insert(value);
                }
                for param in exposed {
                    body_node.exposed_params.insert(param.to_string());
                }
                let body_id = body_node.node_id.clone();
                let wired: BTreeSet<String> =
                    wires.iter().filter(|w| w.to_node == body).map(|w| w.to_port.clone()).collect();
                wires.retain_mut(|w| {
                    if w.to_node != world || !WORLD_SPAWNER_PARAMS.contains(&w.to_port.as_str()) {
                        return true;
                    }
                    if wired.contains(&w.to_port) {
                        return false;
                    }
                    w.to_node = body;
                    true
                });
                report.moved |= had_state;
                bodies.insert(world_id, Some(body_id));
            }
            None => {
                wires.retain(|w| w.to_node != world || !WORLD_SPAWNER_PARAMS.contains(&w.to_port.as_str()));
                report.no_copies_body |= had_state;
                bodies.insert(world_id, None);
            }
        }
    }
    for body in legacy_bodies {
        if let Some(found) = prepare_legacy_body(nodes, wires, body, splits, names) {
            legacy.push(found);
        }
    }
    for node in nodes.iter_mut() {
        if let Some(group) = node.group.as_mut() {
            let group = &mut **group;
            migrate_level(&mut group.nodes, &mut group.wires, bodies, legacy, splits, names, report);
        }
    }
}

/// The wiring half of converting copies body `body`: give it a transform of
/// its own and move a wired rotation onto Copy Turn. Values and cards are
/// converted for the whole graph afterwards ([`convert_copy_groups`]).
fn prepare_legacy_body(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    body: u32,
    splits: &mut Vec<(NodeId, NodeId)>,
    names: &mut BTreeSet<String>,
) -> Option<LegacyBody> {
    let body_node = nodes.iter().find(|n| n.id == body && n.type_id == RIGID_BODY_TYPE_ID)?;
    let name = body_node.handle.clone().unwrap_or_else(|| body_node.node_id.to_string());
    let body_stable = body_node.node_id.clone();
    let wired_into = |node: u32, ports: &[&str]| wires.iter().any(|w| w.to_node == node && ports.contains(&w.to_port.as_str()));
    let fed = wires.iter().find(|w| w.to_node == body && w.to_port == TRANSFORM_INPUT).map(|w| w.from_node);
    let transform = fed.filter(|id| nodes.iter().any(|n| n.id == *id && n.type_id == TRANSFORM_TYPE_ID));
    let foreign = fed.is_some() && transform.is_none();
    let sourced = wired_into(body, &[SOURCE_INPUT]);
    let kept = if foreign {
        Some("its transform does not come from a Transform node")
    } else if sourced {
        Some("it copies an imported mesh, whose size is not known at load")
    } else if wired_into(body, &[METRES_SPACING]) {
        Some("its Spacing is wired in metres")
    } else if wired_into(body, &[SHAPE_PARAM]) {
        Some("its Shape is wired")
    } else if transform.is_some_and(|id| wired_into(id, &SCALE_INPUTS)) {
        Some("its Scale is wired")
    } else if transform.is_some_and(|id| !uniform_scale(nodes, id)) {
        // A layout steps by the copy's size along each axis; old Spacing was
        // the same metres on all three.
        Some("its Scale differs between axes")
    } else {
        None
    };
    // Spacing has no metres form any more; a wire into it now counts sizes.
    for wire in wires.iter_mut().filter(|w| w.to_node == body && w.to_port == METRES_SPACING) {
        wire.to_port = SIZE_SPACING.to_string();
    }
    let sized = !foreign && !sourced;
    let stable_of = |nodes: &[EffectGraphNode], id: u32| nodes.iter().find(|n| n.id == id).map(|n| n.node_id.clone());
    let Some(mut transform) = transform.filter(|_| kept.is_none()) else {
        let transform = transform.and_then(|id| stable_of(&nodes[..], id));
        return Some(LegacyBody { body: body_stable, transform, name, kept, sized });
    };
    let shared = wires
        .iter()
        .any(|w| w.from_node == transform && !(w.to_node == body && w.to_port == TRANSFORM_INPUT));
    if shared {
        let original = nodes.iter().find(|n| n.id == transform).expect("transform found above").clone();
        let id = nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
        let stable = unique_name(names, format!("{}_copies", original.node_id), "_");
        let handle = original.handle.as_ref().map(|handle| unique_name(names, format!("{handle} copies"), " "));
        let copy = EffectGraphNode { id, node_id: NodeId::new(&stable), handle, ..original.clone() };
        splits.push((original.node_id.clone(), copy.node_id.clone()));
        nodes.push(copy);
        // The body's own transform keeps every driver of the original.
        let inputs: Vec<EffectGraphWire> =
            wires.iter().filter(|w| w.to_node == transform).map(|w| EffectGraphWire { to_node: id, ..w.clone() }).collect();
        for wire in wires.iter_mut().filter(|w| w.to_node == body && w.to_port == TRANSFORM_INPUT) {
            wire.from_node = id;
        }
        wires.extend(inputs);
        transform = id;
    }
    for (axis, turn) in COPY_TURN.iter().enumerate() {
        for wire in wires.iter_mut().filter(|w| w.to_node == transform && w.to_port == ROTATION_INPUTS[axis]) {
            wire.to_node = body;
            wire.to_port = (*turn).to_string();
        }
    }
    let transform = stable_of(&nodes[..], transform);
    Some(LegacyBody { body: body_stable, transform, name, kept: None, sized })
}

/// `wanted`, or `wanted` then `separator` and the first free number,
/// recorded as taken.
fn unique_name(names: &mut BTreeSet<String>, wanted: String, separator: &str) -> String {
    let mut name = wanted.clone();
    let mut number = 2;
    while names.contains(&name) {
        name = format!("{wanted}{separator}{number}");
        number += 1;
    }
    names.insert(name.clone());
    name
}

fn collect_names(nodes: &[EffectGraphNode], names: &mut BTreeSet<String>) {
    for node in nodes {
        names.extend(node.handle.iter().cloned());
        names.insert(node.node_id.to_string());
        if let Some(group) = &node.group {
            collect_names(&group.nodes, names);
        }
    }
}

/// The value and card half of converting each legacy copies body: Copy
/// Turn takes the transform's rotation, Spacing becomes a size multiple,
/// and the Layout card names the four layouts.
fn convert_copy_groups(
    def: &mut EffectGraphDef,
    legacy: &[LegacyBody],
    splits: &[(NodeId, NodeId)],
    report: &mut WorldCopiesMigration,
) {
    if let Some(meta) = def.preset_metadata.as_mut() {
        // A split transform's copy answers every card the original did.
        for (original, copy) in splits {
            let fanned: Vec<_> = meta
                .bindings
                .iter()
                .filter(|b| matches!(&b.target, BindingTarget::Node { node_id, .. } if node_id == original))
                .map(|b| {
                    let mut b = b.clone();
                    if let BindingTarget::Node { node_id, .. } = &mut b.target {
                        *node_id = copy.clone();
                    }
                    b
                })
                .collect();
            meta.bindings.extend(fanned);
        }
    }
    for found in legacy {
        match found.kept {
            Some(why) => report.kept.push(format!(
                "Copies: \"{}\" was left as it was because {why}; its copies may sit differently.",
                found.name,
            )),
            None => report.converted += 1,
        }
        let card = |def: &EffectGraphDef, node: &NodeId, param: &str| -> Option<(String, f32)> {
            def.preset_metadata.as_ref()?.bindings.iter().find_map(|b| match &b.target {
                BindingTarget::Node { node_id, param: p } if node_id == node && p == param => {
                    Some((b.id.clone(), b.default_value))
                }
                _ => None,
            })
        };
        let stored = |def: &EffectGraphDef, node: &NodeId, param: &str| -> Option<f32> {
            find_node(&def.nodes, node).and_then(|n| n.params.get(param)).and_then(scalar_value)
        };
        let lookup = |def: &EffectGraphDef, node: &NodeId, param: &str, default: f32| match card(def, node, param) {
            Some((id, value)) => Lookup::Card { id, value },
            None => Lookup::Value(stored(def, node, param).unwrap_or(default)),
        };

        // Copy Turn: the rotation turned each copy; the group is unturned.
        let turned = found.transform.as_ref().filter(|_| found.kept.is_none());
        for (axis, turn) in COPY_TURN.iter().enumerate() {
            let Some(transform) = turned else { break };
            let rotation = ROTATION_INPUTS[axis];
            let value = stored(def, transform, rotation).unwrap_or(0.0);
            let bound = card(def, transform, rotation);
            if let Some(meta) = def.preset_metadata.as_mut() {
                for binding in &mut meta.bindings {
                    if let BindingTarget::Node { node_id, param } = &mut binding.target
                        && node_id == transform
                        && param == rotation
                    {
                        *node_id = found.body.clone();
                        *param = (*turn).to_string();
                    }
                }
            }
            if let Some(node) = find_node_mut(&mut def.nodes, transform) {
                if node.params.contains_key(rotation) {
                    node.params.insert(rotation.to_string(), SerializedParamValue::Float { value: 0.0 });
                }
                let exposed = node.exposed_params.remove(rotation);
                if let Some(body) = find_node_mut(&mut def.nodes, &found.body) {
                    if exposed {
                        body.exposed_params.insert((*turn).to_string());
                    }
                    let turn_value = bound.map_or(value, |(_, default)| default);
                    if turn_value != 0.0 {
                        body.params.insert((*turn).to_string(), SerializedParamValue::Float { value: turn_value });
                    }
                }
            }
        }

        // Spacing: the same metres, as a multiple of the copy's size. A
        // transform that is not a Transform node, or a body with none, sizes
        // its copy at scale one.
        let size = found.sized.then(|| CopySize {
            layout: lookup(def, &found.body, COPY_LAYOUT, 0.0),
            shape: lookup(def, &found.body, SHAPE_PARAM, 1.0),
            scale: found.transform.as_ref().map_or(Lookup::Value(1.0), |t| lookup(def, t, SCALE_INPUTS[0], 1.0)),
        });
        let graph_values = |_: &str| None;
        let multiple = |metres: f32| CopySize::multiple(size.as_ref(), metres, &graph_values);
        let spacing_card = card(def, &found.body, METRES_SPACING);
        if let Some(body) = find_node_mut(&mut def.nodes, &found.body) {
            let metres = body.params.remove(METRES_SPACING).as_ref().and_then(scalar_value).unwrap_or(OLD_DEFAULT_METRES);
            let metres = spacing_card.as_ref().map_or(metres, |(_, default)| *default);
            body.params.insert(SIZE_SPACING.to_string(), SerializedParamValue::Float { value: multiple(metres) });
            if body.exposed_params.remove(METRES_SPACING) {
                body.exposed_params.insert(SIZE_SPACING.to_string());
            }
        }
        let Some(meta) = def.preset_metadata.as_mut() else { continue };
        if let Some((id, _)) = &spacing_card {
            for binding in &mut meta.bindings {
                if let BindingTarget::Node { node_id, param } = &mut binding.target
                    && node_id == &found.body
                    && param == METRES_SPACING
                {
                    *param = SIZE_SPACING.to_string();
                    binding.default_value = multiple(binding.default_value);
                }
            }
            for spec in meta.params.iter_mut().filter(|p| &p.id == id) {
                (spec.min, spec.max) =
                    if size.is_some() { (multiple(spec.min), multiple(spec.max)) } else { SIZE_SPACING_RANGE };
                spec.default_value = multiple(spec.default_value);
            }
            report.spacing_cards.push(SpacingCard { card: id.clone(), size: size.clone() });
        }
        // Layout: Pile keeps 1 and reads Scatter; Line and Ring join it.
        let layout_cards: BTreeSet<String> = meta
            .bindings
            .iter()
            .filter(|b| matches!(&b.target, BindingTarget::Node { node_id, param } if node_id == &found.body && param == COPY_LAYOUT))
            .map(|b| b.id.clone())
            .collect();
        for spec in meta.params.iter_mut().filter(|p| layout_cards.contains(&p.id)) {
            spec.max = (COPY_LAYOUT_LABELS.len() - 1) as f32;
            spec.value_labels = COPY_LAYOUT_LABELS.iter().map(|label| (*label).to_string()).collect();
        }
        for binding in &meta.bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else { continue };
            let live = (node_id == &found.body && LIVE_PARAMS.contains(&param.as_str())) || found.transform.as_ref() == Some(node_id);
            if live && !report.live_cards.contains(&binding.id) {
                report.live_cards.push(binding.id.clone());
            }
        }
    }
}

/// Whether transform node `id` stores the same Scale on every axis.
fn uniform_scale(nodes: &[EffectGraphNode], id: u32) -> bool {
    let Some(node) = nodes.iter().find(|n| n.id == id) else { return true };
    let [x, y, z] = SCALE_INPUTS.map(|axis| node.params.get(axis).and_then(scalar_value).unwrap_or(1.0));
    x == y && y == z
}

fn scalar_value(value: &SerializedParamValue) -> Option<f32> {
    match value {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        SerializedParamValue::Enum { value } => Some(*value as f32),
        _ => None,
    }
}

fn find_node<'a>(nodes: &'a [EffectGraphNode], node: &NodeId) -> Option<&'a EffectGraphNode> {
    nodes.iter().find_map(|n| {
        if &n.node_id == node {
            Some(n)
        } else {
            n.group.as_ref().and_then(|group| find_node(&group.nodes, node))
        }
    })
}

fn find_node_mut<'a>(nodes: &'a mut [EffectGraphNode], node: &NodeId) -> Option<&'a mut EffectGraphNode> {
    for n in nodes {
        if &n.node_id == node {
            return Some(n);
        }
        if let Some(group) = n.group.as_mut()
            && let Some(found) = find_node_mut(&mut group.nodes, node)
        {
            return Some(found);
        }
    }
    None
}

fn world_copy_target(worlds: &BTreeSet<String>, target: &BindingTarget) -> bool {
    matches!(target, BindingTarget::Node { node_id, param }
        if worlds.contains(node_id.as_str()) && WORLD_SPAWNER_PARAMS.contains(&param.as_str()))
}

fn collect_worlds(nodes: &[EffectGraphNode], worlds: &mut BTreeSet<String>) {
    for node in nodes {
        if node.type_id == PHYSICS_WORLD_TYPE_ID && !node.node_id.as_str().is_empty() {
            worlds.insert(node.node_id.to_string());
        }
        if let Some(group) = &node.group {
            collect_worlds(&group.nodes, worlds);
        }
    }
}

fn any_world_copies(nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> bool {
    let is_world = |id: u32| nodes.iter().any(|n| n.id == id && n.type_id == PHYSICS_WORLD_TYPE_ID);
    nodes.iter().any(|n| {
        (n.type_id == PHYSICS_WORLD_TYPE_ID
            && WORLD_SPAWNER_PARAMS.iter().any(|p| n.params.contains_key(*p) || n.exposed_params.contains(*p)))
            || n.group.as_ref().is_some_and(|g| any_world_copies(&g.nodes, &g.wires))
    }) || wires.iter().any(|w| {
        (is_world(w.to_node)
            && (WORLD_SPAWNER_PARAMS.contains(&w.to_port.as_str())
                || LEGACY_GROUP_INPUTS.iter().any(|(old, _)| w.to_port == *old)))
            || (is_world(w.from_node) && LEGACY_GROUP_OUTPUTS.iter().any(|(old, _)| w.from_port == *old))
    })
}

impl crate::project::Project {
    /// Bring every saved graph from before copy groups to today's shape
    /// ([`migrate_world_copies`]): spawner controls move off the Physics
    /// node onto its copies body, and each old copies body becomes a live
    /// copy group resting exactly where its copies did. A show's saved
    /// Spacing card value converts with that show's own card values. The
    /// load report names each change, and each group it had to leave as it
    /// was. Idempotent: a migrated graph carries no pre-count port.
    pub(crate) fn migrate_world_copies_to_body(&mut self) {
        use std::collections::BTreeSet;

        use crate::copies_body::{WorldCopiesMigration, migrate_world_copies};
        use crate::effect_graph_def::EffectGraphDef;
        use crate::effects::PresetInstance;

        fn note(def: &EffectGraphDef, outcome: &WorldCopiesMigration, notes: &mut BTreeSet<String>) {
            let name = def
                .preset_metadata
                .as_ref()
                .map(|m| m.display_name.as_str())
                .filter(|n| !n.is_empty())
                .or(def.name.as_deref())
                .unwrap_or("a scene");
            if outcome.moved {
                notes.insert(format!(
                    "Physics boxes: \"{name}\" box Count, Spacing, Columns and Arrangement moved \
                     from the Physics node onto its copies body."
                ));
            }
            if outcome.no_copies_body {
                notes.insert(format!(
                    "Physics boxes: \"{name}\" had box controls on its Physics node and no copies \
                     body; they did nothing and are gone."
                ));
            }
            if outcome.converted > 0 {
                notes.insert(format!(
                    "Copies: \"{name}\" boxes are a live copy group now, resting exactly where they \
                     did; their old rotation is each copy's Copy Turn and Spacing counts copy sizes."
                ));
            }
            for kept in &outcome.kept {
                notes.insert(format!("\"{name}\": {kept}"));
            }
        }
        fn modulated(fx: &PresetInstance, id: &str) -> bool {
            fx.drivers.iter().flatten().any(|d| d.param_id == id)
                || fx.envelopes.iter().flatten().any(|e| e.param_id == id)
                || fx.audio_mods.iter().flatten().any(|m| m.param_id == id)
        }
        /// Convert one instance's saved Spacing card values, reading its own
        /// cards, and name the modulated cards that now act differently.
        fn convert_cards(fx: &mut PresetInstance, outcome: &WorldCopiesMigration, notes: &mut BTreeSet<String>) {
            for spacing in &outcome.spacing_cards {
                if fx.params.get(&spacing.card).is_none() {
                    continue;
                }
                let metres = fx.get_base_param(&spacing.card);
                if [Some(spacing.card.as_str()), spacing.scale_card()].into_iter().flatten().any(|id| modulated(fx, id)) {
                    notes.insert(format!(
                        "Copies: a modulated Scale or Spacing card ({}) now spaces copies by their size; \
                         they match the old look only at the saved value.",
                        spacing.card
                    ));
                }
                let read = |id: &str| fx.params.get(id).map(|_| fx.get_base_param(id));
                let multiple = spacing.convert(metres, read);
                fx.write_base_param(&spacing.card, multiple);
            }
            for card in outcome.live_cards.iter().filter(|card| modulated(fx, card)) {
                notes.insert(format!(
                    "Copies: a modulated card ({card}) on the copies' Count, Layout, Columns, turn or \
                     transform now acts at the next physics step: Count, Layout and Columns no longer \
                     wait for Reset, and a transform change puts the copies back in formation at rest."
                ));
            }
        }

        let mut notes = BTreeSet::new();
        // Each converted preset the file saved, by id: a layer that tracks it
        // takes this copy as its own graph. The stock preset it tracked now
        // reads its cards differently (Spacing in copy sizes, the transform's
        // rotation turning the whole group), and the file's copy is what the
        // show played when it was saved.
        let mut converted = Vec::new();
        for preset in &mut self.embedded_presets {
            let outcome = migrate_world_copies(&mut preset.def);
            note(&preset.def, &outcome, &mut notes);
            if let Some(id) = preset.id().cloned()
                && outcome.converted > 0
            {
                converted.push((id, preset.def.clone(), outcome));
            }
        }
        self.for_each_preset_instance_mut(|fx| {
            match fx.graph.take() {
                Some(mut def) => {
                    let outcome = migrate_world_copies(&mut def);
                    note(&def, &outcome, &mut notes);
                    fx.graph = Some(def);
                    if outcome.changed {
                        fx.refresh_manifest_from_graph();
                    }
                    convert_cards(fx, &outcome, &mut notes);
                }
                None => {
                    if let Some((_, def, outcome)) = converted.iter().find(|(id, _, _)| id == fx.effect_type()) {
                        fx.graph = Some(def.clone());
                        fx.refresh_manifest_from_graph();
                        convert_cards(fx, outcome, &mut notes);
                        notes.insert(format!(
                            "Copies: a \"{}\" layer now plays the copy of the scene this file saved, so its \
                             boxes stay as they were; it no longer follows changes to the stock scene.",
                            fx.effect_type().as_str()
                        ));
                    }
                }
            }
        });
        for line in notes {
            eprintln!("[Migration] {line}");
            self.load_report.migration_notes.push(line);
        }
    }
}

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::SerializedParamValue;
    use crate::preset_def::PresetKind;
    use crate::project::{EmbeddedOrigin, EmbeddedPreset, Project};

    /// The metres a body's Spacing puts between unit cubes in Scatter.
    fn spaced_metres(body: &EffectGraphNode) -> f32 {
        let multiple = body.params.get("copy_size_spacing").and_then(scalar_value).expect("Spacing counts copy sizes");
        multiple * spacing_unit(true, platonic_width(1, 1.0))[0]
    }

    /// A Physics Boxes graph from before the spawner moved: the four
    /// controls stored on the world, Count exposed on a card, Spacing wired.
    fn old_boxes(with_body: bool) -> EffectGraphDef {
        let float = |v: f32| serde_json::json!({"type": "Float", "value": v});
        let mut wires = vec![
            serde_json::json!({"fromNode": 5, "fromPort": "out", "toNode": 40, "toPort": "copy_spacing"}),
        ];
        if with_body {
            wires.push(serde_json::json!({"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}));
        }
        serde_json::from_value(serde_json::json!({
            "version": 2,
            "name": "Boxes",
            "presetMetadata": {"id": "Boxes", "displayName": "Boxes", "category": "Geometry", "oscPrefix": "b",
                "params": [
                    {"id": "40_copy_count", "name": "Box Count", "min": 0.0, "max": 4000.0, "defaultValue": 256.0},
                    {"id": "40_reset", "name": "Reset", "min": 0.0, "max": 1.0, "defaultValue": 0.0}
                ],
                "bindings": [
                    {"id": "40_copy_count", "label": "Box Count", "defaultValue": 256.0,
                        "target": {"kind": "node", "nodeId": "world", "param": "copy_count"}, "convert": {"type": "IntRound"}},
                    {"id": "40_reset", "label": "Reset", "defaultValue": 0.0,
                        "target": {"kind": "node", "nodeId": "world", "param": "reset"}, "convert": {"type": "Trigger"}}
                ]},
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world",
                    "params": {"copy_count": float(256.0), "copy_spacing": float(1.85), "copy_columns": float(24.0),
                        "copy_layout": {"type": "Enum", "value": 1}, "speed": float(1.0)},
                    "exposedParams": ["copy_count", "reset"]},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body", "params": {"bounce": float(0.38)}},
                {"id": 5, "nodeId": "lfo", "typeId": "node.lfo"}
            ],
            "wires": wires
        }))
        .expect("old boxes graph")
    }

    #[test]
    fn world_controls_move_onto_the_copies_body_with_their_cards() {
        let mut def = old_boxes(true);
        let report = migrate_world_copies(&mut def);
        assert_eq!((report.changed, report.moved, report.no_copies_body), (true, true, false));

        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(COPY_PARAMS.iter().all(|p| !world.params.contains_key(*p) && !world.exposed_params.contains(*p)));
        assert!(world.params.contains_key("speed") && world.exposed_params.contains("reset"), "the world keeps its own");
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        assert_eq!(body.params.get("copy_count"), Some(&SerializedParamValue::Float { value: 256.0 }));
        assert!((spaced_metres(body) - 1.85).abs() < 1e-6, "Spacing counts the cube's size and keeps its metres");
        assert_eq!(body.params.get("copy_columns"), Some(&SerializedParamValue::Float { value: 24.0 }));
        assert_eq!(body.params.get("copy_layout"), Some(&SerializedParamValue::Enum { value: 1 }));
        assert!(body.exposed_params.contains("copy_count"));
        assert!(def.wires.iter().any(|w| w.from_node == 5 && w.to_node == 121 && w.to_port == "copy_size_spacing"), "the wire follows");
        let meta = def.preset_metadata.as_ref().unwrap();
        let count = meta.bindings.iter().find(|b| b.id == "40_copy_count").expect("the card keeps its id");
        assert!(matches!(&count.target, BindingTarget::Node { node_id, param } if node_id == "box_body" && param == "copy_count"));
        assert!(meta.params.iter().any(|p| p.id == "40_copy_count"));

        let migrated = def.clone();
        assert_eq!(migrate_world_copies(&mut def), WorldCopiesMigration::default(), "idempotent");
        assert_eq!(def, migrated);
    }

    #[test]
    fn a_world_with_no_copies_body_loses_its_dead_controls() {
        let mut def = old_boxes(false);
        let report = migrate_world_copies(&mut def);
        assert_eq!((report.changed, report.moved, report.no_copies_body), (true, false, true));
        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(COPY_PARAMS.iter().all(|p| !world.params.contains_key(*p)));
        assert!(!def.wires.iter().any(|w| w.to_port == "copy_spacing"));
        let meta = def.preset_metadata.as_ref().unwrap();
        assert!(!meta.params.iter().any(|p| p.id == "40_copy_count"));
        assert!(meta.params.iter().any(|p| p.id == "40_reset"));
    }

    #[test]
    fn the_body_s_own_controls_and_wiring_win() {
        let mut def = old_boxes(true);
        let body = def.nodes.iter_mut().find(|n| n.node_id == "box_body").unwrap();
        body.params.insert("copy_count".into(), SerializedParamValue::Float { value: 9.0 });
        def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 121, to_port: "copy_spacing".into() });
        migrate_world_copies(&mut def);
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        assert_eq!(body.params.get("copy_count"), Some(&SerializedParamValue::Float { value: 9.0 }));
        assert_eq!(def.wires.iter().filter(|w| w.to_port == "copy_size_spacing").count(), 1, "no double wire");
    }

    #[test]
    fn a_world_s_single_copy_group_takes_group_zero_s_port_names() {
        let mut def = old_boxes(true);
        def.wires.extend([
            EffectGraphWire { from_node: 40, from_port: "instances".into(), to_node: 7, to_port: "instances".into() },
            EffectGraphWire { from_node: 40, from_port: "active_count".into(), to_node: 7, to_port: "instance_count".into() },
            EffectGraphWire { from_node: 5, from_port: "field".into(), to_node: 40, to_port: "copies_acceleration".into() },
            // Another node's ports of the same names stay.
            EffectGraphWire { from_node: 5, from_port: "instances".into(), to_node: 7, to_port: "copies".into() },
        ]);
        assert!(migrate_world_copies(&mut def).changed);
        let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
            def.wires.iter().any(|w| w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port)
        };
        assert!(has(121, "body", 40, "copies_0"));
        assert!(has(40, "instances_0", 7, "instances"));
        assert!(has(40, "active_count_0", 7, "instance_count"));
        assert!(has(5, "field", 40, "copies_acceleration_0"));
        assert!(has(5, "instances", 7, "copies"), "only a world's ports are renamed");
        assert!(!carries_world_copies(&def));
        let migrated = def.clone();
        assert_eq!(migrate_world_copies(&mut def), WorldCopiesMigration::default(), "idempotent");
        assert_eq!(def, migrated);

        // A world saved with no spawner controls still takes the new names.
        let mut quiet: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body"}
            ],
            "wires": [{"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}]
        }))
        .unwrap();
        assert!(carries_world_copies(&quiet));
        let report = migrate_world_copies(&mut quiet);
        assert_eq!((report.changed, report.moved, report.no_copies_body), (true, false, false));
        assert_eq!(quiet.wires[0].to_port, "copies_0");
    }

    #[test]
    fn only_a_body_wired_into_copies_is_a_copies_body() {
        let mut def = old_boxes(true);
        migrate_world_copies(&mut def);
        assert!(is_copies_body(121, &def.nodes, &def.wires));
        assert_eq!(copy_group_of_input("copies_7"), Some(7));
        assert_eq!(copy_group_of_input("copies_acceleration_0"), None);
        assert_eq!(copy_group_of_input("copies"), None);
        assert!(!is_copies_body(5, &def.nodes, &def.wires));
        assert!(is_copies_body_param("node.rigid_body", "copy_count"));
        assert!(!is_copies_body_param("node.physics_world", "copy_count"));
    }

    /// A Physics Boxes scene saved before the box spawner moved: Count,
    /// Spacing, Columns and Arrangement stored on the Physics node, Count on
    /// a card, and a body wired into `copies`.
    fn world_copies_scene() -> EmbeddedPreset {
        let float = |v: f32| serde_json::json!({"type": "Float", "value": v});
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "name": "Box Scene",
            "presetMetadata": {"id": "BoxScene", "displayName": "Box Scene", "category": "Geometry",
                "oscPrefix": "box",
                "params": [{"id": "40_copy_count", "name": "Box Count", "min": 0.0, "max": 4000.0, "defaultValue": 256.0}],
                "bindings": [{"id": "40_copy_count", "label": "Box Count", "defaultValue": 256.0,
                    "target": {"kind": "node", "nodeId": "world", "param": "copy_count"}, "convert": {"type": "IntRound"}}]},
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world", "handle": "world",
                    "params": {"copy_count": float(256.0), "copy_spacing": float(1.85), "copy_columns": float(24.0),
                        "copy_layout": {"type": "Enum", "value": 1}},
                    "exposedParams": ["copy_count"]},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body", "handle": "box_body"}
            ],
            "wires": [{"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}]
        }))
        .expect("box scene def");
        EmbeddedPreset { kind: PresetKind::Generator, def, origin: EmbeddedOrigin::Saved }
    }

    /// The four controls land on the copies body with their values, the
    /// Physics node is left with none, the card keeps its id, the load
    /// report has one line, and loading again changes nothing.
    #[test]
    fn world_box_controls_move_onto_the_copies_body_at_load() {
        use crate::effect_graph_def::{BindingTarget, SerializedParamValue};

        let mut p = Project::default();
        p.embedded_presets.push(world_copies_scene());
        p.on_after_deserialize();

        let def = &p.embedded_presets[0].def;
        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(world.params.keys().all(|k| !k.starts_with("copy_")), "{:?}", world.params);
        assert!(world.exposed_params.iter().all(|k| !k.starts_with("copy_")));
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        for (param, value) in [
            ("copy_count", SerializedParamValue::Float { value: 256.0 }),
            ("copy_columns", SerializedParamValue::Float { value: 24.0 }),
            ("copy_layout", SerializedParamValue::Enum { value: 1 }),
        ] {
            assert_eq!(body.params.get(param), Some(&value), "{param}");
        }
        let meta = def.preset_metadata.as_ref().unwrap();
        assert!(meta.bindings.iter().any(|b| b.id == "40_copy_count"
            && matches!(&b.target, BindingTarget::Node { node_id, param } if node_id == "box_body" && param == "copy_count")));
        assert!((spaced_metres(body) - 1.85).abs() < 1e-6);
        // Nothing feeds this body's transform: an unturned cube at scale one.
        let notes = &p.load_report.migration_notes;
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().all(|n| n.contains("\"Box Scene\"")));
        assert!(notes.iter().any(|n| n.contains("onto its copies body")));
        assert!(notes.iter().any(|n| n.contains("live copy group")));

        let migrated = p.embedded_presets[0].def.clone();
        p.on_after_deserialize();
        assert_eq!(p.embedded_presets[0].def, migrated, "idempotent");
        assert_eq!(p.load_report.migration_notes.len(), 2, "no further note");
    }
}
