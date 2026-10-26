//! World: the physics world once an item has physics, with every body joined
//! to it, the fog pass when fog is on, the render mode, ambient occlusion
//! when it is on, and the render settings on the scene's render node. A
//! Swarm follows the world's gravity, still air and clock, so a scene with
//! one has the world too, and so does a Water, which also follows the Render
//! Mode for Physics View.
//!
//! Ambient occlusion sits behind a picture switch that follows the Strength
//! row, so a Strength of zero prunes the whole occlusion chain.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{ItemFields, WorldFields};

use super::builder::Builder;

pub(crate) fn emit(b: &mut Builder<'_>) {
    let Some(item) = b.doc.world().cloned() else {
        return;
    };
    let ItemFields::World(WorldFields {
        fog,
        ambient_occlusion,
        ..
    }) = &item.fields
    else {
        return;
    };
    let owner = item.id.as_str();
    let mut world = None;
    if b.doc.any_physics()
        || b.doc.any_copies()
        || b.doc.any_swarm()
        || !super::water::waters(b.doc).is_empty()
    {
        let node = b.add(owner, "physics", "node.physics_world");
        super::physics::join_world(b, node);
        world = Some(node);
    }
    let render = b.scene("render");
    if *fog {
        let atmosphere = b.add(owner, "fog", "node.atmosphere");
        b.wire(atmosphere, "atmosphere", render, "atmosphere");
    }
    let mode = b.add(owner, "render_mode", "node.render_mode");
    b.wire(mode, "render_mode", render, "render_mode_in");
    if *ambient_occlusion {
        emit_ambient_occlusion(b, owner);
    }
    b.rows(&item);
    follow_strength(b, owner);
    if let Some(world) = world {
        super::swarm::follow_world(b, world);
    }
    super::water::follow_world(b, world, mode);
}

/// Contact shadows on the active camera's picture, before depth of field and
/// motion blur: `node.ssao_gtao` from the render's depth with its noise
/// moving every frame, averaged over past frames by
/// `node.temporal_accumulate` along the render's motion, then a horizontal
/// and a vertical depth-aware blur (temporal before spatial, as Unreal
/// orders its GTAO filters), multiplied onto the colour, and kept only where
/// the render's `ao_mask` takes it (lit raster surfaces; nothing under ray
/// tracing, which brings its own). A picture switch (`ao_gate`) picks the
/// plain render colour or that result; the executor runs only the picked
/// input's chain, so the plain pick costs no occlusion passes. Needs a
/// camera: with none there is no lens to read depth by.
fn emit_ambient_occlusion(b: &mut Builder<'_>, owner: &str) {
    let Some(lens) = b.active_lens else {
        return;
    };
    let render = b.scene("render");
    let ao = b.add(owner, "ao", "node.ssao_gtao");
    let history = b.add(owner, "ao_history", "node.temporal_accumulate");
    let blur_h = b.add(owner, "ao_blur_h", "node.bilateral_blur");
    let blur_v = b.add(owner, "ao_blur_v", "node.bilateral_blur");
    let multiply = b.add(owner, "ao_multiply", "node.mix");
    let mask = b.add(owner, "ao_mask", "node.masked_mix");
    let gate = b.add(owner, "ao_gate", "node.switch_texture");
    b.set(
        ao,
        "noise_per_frame",
        SerializedParamValue::Bool { value: true },
    );
    b.set(blur_h, "axis", SerializedParamValue::Enum { value: 0 });
    b.set(blur_v, "axis", SerializedParamValue::Enum { value: 1 });
    b.set(
        multiply,
        "amount",
        SerializedParamValue::Float { value: 1.0 },
    );
    // `node.mix`'s Multiply.
    b.set(multiply, "mode", SerializedParamValue::Enum { value: 4 });
    // Input 0 is the plain colour, input 1 the occluded one.
    b.set(gate, "num_inputs", SerializedParamValue::Int { value: 2 });
    b.splice(render, "color", gate, "out");
    b.wire(render, "color", gate, "in_0");
    b.wire(mask, "out", gate, "in_1");
    b.wire(render, "depth", ao, "depth");
    b.wire(lens, "out", ao, "camera");
    b.wire(ao, "out", history, "in");
    b.wire(render, "depth", history, "depth");
    b.wire(render, "velocity", history, "velocity");
    b.wire(lens, "out", history, "camera");
    b.wire(history, "out", blur_h, "in");
    b.wire(blur_h, "out", blur_v, "in");
    for blur in [blur_h, blur_v] {
        b.wire(render, "depth", blur, "depth");
        b.wire(lens, "out", blur, "camera");
    }
    b.wire(render, "color", multiply, "a");
    b.wire(blur_v, "out", multiply, "b");
    b.wire(render, "color", mask, "a");
    b.wire(multiply, "out", mask, "b");
    b.wire(render, "ao_mask", mask, "mask");
}

/// The largest number below one half. The switch rounds its selector, so
/// the Strength plus this rounds to input 0 at exactly zero and to input 1
/// for every Strength above it.
const JUST_UNDER_HALF: f32 = f32::from_bits(0.5f32.to_bits() - 1);

