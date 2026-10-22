//! World: the physics world once an item has physics (bodies join it in lane
//! L4), the fog pass when fog is on, and the render settings on the scene's
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
        b.add(owner, "physics", "node.physics_world");
    }
    if *fog {
        let atmosphere = b.add(owner, "fog", "node.atmosphere");
        let render = b.scene("render");
        b.wire(atmosphere, "atmosphere", render, "atmosphere");
    }
    b.rows(&item);
}
