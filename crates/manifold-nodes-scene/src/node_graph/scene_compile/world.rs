//! World: the physics world once an item has physics, with every body joined
//! to it, the fog pass when fog is on, and the render settings on the scene's
//! render node.

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
    if b.doc.any_physics() {
        let world = b.add(owner, "physics", "node.physics_world");
        super::physics::join_world(b, world);
    }
    if *fog {
        let atmosphere = b.add(owner, "fog", "node.atmosphere");
        let render = b.scene("render");
        b.wire(atmosphere, "atmosphere", render, "atmosphere");
    }
    b.rows(&item);
}
