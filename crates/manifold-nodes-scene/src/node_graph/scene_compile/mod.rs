//! The scene compiler (SCENE_DOCUMENT_DESIGN.md section 7.3 (The compiler)):
//! a pure function from a scene document to today's `EffectGraphDef`, with
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
//! An Ocean shape is the starter sea's chain (`ocean.rs`). Water and its sources
//! splice the water family template (`water.rs`). Forces (`forces.rs`) and
//! Modifier cards (`modifiers.rs`, in chain order) compile to scene modifier
//! instances; Copies emit a copies body per item (`copies.rs`); physics
//! emits a body per item (`physics.rs`).

mod builder;
mod camera;
mod copies;
mod curves;
mod forces;
mod group;
mod light;
mod material;
mod mesh;
mod model_material;
mod modifiers;
mod object;
mod ocean;
mod physics;
mod swarm;
mod water;
mod world;

pub(crate) use builder::Builder;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::SceneDoc;

/// Compile a scene document. Deterministic: the same document always gives
/// the same graph.
pub fn compile(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    let mut b = Builder::new(doc);
    material::emit_all(&mut b);
    camera::emit_all(&mut b);
    light::emit_all(&mut b);
    swarm::emit_all(&mut b);
    group::emit_tree(&mut b);
    camera::aim_all(&mut b);
    swarm::finish(&mut b);
    water::emit_all(&mut b);
    world::emit(&mut b);
    camera::finish_cut(&mut b);
    copies::join_world(&mut b);
    forces::emit_all(&mut b)?;
    modifiers::emit_all(&mut b)?;
    b.finish()
}

/// The Objects `compile` draws, in render-slot order: slot k is the k-th item.
pub fn drawn_items(doc: &SceneDoc) -> Vec<manifold_foundation::ItemId> {
    let mut b = Builder::new(doc);
    material::emit_all(&mut b);
    swarm::emit_all(&mut b);
    group::emit_tree(&mut b);
    water::emit_all(&mut b);
    b.object_items
}

inventory::submit! {
    manifold_core::scene_doc::SceneCompiler { compile }
}
