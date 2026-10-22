//! Every declared shape in the scene, with the params it names resolved to
//! their write address and authored value
//! (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One scene-object model)).
//! The gizmo core builds every target from this list alone; nothing here
//! knows a handle.

use manifold_core::NodeId;
use manifold_core::scene_doc::{Item, ItemKind, SceneDoc, param_id};
use manifold_foundation::ItemId;
use manifold_foundation::scene_surface::SceneShapeDecl;
use manifold_node_engine::scene::transform::{IDENTITY_MAT4, Mat4, Transform};

use super::ParamAddr;
use crate::node_graph::scene_surface::ITEM_SHAPES;

/// The roles a placed fluid source plays (`node.fluid_role_source`'s
/// `role` values 0 to 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FluidRole {
    InitialFill,
    Inflow,
    Outflow,
}

/// Who declares a shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShapeOwner {
    /// A scene item (an Object or a Group), by its id.
    Item(ItemId),
    /// A liquid domain node.
    LiquidDomain(NodeId),
    /// A fluid role source node.
    RoleSource { node: NodeId, role: FluidRole },
    /// A force scene modifier instance.
    Force(NodeId),
    /// A swarm's forces node, whose box its forces are laid over.
    Swarm(NodeId),
    /// A Physics World. It declares no shape; it owns the warning for
    /// copies its pool could not make.
    World(NodeId),
}

/// Where a shape param is written.
#[derive(Debug, Clone, PartialEq)]
pub enum ShapeWrite {
    /// A graph node param. The host writes it through its card when a card
    /// binds it, as that card's knob does.
    Node(ParamAddr),
    /// A host card param. `scale` and `offset` compose the host and modifier
    /// bindings; with the card's own range, curve and invert they map the
    /// card value to the shape value.
    Card { id: String, scale: f32, offset: f32 },
    /// The shape's `via` port has no node yet. The host creates it before
    /// the first write; the value is the node's identity until then.
    Unplaced,
}

/// One param a shape names.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeParamVm {
    pub id: String,
    pub write: ShapeWrite,
    /// The value the shape is placed at, as the knob shows it after its
    /// binding's reshape.
    pub value: f32,
    /// A wire feeds the param: its handle shows locked.
    pub wired: bool,
}

/// One declared shape on one owner.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneShapeVm {
    pub owner: ShapeOwner,
    /// Position in the owner's declaration list.
    pub index: usize,
    pub decl: SceneShapeDecl,
    /// The declared params in `decl.shape.param_ids()` order.
    pub params: Vec<ShapeParamVm>,
    /// The parent space the params are in, as a model matrix. `None` is
    /// world space.
    pub frame: Option<[[f32; 4]; 4]>,
    /// Where an `axis` with no origin sits: the centre of the one liquid
    /// domain it acts on, else the world origin.
    pub anchor: [f32; 3],
    /// The scene item this shape selects, if any.
    pub row: Option<ItemId>,
    pub pickable: bool,
}

/// What an item's transform reads when the manifest has no value yet.
fn transform_default(property: &str) -> f32 {
    if property.starts_with("scale_") { 1.0 } else { 0.0 }
}

/// An item's value for `property`: the live manifest value, else the default.
fn value_of(values: &dyn Fn(&str) -> Option<f32>, item: &ItemId, property: &str) -> f32 {
    values(&param_id(item, property)).unwrap_or_else(|| transform_default(property))
}

fn frame_of(values: &dyn Fn(&str) -> Option<f32>, chain: &[&Item]) -> Option<Mat4> {
    chain.iter().fold(None, |frame: Option<Mat4>, group| {
        let at = |axes: [&str; 3]| axes.map(|p| value_of(values, &group.id, p));
        let transform = Transform {
            pos: at(["pos_x", "pos_y", "pos_z"]),
            rot_euler: at(["rot_x", "rot_y", "rot_z"]),
            scale: at(["scale_x", "scale_y", "scale_z"]),
            parent: frame.unwrap_or(IDENTITY_MAT4),
            ..Transform::default()
        };
        Some(transform.world())
    })
}

/// Each Object's, Group's and Swarm's declared shapes over its transform rows, in the
/// frame of the groups it sits in.
pub(super) fn item_shapes(doc: &SceneDoc, values: &dyn Fn(&str) -> Option<f32>) -> Vec<SceneShapeVm> {
    let mut out = Vec::new();
    doc.walk(|item, ancestors| {
        if !matches!(item.kind(), ItemKind::Object | ItemKind::Group | ItemKind::Swarm) {
            return;
        }
        let frame = frame_of(values, ancestors);
        let visible = values(&param_id(&item.id, "visible")).is_none_or(|v| v > 0.5);
        for (index, decl) in ITEM_SHAPES.iter().enumerate() {
            let params = decl
                .shape
                .param_ids()
                .map(|property| {
                    let id = param_id(&item.id, property);
                    ShapeParamVm {
                        value: values(&id).unwrap_or_else(|| transform_default(property)),
                        write: ShapeWrite::Card { id, scale: 1.0, offset: 0.0 },
                        id: property.to_owned(),
                        wired: false,
                    }
                })
                .collect();
            out.push(SceneShapeVm {
                owner: ShapeOwner::Item(item.id.clone()),
                index,
                decl: decl.clone(),
                params,
                frame,
                anchor: [0.0; 3],
                row: Some(item.id.clone()),
                pickable: visible && ancestors.is_empty(),
            });
        }
    });
    out
}

/// One declared shape over `item`'s own card rows, in world space: each param
/// writes the item's card param and sits at its live value, else at its
/// row's default. Water and its sources are placed this way.
pub(super) fn card_shape(owner: ShapeOwner, item: &Item, decl: &SceneShapeDecl, values: &dyn Fn(&str) -> Option<f32>) -> SceneShapeVm {
    let params = decl
        .shape
        .param_ids()
        .map(|property| {
            let id = param_id(&item.id, property);
            ShapeParamVm {
                value: values(&id).unwrap_or_else(|| resting(item, property)),
                write: ShapeWrite::Card { id, scale: 1.0, offset: 0.0 },
                id: property.to_owned(),
                wired: false,
            }
        })
        .collect();
    SceneShapeVm { owner, index: 0, decl: decl.clone(), params, frame: None, anchor: [0.0; 3], row: Some(item.id.clone()), pickable: true }
}

/// What `item`'s card row `property` rests at before the manifest has it.
pub(super) fn resting(item: &Item, property: &str) -> f32 {
    manifold_core::scene_schema::row_for(&item.fields, property)
        .and_then(|row| row.default)
        .unwrap_or_else(|| transform_default(property))
}