/// Make the picture switch follow the Strength row, once the rows exist.
/// Zero Strength draws the plain colour. A Strength too small to change a
/// pixel (the occlusion map is half-float, so below 2^-12 it stores as
/// plain white) draws the plain colour too, which is the same picture the
/// chain would give.
fn follow_strength(b: &mut Builder<'_>, owner: &str) {
    if let (Some(ao), Some(gate)) = (b.id(owner, "ao"), b.id(owner, "ao_gate")) {
        b.follow_offset(ao, "intensity", gate, "selector", JUST_UNDER_HALF);
    }
}

#[cfg(test)]
mod tests {
    use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
    use manifold_core::effects::apply_card_reshape;
    use manifold_core::macro_bank::MacroCurve;
    use manifold_core::scene_doc::{ItemFields, SceneDoc};
    use manifold_node_engine::exec::effect_node::ParamValues;
    use manifold_node_engine::parameters::ParamValue;
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use std::borrow::Cow;

    use super::JUST_UNDER_HALF;
    use crate::node_graph::scene_compile::compile;

    fn node(def: &EffectGraphDef, node_id: &str) -> u32 {
        def.nodes
            .iter()
            .find(|node| node.node_id.as_str() == node_id)
            .unwrap_or_else(|| panic!("no node `{node_id}`"))
            .id
    }

    /// Every (node, port) that reads `from`'s `port`, sorted.
    fn readers(def: &EffectGraphDef, from: u32, port: &str) -> Vec<(u32, String)> {
        let mut readers: Vec<(u32, String)> = def
            .wires
            .iter()
            .filter(|wire| wire.from_node == from && wire.from_port == port)
            .map(|wire| (wire.to_node, wire.to_port.clone()))
            .collect();
        readers.sort();
        readers
    }

    #[test]
    fn world_occlusion_sits_behind_a_switch_that_picks_plain_or_occluded() {
        let def = compile(&SceneDoc::base()).unwrap();
        let render = node(&def, "scene:render");
        let gate = node(&def, "world:ao_gate");
        let multiply = node(&def, "world:ao_multiply");
        let mask = node(&def, "world:ao_mask");

        let gate_node = def.nodes.iter().find(|n| n.id == gate).unwrap();
        assert_eq!(gate_node.type_id, "node.switch_texture");
        assert_eq!(
            gate_node.params.get("num_inputs"),
            Some(&SerializedParamValue::Int { value: 2 })
        );

        // The plain colour feeds the switch's input 0 and the occlusion
        // blend; nothing else reads it, so the occluded picture is only
        // reachable through the switch.
        let mut expected = vec![
            (gate, "in_0".to_owned()),
            (multiply, "a".to_owned()),
            (mask, "a".to_owned()),
        ];
        expected.sort();
        assert_eq!(readers(&def, render, "color"), expected);
        assert_eq!(readers(&def, mask, "out"), vec![(gate, "in_1".to_owned())]);

        // What read the render's colour before (the camera's depth of field)
        // now reads the switch.
        let after = readers(&def, gate, "out");
        assert_eq!(after.len(), 1, "one reader of the picked picture");
        assert_eq!(after[0].1, "color");
    }

    #[test]
    fn world_without_occlusion_has_no_switch() {
        let mut doc = SceneDoc::base();
        for item in &mut doc.items {
            if let ItemFields::World(world) = &mut item.fields {
                world.ambient_occlusion = false;
            }
        }
        let def = compile(&doc).unwrap();
        assert!(
            def.nodes
                .iter()
                .all(|node| !node.node_id.as_str().starts_with("world:ao")),
            "no occlusion node without the field"
        );
        let render = node(&def, "scene:render");
        assert!(
            readers(&def, render, "color")
                .iter()
                .all(|(_, port)| port == "color"),
            "the camera's tail reads the render colour directly"
        );
    }

    /// The switch runs the picked input's chain only. Strength reaches it as
    /// `strength + JUST_UNDER_HALF`: zero draws the plain colour, anything
    /// that can change a pixel draws the occluded one.
    #[test]
    fn zero_strength_picks_the_plain_picture_and_any_visible_strength_the_occluded_one() {
        let registry = PrimitiveRegistry::with_builtin();
        let mut gate = registry.construct("node.switch_texture").unwrap();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("num_inputs"), ParamValue::Float(2.0));
        gate.reconfigure(&params);
        let mut picked = |strength: f32| {
            let selector = apply_card_reshape(
                strength,
                0.0,
                1.0,
                false,
                MacroCurve::Linear,
                1.0,
                JUST_UNDER_HALF,
            );
            params.insert(Cow::Borrowed("selector"), ParamValue::Float(selector));
            gate.selected_input_branch(&params, &[])
        };

        assert_eq!(picked(0.0), Some("in_0"));
        assert_eq!(picked(-0.0), Some("in_0"));
        // Half-float stores 1 - x as exactly 1 for x <= 2^-12, so any
        // Strength under that leaves the occlusion map white and the
        // picture plain either way; from there up the switch must pick
        // the occluded input.
        let white_below = 2.0_f32.powi(-12);
        assert_eq!(
            half::f16::from_f32(1.0 - white_below),
            half::f16::ONE,
            "the occlusion map rounds to white at that Strength"
        );
        for strength in [white_below, 1e-3, 0.02, 0.5, 1.0, 1.5, 40.0] {
            assert_eq!(picked(strength), Some("in_1"), "Strength {strength}");
        }
    }
}
