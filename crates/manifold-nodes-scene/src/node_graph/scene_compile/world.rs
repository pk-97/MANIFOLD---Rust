//! World: the physics world once an item has physics, with every body joined
//! to it, the fog pass when fog is on, the render mode, ambient occlusion
//! when it is on, and the render settings on the scene's render node. A
//! Swarm follows the world's gravity, still air and clock, so a scene with
//! one has the world too, and so does a Water, which also follows the Render
//! Mode for Physics View.

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
/// tracing, which brings its own). Needs a camera: with none there is no
/// lens to read depth by.
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
    b.splice(render, "color", mask, "out");
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
