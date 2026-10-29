//! The scene compiler (SCENE_DOCUMENT_DESIGN.md section 7.3 (The compiler)):
//! a pure function from a scene document to an `EffectGraphDef`, with
//! the layer's manifest in its `preset_metadata`. Shaped like
//! `manifold_node_engine::load::physics_members::compile`: nothing below the
//! loader changes.
//!
//! One emitter per kind, one module each. Node ids come from (item id, role)
//! (`<item>:<role>`), so a recompile after an unrelated edit keeps every
//! unchanged node's id and the runtime keeps what it carries by node id.
//! Every card row becomes a manifest param `<item id>.<property>` bound to
//! its node param; the (kind, property) table in `manifold_core::scene_schema`
//! decides which rows exist.
//!
//! An Ocean shape is the sea chain (`ocean.rs`). Water and its sources
//! splice the water family template (`water.rs`); Smoke and Fire emit a gas
//! solver and the object that draws it (`gas.rs`). Forces (`forces.rs`) and
//! Modifier cards (`modifiers.rs`, in chain order) compile to scene modifier
//! instances; Copies emit a copies body per item (`copies.rs`); physics
//! emits a body per item (`physics.rs`).

mod builder;
mod camera;
mod copies;
mod curves;
mod deformers;
mod forces;
mod gas;
mod group;
mod light;
mod material;
mod mesh;
mod model_material;
mod modifiers;
mod object;
mod ocean;
mod physics;
mod scene_loop;
mod scene_mirror;
mod swarm;
mod water;
mod world;

pub(crate) use builder::Builder;
pub use material::fed_map_ports;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{Item, ItemKind, SceneDoc};

/// Compile a scene document. Deterministic: the same document always gives
/// the same graph, and so does the same document with items moved among
/// their siblings ([`in_compile_order`]).
pub fn compile(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    let doc = in_compile_order(doc);
    let mut b = Builder::new(&doc);
    material::emit_all(&mut b);
    camera::emit_all(&mut b);
    light::emit_all(&mut b);
    swarm::emit_all(&mut b);
    group::emit_tree(&mut b);
    camera::aim_all(&mut b);
    swarm::finish(&mut b);
    water::emit_all(&mut b);
    gas::emit_all(&mut b);
    world::emit(&mut b);
    camera::finish_cut(&mut b);
    copies::join_world(&mut b);
    swarm::draw_alike(&mut b);
    forces::emit_all(&mut b)?;
    scene_loop::emit(&mut b)?;
    modifiers::emit_all(&mut b)?;
    scene_loop::emit_copies(&mut b)?;
    b.finish()
}

/// The Objects `compile` draws, in render-slot order: slot k is the k-th item.
pub fn drawn_items(doc: &SceneDoc) -> Vec<manifold_foundation::ItemId> {
    let doc = in_compile_order(doc);
    let mut b = Builder::new(&doc);
    material::emit_all(&mut b);
    swarm::emit_all(&mut b);
    group::emit_tree(&mut b);
    water::emit_all(&mut b);
    gas::emit_all(&mut b);
    swarm::draw_alike(&mut b);
    b.object_items
}

/// The document as the compiler reads it: siblings in id order, so where an
/// item sits among its siblings is display only and a reorder recompiles
/// nothing. Modifier cards keep their chain order, the order they apply in
/// (ruling 20).
fn in_compile_order(doc: &SceneDoc) -> SceneDoc {
    fn sorted(items: &[Item]) -> Vec<Item> {
        let mut items: Vec<Item> = items
            .iter()
            .map(|item| Item {
                id: item.id.clone(),
                name: item.name.clone(),
                fields: item.fields.clone(),
                children: sorted(&item.children),
            })
            .collect();
        // Stable: every Modifier card shares one key, so the chain keeps its order.
        items.sort_by(|a, b| key(a).cmp(&key(b)));
        items
    }
    fn key(item: &Item) -> Option<&str> {
        (item.kind() != ItemKind::Modifier).then(|| item.id.as_str())
    }
    SceneDoc {
        items: sorted(&doc.items),
        models: doc.models.clone(),
        next_cut: doc.next_cut,
    }
}

inventory::submit! {
    manifold_core::scene_doc::SceneCompiler { compile }
}
