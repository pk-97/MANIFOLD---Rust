//! Smoke and Fire (docs/SMOKE_AND_FIRE_MAP.md): each is a transform that is the
//! box, a value node that is the cell, one `node.gas_volume` that simulates
//! the gas in the box, and one `node.scene_object` that draws the solver's
//! fields as a volume. The cell follows the World's Cell Size times the
//! item's Quality (`water::follow_cell`), and the solver follows the World's
//! gravity, speed and reset through value nodes. Scene forces and hits reach
//! the solver at load (`load/expand/acceleration.rs` follows the object's
//! `gas_state`); a gas pushes nothing back.
//!
//! Inside a Group, the box sits in the group's frame and the gas follows the
//! group's visibility. The box stays axis-aligned: a turned group moves it
//! but does not turn it.

use manifold_core::fluid_domain::DEFAULT_CELL_SIZE;
use manifold_core::scene_doc::{Item, ItemFields, ItemKind, SceneDoc};
use manifold_core::scene_schema::WORLD_FOLLOWS;
use manifold_core::scene_world_controls::{WORLD_INPUTS, WorldControlInput};
use manifold_foundation::ItemId;

use super::builder::Builder;
use super::water::{MatterFrames, follow_cell, items_of};

/// `node.gas_volume`'s `kind`.
const SMOKE: f32 = 0.0;
const FIRE: f32 = 1.0;

/// The World control a gas leaves alone: still air does not move it.
const AIR_RESISTANCE: &str = "air_resistance";

/// The solver's scalar outputs and the scene object's inputs they feed: the
/// box in the world, and the fire's light. The two fields go beside them.
const SCALARS: [(&str, &str); 12] = [
    ("cell_size", "gas_cell_size"),
    ("origin_x", "gas_origin_x"),
    ("origin_y", "gas_origin_y"),
    ("origin_z", "gas_origin_z"),
    ("dims_x", "gas_dims_x"),
    ("dims_y", "gas_dims_y"),
    ("dims_z", "gas_dims_z"),
    ("light_power", "gas_light_power"),
    ("light_x", "gas_light_x"),
    ("light_y", "gas_light_y"),
    ("light_z", "gas_light_z"),
    ("light_temperature", "gas_light_temperature"),
];

/// Every Smoke and Fire in tree order, with the Group it sits in, if any.
pub(crate) fn gases(doc: &SceneDoc) -> Vec<(Item, Option<ItemId>)> {
    items_of(doc, ItemKind::is_gas)
}

/// The World controls a gas takes, each with the value node that follows it:
/// gravity, speed and reset.
fn world_controls() -> impl Iterator<Item = (WorldControlInput, &'static str)> {
    WORLD_INPUTS
        .into_iter()
        .zip(WORLD_FOLLOWS)
        .filter(|(control, _)| control.world != AIR_RESISTANCE)
}

/// Emit every Smoke and Fire, in tree order. Each flows round the first
/// Water and the first Sand: their particles a frame back are solids to it.
pub(crate) fn emit_all(b: &mut Builder<'_>, matter: MatterFrames) {
    for (item, group) in gases(b.doc) {
        emit(b, &item, group.as_ref(), matter);
    }
}

