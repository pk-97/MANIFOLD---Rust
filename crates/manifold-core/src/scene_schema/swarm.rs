//! Swarm: Emit From, Draw As and Material are fields (section 7.11); its
//! rows reach the nodes `scene_compile::swarm` emits. A swarm simulates in
//! its own emitter frame (its Transform), so everything below follows the
//! emitter, while its forces act in the world. Count, Size and Air Drag are
//! live; Force detail is automatic.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, always, enabled, is_object_or_group, wrong_value,
};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, follows, perform, row, setup};
use crate::scene_doc::{DrawAs, EmitFrom, Item, ItemFields, ItemId, SceneDoc, Shape};

/// Count's ceiling, a memory reserve: every particle buffer is laid at this size when the swarm is built (64 bytes a particle, 6.4 MB a buffer), so Count moves live.
pub const MAX_COUNT: f32 = 100_000.0;

/// [`MAX_COUNT`]'s reason, on the Count row.
pub const MAX_COUNT_REASON: &str = "Up to 100,000 particles, and Count changes live";

const T: Reach = Reach::Own("transform");
const BOUNDS: Reach = Reach::Own("bounds");
const FORCES: Reach = Reach::Own("forces");
const SEED: Reach = Reach::Own("seed");
const START: Reach = Reach::Own("start");
const MOVE: Reach = Reach::Own("move");
const COPIES: Reach = Reach::Own("copies");
const SHAPE: Reach = Reach::Own("shape");
const DRAW: Reach = Reach::Own("object");
use ControlGroup::{Look, Simulation, Source, Transform};

fn emits_box(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Swarm(s) if s.emit_from == EmitFrom::Box)
}

fn emits_sphere(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Swarm(s) if s.emit_from == EmitFrom::Sphere)
}

fn draws_solid(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Swarm(s) if !matches!(s.draw_as, DrawAs::Item(_)))
}

fn draws_sphere(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Swarm(s) if s.draw_as == DrawAs::Sphere)
}

fn draws_platonic(fields: &ItemFields) -> bool {
    draws_solid(fields) && !draws_sphere(fields)
}

/// Every particle draws the sphere, so it is a low-detail one: 360 vertices
/// a particle against the default sphere's 2,880.
const PARTICLE_SPHERE: &str = "a particle sphere is low detail: up to 100,000 draw each frame";

const BOX_OF_SWARM: &str = "the bounds box is fixed in the emitter's own frame, wider than the seed box";
const CLOCK: &str = "the forces node's clock and lattice, wired to the mover";

const fn bounds(property: &'static str, param: &'static str, exposure: Exposure) -> PropertyRow {
    row(property, BOUNDS, Source, exposure).param(param)
}

const fn clock(property: &'static str, param: &'static str) -> PropertyRow {
    row(property, MOVE, Simulation, Exposure::Automatic(CLOCK)).param(param)
}

const FRAME: Reach = Reach::Own("emit_frame");
const EMIT_FRAME: &str = "the emit-from item's own frame, mirrored from its Transform rows";

const fn frame(property: &'static str, param: &'static str) -> PropertyRow {
    row(property, FRAME, Source, Exposure::Automatic(EMIT_FRAME)).param(param)
}

