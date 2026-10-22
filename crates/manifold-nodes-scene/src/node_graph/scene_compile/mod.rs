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
//! Kinds whose emitters arrive in later lanes (Water, Water Source, Swarm)
//! and the Ocean and Mesh shapes compile to nothing yet. Forces and mesh
//! modifiers compile to scene modifier instances (`forces.rs`); Copies emit a
//! copies body per item (`copies.rs`). Every
//! physics setting compiles as None (lane L4 owns the physics emit).

mod builder;
mod camera;
mod copies;
mod forces;
mod group;
mod light;
mod material;
mod object;
mod world;

pub(crate) use builder::Builder;
pub(crate) use light::lamp_node;
pub(crate) use object::object_wires;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::SceneDoc;

/// Compile a scene document. Deterministic: the same document always gives
/// the same graph.
pub fn compile(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    let mut b = Builder::new(doc);
    material::emit_all(&mut b);
    camera::emit_all(&mut b);
    light::emit_all(&mut b);
    group::emit_tree(&mut b);
    world::emit(&mut b);
    copies::join_world(&mut b);
    forces::emit_all(&mut b)?;
    b.finish()
}

inventory::submit! {
    manifold_core::scene_doc::SceneCompiler { compile }
}