fn emit(b: &mut Builder<'_>, item: &Item, group: Option<&ItemId>, matter: MatterFrames) {
    let (kind, quality) = match &item.fields {
        ItemFields::Smoke(gas) => (SMOKE, gas.quality),
        ItemFields::Fire(gas) => (FIRE, gas.quality),
        _ => return,
    };
    let owner = item.id.as_str();
    let frame = group.and_then(|group| b.group_frames.get(group).copied());
    let visible = group.and_then(|group| b.group_visible.get(group).copied());

    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(frame) = frame {
        b.wire(frame, "transform", transform, "parent");
    }
    // The World's Cell Size reaches it in `follow_world`; a scene with no
    // World runs at the default cell.
    let cell_size = b.add(owner, "cell_size", "node.value");
    b.set_number(cell_size, "value", DEFAULT_CELL_SIZE);

    let gas = b.add(owner, "gas", "node.gas_volume");
    b.set_number(gas, "kind", kind);
    b.set_number(gas, "quality", quality.index() as f32);
    b.wire(transform, "transform", gas, "domain");
    b.wire(cell_size, "out", gas, "cell_metres");
    for (control, role) in world_controls() {
        let value = b.add(owner, role, "node.value");
        b.wire(value, "out", gas, control.input);
    }
    for (frame, points, count) in [
        (matter.water, "water", "water_count"),
        (matter.sand, "sand", "sand_count"),
    ] {
        if let Some(frame) = frame {
            b.wire(frame, "particles_b", gas, points);
            b.wire(frame, "count_b", gas, count);
        }
    }

    let object = b.add(owner, "object", "node.scene_object");
    b.wire(gas, "state", object, "gas_state");
    b.wire(gas, "velocity", object, "gas_velocity");
    for (output, input) in SCALARS {
        b.wire(gas, output, object, input);
    }
    b.set_number(object, "gas_quality", quality.index() as f32);
    if let Some(visible) = visible {
        b.wire(visible, "out", object, "parent_visible");
    }
    b.objects.push(object);
    b.object_items.push(item.id.clone());
    b.rows(item);
}

