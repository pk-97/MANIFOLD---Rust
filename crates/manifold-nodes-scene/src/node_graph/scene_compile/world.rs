//! World: the physics world once an item has physics, with every body joined
//! to it, the fog pass when fog is on, and the render settings on the scene's
//! render node. A Swarm follows the world's gravity, still air and clock, so
//! a scene with one has the world too, and so does a Water, which also
//! follows the Render Mode for Physics View.

use manifold_core::scene_doc::{ItemFields, WorldFields};

use super::builder::Builder;

pub(crate) fn emit(b: &mut Builder<'_>) {
    let Some(item) = b.doc.world().cloned() else {
        return;
    };
    let ItemFields::World(WorldFields { fog, .. }) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let mut world = None;
    if b.doc.any_physics() || b.doc.any_copies() || b.doc.any_swarm() || !super::water::waters(b.doc).is_empty() {
        let node = b.add(owner, "physics", "node.physics_world");
        super::physics::join_world(b, node);
        world = Some(node);
    }
    if *fog {
        let atmosphere = b.add(owner, "fog", "node.atmosphere");
        let render = b.scene("render");
        b.wire(atmosphere, "atmosphere", render, "atmosphere");
    }
    b.rows(&item);
    if let Some(world) = world {
        super::swarm::follow_world(b, world);
    }
    super::water::follow_world(b, world);
}