pub static SWARM_ROWS: [PropertyRow; 77] = [
    row("pos_x", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_y", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_z", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("rot_x", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_y", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_z", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("scale_x", T, Transform, setup()),
    row("scale_y", T, Transform, setup()),
    row("scale_z", T, Transform, setup()),
    row("billboard", T, Transform, Exposure::Internal("a swarm's emitter does not face the camera")),
    row("count", SEED, Simulation, perform(CardBand::Scene, 0))
        .param("active_count")
        .default(20_000.0)
        .limit(HardLimit::Between(0.0, MAX_COUNT))
        .tip(MAX_COUNT_REASON),
    row("size", COPIES, Simulation, perform(CardBand::Scene, 1)).limit(AT_LEAST_ZERO),
    row("air_drag", FORCES, Simulation, perform(CardBand::Scene, 2)).unit(Unit::PerSecond).limit(AT_LEAST_ZERO),
    row("visible", DRAW, Look, setup()).toggle().when(draws_solid),
    row("cast_shadows", DRAW, Look, setup()).when(draws_solid),
    row("emission_strength", DRAW, Look, setup()).limit(AT_LEAST_ZERO).when(draws_solid),
    row("gain", DRAW, Look, setup()).limit(AT_LEAST_ZERO).when(draws_solid),
    row("parent_visible", DRAW, Look, Exposure::Automatic("the enclosing group's visibility")),
    row("instances_space", DRAW, Look, Exposure::Internal("particles are placed in the emitter's frame, as object copies")),
    row("force_detail", FORCES, Simulation, Exposure::Internal("force detail is automatic: the default lattice fits the bounds box")),
    row("forces_speed", FORCES, Simulation, Exposure::Automatic("the World's Speed")).param("speed"),
    row("seed_mode", SEED, Source, Exposure::Automatic("the Emit From field: a Box fills its volume, a Sphere or an item scatters over its surface")).param("mode"),
    row("emit_parts", Reach::Own("emit_parts"), Source, Exposure::Automatic("one part for each object in the group the swarm emits from")).param("parts"),
    row("nest_count", Reach::Own("nest"), Source, Exposure::Automatic("the drawn group's own Copies Count")).param("inner_count"),
    row("nest_carriers", Reach::Own("nest"), Source, Exposure::Automatic("the swarm's Count")).param("outer_count"),
    row("seed_capacity", SEED, Source, Exposure::Internal("the most particles a swarm holds; Count's ceiling")).param("max_capacity"),
    row("seed_frame", SEED, Source, Exposure::Internal("a swarm seeds once at its start")).param("frame_seed"),
    row("start_size", START, Source, Exposure::Internal("the seed box is four metres; Scale sizes the emitter")).param("size").default(4.0).when(emits_box),
    row("start_capacity", START, Source, Exposure::Internal("a cube has a fixed vertex count")).param("max_capacity").when(emits_box),
    row("start_radius", START, Source, Exposure::Internal("the seed shell is four metres across")).param("radius").default(2.0).when(emits_sphere),
    row("start_segments", START, Source, Exposure::Internal("a sphere has a fixed vertex count")).param("segments").when(emits_sphere),
    row("start_rings", START, Source, Exposure::Internal("a sphere has a fixed vertex count")).param("rings").when(emits_sphere),
    row("shape_solid", SHAPE, Source, Exposure::Automatic("the Draw As field")).param("shape").when(draws_platonic),
    row("shape_radius", SHAPE, Source, Exposure::Internal("Size scales each particle")).param("radius").when(draws_platonic),
    // A unit-radius ball, the size of the solids beside it.
    row("sphere_radius", SHAPE, Source, Exposure::Internal("Size scales each particle")).param("radius").default(1.0).when(draws_sphere),
    row("sphere_segments", SHAPE, Source, Exposure::Internal(PARTICLE_SPHERE)).param("segments").default(12.0).when(draws_sphere),
    row("sphere_rings", SHAPE, Source, Exposure::Internal(PARTICLE_SPHERE)).param("rings").default(6.0).when(draws_sphere),
    row("move_capacity", MOVE, Simulation, Exposure::Internal("every particle is stepped; dead ones pass through")).param("active_count"),
    clock("move_ticks", "ticks"),
    clock("move_first_tick", "first_tick"),
    clock("move_force_lattices", "force_lattices"),
    clock("move_field_nodes_x", "field_nodes_x"),
    clock("move_field_nodes_y", "field_nodes_y"),
    clock("move_field_nodes_z", "field_nodes_z"),
    clock("move_field_spacing", "field_spacing"),
    clock("move_field_min_x", "field_min_x"),
    clock("move_field_min_y", "field_min_y"),
    clock("move_field_min_z", "field_min_z"),
    clock("move_impulse_tick", "impulse_tick"),
    clock("move_far_terms", "far_terms"),
    bounds("bounds_pos_x", "pos_x", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_pos_y", "pos_y", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_pos_z", "pos_z", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_rot_x", "rot_x", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_rot_y", "rot_y", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_rot_z", "rot_z", Exposure::Internal(BOX_OF_SWARM)),
    bounds("bounds_scale_x", "scale_x", Exposure::Automatic("the bounds box: 1.5 times the seed box")),
    bounds("bounds_scale_y", "scale_y", Exposure::Automatic("the bounds box: 1.5 times the seed box")),
    bounds("bounds_scale_z", "scale_z", Exposure::Automatic("the bounds box: 1.5 times the seed box")),
    bounds("bounds_billboard", "billboard", Exposure::Internal(BOX_OF_SWARM)),
    frame("frame_pos_x", "pos_x"),
    frame("frame_pos_y", "pos_y"),
    frame("frame_pos_z", "pos_z"),
    frame("frame_rot_x", "rot_x"),
    frame("frame_rot_y", "rot_y"),
    frame("frame_rot_z", "rot_z"),
    frame("frame_scale_x", "scale_x"),
    frame("frame_scale_y", "scale_y"),
    frame("frame_scale_z", "scale_z"),
    frame("frame_billboard", "billboard"),
    follows("world_gravity_x", "the World's Gravity X"),
    follows("world_gravity_y", "the World's Gravity Y"),
    follows("world_gravity_z", "the World's Gravity Z"),
    follows("world_air_resistance", "the World's Air Resistance"),
    follows("world_speed", "the World's Speed"),
    follows("world_reset", "the World's Reset"),
    follows("live_count", "the swarm's Count: how many of its copies are drawn"),
];

const EMITTERS: &[Choice] = &[Choice { key: "box", label: "Box volume" }, Choice { key: "sphere", label: "Sphere" }];

const SHAPES: &[Choice] = &[
    Choice { key: "tetrahedron", label: "Tetrahedron" },
    Choice { key: "cube", label: "Cube" },
    Choice { key: "octahedron", label: "Octahedron" },
    Choice { key: "icosahedron", label: "Icosahedron" },
    Choice { key: "sphere", label: "Sphere" },
];

pub static SWARM_FIELDS: [FieldRow; 3] = [
    FieldRow {
        id: "emit_from",
        label: "Emit From",
        section: ControlGroup::Source,
        control: FieldControl::ClosedOrItem { closed: EMITTERS, items: is_object_or_group },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Swarm(s) => match &s.emit_from {
                EmitFrom::Box => FieldValue::Choice("box".to_owned()),
                EmitFrom::Sphere => FieldValue::Choice("sphere".to_owned()),
                EmitFrom::Item(id) => FieldValue::Item(Some(id.clone())),
            },
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let ItemFields::Swarm(s) = fields else {
                return Err(wrong_value("emit_from", value));
            };
            s.emit_from = match value {
                FieldValue::Choice(key) if key == "box" => EmitFrom::Box,
                FieldValue::Choice(key) if key == "sphere" => EmitFrom::Sphere,
                FieldValue::Item(Some(id)) => EmitFrom::Item(id.clone()),
                other => return Err(wrong_value("emit_from", other)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "draw_as",
        label: "Draw As",
        section: ControlGroup::Source,
        control: FieldControl::ClosedOrItem { closed: SHAPES, items: is_object_or_group },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Swarm(s) => match &s.draw_as {
                DrawAs::Tetrahedron => FieldValue::Choice("tetrahedron".to_owned()),
                DrawAs::Cube => FieldValue::Choice("cube".to_owned()),
                DrawAs::Octahedron => FieldValue::Choice("octahedron".to_owned()),
                DrawAs::Icosahedron => FieldValue::Choice("icosahedron".to_owned()),
                DrawAs::Sphere => FieldValue::Choice("sphere".to_owned()),
                DrawAs::Item(id) => FieldValue::Item(Some(id.clone())),
            },
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let ItemFields::Swarm(s) = fields else {
                return Err(wrong_value("draw_as", value));
            };
            s.draw_as = match value {
                FieldValue::Choice(key) => match key.as_str() {
                    "tetrahedron" => DrawAs::Tetrahedron,
                    "cube" => DrawAs::Cube,
                    "octahedron" => DrawAs::Octahedron,
                    "icosahedron" => DrawAs::Icosahedron,
                    "sphere" => DrawAs::Sphere,
                    _ => return Err(wrong_value("draw_as", value)),
                },
                FieldValue::Item(Some(id)) => DrawAs::Item(id.clone()),
                other => return Err(wrong_value("draw_as", other)),
            };
            Ok(())
        },
    },
    MATERIAL_BINDING,
];

/// Why `swarm` cannot take `pick` as its field `field`'s item, or `None`.
/// The picker draws the item disabled with the reason, and a pick of it is
/// refused, so a swarm never sits on an item it cannot use.
pub(super) fn pick_unavailable(doc: &SceneDoc, swarm: &Item, field: &str, pick: &Item) -> Option<&'static str> {
    let ocean = matches!(&pick.fields, ItemFields::Object(object) if object.shape == Shape::Ocean);
    match field {
        "emit_from" if ocean => Some("An Ocean is endless: it has no surface to emit from"),
        "draw_as" if ocean => Some("An Ocean is endless: it cannot be drawn at every particle"),
        "draw_as" => draw_as_refused(doc, swarm, pick),
        _ => None,
    }
}

/// Any number of swarms draw one item, each with its own particles. An item
/// inside a Group whose Copies (or a Swarm) draw it is placed by that group,
/// which leaves nothing for this swarm to place.
fn draw_as_refused(doc: &SceneDoc, swarm: &Item, pick: &Item) -> Option<&'static str> {
    let drawn_by_other = |id: &ItemId| {
        let mut drawn = false;
        doc.walk(|other, _| {
            if other.id != swarm.id
                && let ItemFields::Swarm(fields) = &other.fields
                && fields.draw_as == DrawAs::Item(id.clone())
            {
                drawn = true;
            }
        });
        drawn
    };
    let mut copied_above = false;
    doc.walk(|other, ancestors| {
        if other.id == pick.id {
            copied_above = ancestors
                .iter()
                .any(|holder| matches!(&holder.fields, ItemFields::Group(group) if group.copies) || drawn_by_other(&holder.id));
        }
    });
    copied_above.then_some("Inside a group whose copies draw it already")
}

#[cfg(test)]
mod tests {
    use super::super::{field_for, set_field};
    use super::*;

    /// A pick a swarm cannot use is refused with its reason, never held silently.
    #[test]
    fn a_swarm_refuses_picks_it_cannot_use() {
        let mut doc = SceneDoc::base();
        let swarm = |id: &str, draw: Option<&str>| {
            let mut fields = ItemFields::default_for(crate::scene_doc::ItemKind::Swarm);
            if let (ItemFields::Swarm(s), Some(draw)) = (&mut fields, draw) {
                s.draw_as = DrawAs::Item(ItemId::new(draw));
            }
            Item::new(ItemId::new(id), id, fields)
        };
        let mut sea = ItemFields::default_for(crate::scene_doc::ItemKind::Object);
        if let ItemFields::Object(object) = &mut sea {
            object.shape = Shape::Ocean;
        }
        let mut copied = ItemFields::default_for(crate::scene_doc::ItemKind::Group);
        if let ItemFields::Group(group) = &mut copied {
            group.copies = true;
        }
        let end = doc.items.len();
        doc.insert(None, end, Item::new(ItemId::new("sea"), "sea", sea)).unwrap();
        doc.insert(None, end + 1, Item::new(ItemId::new("rack"), "rack", copied)).unwrap();
        doc.insert(Some(&ItemId::new("rack")), 0, Item::new(ItemId::new("ball"), "ball", ItemFields::default_for(crate::scene_doc::ItemKind::Object))).unwrap();
        doc.insert(None, end + 2, swarm("first", Some("rack"))).unwrap();
        doc.insert(None, end + 3, swarm("second", None)).unwrap();
        let second = doc.find(&ItemId::new("second")).unwrap().clone();
        let pick = |field: &str, id: &str| {
            let row = field_for(&second.fields, field).unwrap();
            set_field(&doc, &second, row, &FieldValue::Item(Some(ItemId::new(id)))).err()
        };
        assert_eq!(pick("draw_as", "rack"), None, "two swarms draw one item");
        assert_eq!(pick("draw_as", "ball").as_deref(), Some("Inside a group whose copies draw it already"));
        assert!(pick("draw_as", "sea").is_some_and(|reason| reason.starts_with("An Ocean")));
        assert!(pick("emit_from", "sea").is_some_and(|reason| reason.starts_with("An Ocean")));
        assert_eq!(pick("emit_from", "rack"), None, "emitting from a drawn group is fine");
    }
}