/// Make every Smoke and Fire follow the World: its Cell Size, and its
/// gravity, speed and reset through the value nodes.
pub(crate) fn follow_world(b: &mut Builder<'_>, world: Option<u32>) {
    for (item, _) in gases(b.doc) {
        let owner = item.id.as_str();
        let (ItemFields::Smoke(gas) | ItemFields::Fire(gas)) = &item.fields else {
            continue;
        };
        follow_cell(b, owner, gas.quality);
        if let Some(world) = world {
            for (control, role) in world_controls() {
                if let Some(value) = b.id(owner, role) {
                    b.follow(world, control.world, value, "value");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};
    use manifold_core::scene_doc::{
        GroupFields, Item, ItemFields, ItemId, ItemKind, Quality, SceneDoc,
    };

    use super::super::builder::number;
    use super::super::{compile, drawn_items};
    use super::DEFAULT_CELL_SIZE;

    fn node<'a>(def: &'a EffectGraphDef, node_id: &str) -> &'a EffectGraphNode {
        def.nodes
            .iter()
            .find(|node| node.node_id.as_str() == node_id)
            .unwrap_or_else(|| panic!("no node `{node_id}`"))
    }

    /// Whether `from`'s `out_port` feeds `to`'s `in_port`.
    fn wired(
        def: &EffectGraphDef,
        (from, out_port): (&EffectGraphNode, &str),
        (to, in_port): (&EffectGraphNode, &str),
    ) -> bool {
        def.wires.iter().any(|wire| {
            wire.from_node == from.id
                && wire.from_port == out_port
                && wire.to_node == to.id
                && wire.to_port == in_port
        })
    }

    /// The base scene with a Smoke or Fire called `gas`, at `quality`, inside
    /// a Group called `group` when `grouped`.
    fn scene(kind: ItemKind, quality: Quality, grouped: bool) -> SceneDoc {
        let mut doc = SceneDoc::base();
        let mut fields = ItemFields::default_for(kind);
        if let ItemFields::Smoke(gas) | ItemFields::Fire(gas) = &mut fields {
            gas.quality = quality;
        }
        let end = doc.items.len();
        let parent = grouped.then(|| ItemId::new("group"));
        if grouped {
            let group = Item::new(
                ItemId::new("group"),
                "Group",
                ItemFields::Group(GroupFields::default()),
            );
            doc.insert(None, end, group).unwrap();
        }
        doc.insert(
            parent.as_ref(),
            0,
            Item::new(ItemId::new("gas"), "Gas", fields),
        )
        .unwrap();
        doc
    }

    /// A Fire and a Smoke each compile to one gas node of their kind and a
    /// scene object that draws it, in a Group or not; the cell is the World's
    /// times the item's Quality, the solver follows gravity, speed and reset
    /// but not still air, and a Group's frame and visibility reach the box
    /// and the object.
    #[test]
    fn each_gas_compiles_to_a_solver_and_the_object_that_draws_it() {
        for (kind, kind_value, quality, grouped) in [
            (ItemKind::Fire, 1.0, Quality::High, false),
            (ItemKind::Smoke, 0.0, Quality::Low, true),
        ] {
            let doc = scene(kind, quality, grouped);
            let def = compile(&doc).unwrap();
            let gases: Vec<_> = def
                .nodes
                .iter()
                .filter(|node| node.type_id == "node.gas_volume")
                .collect();
            assert_eq!(gases.len(), 1, "one solver for one {kind:?}");
            let gas = gases[0];
            assert_eq!(gas.node_id.as_str(), "gas:gas");
            assert_eq!(number(&gas.params["kind"]), kind_value);
            assert_eq!(number(&gas.params["quality"]), quality.index() as f32);

            let object = node(&def, "gas:object");
            assert_eq!(object.type_id, "node.scene_object");
            assert_eq!(
                number(&object.params["gas_quality"]),
                quality.index() as f32
            );
            for (output, input) in [
                ("state", "gas_state"),
                ("velocity", "gas_velocity"),
                ("cell_size", "gas_cell_size"),
                ("origin_y", "gas_origin_y"),
                ("dims_z", "gas_dims_z"),
                ("light_power", "gas_light_power"),
                ("light_temperature", "gas_light_temperature"),
            ] {
                assert!(wired(&def, (gas, output), (object, input)), "{output}");
            }
            let render = node(&def, "scene:render");
            assert!(
                def.wires.iter().any(|wire| wire.from_node == object.id
                    && wire.from_port == "object"
                    && wire.to_node == render.id
                    && wire.to_port.starts_with("object_")),
                "the render draws the gas object"
            );
            assert!(
                drawn_items(&doc).contains(&ItemId::new("gas")),
                "the gas holds a render slot"
            );

            // One grid per World: the cell is the World's, scaled by Quality.
            let cell = node(&def, "gas:cell_size");
            assert!(wired(&def, (cell, "out"), (gas, "cell_metres")));
            assert_eq!(
                number(&cell.params["value"]),
                DEFAULT_CELL_SIZE * quality.cell_scale()
            );

            // The World's gravity, speed and reset, and not still air.
            assert!(wired(
                &def,
                (node(&def, "gas:world_gravity_y"), "out"),
                (gas, "gravity")
            ));
            assert!(wired(
                &def,
                (node(&def, "gas:world_speed"), "out"),
                (gas, "speed")
            ));
            assert!(wired(
                &def,
                (node(&def, "gas:world_reset"), "out"),
                (gas, "reset")
            ));
            assert!(
                def.wires
                    .iter()
                    .all(|wire| wire.to_node != gas.id || wire.to_port != "air_resistance"),
                "still air does not move a gas"
            );
            assert!(
                def.nodes
                    .iter()
                    .all(|node| node.node_id.as_str() != "gas:world_air_resistance"),
                "no value node follows still air"
            );

            // A Group's frame places the box and its visibility hides the gas.
            let transform = node(&def, "gas:transform");
            assert!(wired(&def, (transform, "transform"), (gas, "domain")));
            let framed = def
                .wires
                .iter()
                .any(|wire| wire.to_node == transform.id && wire.to_port == "parent");
            let hidden = def
                .wires
                .iter()
                .any(|wire| wire.to_node == object.id && wire.to_port == "parent_visible");
            assert_eq!(framed, grouped, "the group's frame places the box");
            assert_eq!(hidden, grouped, "the group's visibility hides the gas");
        }
    }
}
